/**
 * ADR-083 × ADR-076 §决策 4: `cancelCompressAction` must reach the Runtime as
 * `compress_type = 3` (CompressType::CANCEL) through the Gateway's
 * authenticated HTTP endpoint — NOT as a broker-published control command.
 *
 * Two things are pinned:
 *
 *   1. the value is 3 (CANCEL). A regression to 1 (SUMMARY) would start a
 *      *second* compaction instead of cancelling the running one, which is
 *      worse than doing nothing at all;
 *   2. the transport is HTTP. ADR-076 §决策 4 deleted the MQTT control
 *      commands because that channel carries no identity — a `compress_action`
 *      published straight to the broker skips the owner check the HTTP path
 *      performs, and `compress` was explicitly in the deleted set.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

const mockInvoke = vi.fn<[string, unknown?], Promise<unknown>>();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: unknown) => mockInvoke(cmd, args),
}));

vi.mock("../lib/logger", () => ({
  log: { debug: () => {}, info: () => {}, warn: () => {}, error: () => {} },
  setLevel: () => {},
  getLevel: () => "off" as const,
}));

import { useChatStore } from "./chatStore";

const AGENT = "com.test.Agent";
const SESSION = "sess-test";

interface FetchCall {
  url: string;
  method: string;
  body: unknown;
}

let fetchCalls: FetchCall[] = [];

beforeEach(() => {
  mockInvoke.mockClear();
  fetchCalls = [];
  vi.stubGlobal(
    "fetch",
    vi.fn((url: string, init?: { method?: string; body?: string }) => {
      fetchCalls.push({
        url: String(url),
        method: init?.method ?? "GET",
        body: init?.body ? JSON.parse(init.body) : undefined,
      });
      return Promise.resolve({
        ok: true,
        status: 202,
        text: () => Promise.resolve(""),
        json: () => Promise.resolve({}),
      });
    }),
  );
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("ADR-083: cancelCompressAction", () => {
  it("POSTs compress_type 3 (CANCEL) to the compress endpoint", async () => {
    useChatStore.getState().cancelCompressAction(AGENT, SESSION);
    await vi.waitFor(() => expect(fetchCalls).toHaveLength(1));

    expect(fetchCalls[0].method).toBe("POST");
    expect(fetchCalls[0].url).toContain(`/api/agents/${AGENT}/sessions/${SESSION}/compress`);
    expect(fetchCalls[0].body).toEqual({ compress_type: 3 });
  });

  it("does not fall back to the MQTT control plane (ADR-076 §决策 4)", async () => {
    useChatStore.getState().cancelCompressAction(AGENT, SESSION);
    await vi.waitFor(() => expect(fetchCalls).toHaveLength(1));

    expect(mockInvoke).not.toHaveBeenCalled();
  });

  it("sendCompressAction still uses compress_type 1 (SUMMARY) — the two paths must not cross", async () => {
    useChatStore.getState().sendCompressAction(AGENT, SESSION, 1);
    await vi.waitFor(() => expect(fetchCalls).toHaveLength(1));

    expect(fetchCalls[0].body).toEqual({ compress_type: 1 });
    expect(mockInvoke).not.toHaveBeenCalled();
  });
});
