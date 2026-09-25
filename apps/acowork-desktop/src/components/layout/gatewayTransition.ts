import type { GatewayAlive } from "../../stores/gatewayStore";

export interface GatewayTransitionActions {
  /** Snapshot of known agents (instance id → any). The helper only needs keys. */
  getAgentIds: () => string[];
  /** Patch an agent's liveness (mirrors `updateAgentLiveness(id, alive)`). */
  setAgentOffline: (agentId: string) => void;
  /** Drop every cached session's runtime state for an agent — releases
   *  the messages / pending approvals / tool progress / abort controllers
   *  so the underlying attachment blobs can be GC'd. */
  clearAgentSessions: (agentId: string) => void;
  /** Mark every node in the topology snapshot offline. Gateway drop
   *  edge: the snapshot is stale the moment the Gateway dies and no new
   *  `bootstrap-state` snapshot will arrive (the MQTT broker lived in
   *  the Gateway) — the sidebar node group headers would otherwise keep
   *  showing the dead nodes as online. */
  markNodesOffline: () => void;
  /** Re-run the services diagnostic probe. Called on BOTH edges: a
   *  Gateway drop makes every probe target unreachable, so a fresh
   *  pass yields the honest all-offline report (gateway_reachable
   *  false → the panel's red banner + red dots) instead of the stale
   *  pre-disconnect snapshot; a rise re-probes so the report recovers
   *  the real state. `diagnose()` never throws and self-throttles
   *  while a pass is in flight. */
  refreshServices: () => void;
  /** Reconcile the agent list + liveness from the Gateway. Called on
   *  the reconnect edge to pick up the freshly-respawned system agent. */
  fetchAgents: () => Promise<unknown>;
  /** Refetch the node topology from the Gateway. Called on the rise
   *  edge — the drop edge marked the snapshot offline, now resync it
   *  with the live topology. */
  fetchNodes: () => Promise<unknown>;
}

/**
 * Decide what to do on a Gateway-liveness transition.
 *
 * `GatewayAlive` is maintained by `gatewayStore` under the
 * single-authority model: MQTT CONNACK (the broker lives inside the
 * Gateway process) is the liveness authority; an HTTP `/health` probe
 * runs only as a subordinate death classifier while MQTT is down.
 *
 *   prev=null                 → first render (SplashScreen already drove
 *                                liveness to `alive` before AppLayout
 *                                mounts); nothing to do.
 *   alive → dead              → "drop" edge: every known agent is marked
 *                                offline and its sessions are cleared,
 *                                mirroring the natural `agent_status
 *                                offline` MQTT flow that node-stop / an
 *                                individual stopAgent would have produced.
 *                                ChatPanel's `!selectedAgent.alive` gate
 *                                then renders the sleeping screen. The
 *                                services report is re-diagnosed so the
 *                                diagnostic panel shows the honest
 *                                all-offline state (e.g. node offline)
 *                                rather than the stale snapshot, and the
 *                                node topology snapshot is marked
 *                                offline (no new `bootstrap-state` will
 *                                arrive — the broker died with the
 *                                Gateway — so the sidebar group headers
 *                                would otherwise stay green).
 *   dead → alive              → "rise" edge: pull the fresh agent list
 *                                from the Gateway (system agent gets
 *                                respawned on restart), re-probe the
 *                                services report, resync the node
 *                                topology, and let the normal
 *                                `selectAgent → fetchLatestSession →
 *                                openSession` chain re-hydrate the user's
 *                                last session.
 *   anything else             → no-op. `unknown` never fires an edge: a
 *                                link flap that was never classified
 *                                leaves agent state untouched (the old
 *                                behavior — the verdict stayed healthy
 *                                through a quick MQTT reconnect — is
 *                                preserved).
 *
 * Side-effect helper so the AppLayout effect body stays one-liner and
 * the edge-detection logic is unit-testable without rendering React.
 */
export function applyGatewayTransition(
  prev: GatewayAlive | null,
  next: GatewayAlive,
  actions: GatewayTransitionActions,
): void {
  if (prev === null) return;
  if (prev === "alive" && next === "dead") {
    actions.refreshServices();
    actions.markNodesOffline();
    for (const id of actions.getAgentIds()) {
      actions.setAgentOffline(id);
      actions.clearAgentSessions(id);
    }
  } else if (prev === "dead" && next === "alive") {
    actions.refreshServices();
    void actions.fetchAgents();
    void actions.fetchNodes();
  }
}

export interface MqttEdgeHandlers {
  /**
   * MQTT just went down. MQTT down alone does NOT mean the Gateway died
   * (only the path was cut) — start the death classifier so the verdict
   * comes from evidence, and let `applyGatewayTransition` fire once/if
   * it lands on `dead`.
   */
  onDrop: () => void;
  /**
   * MQTT just came up (CONNACK). The broker answered from inside the
   * Gateway process — sufficient proof of liveness. The wiring must
   * cancel any in-flight classifier probe (its question is moot), stop
   * the classifier watch, and mark the verdict `alive`.
   */
  onRise: () => void;
}

/**
 * Route an MQTT connection-edge transition to the liveness wiring.
 *
 * `prevUp === null` (first render) syncs with the current state: MQTT
 * already up takes the rise path (idempotent — SplashScreen's boot probe
 * likely already marked the verdict `alive`), MQTT already down starts
 * the death watch (covers a drop that happened before AppLayout
 * mounted).
 */
export function onMqttConnectionEdge(
  prevUp: boolean | null,
  up: boolean,
  handlers: MqttEdgeHandlers,
): void {
  if (prevUp === up) return;
  if (up) handlers.onRise();
  else handlers.onDrop();
}
