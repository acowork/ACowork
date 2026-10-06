/**
 * ADR-087 — "listed but permanently loading" regression.
 *
 * Background (the reported bug): a private agent the caller had no grant
 * for was rendered in the sidebar for every account — the Gateway's list
 * filter only dropped rows that had an ownership record, so an agent
 * missing from `agent_owners.json` passed through. Clicking it hit
 * `GET /api/agents/{id}`, which IS gated, and got a 404.
 *
 * Every session call swallowed its error, so `sessionTitle` stayed
 * `undefined` forever and the row showed its pulse skeleton
 * indefinitely — indistinguishable from "still loading".
 *
 * The backend now filters those agents out of the list (fail-closed).
 * This suite pins the client half that must still hold when a grant is
 * revoked between two list polls: a refusal becomes a visible reason,
 * never an endless spinner.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: () => Promise.reject(new Error("not used in this test")),
}));

vi.mock("../lib/logger", () => ({
  log: { debug: () => {}, info: () => {}, warn: () => {}, error: () => {} },
  setLevel: () => {},
  getLevel: () => "off" as const,
}));

vi.mock("../lib/profileStore", () => ({
  loadAllProfiles: () => ({}),
  loadProfile: () => null,
  saveProfile: () => {},
  DEFAULT_PROFILE: {
    language: "zh-CN",
    timezone: "Asia/Shanghai",
    model_override: null,
    reasoning_effort: null,
    auto_approve_tools: [],
    yolo_mode: false,
  },
}));

import { useAgentStore } from "./agentStore";
import type { AgentInfo } from "./agentStore";

const AGENT_ID = "com.acowork.senior-engineer";
const INSTANCE_ID = "aaaaaaaa-0b1b-4c2c-8d3d-9e4e5f6a7b8c";

function makeMeta(overrides: Partial<AgentInfo> = {}): AgentInfo {
  return {
    agent_id: AGENT_ID,
    instance_id: INSTANCE_ID,
    name: "Senior Engineer",
    version: "1.0.0",
    avatar: null,
    builtin_avatar: null,
    display_name: null,
    role: null,
    alive: true,
    ready: true,
    debug_state: "disabled",
    debug_port: null,
    workspace: "",
    workspace_config_json: null,
    current_embed_dim: null,
    migration: null,
    started_at: "2026-01-01T00:00:00Z",
    last_interaction_at: "2026-01-01T00:00:00Z",
    ...overrides,
  };
}

function seedAgent() {
  useAgentStore.setState({
    agents: {
      [INSTANCE_ID]: {
        meta: makeMeta(),
        profile: {} as never,
        sessions: [],
        // `undefined` is what the sidebar renders as the loading skeleton.
        sessionTitle: undefined,
        pagination: { currentPage: 1, totalPages: 1, totalCount: 0, pageSize: 20 },
        isLoading: false,
        accessDenied: undefined,
      },
    },
  });
}

/** Install a `fetch` stub answering the latest-session probe. */
function stubFetch(status: number, body: string) {
  const spy = vi.fn(async () => new Response(body, { status }));
  vi.stubGlobal("fetch", spy);
  return spy;
}

beforeEach(() => {
  useAgentStore.setState({ agents: {} });
});
afterEach(() => {
  vi.unstubAllGlobals();
  vi.clearAllMocks();
});

describe("ADR-087 access-denied handling", () => {
  it("marks the agent denied on a 403 from the session control plane", async () => {
    seedAgent();
    stubFetch(403, JSON.stringify({ error: "forbidden", code: "not_authorized" }));

    await useAgentStore.getState().fetchLatestSession(AGENT_ID);

    expect(useAgentStore.getState().agents[INSTANCE_ID].accessDenied).toBe(true);
  });

  it("treats a bare `not found` 404 as a visibility refusal, not 'no session'", async () => {
    // ADR-087 D5: the permission middleware answers 404 with
    // `{"error":"not found"}` so the agent's existence is not leaked.
    seedAgent();
    stubFetch(404, JSON.stringify({ error: "not found" }));

    const res = await useAgentStore.getState().fetchLatestSession(AGENT_ID);

    expect(res.status).toBe("unavailable");
    expect(useAgentStore.getState().agents[INSTANCE_ID].accessDenied).toBe(true);
  });

  it("still reads a genuine 404 as 'no session' and does NOT flag the agent", async () => {
    // ADR-085 D4: the Runtime answers 404 when it is SESSIONS_READY and
    // the account simply has no session. That is a normal first-run state
    // and must keep driving the create-a-session path — flagging it would
    // show "no access" to a user who is perfectly authorized.
    seedAgent();
    stubFetch(404, JSON.stringify({ error: "session not found" }));

    const res = await useAgentStore.getState().fetchLatestSession(AGENT_ID);

    expect(res.status).toBe("no_session");
    expect(useAgentStore.getState().agents[INSTANCE_ID].accessDenied).toBeUndefined();
  });

  it("clears a stale denial once the Gateway accepts a call again", async () => {
    seedAgent();
    useAgentStore.setState((s) => ({
      agents: {
        [INSTANCE_ID]: { ...s.agents[INSTANCE_ID], accessDenied: true },
      },
    }));
    stubFetch(200, JSON.stringify({ session_id: "s1", title: "hello", created_at: null }));

    await useAgentStore.getState().fetchLatestSession(AGENT_ID);

    expect(useAgentStore.getState().agents[INSTANCE_ID].accessDenied).toBe(false);
    expect(useAgentStore.getState().agents[INSTANCE_ID].sessionTitle).toBe("hello");
  });
});
