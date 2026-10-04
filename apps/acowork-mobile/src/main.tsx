import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import { App } from './App'
import { setChatTransport } from './stores/chatStore'
import { setDirectorySource, useAgentStore } from './stores/agentStore'
import { httpChatTransport, fetchAgents, fetchDirectory, tokenClaims } from './lib/api'
import { setRealtimeDeps } from './lib/realtime'
import { useAuthStore } from './stores/authStore'
import type { AgentSummary, UserSummary } from './lib/types'
import './styles/tokens.css'
import './styles/app.css'

// Wire the transport before the first render: a screen that mounts and
// immediately calls openSession would otherwise find a null transport and
// silently no-op. `httpChatTransport` is the complete implementation —
// spreading overrides over it silently dropped its 404 handling, so bind
// the object itself and nothing else.
setChatTransport(httpChatTransport)

/* Realtime channel selection (v1.2 §8.0): an https base URL is the relay
 * shape — the Gateway's strict MQTT-over-WSS bridge rides the same public
 * entry (`wss://<gw-id>.relay/mqtt`), so the phone subscribes precisely.
 * A plain http URL (LAN) has no exposed bridge: polling fallback. */
setRealtimeDeps({
  wsUrl: () => {
    const u = useAuthStore.getState().baseUrl
    return u.startsWith('https://') ? `wss://${u.slice('https://'.length)}/mqtt` : null
  },
  connectOpts: async () => {
    const auth = useAuthStore.getState()
    if (!auth.accessToken) return null
    // MQTT 3.1.1 cannot re-authenticate, so every (re)connect must carry a
    // live token — ask for a rotation before it expires (Desktop refresher
    // semantics, §8.1).
    const claims = tokenClaims(auth.accessToken)
    const nearExpiry = claims.exp !== undefined && claims.exp * 1000 - Date.now() < 60_000
    const token = nearExpiry ? await auth._refresh() : auth.accessToken
    if (!token) return null
    const sub = tokenClaims(token).sub ?? 'mobile'
    return { clientId: `user:${sub}:mobile:${deviceId()}`, username: sub, password: token }
  },
})

function deviceId(): string {
  const KEY = 'acowor...'
  try {
    let id = localStorage.getItem(KEY)
    if (!id) {
      id = crypto.randomUUID()
      localStorage.setItem(KEY, id)
    }
    return id
  } catch {
    return crypto.randomUUID()
  }
}

// Directory source: wire shapes → store shapes. The mapping lives at the
// transport boundary so no store ever sees a Gateway field name.
setDirectorySource({
  loadAgents: async (): Promise<AgentSummary[]> => {
    const rows = await fetchAgents()
    return rows.map((r) => ({
      id: r.instance_id,
      name: r.display_name ?? r.name,
      icon: r.builtin_avatar ?? null,
      status: !r.alive ? 'offline' : r.lifecycle === 'failed' ? 'error' : 'idle',
    }))
  },
  loadUsers: async (): Promise<UserSummary[]> => {
    const rows = await fetchDirectory()
    return rows.map((u) => ({ id: u.user_id, display_name: u.display_name, avatar: u.avatar ?? null }))
  },
})

// Cold start: decide the boot phase from persisted state (§7.1).
void useAuthStore.getState().boot()
// Directory + previews refresh whenever we reach `ready`.
useAuthStore.subscribe((s, prev) => {
  if (s.phase === 'ready' && prev.phase !== 'ready') void useAgentStore.getState().refreshDirectory()
})

const el = document.getElementById('root')
if (!el) throw new Error('#root not found')
createRoot(el).render(
  <StrictMode>
    <App />
  </StrictMode>,
)
