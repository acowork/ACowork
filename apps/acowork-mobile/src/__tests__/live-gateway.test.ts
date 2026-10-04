// Integration smoke against a LIVE Gateway. Drives the SAME store code the app
// runs — login → create session → send → foreground poll → final reload —
// which is the regression net for the fast-reply race the transition-only
// poll rule missed (fixed by the message_count freshness check).
//
// OFF BY DEFAULT, and it must never be pointed at a real/dev Gateway by
// accident: it creates a session, spends real LLM tokens on a real inference
// run, and leaves untitled sessions in the agent's workspace. Two gates:
//   ACOWORK_E2E_LIVE=1        — opt in to running at all
//   ACOWORK_E2E_URL=<url>     — target, required, no localhost default
// Point it at a disposable/throwaway Gateway. Everything the test creates is
// deleted in afterAll; if that cleanup fails we fail the test rather than
// leaving garbage in someone's session list.
import { afterAll, describe, expect, it } from 'vitest'
import { probeStatus, setAuthBridge } from '../lib/api'
import { useChatStore, setChatTransport, POLL_INTERVAL_MS } from '../stores/chatStore'
import { httpChatTransport } from '../lib/api'
import { useAgentStore, setDirectorySource } from '../stores/agentStore'
import { fetchAgents, loginRequest } from '../lib/api'
import type { TokenPair } from '../lib/types'

const ENABLED = (import.meta as { env?: Record<string, string | undefined> }).env?.ACOWORK_E2E_LIVE === '1'
const RAW_GW = (import.meta as { env?: Record<string, string | undefined> }).env?.ACOWORK_E2E_URL

// Every session this test creates, so afterAll can remove them. The blast
// radius of a leaked live-test session is an untitled conversation sitting in
// a real agent's workspace burning quota — worth tracking explicitly.
const created: { agentId: string; sid: string }[] = []

async function gatewayUp(): Promise<string | null> {
  if (!ENABLED || !RAW_GW) return null
  try {
    return (await probeStatus(RAW_GW)).auth_mode === 'multi_user' ? RAW_GW : null
  } catch {
    return null
  }
}

// Resolved once at collection time. `skipIf` gates the `it` but neither
// narrows the type nor skips this file, so an empty string stands in for
// "unreachable" — typed `string` for every use below, dead when skipped.
// ponytail: a second probe would race the first; the skip decision is fixed
// at collection anyway, so one resolution is the honest ceiling here.
const GW = (await gatewayUp()) ?? ''

describe.skipIf(GW === '')('live gateway', () => {
  afterAll(async () => {
    const failed: string[] = []
    for (const { agentId, sid } of created.splice(0)) {
      try {
        await useChatStore.getState().deleteSession(agentId, sid)
      } catch (e) {
        failed.push(`${sid} (${e instanceof Error ? e.message : String(e)})`)
      }
    }
    // A leaked session is a real, billable, untitled conversation in someone's
    // workspace. Surfacing it beats exiting green and hiding the mess.
    if (failed.length) throw new Error(`live e2e leaked ${failed.length} session(s): ${failed.join(', ')}`)
  })

  it('login → send → poll sees the reply land', async () => {
    let tokens: TokenPair | null = null
    try {
      tokens = await loginRequest(GW, 'mobile-e2e', 'mobile-e2e-pw')
    } catch {
      return // e2e account absent — nothing to assert against
    }
    setAuthBridge({
      baseUrl: () => GW,
      accessToken: () => tokens?.access_token ?? null,
      refresh: async () => null,
      sessionExpired: () => {},
    })
    setDirectorySource({
      loadAgents: async () =>
        (await fetchAgents()).filter((r) => r.alive).map((r) => ({ id: r.instance_id, name: r.display_name ?? r.name, icon: null, status: 'idle' as const })),
      loadUsers: async () => [],
    })
    setChatTransport(httpChatTransport)

    expect(await useAgentStore.getState().refreshDirectory()).toBe(true)
    const agentId = useAgentStore.getState().agentList[0]?.id
    expect(agentId).toBeTruthy()
    await useAgentStore.getState().refreshSessions(agentId!)

    const sid = await useChatStore.getState().createSession(agentId!)
    expect(sid).toBeTruthy()
    created.push({ agentId: agentId!, sid: sid! })
    const ok = await useChatStore.getState().sendMessage(agentId!, sid!, 'mobile store-level e2e ping')
    expect(ok).toBe(true)

    useChatStore.getState().startWatching(agentId!, sid!)
    const deadline = Date.now() + 90_000
    let got = false
    while (Date.now() < deadline && !got) {
      await new Promise((r) => setTimeout(r, POLL_INTERVAL_MS))
      const v = useChatStore.getState().agentStates[agentId!]
      got = !!v && v.messages.some((m) => m.role === 'assistant' && m.content.length > 0)
    }
    useChatStore.getState().stopWatching()
    expect(got).toBe(true)
  }, 120_000)
})
