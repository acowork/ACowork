/**
 * Hand-written minimal protobuf WIRE reader for the two envelopes the mobile
 * realtime channel consumes (v1.2 §8.1): `DataEnvelope{session_state=18 |
 * session_message=15}` from `core/acowork-core/proto/mqtt_payload.proto`.
 *
 * Deliberately NOT a protobuf runtime: we read a fixed handful of fields by
 * number, so a ~60-line wire reader replaces a dependency. Unknown fields are
 * skipped per the wire spec, which makes the reader forward-compatible with
 * additive proto changes.
 *
 * Wire types: 0=varint 1=64bit 2=length-delimited 5=32bit.
 */

export interface ProtoField {
  no: number
  wt: number
  /** varint (wt 0) as number; length-delimited (wt 2) as sub-bytes. */
  val: number | Uint8Array
}

export function* readFields(buf: Uint8Array): Generator<ProtoField> {
  let i = 0
  const rdVar = (): number => {
    let shift = 1, out = 0
    for (;;) {
      const b = buf[i++] ?? 0
      out += (b & 0x7f) * shift
      if (!(b & 0x80)) return out
      shift *= 128
    }
  }
  while (i < buf.length) {
    const key = rdVar()
    const no = Math.floor(key / 8)
    const wt = key & 7
    if (wt === 0) yield { no, wt, val: rdVar() }
    else if (wt === 2) {
      const l = rdVar()
      yield { no, wt, val: buf.subarray(i, i + l) }
      i += l
    } else if (wt === 5) { yield { no, wt, val: 0 }; i += 4 }
    else if (wt === 1) { yield { no, wt, val: 0 }; i += 8 }
    else return // group types: out of scope, stop rather than misparse
  }
}

const s = (v: number | Uint8Array): string =>
  typeof v === 'number' ? '' : new TextDecoder().decode(v)
const n = (v: number | Uint8Array): number => (typeof v === 'number' ? v : 0)

/* SessionMessage oneof event numbers (mqtt_payload.proto:629). */
const EV_DONE = 6, EV_ERROR = 7, EV_STOPPED = 8, EV_QUESTION = 9, EV_APPROVAL = 19

export type RealtimeEvent =
  | { kind: 'state'; status: string; messageCount: number }
  | { kind: 'done' }
  | { kind: 'error'; error: string }
  | { kind: 'stopped' }
  | { kind: 'ask_question'; requestId: string; questionJson: string }
  | { kind: 'tool_approval_needed'; requestId: string; toolName: string; action: string; riskLevel: string; reason: string; timeoutSecs: number }
  | { kind: 'ignore' }

function decodeSessionMessage(buf: Uint8Array): RealtimeEvent {
  let ev: RealtimeEvent = { kind: 'ignore' }
  for (const f of readFields(buf)) {
    switch (f.no) {
      case EV_DONE: ev = { kind: 'done' }; break
      case EV_ERROR: {
        let msg = ''
        for (const g of readFields(f.val as Uint8Array)) if (g.no === 2) msg = s(g.val)
        ev = { kind: 'error', error: msg }
        break
      }
      case EV_STOPPED: ev = { kind: 'stopped' }; break
      case EV_QUESTION: {
        let rid = '', qj = ''
        for (const g of readFields(f.val as Uint8Array)) {
          if (g.no === 1) rid = s(g.val)
          if (g.no === 2) qj = s(g.val)
        }
        ev = { kind: 'ask_question', requestId: rid, questionJson: qj }
        break
      }
      case EV_APPROVAL: {
        const d = { request_id: '', tool_name: '', action: '', risk_level: '', reason: '', timeout: 0 }
        for (const g of readFields(f.val as Uint8Array)) {
          if (g.no === 2) d.request_id = s(g.val)
          if (g.no === 3) d.tool_name = s(g.val)
          if (g.no === 4) d.action = s(g.val)
          if (g.no === 5) d.risk_level = s(g.val)
          if (g.no === 6) d.reason = s(g.val)
          if (g.no === 8) d.timeout = n(g.val)
        }
        ev = { kind: 'tool_approval_needed', requestId: d.request_id, toolName: d.tool_name, action: d.action, riskLevel: d.risk_level, reason: d.reason, timeoutSecs: d.timeout }
        break
      }
      default: break // chunk/tool_call/... — never subscribed, ignore if seen
    }
  }
  return ev
}

function decodeSessionState(buf: Uint8Array): RealtimeEvent {
  let status = '', count = 0
  for (const f of readFields(buf)) {
    if (f.no === 3) status = s(f.val)
    if (f.no === 4) count = n(f.val)
  }
  return { kind: 'state', status, messageCount: count }
}

/** DataEnvelope{version=1, payload oneof: session_message=15, session_state=18}. */
export function decodeEnvelope(buf: Uint8Array): RealtimeEvent {
  for (const f of readFields(buf)) {
    if (f.no === 15) return decodeSessionMessage(f.val as Uint8Array)
    if (f.no === 18) return decodeSessionState(f.val as Uint8Array)
  }
  return { kind: 'ignore' }
}

/* ---- tiny encoder, tests only (hand-built golden vectors otherwise) ---- */

function wVar(n: number): number[] {
  const out: number[] = []
  do { let b = n % 128; n = Math.floor(n / 128); if (n > 0) b |= 0x80; out.push(b) } while (n > 0)
  return out
}
export function wStr(fieldNo: number, text: string): number[] {
  const b = Array.from(new TextEncoder().encode(text))
  return [...wVar(fieldNo * 8 + 2), ...wVar(b.length), ...b]
}
export function wVarint(fieldNo: number, v: number): number[] {
  return [...wVar(fieldNo * 8), ...wVar(v)]
}
export function wBytes(fieldNo: number, inner: number[]): number[] {
  return [...wVar(fieldNo * 8 + 2), ...wVar(inner.length), ...inner]
}
