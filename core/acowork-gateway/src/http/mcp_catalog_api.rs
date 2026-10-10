//! MCP Catalog HTTP API handlers
//!
//! Manages the global MCP server catalog — a shared registry of server
//! definitions (including credentials/API keys) that all agents can
//! selectively activate. Analogous to the Vault for LLM providers.
//!
//! - GET    /api/mcp-catalog         — list all catalog entries (env values masked)
//! - PUT    /api/mcp-catalog         — replace the entire catalog
//! - POST   /api/mcp-catalog         — add a single server entry
//! - DELETE /api/mcp-catalog/{name}   — remove a server entry
//!
//! ADR-090: install execution moved to the Agent (Runtime) process —
//! the Gateway is registry + key vault only. The former
//! `/api/mcp-catalog/install*` endpoints are removed. Runtime health
//! checks (probing) are likewise removed: a stdio probe spawns a process
//! on whatever machine the Gateway runs on, which is meaningless once
//! Runtimes live on arbitrary Nodes. Server usability is verified by the
//! Agent at first enable (see `acowork-runtime/src/mcp_ensure.rs`).

use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{delete, get},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::http::routes::{ApiError, AppState, OperationAck};
use crate::resource_cache;
use acowork_core::operation::{OperationRecord, OperationState};
use acowork_core::protocol::McpServerConfigDef;

/// Build the MCP catalog router
pub fn mcp_catalog_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/mcp-catalog",
            get(list_catalog)
                .put(replace_catalog)
                .post(add_catalog_entry),
        )
        .route(
            "/api/mcp-catalog/{name}",
            delete(remove_catalog_entry).put(update_catalog_entry),
        )
}

// ── Persistence helpers ──────────────────────────────────────────────

/// Build the path to the MCP catalog file.
fn catalog_path(data_dir: &std::path::Path) -> PathBuf {
    data_dir.join("mcp_catalog.json")
}

/// Load the MCP catalog from disk.
/// Returns an empty Vec if the file does not exist.
pub fn load_mcp_catalog(data_dir: &std::path::Path) -> Result<Vec<McpServerConfigDef>, String> {
    let path = catalog_path(data_dir);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw =
        std::fs::read_to_string(&path).map_err(|e| format!("Failed to read MCP catalog: {}", e))?;
    serde_json::from_str(&raw).map_err(|e| format!("Failed to parse MCP catalog: {}", e))
}

/// Save the MCP catalog to disk.
pub fn save_mcp_catalog(
    data_dir: &std::path::Path,
    catalog: &[McpServerConfigDef],
) -> Result<(), String> {
    let json = serde_json::to_string_pretty(catalog)
        .map_err(|e| format!("Failed to serialize MCP catalog: {}", e))?;
    std::fs::write(catalog_path(data_dir), json)
        .map_err(|e| format!("Failed to write MCP catalog: {}", e))?;
    tracing::info!(count = catalog.len(), "MCP catalog saved");
    Ok(())
}

// ── Masking helper ───────────────────────────────────────────────────

/// A field key (env var name or HTTP header name) whose value is a secret and
/// must be masked in API responses. `authorization` is included so a Bearer
/// token header is treated as sensitive the same way an `*_API_KEY` env is.
fn is_sensitive_key(name: &str) -> bool {
    let lower = name.to_lowercase();
    [
        "key",
        "token",
        "secret",
        "password",
        "authorization",
    ]
    .iter()
    .any(|kw| lower.contains(kw))
}

/// Replace sensitive values in a key/value map with "••••" (see is_sensitive_key).
fn mask_sensitive_map(
    map: &std::collections::HashMap<String, String>,
) -> std::collections::HashMap<String, String> {
    map.iter()
        .map(|(k, v)| {
            (
                k.clone(),
                if is_sensitive_key(k) {
                    "••••".to_string()
                } else {
                    v.clone()
                },
            )
        })
        .collect()
}

/// Whether a config holds any secret (a sensitive env key or header key) that
/// GET masks and PUT preserves — drives the "has API key" badge in the UI.
fn has_sensitive_fields(config: &McpServerConfigDef) -> bool {
    config.env.keys().any(|k| is_sensitive_key(k))
        || config.headers.keys().any(|k| is_sensitive_key(k))
}

