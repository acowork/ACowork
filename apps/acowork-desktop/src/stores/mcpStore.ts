//! MCP catalog and per-agent activation state management
//!
//! Manages two concerns:
//! 1. Global MCP catalog — server definitions + credentials (analogous to Vault for providers)
//! 2. Per-agent MCP activation — which servers are active for each agent

import { create } from "zustand";
import { getGatewayUrl } from "../lib/config";
import { with503Retry } from "../lib/httpRetry";
import { log } from "../lib/logger";
import { emitAgentConfigRefresh } from "../lib/refresh";
import type {
  McpCatalogEntryResponse,
  McpServerConfigDef,
  McpProbeResponse,
  McpHealthStatus,
  McpInstallSpec,
  OperationAck,
} from "../lib/types";

/** ADR-072: install run response — mirrors McpInstallRunResponse in the Gateway */
export interface McpInstallRunResponse {
  name: string;
  success: boolean;
  exit_code?: number | null;
  stdout: string;
  stderr: string;
  install_duration_ms: number;
  tool_count?: number | null;
  health_error?: string | null;
  spawn?: McpServerConfigDef | null;
}

/**
 * Coarse stage of a running install — mirrors `InstallStage` in the
 * Gateway (`GET /api/mcp-catalog/install/{name}/status`). The pipeline is
 * runtime probe → install command → MCP handshake, so the stage is what
 * turns "a spinner" into "it is pulling dependencies right now".
 */
export type McpInstallStage = "checking_runtime" | "installing" | "verifying";

/** Last terminal outcome of an install attempt, per server name. */
export interface McpInstallOutcome {
  success: boolean;
  /** Human-readable failure reason (stderr or health-check error). */
  error?: string;
  /** Raw installer stdout, shown behind a "show output" toggle. */
  stdout?: string;
}

/**
 * Per-agent in-flight `PUT /mcp-servers` controllers.
 *
 * A newer toggle aborts the previous request so a slow response cannot
 * overwrite the optimistic state written by the newer call. This is a
 * module-level Map (not in the store) because AbortController is mutable
 * and shouldn't trigger Zustand subscriptions.
 */
const inflightMcpPuts: Map<string, AbortController> = new Map();

// ── Catalog types ────────────────────────────────────────────────────

interface McpCatalogState {
  /** Server entries from the global catalog */
  catalog: McpCatalogEntryResponse[];
  /** Loading state */
  loading: boolean;
  /** Error message */
  error: string | null;
}

interface McpCatalogActions {
  /** Load the global MCP catalog from Gateway */
  loadCatalog: () => Promise<void>;
  /** Add a single server entry to the catalog */
  addServer: (config: McpServerConfigDef) => Promise<void>;
  /** Update a single server entry in the catalog */
  updateServer: (name: string, config: McpServerConfigDef) => Promise<void>;
  /** Remove a server entry from the catalog */
  removeServer: (name: string) => Promise<void>;
  /** Replace the entire catalog */
  replaceCatalog: (servers: McpServerConfigDef[]) => Promise<void>;
  /**
   * ADR-072: install a preset MCP server via the Gateway install pipeline
   * (runtime check → install → health check → write catalog). Resolves with
   * the run response; caller surfaces stdout/stderr in the row.
   */
  installMcp: (
    name: string,
    install: McpInstallSpec,
    env?: Record<string, string>,
  ) => Promise<McpInstallRunResponse>;
}

/**
 * ADR-072 install progress, keyed by server name.
 *
 * The install is a single long HTTP request (a first `npx`/`uvx` resolve
 * can run for minutes), so the row has to render its own state instead of
 * blocking on a modal — a modal can be dismissed, and dismissing it used to
 * leave the list looking untouched while the install kept running.
 */
interface McpInstallProgressState {
  /** Server names with an install in flight (row renders the busy state). */
  installing: string[];
  /** Last stage reported by the Gateway per running server. */
  installStages: Record<string, McpInstallStage | undefined>;
  /** Wall-clock ms since the install started, per running server. */
  installElapsed: Record<string, number>;
  /** Terminal outcome of the most recent attempt per server. */
  installOutcomes: Record<string, McpInstallOutcome | undefined>;
}

interface McpInstallProgressActions {
  /** Record an install as started (called before the request fires). */
  beginInstall: (name: string) => void;
  /** Record a terminal install outcome and clear the busy state. */
  endInstall: (name: string, outcome: McpInstallOutcome) => void;
  /** Drop a terminal outcome once the user has read it. */
  clearInstallOutcome: (name: string) => void;
  /**
   * Poll one running install's stage + elapsed time. Best-effort: a Gateway
   * without the status endpoint (404) leaves the row on a plain elapsed
   * counter rather than erroring — the install itself still works.
   */
  pollInstallStatus: (name: string) => Promise<void>;
}

