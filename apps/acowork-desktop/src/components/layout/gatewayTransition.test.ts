/**
 * Self-check for the Gateway-liveness transition handler.
 *
 * Background: the liveness verdict (`GatewayAlive`, maintained by
 * `gatewayStore`) drives the agent-side UI state. MQTT CONNACK is the
 * liveness authority (the broker lives inside the Gateway process);
 * the HTTP `/health` probe is a subordinate death classifier that runs
 * only while MQTT is down. When the verdict lands on `dead`, nothing
 * else will arrive to refresh the UI — the broker died with the
 * Gateway — so the drop edge must mark agents offline, release their
 * sessions and mark the node snapshot offline; the rise re-pulls the
 * agent list + node topology.
 *
 * Properties verified:
 *   1. First render (prev=null) is a no-op regardless of next.
 *   2. `alive → dead` flips every known agent offline AND releases its
 *      session runtime state (so attachment blobs can be GC'd — see
 *      `clearAgentSessions` for the GC contract).
 *   3. `dead → alive` triggers `fetchAgents` to reconcile and re-hydrate
 *      the user's last session.
 *   4. `unknown` never fires an edge — an unclassified link flap
 *      leaves the UI untouched.
 *   5. Same-state transitions are no-ops.
 */
import { describe, it, expect, vi } from "vitest";
import {
  applyGatewayTransition,
  onMqttConnectionEdge,
  type GatewayTransitionActions,
} from "./gatewayTransition";

function makeActions(): GatewayTransitionActions & {
  setAgentOffline: ReturnType<typeof vi.fn>;
  clearAgentSessions: ReturnType<typeof vi.fn>;
  refreshServices: ReturnType<typeof vi.fn>;
  markNodesOffline: ReturnType<typeof vi.fn>;
  fetchAgents: ReturnType<typeof vi.fn>;
  fetchNodes: ReturnType<typeof vi.fn>;
  getAgentIds: ReturnType<typeof vi.fn>;
} {
  return {
    getAgentIds: vi.fn(() => []),
    setAgentOffline: vi.fn(),
    clearAgentSessions: vi.fn(),
    refreshServices: vi.fn(),
    markNodesOffline: vi.fn(),
    fetchAgents: vi.fn(async () => undefined),
    fetchNodes: vi.fn(async () => undefined),
  };
}

