//! MCP (Model Context Protocol) manager — connection lifecycle and tool injection.
//!
//! Manages MCP server connections and provides [`McpToolWrapper`] instances
//! that implement the built-in [`Tool`](acowork_core::tools::traits::Tool) trait,
//! enabling MCP tools to be dispatched transparently alongside native ACowork tools.

use std::path::Path;
use std::sync::Arc;

use acowork_core::protocol::McpServerConfigDef;
use acowork_core::tools::traits::Tool;
use acowork_mcp::client::McpRegistry;
use acowork_mcp::wrapper::McpToolWrapper;
use indexmap::IndexMap;

use crate::agent_config::{AgentMcpToolsConfig, McpToolDescriptor, tool_enabled_in};

/// Re-export from acowork-mcp so SessionManager can reference it.
pub use acowork_mcp::client::McpConnectionFailure;

/// Result of an asynchronous MCP server connection attempt.
///
/// Produced by a background task and applied to SessionManager
/// via [`SessionManager::apply_mcp_connection_result`].
pub type McpConnectResult = (
    Arc<McpRegistry>,
    Vec<McpToolWrapper>,
    Vec<(String, serde_json::Value)>,
    Vec<McpConnectionFailure>,
);

/// MCP connection manager.
///
/// Holds a shared [`McpRegistry`] and provides helpers for connecting
/// servers and building tool wrappers.
pub struct McpManager {
    registry: Option<Arc<McpRegistry>>,
    /// Workspace root — needed by [`Self::connect`] to reconcile
    /// `agent_mcp_tools.json` against the live MCP `tools/list`
    /// (ADR-069). When empty (the `Default`/`new()` case, including
    /// unit tests), reconciliation is skipped and the caller-supplied
    /// `tools_cfg` is used verbatim.
    work_dir: Arc<Path>,
}

impl McpManager {
    /// Create an empty MCP manager (no servers connected). Uses an
    /// empty path as the workspace root — [`Self::connect`] will still
    /// work but skips the reconciliation pass.
    pub fn new() -> Self {
        Self {
            registry: None,
            work_dir: Arc::from(Path::new("")),
        }
    }

    /// Set the workspace root for `agent_mcp_tools.json` reconciliation
    /// (ADR-069). Required in production so that every `connect` call
    /// reconciles the flat per-server tool list against the live
    /// `tools/list` before applying the filter. Cheap — just swaps an
    /// `Arc<Path>`.
    pub fn set_work_dir(&mut self, work_dir: Arc<Path>) {
        self.work_dir = work_dir;
    }

