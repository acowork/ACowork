/**
 * The realtime seam (v1.2 §8). Upper layers call `startWatching/stopWatching`
 * and never learn which channel delivered the signal:
 *
 *   - relay shape (https baseUrl)  → MQTT-over-WSS precise subscriptions (§8.1)
 *   - local/LAN shape, or WS dead  → foreground polling (§8.3)
 *
 * Both channels emit the SAME `SessionSnapshot` (status + authoritative
 * message_count) into `onSnapshot`; the reload decision stays in chatStore
 * (one rule, §8.3 note). WS events additionally carry card details polling
 * cannot see (`tool_approval_needed`, `ask_question`) → `onEvent`.
 *
 * Channel exclusivity (§8.0): while WS is healthy the poll timer is off;
 * WS drops → polling takes over immediately and WS retries keep running in
 * the background at a slow cadence.
 */
import type { LiveStatus, SessionSnapshot } from './types'
import {
  decodeEnvelope,
  type RealtimeEvent,
} from './proto-wire'
import {
  decodePacket, encodeConnect, encodeDisconnect, encodePingreq, encodePuback,
  encodeSubscribe, PKT, type ConnectOpts,
} from './mqtt-wire'

export const POLL_INTERVAL_MS = 2000
const WS_RETRY_FAST = [1000, 2000, 4000]
const WS_RETRY_SLOW_MS = 30_000
const PING_INTERVAL_MS = 25_000
const STALE_MS = 70_000
/** A WS that never reaches CONNACK (black-holed tunnel) must not strand us:
 *  polling starts only after this fires. */
const CONNECT_TIMEOUT_MS = 8_000

export interface RealtimeHandlers {
  onSnapshot(snap: SessionSnapshot): void
  onEvent(ev: RealtimeEvent): void
  onChannel(ch: 'ws' | 'polling'): void
}

export interface WebSocketLike {
  binaryType: string
  send(data: Uint8Array): void
  close(): void
  onopen: (() => void) | null
  onmessage: ((ev: { data: ArrayBuffer | Uint8Array }) => void) | null
  onerror: (() => void) | null
  onclose: (() => void) | null
  readyState: number
}

export interface RealtimeDeps {
  /** wss URL for the MQTT bridge, or null when the shape is not relay. */
  wsUrl?(): string | null
  /** Fresh CONNECT credentials; null = no session, stay on polling. */
  connectOpts?(): Promise<ConnectOpts | null>
  /** The polling primitive: one authenticated session snapshot. */
  snapshot(agentId: string, sessionId: string): Promise<SessionSnapshot>
  /** Injectable for tests; defaults to the global WebSocket. */
  makeWs?(url: string): WebSocketLike
}

let deps: RealtimeDeps | null = null
/** Partials merge: the store wires `snapshot` with its transport, the app
 *  boot wires `wsUrl`/`connectOpts` with auth. Either may arrive first. */
export function setRealtimeDeps(d: Partial<RealtimeDeps>): void {
  deps = { ...(deps ?? {}), ...d } as RealtimeDeps
}

/* ---------------- module state (one global watch, §8.0) ---------------- */

let active: { agentId: string; sessionId: string; h: RealtimeHandlers } | null = null
let ws: WebSocketLike | null = null
let wsAlive = false
let pollTimer: ReturnType<typeof setInterval> | null = null
let retryTimer: ReturnType<typeof setTimeout> | null = null
let pingTimer: ReturnType<typeof setInterval> | null = null
let lastRx = 0
let retries = 0
let consecutiveFails = 0
let packetId = 1
let connectTimer: ReturnType<typeof setTimeout> | null = null

export function pollNetworkState(): { online: boolean; fails: number } {
  return { online: consecutiveFails === 0, fails: consecutiveFails }
}

export function channelState(): 'ws' | 'polling' | 'off' {
  if (wsAlive) return 'ws'
  if (pollTimer !== null) return 'polling'
  return 'off'
}