/// Mask sensitive env values for API responses.
/// Returns a copy of the config with env values containing "key", "token",
/// "secret", or "password" in their key name replaced with "•••". Sensitive
/// HTTP header values are masked the same way.
fn mask_sensitive_env(config: &McpServerConfigDef) -> McpServerConfigDef {
    let masked_env = mask_sensitive_map(&config.env);
    let masked_headers = mask_sensitive_map(&config.headers);

    McpServerConfigDef {
        name: config.name.clone(),
        display_name: config.display_name.clone(),
        transport: config.transport.clone(),
        url: config.url.clone(),
        command: config.command.clone(),
        args: config.args.clone(),
        env: masked_env,
        headers: masked_headers,
        tool_timeout_secs: config.tool_timeout_secs,
        install: config.install.clone(),
        required_secrets: config.required_secrets.clone(),
    }
}

// ── Response types ───────────────────────────────────────────────────

/// Catalog entry response (env values with sensitive fields masked)
#[derive(Serialize)]
pub struct McpCatalogEntryResponse {
    #[serde(flatten)]
    pub config: McpServerConfigDef,
    /// Whether this entry has sensitive env vars that are masked
    pub has_secrets: bool,
}

/// Full catalog response
#[derive(Serialize)]
pub struct McpCatalogResponse {
    pub servers: Vec<McpCatalogEntryResponse>,
}

/// Request to add a single MCP server entry
#[derive(Deserialize)]
pub struct AddCatalogEntryRequest {
    #[serde(flatten)]
    pub config: McpServerConfigDef,
    /// ADR-059 §7.3: the BootstrapState `version` the client read
    /// before writing (optimistic concurrency — stale clients are
    /// rejected with `resource_version_conflict`). Absent → no
    /// precondition.
    #[serde(default)]
    pub expected_version: Option<u64>,
}

/// Request to update a single MCP server entry
#[derive(Deserialize)]
pub struct UpdateCatalogEntryRequest {
    #[serde(flatten)]
    pub config: McpServerConfigDef,
}

/// Generic message response
#[derive(Serialize)]
pub struct MessageResponse {
    pub message: String,
}

// ── Handlers ──────────────────────────────────────────────────────────

/// `GET /api/mcp-catalog` — list all MCP server definitions (env values masked)
pub async fn list_catalog(
    State(state): State<AppState>,
) -> Result<Json<McpCatalogResponse>, ApiError> {
    let data_dir = get_data_dir(&state).await?;
    let catalog = load_mcp_catalog(&data_dir).map_err(|e| ApiError::internal(&e))?;

    let servers: Vec<McpCatalogEntryResponse> = catalog
        .iter()
        .map(|c| {
            let masked = mask_sensitive_env(c);
            let has_secrets = has_sensitive_fields(c);
            McpCatalogEntryResponse {
                config: masked,
                has_secrets,
            }
        })
        .collect();

    Ok(Json(McpCatalogResponse { servers }))
}

/// `PUT /api/mcp-catalog` — replace the entire catalog
pub async fn replace_catalog(
    State(state): State<AppState>,
    Json(new_catalog): Json<Vec<McpServerConfigDef>>,
) -> Result<Json<McpCatalogResponse>, ApiError> {
    // Validate: no duplicate names
    let mut seen = std::collections::HashSet::new();
    for entry in &new_catalog {
        if !seen.insert(entry.name.clone()) {
            return Err(ApiError::bad_request(&format!(
                "Duplicate MCP server name: '{}'",
                entry.name
            )));
        }
        if entry.name.is_empty() {
            return Err(ApiError::bad_request("MCP server name must not be empty"));
        }
    }

    let data_dir = get_data_dir(&state).await?;
    save_mcp_catalog(&data_dir, &new_catalog).map_err(|e| ApiError::internal(&e))?;

    // Rebuild mcp_list cache for AgentHello diff sync.
    {
        let mut gw = state.gateway_state.write().await;
        resource_cache::rebuild_and_save_mcp_cache(&mut gw, &data_dir, &new_catalog);
    }

    // Hot-push MCP config — handled by MQTT publisher trigger.
    // (The old ResourcePusher stub has been removed.)

    // Return masked response
    let servers: Vec<McpCatalogEntryResponse> = new_catalog
        .iter()
        .map(|c| {
            let masked = mask_sensitive_env(c);
            let has_secrets = has_sensitive_fields(c);
            McpCatalogEntryResponse {
                config: masked,
                has_secrets,
            }
        })
        .collect();

    Ok(Json(McpCatalogResponse { servers }))
}