    /// Connect to MCP servers and create tool wrappers.
    ///
    /// - `configs`: list of MCP server configurations.
    /// - `tools_cfg`: per-agent allowlist from
    ///   `workspace/config/agent_mcp_tools.json` (ADR-069). Used as a
    ///   fallback when the manager has no `work_dir` set (see
    ///   [`Self::set_work_dir`]); when `work_dir` IS set, the
    ///   persisted flat list is first reconciled with the live
    ///   `tools/list` via [`reconcile_and_persist_mcp_tools`] and the
    ///   caller-supplied `tools_cfg` is ignored.
    ///
    /// Returns a tuple of:
    ///   - `Arc<McpRegistry>` — shared registry for tool dispatch
    ///   - `Vec<McpToolWrapper>` — one wrapper per MCP tool (filtered)
    ///   - `Vec<(String, serde_json::Value)>` — tool specs for LLM definitions (filtered)
    ///   - `Vec<McpConnectionFailure>` — connection failures to surface to LLM
    ///
    /// On connection failure, individual servers are skipped (logged as errors).
    /// The returned registry may be empty if no servers connected successfully.
    ///
    /// **Filtering (ADR-069):** for each `mcp_<server>__<tool>` produced
    /// by the registry's `tools/list`, the reconciled config's per-row
    /// `enabled` flag decides exposure. The raw registry still exposes
    /// every tool via [`Self::registry`] / `call_tool` — filtering is
    /// **LLM-visible** only, not transport-level.
    pub async fn connect(
        &mut self,
        configs: &[McpServerConfigDef],
        tools_cfg: &AgentMcpToolsConfig,
    ) -> (
        Arc<McpRegistry>,
        Vec<McpToolWrapper>,
        Vec<(String, serde_json::Value)>,
        Vec<McpConnectionFailure>,
    ) {
        // McpServerConfigDef is now the single source of truth for MCP config,
        // shared between acowork-core (wire format) and acowork-mcp (runtime).
        // No conversion needed — the same type flows through both crates.
        let (registry, failures) = McpRegistry::connect_all(configs)
            .await
            .expect("connect_all is non-fatal and should never fail");
        let registry = Arc::new(registry);

        // ADR-069: reconcile the persisted flat list against the live
        // `tools/list` BEFORE the filter pass. Uses the work_dir set
        // via `set_work_dir`; if the work_dir is empty (e.g. a unit
        // test), the reconciliation is a no-op and the caller-supplied
        // `tools_cfg` is used verbatim.
        let active_cfg = if self.work_dir.as_os_str().is_empty() {
            tools_cfg.clone()
        } else {
            reconcile_and_persist_mcp_tools(&self.work_dir, &registry)
        };

        // Build tool wrappers and specs from the registry, applying
        // ADR-069 per-tool filtering along the way.
        let mut wrappers = Vec::new();
        let mut specs = Vec::new();
        let mut filtered_out: usize = 0;

        for prefixed_name in registry.tool_names() {
            let prefixed = prefixed_name.clone();
            let Some((server_name, tool_name)) = split_prefixed_tool(&prefixed) else {
                tracing::warn!(
                    prefixed = %prefixed,
                    "MCP tool name missing `mcp_<server>__<tool>` shape; passing through unfiltered"
                );
                if let Some(def) = registry.get_tool_def(&prefixed) {
                    let wrapper = McpToolWrapper::new(prefixed.clone(), def, registry.clone());
                    let spec = wrapper.spec();
                    let serialized = serde_json::to_value(&spec).unwrap_or_default();
                    specs.push((spec.name.clone(), serialized));
                    wrappers.push(wrapper);
                }
                continue;
            };

            if !tool_allowed(&active_cfg, server_name, tool_name) {
                filtered_out += 1;
                tracing::debug!(
                    server = %server_name,
                    tool = %tool_name,
                    "MCP tool filtered out by agent_mcp_tools.json (ADR-069)"
                );
                continue;
            }

            if let Some(def) = registry.get_tool_def(&prefixed) {
                let wrapper = McpToolWrapper::new(prefixed.clone(), def, registry.clone());
                let spec = wrapper.spec();
                let serialized = serde_json::to_value(&spec).unwrap_or_default();
                specs.push((spec.name.clone(), serialized));
                wrappers.push(wrapper);
            }
        }

        tracing::info!(
            server_count = registry.server_count(),
            exposed_tool_count = wrappers.len(),
            filtered_out,
            failure_count = failures.len(),
            "MCP manager: connected (with ADR-069 reconcile+filter applied)"
        );

        self.registry = Some(registry.clone());
        (registry, wrappers, specs, failures)
    }

    /// Get the current MCP registry, if any servers are connected.
    pub fn registry(&self) -> Option<&Arc<McpRegistry>> {
        self.registry.as_ref()
    }

    /// Check whether any MCP servers are connected.
    pub fn is_connected(&self) -> bool {
        self.registry.as_ref().is_some_and(|r| !r.is_empty())
    }

    /// Set the registry directly (used when MCP connection results are
    /// produced by a background task and applied asynchronously).
    pub fn set_registry(&mut self, registry: Arc<McpRegistry>) {
        self.registry = Some(registry);
    }

    /// Disconnect from all MCP servers and release resources.
    ///
    /// Closes transport connections (kills stdio child processes, releases
    /// HTTP connection pools). After calling disconnect, the manager is
    /// reset to the empty state and `connect()` must be called again before
    /// using MCP tools.
    pub async fn disconnect(&mut self) {
        if let Some(registry) = self.registry.take() {
            registry.disconnect().await;
            tracing::info!("MCP manager: disconnected from all servers");
        }
    }
}