/* ---------------- public API ---------------- */

export function startWatching(agentId: string, sessionId: string, h: RealtimeHandlers): void {
  // Idempotent for the same session: a re-start (e.g. after `send`) must not
  // tear down a healthy WS — it only forces one immediate snapshot.
  if (active && active.agentId === agentId && active.sessionId === sessionId && (wsAlive || pollTimer !== null)) {
    void pollTick()
    return
  }
  stopWatching()
  active = { agentId, sessionId, h }
  retries = 0
  const url = deps?.wsUrl?.() ?? null
  if (url) {
    void openWs(url)
  } else {
    startPollingOnly()
  }
}

export function stopWatching(): void {
  active = null
  closeWs()
  stopPoll()
  if (retryTimer !== null) { clearTimeout(retryTimer); retryTimer = null }
  consecutiveFails = 0
}

/* ---------------- polling engine (moved out of chatStore, §8.0) ---------------- */

function startPollOnly(): void {
  if (pollTimer !== null) return
  void pollTick()
  pollTimer = setInterval(() => void pollTick(), POLL_INTERVAL_MS)
}
function startPollingOnly(): void {
  active?.h.onChannel('polling')
  startPollOnly()
}
function stopPoll(): void {
  if (pollTimer !== null) { clearInterval(pollTimer); pollTimer = null }
}

async function pollTick(): Promise<void> {
  if (!deps || !active) return
  try {
    const snap = await deps.snapshot(active.agentId, active.sessionId)
    consecutiveFails = 0
    active.h.onSnapshot(snap)
  } catch {
    consecutiveFails += 1
  }
}

/* ---------------- WS engine ---------------- */

function topics(a: string, s: string): { state: string; events: string[] } {
  const base = `acowork/agents/${a}/sessions/${s}`
  return { state: `${base}/state`, events: ['done', 'error', 'stopped', 'tool_approval_needed', 'ask_question'].map((e) => `${base}/messages/${e}`) }
}

function closeWs(): void {
  // Detach FIRST: FakeSocket/real `close()` fire `onclose` synchronously in
  // some environments, and the `ws === sock` guard would re-enter onWsDown.
  const sock = ws
  ws = null
  wsAlive = false
  if (pingTimer !== null) { clearInterval(pingTimer); pingTimer = null }
  if (connectTimer !== null) { clearTimeout(connectTimer); connectTimer = null }
  if (sock) {
    if (sock.readyState === 1) {
      try { sock.send(encodeDisconnect()) } catch { /* socket already dying */ }
    }
    try { sock.close() } catch { /* already gone */ }
  }
}

async function openWs(url: string): Promise<void> {
  if (!active || !deps) return
  const opts = await deps.connectOpts?.()
  if (!opts) { startPollingOnly(); return } // no session yet — polling carries us
  closeWs()
  // The relay bridge echoes the `mqtt` subprotocol (remote_listener.rs
  // §7.2); offering it keeps the browser handshake identical to rumqttc's.
  const sock = deps.makeWs
    ? deps.makeWs(url)
    : (new WebSocket(url, ['mqtt']) as unknown as WebSocketLike)
  ws = sock
  sock.binaryType = 'arraybuffer'
  // A TCP-level open that never yields CONNACK must not strand the session
  // without a freshness channel: arm the fallback clock at socket open.
  connectTimer = setTimeout(() => { if (ws === sock && !wsAlive) onWsDown() }, CONNECT_TIMEOUT_MS)
  sock.onopen = () => {
    if (ws !== sock) return
    sock.send(encodeConnect(opts))
  }
  sock.onmessage = (ev) => {
    if (ws !== sock) return
    lastRx = Date.now()
    const bytes = ev.data instanceof Uint8Array ? ev.data : new Uint8Array(ev.data)
    handlePacket(bytes, sock)
  }
  sock.onerror = () => { if (ws === sock) onWsDown() }
  sock.onclose = () => { if (ws === sock) onWsDown() }
}