/// `POST /api/mcp-catalog` — add a single server entry
pub async fn add_catalog_entry(
    State(state): State<AppState>,
    Json(body): Json<AddCatalogEntryRequest>,
) -> Result<(StatusCode, Json<OperationAck>), ApiError> {
    // ADR-059 §7.3: reject stale writers before touching the catalog.
    crate::http::routes::check_expected_version(&state, body.expected_version).await?;

    if body.config.name.is_empty() {
        return Err(ApiError::bad_request("MCP server name must not be empty"));
    }

    let data_dir = get_data_dir(&state).await?;
    let mut catalog = load_mcp_catalog(&data_dir).map_err(|e| ApiError::internal(&e))?;

    // Check for duplicate name
    if catalog.iter().any(|c| c.name == body.config.name) {
        return Err(ApiError::bad_request(&format!(
            "MCP server '{}' already exists in catalog",
            body.config.name
        )));
    }

    catalog.push(body.config);
    save_mcp_catalog(&data_dir, &catalog).map_err(|e| ApiError::internal(&e))?;

    // Rebuild mcp_list cache for AgentHello diff sync.
    {
        let mut gw = state.gateway_state.write().await;
        resource_cache::rebuild_and_save_mcp_cache(&mut gw, &data_dir, &catalog);
    }

    // Hot-push MCP config to all running agents — handled by MQTT publisher trigger below.
    // ADR-033: Trigger MQTT global resource republish after resource change.
    if let Some(ref trigger) = state.mqtt_publisher_trigger {
        trigger.trigger();
    }

    // ADR-059 §6: open a committed operation record so the client can
    // correlate this mutation by `operation_id` and observe the
    // resulting `resource_version`. The side effect (catalog + disk +
    // mcp_list cache) already completed synchronously.
    let resource_version = {
        let gw = state.gateway_state.read().await;
        gw.resource_cache.mcp_list.version
    };
    let mut record = OperationRecord::new(body.expected_version.unwrap_or(0));
    record.state = OperationState::Committed;
    record.resource_version = Some(resource_version);
    let ack = OperationAck::from_record(&record);
    if let Some(store) = state.operation_store.as_ref() {
        store.insert(record);
    }

    Ok((StatusCode::CREATED, Json(ack)))
}

/// `PUT /api/mcp-catalog/{name}` — update a single server entry
pub async fn update_catalog_entry(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<UpdateCatalogEntryRequest>,
) -> Result<Json<MessageResponse>, ApiError> {
    let data_dir = get_data_dir(&state).await?;
    let mut catalog = load_mcp_catalog(&data_dir).map_err(|e| ApiError::internal(&e))?;

    // If the name is being changed, check for conflicts first
    if body.config.name != name && catalog.iter().any(|c| c.name == body.config.name) {
        return Err(ApiError::bad_request(&format!(
            "MCP server '{}' already exists in catalog",
            body.config.name
        )));
    }

    // Find the existing entry index
    let idx = catalog.iter().position(|c| c.name == name).ok_or_else(|| {
        ApiError::not_found(&format!("MCP server '{}' not found in catalog", name))
    })?;

    // Preserve sensitive env values that were sent as "••••" (masked)
    // If the user didn't change a secret field, keep the old value
    let old_env = catalog[idx].env.clone();
    let merged_env: std::collections::HashMap<String, String> = body
        .config
        .env
        .into_iter()
        .map(|(k, v)| {
            if v == "••••" {
                // Keep the old value for this key
                let old_val = old_env.get(&k).cloned().unwrap_or_default();
                (k, old_val)
            } else {
                (k, v)
            }
        })
        .collect();

    // Same masked-value round-trip for HTTP headers (e.g. an Authorization
    // Bearer token): a value returned masked and sent back unchanged keeps
    // the stored secret rather than overwriting it with "••••".
    let old_headers = catalog[idx].headers.clone();
    let merged_headers: std::collections::HashMap<String, String> = body
        .config
        .headers
        .into_iter()
        .map(|(k, v)| {
            if v == "••••" {
                let old_val = old_headers.get(&k).cloned().unwrap_or_default();
                (k, old_val)
            } else {
                (k, v)
            }
        })
        .collect();

    let new_name = body.config.name.clone();
    catalog[idx] = McpServerConfigDef {
        name: body.config.name,
        display_name: body.config.display_name,
        transport: body.config.transport,
        url: body.config.url,
        command: body.config.command,
        args: body.config.args,
        env: merged_env,
        headers: merged_headers,
        tool_timeout_secs: body.config.tool_timeout_secs,
        install: body.config.install,
        required_secrets: body.config.required_secrets,
    };

    save_mcp_catalog(&data_dir, &catalog).map_err(|e| ApiError::internal(&e))?;

    // Rebuild mcp_list cache for AgentHello diff sync.
    {
        let mut gw = state.gateway_state.write().await;
        resource_cache::rebuild_and_save_mcp_cache(&mut gw, &data_dir, &catalog);
    }

    // Hot-push MCP config — handled by MQTT publisher trigger below.
    // ADR-033: Trigger MQTT global resource republish after resource change.
    if let Some(ref trigger) = state.mqtt_publisher_trigger {
        trigger.trigger();
    }

    Ok(Json(MessageResponse {
        message: format!("MCP server '{}' updated in catalog", new_name),
    }))
}