impl Default for McpManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acowork_core::protocol::McpTransportDef;

    #[test]
    fn mcp_manager_default_is_not_connected() {
        let mgr = McpManager::default();
        assert!(!mgr.is_connected());
        assert!(mgr.registry().is_none());
    }

    #[tokio::test]
    async fn connect_empty_yields_empty_registry() {
        let mut mgr = McpManager::new();
        let (registry, wrappers, specs, failures) = mgr
            .connect(&[], &crate::agent_config::AgentMcpToolsConfig::default())
            .await;
        assert!(registry.is_empty());
        assert!(wrappers.is_empty());
        assert!(specs.is_empty());
        assert!(failures.is_empty());
        assert!(!mgr.is_connected());
    }

    #[test]
    fn config_def_is_shared_type() {
        // McpServerConfigDef is now used directly by acowork-mcp,
        // no separate conversion step needed.
        let def = McpServerConfigDef {
            name: "test-server".to_string(),
            transport: McpTransportDef::Stdio,
            url: None,
            command: "test-cmd".to_string(),
            args: vec!["--verbose".to_string()],
            env: Default::default(),
            headers: Default::default(),
            tool_timeout_secs: Some(30),
            install: None,
        };
        assert_eq!(def.name, "test-server");
        assert_eq!(def.command, "test-cmd");
        assert_eq!(def.args, vec!["--verbose"]);
        assert_eq!(def.tool_timeout_secs, Some(30));
        assert!(matches!(def.transport, McpTransportDef::Stdio));
        assert!(def.url.is_none());
    }
}

/// Parse a prefixed MCP tool name into `(server_name, tool_name)`.
///
/// Format produced by `McpRegistry::connect_all`:
///   `mcp_<server_name>__<tool_name>`
fn split_prefixed_tool(prefixed: &str) -> Option<(&str, &str)> {
    let stripped = prefixed.strip_prefix("mcp_")?;
    stripped.split_once("__")
}

/// Decide whether a single `(server, tool)` pair should be exposed to
/// the LLM. ADR-069 flat-per-tool semantics:
///
/// 1. Server absent from `tools_cfg` entirely → permissive
///    "expose everything". Conservative behaviour for brand-new
///    servers the user hasn't yet configured; the next reconcile
///    (after the first successful `connect_all`) materialises the
///    flat list into the file via `merge_mcp_tools_config`.
/// 2. Server present, tool row missing from the per-server flat list
///    → conservative "expose nothing".
/// 3. Server present, tool row present → use `row.enabled` directly.
fn tool_allowed(tools_cfg: &AgentMcpToolsConfig, server_name: &str, tool_name: &str) -> bool {
    match tools_cfg.servers.get(server_name) {
        None => true,
        Some(rows) => tool_enabled_in(rows, tool_name).unwrap_or(false),
    }
}

/// Extract the live `tools/list` for every connected MCP server.
///
/// ADR-069 follow-up: returns `IndexMap`, not `HashMap`. `merge_mcp_tools_config`
/// iterates this map and writes the result to `agent_mcp_tools.json`;
/// `HashMap` would reshuffle the per-tool order on every reconnect
/// (which `PUT /mcp-tools` triggers), making the Tools-panel tool list
/// reshuffle whenever the user toggles a single tool.
pub fn collect_server_tools_from_registry(
    registry: &McpRegistry,
) -> IndexMap<String, Vec<McpToolDescriptor>> {
    let mut out: IndexMap<String, Vec<McpToolDescriptor>> = IndexMap::new();
    for prefixed in registry.tool_names() {
        let Some((server_name, tool_name)) = split_prefixed_tool(&prefixed) else {
            continue;
        };
        let Some(def) = registry.get_tool_def(&prefixed) else {
            continue;
        };
        out.entry(server_name.to_string())
            .or_default()
            .push(McpToolDescriptor {
                name: tool_name.to_string(),
                description: def.description,
            });
    }
    out
}