function handlePacket(bytes: Uint8Array, sock: WebSocketLike): void {
  const d = decodePacket(bytes)
  if (!d || !active) return
  if (d.type === PKT.CONNACK) {
    if (d.code === 0) {
      const t = topics(active.agentId, active.sessionId)
      // state + cards ride QoS1 (retained, authoritative); turn-end signals
      // QoS0 — a lost `done` only delays a reload the state push will do.
      sock.send(encodeSubscribe([t.state, ...t.events], [1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0], packetId++))
    } else {
      // Rejected (bad token / shape) — credentials are retried on the next
      // attempt via connectOpts(); fall to polling meanwhile.
      onWsDown()
    }
    return
  }
  if (d.type === PKT.SUBACK) {
    if (d.codes.some((c) => c !== 0x80)) {
      wsAlive = true
      if (connectTimer !== null) { clearTimeout(connectTimer); connectTimer = null }
      stopPoll() // WS owns the freshness signal now (§8.0 exclusivity)
      retries = 0
      active.h.onChannel('ws')
      if (pingTimer !== null) clearInterval(pingTimer)
      pingTimer = setInterval(() => {
        if (Date.now() - lastRx > STALE_MS) { onWsDown(); return }
        try { sock.send(encodePingreq()) } catch { onWsDown() }
      }, PING_INTERVAL_MS)
    }
    return
  }
  if (d.type === PKT.PUBLISH) {
    const p = d.packet
    if (p.qos === 1 && p.packetId !== undefined) {
      try { sock.send(encodePuback(p.packetId)) } catch { /* racing close */ }
    }
    const topic = p.topic
    const suffix = topic.slice(topic.lastIndexOf('/') + 1)
    if (suffix === 'state' && topic.endsWith('/state')) {
      const ev = decodeEnvelope(p.payload)
      if (ev.kind === 'state') {
        active.h.onSnapshot({ status: mapStatus(ev.status), messageCount: ev.messageCount })
      }
      return
    }
    const ev = decodeEnvelope(p.payload)
    // Retained-trap rule (§8.1): a retained `ask_question` is history — the
    // card must never pop from it. Approval retained copies are still useful
    // as a detail source because chatStore gates card EXISTENCE on status.
    if (ev.kind === 'ask_question' && p.retain) return
    if (ev.kind !== 'ignore') active.h.onEvent(ev)
    return
  }
}

function mapStatus(st: string): LiveStatus | null {
  switch (st) {
    case 'idle': return { status: 'idle' }
    case 'llm_awaiting_first_chunk': return { status: 'llm_awaiting_first_chunk' }
    case 'thinking': return { status: 'thinking' }
    case 'llm_streaming': return { status: 'llm_streaming' }
    case 'tool_executing': return { status: 'tool_executing' }
    // SessionState carries no request_id; waiting_approval without the id is
    // still the existence signal the approval card gates on (§8.1).
    case 'waiting_approval': return { status: 'waiting_approval', detail: { request_id: '' } }
    case 'paused': return { status: 'paused' }
    case 'errored': return { status: 'errored' }
    default: return null
  }
}

function onWsDown(): void {
  if (!active) return
  wsAlive = false
  if (pingTimer !== null) { clearInterval(pingTimer); pingTimer = null }
  closeWs()
  // Polling takes over immediately (§8.0), WS retries fast→slow.
  active.h.onChannel('polling')
  startPollOnly()
  const delay = retries < WS_RETRY_FAST.length ? WS_RETRY_FAST[retries] : WS_RETRY_SLOW_MS
  retries += 1
  if (retryTimer !== null) clearTimeout(retryTimer)
  retryTimer = setTimeout(() => {
    retryTimer = null
    const url = deps?.wsUrl?.()
    if (url && active) void openWs(url)
  }, delay)
}