describe("applyGatewayTransition", () => {
  it("is a no-op on first render (prev=null) regardless of next", () => {
    const a = makeActions();
    applyGatewayTransition(null, "alive", a);
    applyGatewayTransition(null, "dead", a);
    applyGatewayTransition(null, "unknown", a);
    expect(a.setAgentOffline).not.toHaveBeenCalled();
    expect(a.clearAgentSessions).not.toHaveBeenCalled();
    expect(a.refreshServices).not.toHaveBeenCalled();
    expect(a.markNodesOffline).not.toHaveBeenCalled();
    expect(a.fetchAgents).not.toHaveBeenCalled();
    expect(a.fetchNodes).not.toHaveBeenCalled();
  });

  it("on alive→dead marks every agent offline AND clears its sessions", () => {
    const a = makeActions();
    a.getAgentIds.mockReturnValue(["agent-a", "agent-b", "agent-c"]);
    applyGatewayTransition("alive", "dead", a);
    expect(a.setAgentOffline).toHaveBeenCalledTimes(3);
    expect(a.setAgentOffline).toHaveBeenNthCalledWith(1, "agent-a");
    expect(a.setAgentOffline).toHaveBeenNthCalledWith(2, "agent-b");
    expect(a.setAgentOffline).toHaveBeenNthCalledWith(3, "agent-c");
    expect(a.clearAgentSessions).toHaveBeenCalledTimes(3);
    expect(a.clearAgentSessions).toHaveBeenNthCalledWith(1, "agent-a");
    expect(a.clearAgentSessions).toHaveBeenNthCalledWith(2, "agent-b");
    expect(a.clearAgentSessions).toHaveBeenNthCalledWith(3, "agent-c");
    // The services diagnostic report must also be re-probed on drop —
    // the Gateway hosted every probe target, so a fresh pass yields the
    // honest all-offline report instead of the stale snapshot. The node
    // topology snapshot goes offline too (no new `bootstrap-state` will
    // arrive to refresh it — the broker died with the Gateway).
    expect(a.refreshServices).toHaveBeenCalledTimes(1);
    expect(a.markNodesOffline).toHaveBeenCalledTimes(1);
    expect(a.fetchAgents).not.toHaveBeenCalled();
    expect(a.fetchNodes).not.toHaveBeenCalled();
  });

  it("drop with zero agents still re-probes services AND marks nodes offline (Gateway is gone regardless)", () => {
    const a = makeActions();
    a.getAgentIds.mockReturnValue([]);
    applyGatewayTransition("alive", "dead", a);
    expect(a.setAgentOffline).not.toHaveBeenCalled();
    expect(a.clearAgentSessions).not.toHaveBeenCalled();
    // Even with no agents, the diagnostic report must not stay stale —
    // and the node topology snapshot must not keep showing dead nodes.
    expect(a.refreshServices).toHaveBeenCalledTimes(1);
    expect(a.markNodesOffline).toHaveBeenCalledTimes(1);
  });

  it("on dead→alive triggers fetchAgents AND a services re-probe AND a nodes refetch", () => {
    const a = makeActions();
    a.getAgentIds.mockReturnValue(["agent-x"]);
    applyGatewayTransition("dead", "alive", a);
    expect(a.fetchAgents).toHaveBeenCalledTimes(1);
    expect(a.fetchNodes).toHaveBeenCalledTimes(1);
    expect(a.setAgentOffline).not.toHaveBeenCalled();
    expect(a.clearAgentSessions).not.toHaveBeenCalled();
    expect(a.markNodesOffline).not.toHaveBeenCalled();
    // The diagnostic report recovers the real (live) state on rise.
    expect(a.refreshServices).toHaveBeenCalledTimes(1);
  });

  it("unknown never fires an edge (unclassified link flap leaves the UI untouched)", () => {
    const a = makeActions();
    a.getAgentIds.mockReturnValue(["agent-x"]);
    // Startup: nothing verified yet → alive is a sync, not a "rise".
    applyGatewayTransition("unknown", "alive", a);
    // Classifier verdict before any liveness was confirmed → no "drop".
    applyGatewayTransition("unknown", "dead", a);
    expect(a.setAgentOffline).not.toHaveBeenCalled();
    expect(a.clearAgentSessions).not.toHaveBeenCalled();
    expect(a.refreshServices).not.toHaveBeenCalled();
    expect(a.markNodesOffline).not.toHaveBeenCalled();
    expect(a.fetchAgents).not.toHaveBeenCalled();
    expect(a.fetchNodes).not.toHaveBeenCalled();
  });

  it("same-state transitions are no-ops (alive→alive, dead→dead)", () => {
    const a = makeActions();
    a.getAgentIds.mockReturnValue(["agent-x"]);
    applyGatewayTransition("alive", "alive", a);
    applyGatewayTransition("dead", "dead", a);
    expect(a.setAgentOffline).not.toHaveBeenCalled();
    expect(a.clearAgentSessions).not.toHaveBeenCalled();
    expect(a.refreshServices).not.toHaveBeenCalled();
    expect(a.markNodesOffline).not.toHaveBeenCalled();
    expect(a.fetchAgents).not.toHaveBeenCalled();
    expect(a.fetchNodes).not.toHaveBeenCalled();
  });

  it("a full alive→dead→alive sequence drops once and rises once", () => {
    const a = makeActions();
    a.getAgentIds.mockReturnValue(["agent-x"]);

    applyGatewayTransition("alive", "dead", a);
    expect(a.setAgentOffline).toHaveBeenCalledTimes(1);
    expect(a.clearAgentSessions).toHaveBeenCalledTimes(1);
    expect(a.refreshServices).toHaveBeenCalledTimes(1);
    expect(a.markNodesOffline).toHaveBeenCalledTimes(1);
    expect(a.fetchAgents).not.toHaveBeenCalled();

    applyGatewayTransition("dead", "alive", a);
    expect(a.setAgentOffline).toHaveBeenCalledTimes(1); // still just the drop
    expect(a.clearAgentSessions).toHaveBeenCalledTimes(1);
    expect(a.refreshServices).toHaveBeenCalledTimes(2); // once on drop, once on rise
    expect(a.markNodesOffline).toHaveBeenCalledTimes(1); // drop only
    expect(a.fetchAgents).toHaveBeenCalledTimes(1); // rise only
    expect(a.fetchNodes).toHaveBeenCalledTimes(1); // rise only
  });
});

describe("onMqttConnectionEdge", () => {
  it("first render syncs with the current state: up → onRise, down → onDrop", () => {
    const onRise = vi.fn();
    const onDrop = vi.fn();
    onMqttConnectionEdge(null, true, { onRise, onDrop });
    expect(onRise).toHaveBeenCalledTimes(1);
    expect(onDrop).not.toHaveBeenCalled();

    onMqttConnectionEdge(null, false, { onRise, onDrop });
    expect(onRise).toHaveBeenCalledTimes(1);
    expect(onDrop).toHaveBeenCalledTimes(1);
  });

  it("does NOT fire when the MQTT state is unchanged", () => {
    const onRise = vi.fn();
    const onDrop = vi.fn();
    onMqttConnectionEdge(true, true, { onRise, onDrop });
    onMqttConnectionEdge(false, false, { onRise, onDrop });
    expect(onRise).not.toHaveBeenCalled();
    expect(onDrop).not.toHaveBeenCalled();
  });

  it("drop edge (connected → down) routes to onDrop (start the death classifier)", () => {
    const onRise = vi.fn();
    const onDrop = vi.fn();
    onMqttConnectionEdge(true, false, { onRise, onDrop });
    expect(onDrop).toHaveBeenCalledTimes(1);
    expect(onRise).not.toHaveBeenCalled();
  });

  it("rise edge (down → connected) routes to onRise (cancel in-flight probe, mark alive)", () => {
    const onRise = vi.fn();
    const onDrop = vi.fn();
    onMqttConnectionEdge(false, true, { onRise, onDrop });
    expect(onRise).toHaveBeenCalledTimes(1);
    expect(onDrop).not.toHaveBeenCalled();
  });

  it("fires exactly one handler per edge across a full drop→rise cycle", () => {
    const onRise = vi.fn();
    const onDrop = vi.fn();
    onMqttConnectionEdge(true, false, { onRise, onDrop }); // drop
    onMqttConnectionEdge(false, false, { onRise, onDrop }); // settled down
    onMqttConnectionEdge(false, true, { onRise, onDrop }); // rise
    expect(onDrop).toHaveBeenCalledTimes(1);
    expect(onRise).toHaveBeenCalledTimes(1);
  });
});