/// Load persisted config -> reconcile with the live registry -> write
/// back -> return the merged config used for the subsequent filter
/// pass.
pub fn reconcile_and_persist_mcp_tools(
    work_dir: &Path,
    registry: &McpRegistry,
) -> AgentMcpToolsConfig {
    let server_tools = collect_server_tools_from_registry(registry);
    let persisted = crate::agent_config::load_agent_mcp_tools_config(work_dir)
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "reconcile: failed to load persisted");
            None
        })
        .unwrap_or_default();
    let merged = crate::agent_config::merge_mcp_tools_config(&persisted, &server_tools);
    if let Err(e) = crate::agent_config::save_agent_mcp_tools_config(work_dir, &merged) {
        tracing::warn!(
            error = %e,
            "reconcile: failed to persist merged config; using in-memory copy"
        );
    } else {
        tracing::info!(
            server_count = merged.servers.len(),
            tool_count = merged.servers.values().map(|v| v.len()).sum::<usize>(),
            "reconcile: persisted agent_mcp_tools.json"
        );
    }
    merged
}

/// Resolve the Gateway-published `{node_token}` placeholder into a real
/// credential, on the in-memory copy of the MCP config only.
///
/// ## Why this exists
///
/// Under `AUTH_MODE=multi_user` the Gateway's `auth_middleware` gates every
/// `/api/*` route, and the pm / doc MCP endpoints are no exception — they
/// are Gateway reverse-proxies like any other. A Runtime that connects
/// without a machine credential gets a 401 on `tools/list`, which the
/// failure path swallows: the tool list comes back empty,
/// `agent_mcp_tools.json` reconciles to zero rows, and the Desktop Tools
/// panel renders the server with **no expandable row and no error** — it
/// just looks switched off. So the credential is not optional here.
///
/// ## Why it is safe
///
/// The resolved value is a `String` in a `McpServerConfigDef` that lives
/// only for the duration of this call. It is deliberately **never**
/// written back to `agent_mcp.json`: that file is persisted to the agent's
/// workspace *and* surfaced in the Tools panel, so a token landing there
/// would be readable by anything that can read the workspace. The
/// placeholder itself is all that ever reaches disk (the MQTT handler
/// substitutes only `{instance_id}` when it writes the catalog).
///
/// ## Why the template and not a direct value
///
/// The Gateway cannot substitute a real token: the credential is
/// node-scoped and minted at enrollment, so the Gateway has no
/// per-Runtime copy to send. It publishes the placeholder, and the
/// Runtime — which received the real value from the Node at spawn — fills
/// it in. This mirrors the existing `{instance_id}` convention.
fn resolve_node_token_template(
    configs: &[McpServerConfigDef],
    node_token: Option<&str>,
) -> Vec<McpServerConfigDef> {
    let Some(token) = node_token.filter(|t| !t.is_empty()) else {
        // No credential (standalone Runtime, or a node that never
        // enrolled). Leave the placeholder in place: the request will 401
        // and the existing failure reporting surfaces it. Substituting an
        // empty string would be worse — it would look authenticated and
        // fail somewhere less legible.
        return configs.to_vec();
    };
    configs
        .iter()
        .map(|c| {
            let mut c = c.clone();
            if let Some(v) = c.headers.get_mut(acowork_core::auth::NODE_TOKEN_HEADER)
                && v == acowork_core::auth::NODE_TOKEN_TEMPLATE
            {
                *v = token.to_string();
            }
            c
        })
        .collect()
}

