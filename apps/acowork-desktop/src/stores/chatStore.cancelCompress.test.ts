/**
 * ADR-083 self-check: `cancelCompressAction` must publish the
 * `compress_action` control command with `compress_type = 3`
 * (CompressType::CANCEL). If the value regressed to 1 (SUMMARY) the button
 * would start a *second* compaction instead of cancelling the running one.
 */
import { describe, it, expect, vi, beforeEach } from "vitest";

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

describe("ADR-083: cancelCompressAction", () => {
  beforeEach(() => mockInvoke.mockClear());

  it("publishes compress_action with compress_type 3 (CANCEL)", () => {
    mockInvoke.mockResolvedValue(undefined);
    useChatStore.getState().cancelCompressAction(AGENT, SESSION);
    expect(mockInvoke).toHaveBeenCalledWith("mqtt_publish_control", {
      instanceId: AGENT,
      command: "compress_action",
      payloadJson: { session_id: SESSION, compress_type: 3 },
    });
  });

  it("sendCompressAction still uses compress_type 1 (SUMMARY) — the two paths must not cross", () => {
    mockInvoke.mockResolvedValue(undefined);
    useChatStore.getState().sendCompressAction(AGENT, SESSION, 1);
    expect(mockInvoke).toHaveBeenCalledWith("mqtt_publish_control", {
      instanceId: AGENT,
      command: "compress_action",
      payloadJson: { session_id: SESSION, compress_type: 1 },
    });
  });
});
