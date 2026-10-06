/**
 * The memory panel must not forget a rebuild that is still running.
 *
 * A rebuild executes detached in the Gateway and Runtime; switching agents
 * only dropped the UI's handle on it (`clearMemory` cleared the flag and the
 * poll), so the panel came back showing an armed Rebuild button whose only
 * effect was to queue a second migration over the same store. `resumeRebuild`
 * re-attaches from the Gateway's own state, which is the authoritative one.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

vi.mock("../lib/config", () => ({ getGatewayUrl: () => "http://gw.test" }));
vi.mock("../lib/logger", () => ({
  log: { debug: () => {}, info: () => {}, warn: () => {}, error: () => {} },
}));
vi.mock("../i18n", () => ({
  default: { t: (key: string) => key },
}));
vi.mock("../lib/gateway-api", () => ({
  fetchEmbeddingModels: vi.fn(),
  startMigration: vi.fn(),
}));

const gateway = {
  status: "connected",
  migrationProgress: {} as Record<string, unknown>,
  pollMigrationProgress: vi.fn(async () => true),
};
vi.mock("./gatewayStore", () => ({
  useGatewayStore: { getState: () => gateway },
}));

import { useMemoryStore } from "./memoryStore";

const AGENT = "1de45b14-0000-4000-8000-000000000000";

function running() {
  gateway.migrationProgress = {
    [AGENT]: {
      instance_id: AGENT,
      request_id: "r1",
      target_model_id: "bge-m3",
      target_dimension: 1024,
      progress: { rebuilt: 36, total_scanned: 1024, errors: 0, phase: "reembed", label: "running" },
      done: false,
      error: null,
    },
  };
}

beforeEach(() => {
  gateway.status = "connected";
  gateway.migrationProgress = {};
  gateway.pollMigrationProgress.mockClear();
  // The finish path refreshes stats over HTTP; this test is about the flag.
  useMemoryStore.setState({ fetchStats: async () => {} });
});

afterEach(() => {
  // Stop any poll a test started so it cannot leak into the next one.
  useMemoryStore.getState().clearMemory();
  vi.useRealTimers();
});

describe("memoryStore.resumeRebuild", () => {
  it("re-arms the in-progress state for a rebuild the Gateway is still running", async () => {
    running();
    expect(useMemoryStore.getState().migrationInProgress).toBe(false);

    await useMemoryStore.getState().resumeRebuild(AGENT);

    expect(useMemoryStore.getState().migrationInProgress).toBe(true);
  });

  it("leaves the button alone when the last rebuild has finished", async () => {
    gateway.migrationProgress = {
      [AGENT]: { instance_id: AGENT, done: true, error: null },
    };

    await useMemoryStore.getState().resumeRebuild(AGENT);

    expect(useMemoryStore.getState().migrationInProgress).toBe(false);
  });

  it("watches only its own agent - another agent migrating is not our business", async () => {
    gateway.migrationProgress = {
      "other-agent": { instance_id: "other-agent", done: false, error: null },
    };

    await useMemoryStore.getState().resumeRebuild(AGENT);

    expect(useMemoryStore.getState().migrationInProgress).toBe(false);
  });

  it("does not trust a cached entry while the Gateway is unreachable", async () => {
    running();
    gateway.status = "disconnected";

    await useMemoryStore.getState().resumeRebuild(AGENT);

    expect(useMemoryStore.getState().migrationInProgress).toBe(false);
    expect(gateway.pollMigrationProgress).not.toHaveBeenCalled();
  });

  it("releases the flag once the rebuild it re-attached to completes", async () => {
    vi.useFakeTimers();
    running();
    await useMemoryStore.getState().resumeRebuild(AGENT);
    expect(useMemoryStore.getState().migrationInProgress).toBe(true);

    gateway.migrationProgress = {
      [AGENT]: { instance_id: AGENT, done: true, error: null },
    };
    await vi.advanceTimersByTimeAsync(2000);

    expect(useMemoryStore.getState().migrationInProgress).toBe(false);
  });
});