/// Connect + reconcile + filter, all in one pass.
///
/// `node_token` is the machine credential the Node injected at spawn
/// (`--mqtt-password`, which carries the same node_token value the Node
/// itself uses). It is substituted into the Gateway-published
/// `{node_token}` header placeholder so pm / doc MCP survive
/// `AUTH_MODE=multi_user`; see [`resolve_node_token_template`]. Pass
/// `None` in standalone mode, where no Gateway credential exists.
pub async fn connect_mcp_with_reconcile_and_filter(
    work_dir: &Path,
    configs: &[McpServerConfigDef],
    node_token: Option<&str>,
) -> McpConnectResult {
    let configs = resolve_node_token_template(configs, node_token);
    let (registry, failures) = McpRegistry::connect_all(&configs)
        .await
        .expect("connect_all is non-fatal and should never fail");
    let registry = Arc::new(registry);
    let merged = reconcile_and_persist_mcp_tools(work_dir, &registry);

    let mut wrappers = Vec::new();
    let mut specs = Vec::new();
    let mut filtered_out: usize = 0;

    for prefixed_name in registry.tool_names() {
        let prefixed = prefixed_name.clone();
        let Some((server_name, tool_name)) = split_prefixed_tool(&prefixed) else {
            if let Some(def) = registry.get_tool_def(&prefixed) {
                let wrapper = McpToolWrapper::new(prefixed.clone(), def, registry.clone());
                let spec = wrapper.spec();
                let serialized = serde_json::to_value(&spec).unwrap_or_default();
                specs.push((spec.name.clone(), serialized));
                wrappers.push(wrapper);
            }
            continue;
        };

        if !tool_allowed(&merged, server_name, tool_name) {
            filtered_out += 1;
            continue;
        }

        if let Some(def) = registry.get_tool_def(&prefixed) {
            let wrapper = McpToolWrapper::new(prefixed.clone(), def, registry.clone());
            let spec = wrapper.spec();
            let serialized = serde_json::to_value(&spec).unwrap_or_default();
            specs.push((spec.name.clone(), serialized));
            wrappers.push(wrapper);
        }
    }

    tracing::info!(
        server_count = registry.server_count(),
        exposed_tool_count = wrappers.len(),
        filtered_out,
        failure_count = failures.len(),
        "MCP startup: connect_mcp_with_reconcile_and_filter applied ADR-069 reconcile+filter"
    );

    (registry, wrappers, specs, failures)
}

#[cfg(test)]
mod filter_tests {
    use super::*;
    use crate::agent_config::AgentMcpToolItem;
    use acowork_core::protocol::McpTransportDef;

    #[test]
    fn tool_allowed_server_absent_is_permissive() {
        let cfg = AgentMcpToolsConfig::default();
        assert!(tool_allowed(&cfg, "docling", "any_tool"));
    }

    #[test]
    fn tool_allowed_row_missing_is_conservative() {
        let mut cfg = AgentMcpToolsConfig::default();
        cfg.servers.insert(
            "pm".to_string(),
            vec![AgentMcpToolItem::new("pm_claim_task", true)],
        );
        assert!(tool_allowed(&cfg, "pm", "pm_claim_task"));
        assert!(!tool_allowed(&cfg, "pm", "pm_submit_task"));
    }

    #[test]
    fn tool_allowed_uses_row_enabled_directly() {
        let mut cfg = AgentMcpToolsConfig::default();
        cfg.servers.insert(
            "pm".to_string(),
            vec![
                AgentMcpToolItem::new("pm_claim_task", true),
                AgentMcpToolItem::new("pm_submit_task", false),
                AgentMcpToolItem::new("pm_list_projects", false),
            ],
        );
        assert!(tool_allowed(&cfg, "pm", "pm_claim_task"));
        assert!(!tool_allowed(&cfg, "pm", "pm_submit_task"));
        assert!(!tool_allowed(&cfg, "pm", "pm_list_projects"));
    }

    #[test]
    fn tool_allowed_user_disabled_wins_over_default_enabled() {
        let mut cfg = AgentMcpToolsConfig::default();
        cfg.servers.insert(
            "pm".to_string(),
            vec![
                AgentMcpToolItem::new("pm_claim_task", false),
                AgentMcpToolItem::new("pm_submit_task", true),
            ],
        );
        assert!(!tool_allowed(&cfg, "pm", "pm_claim_task"));
        assert!(tool_allowed(&cfg, "pm", "pm_submit_task"));
    }

    // ── ADR-076: `{node_token}` resolution ────────────────────────────