// ── Per-agent activation types ───────────────────────────────────────

interface McpActivationState {
  /** Active MCP server names per agent (agentId -> server names) */
  activeServers: Record<string, string[]>;
  /** Loading state per agent */
  activationLoading: Record<string, boolean>;
  /**
   * Per-server reconcile-pending flag: agentId → serverName → true when
   * the user toggled that server but the backend has not yet finished
   * reconnect+reconcile (`agent_mcp_tools.json` not yet refreshed) or
   * the subsequent `/mcp-tools` re-fetch has not returned. Cleared by
   * `clearServerLoading` once the Tools panel reload handler observes
   * that the per-server tool list matches the user's intent
   * (on+non-empty / off+empty). Used to drive the in-card spinner so
   * third-party MCP reconnects (e.g. `playwright` ~5s) do not look like
   * a stuck UI.
   */
  perServerLoading: Record<string, Record<string, boolean>>;
}

interface McpHealthState {
  /** Health status per server name (serverName -> status) */
  healthStatus: Record<string, McpHealthStatus>;
  /** Last probe error per server name (serverName -> error message) */
  healthErrors: Record<string, string | null>;
  /** Tool count per server name (serverName -> count) */
  healthToolCounts: Record<string, number>;
}

interface McpHealthActions {
  /** Probe a server config (before adding) — does NOT save to catalog */
  probeServer: (config: McpServerConfigDef) => Promise<McpProbeResponse>;
  /** Probe an existing catalog entry by name */
  probeByName: (name: string) => Promise<McpProbeResponse>;
}

interface McpActivationActions {
  /** Load active MCP server names for an agent */
  loadActiveServers: (agentId: string) => Promise<void>;
  /** Set active MCP servers for an agent (replaces the entire list) */
  setActiveServers: (agentId: string, serverNames: string[]) => Promise<void>;
  /** Toggle a single MCP server on/off for an agent */
  toggleServer: (agentId: string, serverName: string) => Promise<void>;
  /**
   * Drop the reconcile-pending flag for one server. Called by the
   * Tools panel after a successful `/mcp-tools` re-fetch observes
   * the server state has settled (on+tools present / off+tools
   * cleared). Idempotent.
   */
  clearServerLoading: (agentId: string, serverName: string) => void;
}

// ── Combined store ───────────────────────────────────────────────────

export type McpStore = McpCatalogState &
  McpCatalogActions &
  McpActivationState &
  McpActivationActions &
  McpHealthState &
  McpHealthActions &
  McpInstallProgressState &
  McpInstallProgressActions;