/// `DELETE /api/mcp-catalog/{name}` — remove a server entry
pub async fn remove_catalog_entry(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<MessageResponse>, ApiError> {
    let data_dir = get_data_dir(&state).await?;
    let mut catalog = load_mcp_catalog(&data_dir).map_err(|e| ApiError::internal(&e))?;

    let original_len = catalog.len();
    catalog.retain(|c| c.name != name);
    if catalog.len() == original_len {
        return Err(ApiError::not_found(&format!(
            "MCP server '{}' not found in catalog",
            name
        )));
    }

    save_mcp_catalog(&data_dir, &catalog).map_err(|e| ApiError::internal(&e))?;

    // Rebuild mcp_list cache for AgentHello diff sync.
    {
        let mut gw = state.gateway_state.write().await;
        resource_cache::rebuild_and_save_mcp_cache(&mut gw, &data_dir, &catalog);
    }

    // Hot-push MCP config — handled by MQTT publisher trigger below.
    // ADR-033: Trigger MQTT global resource republish after resource change.
    if let Some(ref trigger) = state.mqtt_publisher_trigger {
        trigger.trigger();
    }

    Ok(Json(MessageResponse {
        message: format!("MCP server '{}' removed from catalog", name),
    }))
}

// ── Helpers ───────────────────────────────────────────────────────────

/// Get the data_dir from Gateway state
async fn get_data_dir(state: &AppState) -> Result<PathBuf, ApiError> {
    let gw = state.gateway_state.read().await;
    Ok(gw
        .config
        .as_ref()
        .map(|c| PathBuf::from(&c.data_dir))
        .unwrap_or_else(|| PathBuf::from("./data")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mask_sensitive_env() {
        let config = McpServerConfigDef {
            name: "github".to_string(),
            display_name: None,
            transport: acowork_core::protocol::McpTransportDef::Stdio,
            command: "npx".to_string(),
            args: vec![
                "-y".to_string(),
                "@modelcontextprotocol/server-github".to_string(),
            ],
            env: std::collections::HashMap::from([
                (
                    "GITHUB_PERSONAL_ACCESS_TOKEN".to_string(),
                    "ghp_abc123".to_string(),
                ),
                ("SOME_OTHER_VAR".to_string(), "visible_value".to_string()),
            ]),
            ..Default::default()
        };

        let masked = mask_sensitive_env(&config);
        assert_eq!(
            masked.env.get("GITHUB_PERSONAL_ACCESS_TOKEN"),
            Some(&"••••".to_string())
        );
        assert_eq!(
            masked.env.get("SOME_OTHER_VAR"),
            Some(&"visible_value".to_string())
        );
    }

    #[test]
    fn test_catalog_save_load_roundtrip() {
        let dir =
            std::env::temp_dir().join(format!("acowork-test-mcp-catalog-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let catalog = vec![McpServerConfigDef {
            name: "filesystem".to_string(),
            display_name: None,
            transport: acowork_core::protocol::McpTransportDef::Stdio,
            command: "npx".to_string(),
            args: vec![
                "-y".to_string(),
                "@modelcontextprotocol/server-filesystem".to_string(),
            ],
            ..Default::default()
        }];

        save_mcp_catalog(&dir, &catalog).unwrap();
        let loaded = load_mcp_catalog(&dir).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "filesystem");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_load_nonexistent_catalog() {
        let dir = std::env::temp_dir().join(format!(
            "acowork-test-mcp-catalog-empty-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let loaded = load_mcp_catalog(&dir).unwrap();
        assert!(loaded.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