    /// Build a def shaped like the ones the Gateway injects for pm / doc.
    fn gateway_hosted_def(name: &str) -> McpServerConfigDef {
        let mut headers = std::collections::HashMap::new();
        headers.insert("X-MCP-Actor".to_string(), "{instance_id}".to_string());
        headers.insert(
            acowork_core::auth::NODE_TOKEN_HEADER.to_string(),
            acowork_core::auth::NODE_TOKEN_TEMPLATE.to_string(),
        );
        McpServerConfigDef {
            name: name.to_string(),
            transport: McpTransportDef::Http,
            url: Some("http://gw:19876/api/pm/mcp".to_string()),
            headers,
            ..Default::default()
        }
    }

    /// The happy path: pm / doc stop 401-ing at `auth_middleware` and the
    /// `tools/list` that drives the whole ADR-069 tool list works again.
    #[test]
    fn node_token_template_is_resolved_for_gateway_hosted_mcps() {
        let out = resolve_node_token_template(
            &[gateway_hosted_def("pm"), gateway_hosted_def("doc")],
            Some("tok-abc"),
        );
        for s in &out {
            assert_eq!(
                s.headers
                    .get(acowork_core::auth::NODE_TOKEN_HEADER)
                    .map(String::as_str),
                Some("tok-abc"),
                "{} must carry the real credential, not the placeholder",
                s.name
            );
            // The agent-identity template is a separate placeholder resolved
            // elsewhere; it must survive untouched.
            assert_eq!(
                s.headers.get("X-MCP-Actor").map(String::as_str),
                Some("{instance_id}"),
                "{}: resolving the node token must not disturb X-MCP-Actor",
                s.name
            );
        }
    }

    /// Security assertion. A third-party MCP server (user-added via the
    /// Tools panel) must never receive the node credential: it is node-scoped
    /// and unlocks the whole Gateway API, so handing it to an arbitrary
    /// remote endpoint the user configured would be a privilege escalation.
    /// Only entries carrying the Gateway-published placeholder are touched.
    #[test]
    fn node_token_is_not_injected_into_third_party_mcp_servers() {
        let mut third_party = McpServerConfigDef {
            name: "playwright".to_string(),
            transport: McpTransportDef::Http,
            url: Some("https://third-party.example.com/mcp".to_string()),
            ..Default::default()
        };
        // A user could even hand-type the placeholder — only the exact
        // template on the exact header is substituted.
        third_party.headers.insert(
            acowork_core::auth::NODE_TOKEN_HEADER.to_string(),
            "user-supplied".to_string(),
        );

        let out = resolve_node_token_template(&[third_party], Some("tok-abc"));
        assert_eq!(
            out[0]
                .headers
                .get(acowork_core::auth::NODE_TOKEN_HEADER)
                .map(String::as_str),
            Some("user-supplied"),
            "a server without the Gateway-published template must not be \
             given the node credential"
        );
        assert!(
            !out.iter()
                .any(|s| s.headers.values().any(|v| v == "tok-abc")),
            "the node token must not reach any third-party MCP config"
        );
    }

    /// Standalone Runtime: no node credential exists, so the placeholder is
    /// left in place rather than replaced with an empty string (which would
    /// look like a present-but-wrong credential and fail less legibly).
    #[test]
    fn node_token_template_left_intact_without_credential() {
        for tok in [None, Some("")] {
            let out = resolve_node_token_template(&[gateway_hosted_def("pm")], tok);
            assert_eq!(
                out[0]
                    .headers
                    .get(acowork_core::auth::NODE_TOKEN_HEADER)
                    .map(String::as_str),
                Some(acowork_core::auth::NODE_TOKEN_TEMPLATE),
                "no credential ({tok:?}) must leave the template, not blank it"
            );
        }
    }

    /// The caller's slice must not be mutated — the resolved copy is what
    /// reaches the transport, and the original may be the on-disk config
    /// that gets persisted.
    #[test]
    fn resolving_does_not_mutate_the_caller_s_config() {
        let input = vec![gateway_hosted_def("pm")];
        let _ = resolve_node_token_template(&input, Some("tok-abc"));
        assert_eq!(
            input[0]
                .headers
                .get(acowork_core::auth::NODE_TOKEN_HEADER)
                .map(String::as_str),
            Some(acowork_core::auth::NODE_TOKEN_TEMPLATE),
            "the input config must be untouched so the token can never be \
             written back to agent_mcp.json"
        );
    }
}