export const useMcpStore = create<McpStore>((set, get) => ({
  // ── Catalog state ──
  catalog: [],
  loading: false,
  error: null,

  // ── Activation state ──
  activeServers: {},
  activationLoading: {},
  perServerLoading: {},

  // ── Health state ──
  healthStatus: {},
  healthErrors: {},
  healthToolCounts: {},

  // ── Install progress state ──
  installing: [],
  installStages: {},
  installElapsed: {},
  installOutcomes: {},

  // ── Install progress actions ──

  beginInstall: (name) =>
    set((s) => ({
      installing: s.installing.includes(name) ? s.installing : [...s.installing, name],
      installStages: { ...s.installStages, [name]: undefined },
      installElapsed: { ...s.installElapsed, [name]: 0 },
      installOutcomes: { ...s.installOutcomes, [name]: undefined },
    })),

  endInstall: (name, outcome) =>
    set((s) => {
      const stages = { ...s.installStages };
      const elapsed = { ...s.installElapsed };
      delete stages[name];
      delete elapsed[name];
      return {
        installing: s.installing.filter((n) => n !== name),
        installStages: stages,
        installElapsed: elapsed,
        installOutcomes: { ...s.installOutcomes, [name]: outcome },
      };
    }),

  clearInstallOutcome: (name) =>
    set((s) => ({ installOutcomes: { ...s.installOutcomes, [name]: undefined } })),

  pollInstallStatus: async (name) => {
    try {
      const resp = await fetch(
        `${getGatewayUrl()}/api/mcp-catalog/install/${encodeURIComponent(name)}/status`,
      );
      // 404 = Gateway predates the status endpoint. The install is still
      // running; the row just keeps a plain elapsed counter.
      if (!resp.ok) return;
      const data = (await resp.json()) as {
        running: boolean;
        stage?: McpInstallStage;
        elapsed_ms?: number;
      };
      set((s) => ({
        installStages: { ...s.installStages, [name]: data.stage },
        installElapsed: { ...s.installElapsed, [name]: data.elapsed_ms ?? 0 },
      }));
    } catch {
      // Network hiccup on a 2s poll — the next tick recovers.
    }
  },

  // ── Catalog actions ──

  loadCatalog: async () => {
    set({ loading: true, error: null });
    try {
      const resp = await fetch(`${getGatewayUrl()}/api/mcp-catalog`);
      if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
      const data = (await resp.json()) as { servers: McpCatalogEntryResponse[] };
      set({ catalog: data.servers, loading: false });
    } catch (e: unknown) {
      const message = e instanceof Error ? e.message : String(e);
      set({ error: message, loading: false });
    }
  },

  addServer: async (config: McpServerConfigDef) => {
    set({ loading: true, error: null });
    try {
      const resp = await fetch(`${getGatewayUrl()}/api/mcp-catalog`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ ...config }),
      });
      if (!resp.ok) {
        // On error Gateway answers with the standard `ApiError` envelope.
        const err = await resp.json().catch(() => ({ error: `HTTP ${resp.status}` }));
        throw new Error(err.error || `HTTP ${resp.status}`);
      }
      // On success Gateway answers with `OperationAck` (ADR-059 §7.3).
      // We don't surface the ack — the catalog is reloaded below and
      // the per-agent tool wiring is re-applied via the refresh event,
      // both of which pick up the just-added server. The typed
      // `OperationAck` parse is here purely so the wire-shape contract
      // is enforced at compile time (the value is discarded).
      await resp.json().catch(() => undefined) as OperationAck | undefined;
      // Reload catalog after adding
      await get().loadCatalog();
      emitAgentConfigRefresh();
    } catch (e: unknown) {
      const message = e instanceof Error ? e.message : String(e);
      set({ error: message, loading: false });
    }
  },

  updateServer: async (name: string, config: McpServerConfigDef) => {
    set({ loading: true, error: null });
    try {
      const resp = await fetch(`${getGatewayUrl()}/api/mcp-catalog/${encodeURIComponent(name)}`, {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ ...config }),
      });
      if (!resp.ok) {
        const err = await resp.json().catch(() => ({ error: `HTTP ${resp.status}` }));
        throw new Error(err.error || `HTTP ${resp.status}`);
      }
      // Reload catalog after updating
      await get().loadCatalog();
      emitAgentConfigRefresh();
    } catch (e: unknown) {
      const message = e instanceof Error ? e.message : String(e);
      set({ error: message, loading: false });
    }
  },

  removeServer: async (name: string) => {
    set({ loading: true, error: null });
    try {
      const resp = await fetch(`${getGatewayUrl()}/api/mcp-catalog/${encodeURIComponent(name)}`, {
        method: "DELETE",
      });
      if (!resp.ok) {
        const err = await resp.json().catch(() => ({ error: `HTTP ${resp.status}` }));
        throw new Error(err.error || `HTTP ${resp.status}`);
      }
      // Reload catalog after removing
      await get().loadCatalog();
      emitAgentConfigRefresh();
    } catch (e: unknown) {
      const message = e instanceof Error ? e.message : String(e);
      set({ error: message, loading: false });
    }
  },

  replaceCatalog: async (servers: McpServerConfigDef[]) => {
    set({ loading: true, error: null });
    try {
      const resp = await fetch(`${getGatewayUrl()}/api/mcp-catalog`, {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(servers),
      });
      if (!resp.ok) {
        const err = await resp.json().catch(() => ({ error: `HTTP ${resp.status}` }));
        throw new Error(err.error || `HTTP ${resp.status}`);
      }
      // Reload catalog after replacing
      await get().loadCatalog();
      emitAgentConfigRefresh();
    } catch (e: unknown) {
      const message = e instanceof Error ? e.message : String(e);
      set({ error: message, loading: false });
    }
  },

  // ── Install actions (ADR-072) ──

  installMcp: async (name: string, install: McpInstallSpec, env = {}) => {
    set({ error: null });
    get().beginInstall(name);
    const finish = (outcome: McpInstallOutcome) => get().endInstall(name, outcome);
    try {
      const resp = await fetch(`${getGatewayUrl()}/api/mcp-catalog/install`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ name, install, env }),
      });
      if (!resp.ok) {
        // Two 409s land here: a missing runtime ("Missing runtime 'uvx' …")
        // and the duplicate-install guard. Both carry structured guidance —
        // surface it verbatim on the row.
        const err = await resp.json().catch(() => ({ error: `HTTP ${resp.status}` }));
        throw new Error(err.error || `HTTP ${resp.status}`);
      }
      const data = (await resp.json()) as McpInstallRunResponse;
      if (data.success) {
        // Catalog + per-agent wiring refreshed server-side; reload here so
        // the "Install" button hides (entry now has install.state=installed).
        await get().loadCatalog();
        emitAgentConfigRefresh();
      }
      finish({
        success: data.success,
        error: data.success
          ? undefined
          : data.health_error || data.stderr || "install failed",
        stdout: data.stdout,
      });
      return data;
    } catch (e: unknown) {
      const message = e instanceof Error ? e.message : String(e);
      set({ error: message });
      finish({ success: false, error: message });
      return {
        name,
        success: false,
        stdout: "",
        stderr: message,
        install_duration_ms: 0,
      };
    }
  },

  // ── Activation actions ──

  loadActiveServers: async (agentId: string) => {
    set((s) => ({
      activationLoading: { ...s.activationLoading, [agentId]: true },
      error: null,
    }));
    try {
      // Bug B v3: 503 while the Runtime's HTTP endpoint is still being
      // discovered after an agent starts. Retrying rides out the boot
      // window instead of blanking the MCP server list.
      const resp = await with503Retry(
        () => fetch(`${getGatewayUrl()}/api/agents/${encodeURIComponent(agentId)}/tools`),
        { tag: `McpStore.loadActiveServers(${agentId})`, logger: log },
      );
      if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
      const data = await resp.json() as { mcp_servers?: string[] };
      set((s) => ({
        activeServers: { ...s.activeServers, [agentId]: data.mcp_servers ?? [] },
        activationLoading: { ...s.activationLoading, [agentId]: false },
      }));
    } catch (e: unknown) {
      const message = e instanceof Error ? e.message : String(e);
      set((s) => ({
        error: message,
        activationLoading: { ...s.activationLoading, [agentId]: false },
        activeServers: { ...s.activeServers, [agentId]: [] },
      }));
    }
  },

  setActiveServers: async (agentId: string, serverNames: string[]) => {
    // Cancel any in-flight PUT for this agent so a slow response from
    // an earlier click cannot overwrite a later toggle's optimistic state.
    const existing = inflightMcpPuts.get(agentId);
    if (existing) existing.abort();
    const controller = new AbortController();
    inflightMcpPuts.set(agentId, controller);

    // Snapshot the previous value for rollback on error.
    const previous = get().activeServers[agentId] ?? [];

    // Optimistic update: write the new active list immediately so the
    // checkbox reflects the click without waiting for the PUT round-trip.
    // The Rust trait layer validates every name against `cfg.merged()`
    // and rejects with HTTP 400 on unknowns — those rejections will
    // trigger the rollback below.
    set((s) => ({
      activationLoading: { ...s.activationLoading, [agentId]: true },
      error: null,
      activeServers: { ...s.activeServers, [agentId]: serverNames },
    }));
    try {
      const resp = await fetch(
        `${getGatewayUrl()}/api/agents/${encodeURIComponent(agentId)}/mcp-servers`,
        {
          method: "PUT",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ servers: serverNames }),
          signal: controller.signal,
        },
      );
      if (!resp.ok) {
        const err = await resp.json().catch(() => ({ error: `HTTP ${resp.status}` }));
        throw new Error(err.error || `HTTP ${resp.status}`);
      }
      // Clear the in-flight marker only if it's still ours; a newer
      // call may have replaced it while we were awaiting.
      if (inflightMcpPuts.get(agentId) === controller) {
        inflightMcpPuts.delete(agentId);
      }
      set((s) => ({
        activeServers: { ...s.activeServers, [agentId]: serverNames },
        activationLoading: { ...s.activationLoading, [agentId]: false },
      }));
    } catch (e: unknown) {
      // Aborted by a newer call → don't touch the optimistic state; the
      // newer call already wrote its own serverNames.
      if (controller.signal.aborted) return;
      if (inflightMcpPuts.get(agentId) === controller) {
        inflightMcpPuts.delete(agentId);
      }
      const message = e instanceof Error ? e.message : String(e);
      set((s) => {
        // PUT failed → roll back the active list AND clear any
        // per-server reconcile-pending flags that were set for this
        // toggle. The user will see the Switch snap back to its
        // previous state; a stuck spinner would otherwise imply
        // reconciliation is still in flight when in fact the request
        // never succeeded.
        const pending = s.perServerLoading[agentId];
        const clearedPending = pending
          ? Object.fromEntries(
                Object.entries(pending).map(([k]) => [k, false]),
              )
          : undefined;
        return {
          error: message,
          // Roll back to the snapshot taken before the optimistic write.
          activeServers: { ...s.activeServers, [agentId]: previous },
          activationLoading: { ...s.activationLoading, [agentId]: false },
          perServerLoading: clearedPending
            ? { ...s.perServerLoading, [agentId]: clearedPending }
            : s.perServerLoading,
        };
      });
    }
  },

  toggleServer: async (agentId: string, serverName: string) => {
    const currentActive = get().activeServers[agentId] ?? [];
    const isActive = currentActive.includes(serverName);
    const newServers = isActive
      ? currentActive.filter((s) => s !== serverName)
      : [...currentActive, serverName];

    // Mark this server as reconcile-pending BEFORE the PUT so the
    // spinner starts on the same frame as the optimistic Switch
    // flip. `clearServerLoading` will be called by the Tools panel
    // reload handler once the per-server tool list settles (see
    // `acowork:refresh-agent-config` in ToolsTab). We do not gate
    // the loading flag on the PUT outcome — if the PUT fails the
    // store rolls back `activeServers` and the user toggles again,
    // which will overwrite this entry naturally.
    set((s) => ({
      perServerLoading: {
        ...s.perServerLoading,
        [agentId]: {
          ...(s.perServerLoading[agentId] ?? {}),
          [serverName]: true,
        },
      },
    }));

    await get().setActiveServers(agentId, newServers);
  },

  clearServerLoading: (agentId: string, serverName: string) => {
    // Idempotent — does nothing if the flag is already false/absent
    // (the user may have toggled twice and the second call replaced
    // the flag). Zustand will only notify subscribers if the slice
    // reference actually changes, so we guard with a same-reference
    // early return when the value is already settled.
    set((s) => {
      const agentLoads = s.perServerLoading[agentId];
      if (!agentLoads?.[serverName]) return s;
      return {
        perServerLoading: {
          ...s.perServerLoading,
          [agentId]: {
            ...agentLoads,
            [serverName]: false,
          },
        },
      };
    });
  },

  // ── Health actions ──

  probeServer: async (config: McpServerConfigDef) => {
    const name = config.name;
    set((s) => ({
      healthStatus: { ...s.healthStatus, [name]: "probing" },
      healthErrors: { ...s.healthErrors, [name]: null },
    }));
    try {
      const resp = await fetch(`${getGatewayUrl()}/api/mcp-catalog/probe`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(config),
      });
      if (!resp.ok) {
        const err = await resp.json().catch(() => ({ error: `HTTP ${resp.status}` }));
        throw new Error(err.error || `HTTP ${resp.status}`);
      }
      const data = (await resp.json()) as McpProbeResponse;
      set((s) => ({
        healthStatus: { ...s.healthStatus, [name]: data.success ? "healthy" : "unhealthy" },
        healthErrors: { ...s.healthErrors, [name]: data.error ?? null },
        healthToolCounts: { ...s.healthToolCounts, [name]: data.tool_count },
      }));
      return data;
    } catch (e: unknown) {
      const message = e instanceof Error ? e.message : String(e);
      set((s) => ({
        healthStatus: { ...s.healthStatus, [name]: "unhealthy" },
        healthErrors: { ...s.healthErrors, [name]: message },
      }));
      return { success: false, tool_count: 0, tools: [], error: message, duration_ms: 0 };
    }
  },

  probeByName: async (name: string) => {
    set((s) => ({
      healthStatus: { ...s.healthStatus, [name]: "probing" },
      healthErrors: { ...s.healthErrors, [name]: null },
    }));
    try {
      const resp = await fetch(
        `${getGatewayUrl()}/api/mcp-catalog/${encodeURIComponent(name)}/probe`,
        { method: "POST" },
      );
      if (!resp.ok) {
        const err = await resp.json().catch(() => ({ error: `HTTP ${resp.status}` }));
        throw new Error(err.error || `HTTP ${resp.status}`);
      }
      const data = (await resp.json()) as McpProbeResponse;
      set((s) => ({
        healthStatus: { ...s.healthStatus, [name]: data.success ? "healthy" : "unhealthy" },
        healthErrors: { ...s.healthErrors, [name]: data.error ?? null },
        healthToolCounts: { ...s.healthToolCounts, [name]: data.tool_count },
      }));
      return data;
    } catch (e: unknown) {
      const message = e instanceof Error ? e.message : String(e);
      set((s) => ({
        healthStatus: { ...s.healthStatus, [name]: "unhealthy" },
        healthErrors: { ...s.healthErrors, [name]: message },
      }));
      return { success: false, tool_count: 0, tools: [], error: message, duration_ms: 0 };
    }
  },
}));
