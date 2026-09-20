//! Provider configuration HTTP API handlers
//!
//! Full provider lifecycle management: API key (encrypted Vault) +
//! configuration (provider_list.json: base_url, models, capabilities, compact_model).
//!
//! - GET    /api/providers          — list providers (masked keys) + config
//! - POST   /api/providers          — add a provider (key + config)
//! - DELETE /api/providers/:provider — remove a provider
//! - PUT    /api/providers/:provider — update a provider (key and/or config)

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{delete, get},
};
use serde::{Deserialize, Serialize};

use crate::http::models_api;
use crate::http::routes::{ApiError, AppState, OperationAck};
use crate::resource_cache;
use acowork_core::operation::{OperationRecord, OperationState};
use acowork_core::protocol::ModelCapabilitiesInfo;
use std::collections::HashMap;
use std::path::PathBuf;

/// Build the provider configuration router
pub fn provider_routes() -> Router<AppState> {
    Router::new()
        .route("/api/providers", get(list_providers).post(add_provider))
        .route(
            "/api/providers/{provider}",
            delete(remove_provider).put(update_provider),
        )
        .route(
            "/api/providers/{provider}/keys/{account_id}",
            delete(remove_provider_account).patch(update_provider_account),
        )
        .route(
            "/api/search/keys",
            get(list_search_keys).post(add_search_key),
        )
        .route(
            "/api/search/keys/{provider}",
            delete(remove_search_key).put(update_search_key),
        )
}

// ── Response types ────────────────────────────────────────────────────

/// Masked key entry with provider config (first 3 + last 3 chars visible).
///
/// Config fields (base_url, models, compact_model) are read from
/// provider_list.json, NOT from Vault.
#[derive(Serialize)]
pub struct ProviderEntryResponse {
    pub provider: String,
    /// Stable account UUID — empty for legacy single-key rows.
    /// Use this in subsequent add/update/remove calls.
    pub account_id: String,
    /// User-facing label for this account. Empty for legacy rows.
    pub alias: String,
    pub key_preview: String,
    /// Configured base URL (if any)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Configured default model (models[0])
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    /// Selected models list (may be empty)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
    /// Compact model for LLM summarization (ADR-010). None = use current model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compact_model: Option<String>,
    /// Whether this is a local (self-hosted) provider (no API key required)
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub local: bool,
    /// Whether this is a user-defined custom provider (not listed in models.dev)
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub custom: bool,
    /// Per-model capabilities map (model ID → capabilities), including user-configured overrides.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_capabilities: Option<HashMap<String, ModelCapabilitiesInfo>>,
}

/// Default max output tokens when gateway config doesn't specify a limit.
const DEFAULT_MAX_OUTPUT_TOKENS: u64 = 32_768;

/// One API key entry inside an `AddProviderRequest.keys` array.
#[derive(Deserialize)]
pub struct AddProviderKey {
    /// User-facing label; optional. Empty/missing → `"{provider}-default"`.
    #[serde(default)]
    pub alias: Option<String>,
    /// API key value. Empty allowed for local or key-less custom providers.
    #[serde(default)]
    pub key: String,
}

/// Add provider request.
///
/// Backwards compatibility: `key` (single, no alias) is still accepted
/// and is treated as a one-element `keys` list. New callers should use
/// `keys` to attach multiple accounts (e.g. work + personal) to the
/// same provider.
///
/// `base_url`, `models`, `compact_model` → stored in provider_list.json.
/// `model_capabilities` → user-configured overrides merged into offline data.
#[derive(Deserialize)]
pub struct AddProviderRequest {
    pub provider: String,
    /// Legacy single-key field. Ignored when `keys` is non-empty.
    #[serde(default)]
    pub key: String,
    /// Multi-account entry: each entry creates a separate vault account.
    #[serde(default)]
    pub keys: Vec<AddProviderKey>,
    /// Optional base URL override (e.g. "https://api.deepseek.com/v1")
    #[serde(default)]
    pub base_url: Option<String>,
    /// Optional default model (fallback if `models` is empty)
    #[serde(default)]
    pub default_model: Option<String>,
    /// Selected models for this provider (from models.dev).
    /// models[0] is the default/active model.
    #[serde(default)]
    pub models: Vec<String>,
    /// Compact model for LLM summarization (ADR-010). None = use current model.
    #[serde(default)]
    pub compact_model: Option<String>,
    /// Whether this is a custom (user-defined) provider not listed in models.dev.
    /// Custom providers always use OpenAI-compatible protocol.
    #[serde(default)]
    pub custom: Option<bool>,
    /// Per-model capabilities overrides (model ID → capabilities).
    /// User-configured fields (e.g. `default_reasoning_effort`) are merged
    /// into the offline models.dev data so the Runtime sees user preferences.
    #[serde(default)]
    pub model_capabilities: Option<HashMap<String, ModelCapabilitiesInfo>>,
    /// ADR-059 §7.3: the BootstrapState `version` the client read
    /// before writing (optimistic concurrency — stale clients are
    /// rejected with `resource_version_conflict`). Absent → no
    /// precondition.
    #[serde(default)]
    pub expected_version: Option<u64>,
}

/// Update provider request (supports partial updates — config and
/// incremental key additions are optional).
///
/// Two key-bearing fields coexist for backwards compatibility:
/// - `key` (single, no alias): replaces the first account's key when
///   present. Pre-multi-account clients keep working.
/// - `keys` (multi): each entry becomes a brand-new account on the
///   Vault side, leaving existing accounts untouched. Used by the
///   edit dialog to add new accounts without disturbing the
///   already-configured ones.
///
/// `base_url`, `models`, `compact_model`, `model_capabilities` cover
/// provider-level config; they're shared across all accounts.
#[derive(Deserialize)]
pub struct UpdateProviderRequest {
    /// Legacy single-key field. If `None` or empty, the existing first
    /// account's key is preserved. Mutually compatible with `keys`
    /// (which adds accounts instead of replacing).
    #[serde(default)]
    pub key: Option<String>,
    /// Multi-account entries to append. Each entry creates a fresh
    /// vault account; existing accounts are not modified.
    #[serde(default)]
    pub keys: Vec<AddProviderKey>,
    /// Optional base URL override
    #[serde(default)]
    pub base_url: Option<String>,
    /// Optional default model (fallback if `models` is empty)
    #[serde(default)]
    pub default_model: Option<String>,
    /// Selected models for this provider (from models.dev).
    #[serde(default)]
    pub models: Vec<String>,
    /// Compact model for LLM summarization (ADR-010).
    #[serde(default)]
    pub compact_model: Option<String>,
    /// Per-model capabilities overrides (model ID → capabilities).
    /// User-configured fields (e.g. `default_reasoning_effort`) are merged
    /// into the offline models.dev data so the Runtime sees user preferences.
    #[serde(default)]
    pub model_capabilities: Option<HashMap<String, ModelCapabilitiesInfo>>,
}

/// Generic message response
#[derive(Serialize)]
pub struct MessageResponse {
    pub message: String,
}

// ── Search key types ──────────────────────────────────────────────────

/// Search key entry response (masked preview)
#[derive(Serialize)]
pub struct SearchKeyEntryResponse {
    pub provider: String,
    pub key_preview: String,
}

/// Add search key request
#[derive(Deserialize)]
pub struct AddSearchKeyRequest {
    pub provider: String,
    pub key: String,
}

/// Update search key request (partial update — key is optional)
#[derive(Deserialize)]
pub struct UpdateSearchKeyRequest {
    #[serde(default)]
    pub key: Option<String>,
}

// ── Handlers ──────────────────────────────────────────────────────────

/// Which `(account_id, alias, key_preview)` rows to emit for one configured
/// provider.
///
/// Local providers have no key by design. A non-local provider whose vault
/// accounts are all gone (last key removed, or a leftover id from a rename)
/// must still surface one empty-account row: without it the provider is
/// invisible in the UI while `provider_list.json` and every Runtime still
/// carry it, so the user has no way to delete it.
fn account_rows_for_provider(
    is_local: bool,
    accounts: Option<Vec<(String, String, String)>>,
) -> Vec<(String, String, String)> {
    if is_local {
        return vec![(String::new(), String::new(), "(local)".to_string())];
    }
    accounts.unwrap_or_else(|| vec![(String::new(), String::new(), "(no key)".to_string())])
}

/// `GET /api/providers` — list stored providers (masked keys) with config.
///
/// For providers with multiple accounts the response contains one entry
/// per `(provider, account_id)` pair; provider-level config
/// (`base_url`, `models`, `compact_model`, `model_capabilities`) is
/// duplicated across each row for the same provider. Local providers
/// (no API key) keep emitting a single row with empty `account_id`.
pub async fn list_providers(
    State(state): State<AppState>,
) -> Result<Json<Vec<ProviderEntryResponse>>, ApiError> {
    let gw = state.gateway_state.read().await;

    // Group vault accounts by provider so we can join against the
    // provider_list.json cache below. `vault.list_keys()` already
    // expands one row per account, so this is a 1:N fan-out.
    let mut accounts_by_provider: std::collections::HashMap<
        String,
        Vec<(String, String, String)>,
    > = std::collections::HashMap::new();
    if let Ok(entries) = gw.vault.list_keys() {
        for e in entries {
            accounts_by_provider
                .entry(e.provider)
                .or_default()
                .push((e.account_id, e.alias, e.key_preview));
        }
    }

    // Iterate resource_cache as source of truth for which providers exist.
    let mut response: Vec<ProviderEntryResponse> = Vec::new();
    for cfg in &gw.resource_cache.provider_list.providers {
        let is_local = models_api::is_local_provider(&cfg.id);
        let accounts = accounts_by_provider.remove(&cfg.id);
        let accounts_for_provider = account_rows_for_provider(is_local, accounts);

        for (account_id, alias, key_preview) in accounts_for_provider {
            response.push(ProviderEntryResponse {
                provider: cfg.id.clone(),
                account_id,
                alias,
                key_preview: key_preview.clone(),
                base_url: if cfg.base_url.is_empty() {
                    None
                } else {
                    Some(cfg.base_url.clone())
                },
                default_model: cfg.models.first().map(|m| m.id.clone()),
                models: cfg.models.iter().map(|m| m.id.clone()).collect(),
                compact_model: cfg.compact_model.clone(),
                local: is_local,
                custom: cfg.custom,
                model_capabilities: {
                    let caps: HashMap<String, ModelCapabilitiesInfo> = cfg
                        .models
                        .iter()
                        .map(|m| (m.id.clone(), m.capabilities.clone()))
                        .collect();
                    if caps.is_empty() { None } else { Some(caps) }
                },
            });
        }
    }

    // Account rows for which there's no provider_list.json entry
    // (e.g. a key was added but the provider config hasn't been
    // populated yet). Surface them too so the user can see and edit
    // orphaned accounts.
    for (provider, accounts) in accounts_by_provider {
        for (account_id, alias, key_preview) in accounts {
            response.push(ProviderEntryResponse {
                provider: provider.clone(),
                account_id,
                key_preview,
                alias,
                base_url: None,
                default_model: None,
                models: Vec::new(),
                compact_model: None,
                local: false,
                custom: false,
                model_capabilities: None,
            });
        }
    }

    Ok(Json(response))
}

/// `POST /api/providers` — add a provider (key + config).
///
/// API key → stored in encrypted Vault.
/// Config (base_url, models, compact_model, model_capabilities) → built from
/// request + offline capabilities, stored in provider_list.json via resource_cache.
pub async fn add_provider(
    State(state): State<AppState>,
    Json(body): Json<AddProviderRequest>,
) -> Result<(StatusCode, Json<OperationAck>), ApiError> {
    // ADR-059 §7.3: reject stale writers before touching the vault.
    crate::http::routes::check_expected_version(&state, body.expected_version).await?;

    // Validate base_url format if provided
    if let Some(ref url) = body.base_url
        && !url.is_empty()
        && !url.starts_with("http://")
        && !url.starts_with("https://")
    {
        return Err(ApiError::bad_request(
            "base_url must start with http:// or https://",
        ));
    }
    if body.provider.is_empty() {
        return Err(ApiError::bad_request("provider must not be empty"));
    }
    let is_local = models_api::is_local_provider(&body.provider);
    let is_custom = body.custom.unwrap_or(false);

    // Normalise `key` + `keys[]` into a single `[(alias, key)]` list.
    // Legacy single-key callers keep working; new callers pass `keys`
    // for multi-account setups.
    let key_entries: Vec<(Option<String>, String)> = if !body.keys.is_empty() {
        body.keys
            .iter()
            .map(|k| (k.alias.clone(), k.key.clone()))
            .collect()
    } else if !body.key.is_empty() || is_local || is_custom {
        vec![(None, body.key.clone())]
    } else {
        Vec::new()
    };
    if !is_local && !is_custom && key_entries.is_empty() {
        return Err(ApiError::bad_request("key must not be empty"));
    }

    let mut gw = state.gateway_state.write().await;

    // 1. Store each API key as a separate Vault account. Local / key-less
    //    custom providers still get a single default account so the
    //    Runtime can resolve a (provider, account) pair.
    for (alias, raw_key) in &key_entries {
        let effective_key = if is_local {
            "local".to_string()
        } else if is_custom && raw_key.is_empty() {
            "custom".to_string()
        } else {
            raw_key.clone()
        };
        gw.vault
            .add_account(&body.provider, alias.as_deref(), &effective_key)
            .map_err(|e| ApiError::internal(&format!("Failed to store key: {}", e)))?;
    }

    // 2. Resolve models list.
    let resolved_models: Vec<String> = if !body.models.is_empty() {
        body.models.clone()
    } else if let Some(ref m) = body.default_model {
        vec![m.clone()]
    } else {
        vec![]
    };

    // 3. Build ProviderListItem (capabilities from offline_providers.json).
    let max_output_tokens = gw
        .config
        .as_ref()
        .map(|c| c.max_output_tokens_limit)
        .unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS);
    let mut item = resource_cache::build_provider_list_item(
        &body.provider,
        body.base_url.as_deref(),
        &resolved_models,
        body.compact_model.as_deref(),
        max_output_tokens,
        is_custom,
    );

    // 3b. Merge user-provided model_capabilities overrides (e.g. default_reasoning_effort).
    if let Some(ref user_caps) = body.model_capabilities {
        merge_user_capabilities(&mut item, user_caps);
    }

    // 4. Add to in-memory provider list (replace if already exists).
    resource_cache::remove_provider_from_memory(&mut gw, &body.provider);
    gw.resource_cache
        .provider_list
        .providers
        .push(item);

    // 5. Persist to disk and bump version.
    let data_dir = get_data_dir_from_gw(&gw);
    resource_cache::persist_provider_cache(&mut gw, &data_dir);
    let resource_version = gw.resource_cache.provider_list.version;
    drop(gw);

    // 6. Hot-push to running agents — handled by MQTT publisher trigger below.
    // ADR-033: Trigger MQTT global resource republish after resource change.
    if let Some(ref trigger) = state.mqtt_publisher_trigger {
        trigger.trigger();
    }

    // 7. ADR-059 §6: open a committed operation record so the client
    // can correlate this mutation by `operation_id` and observe the
    // resulting `resource_version`. The side effect (vault + disk)
    // already completed synchronously, so the record starts terminal-
    // ready in `Committed` and is swept after its deadline.
    let mut record = OperationRecord::new(body.expected_version.unwrap_or(0));
    record.state = OperationState::Committed;
    record.resource_version = Some(resource_version);
    let ack = OperationAck::from_record(&record);
    if let Some(store) = state.operation_store.as_ref() {
        store.insert(record);
    }

    Ok((StatusCode::CREATED, Json(ack)))
}

/// `DELETE /api/providers/:provider` — remove a provider (key + config).
pub async fn remove_provider(
    State(state): State<AppState>,
    Path(provider): Path<String>,
) -> Result<Json<MessageResponse>, ApiError> {
    let mut gw = state.gateway_state.write().await;

    // 1. Remove API keys from Vault. A provider can legitimately have no
    //    account left (every key removed, or a leftover id from a rename)
    //    and is still configured in `provider_list.json` — such a provider
    //    must stay deletable, so a missing key is not an error here. Only
    //    skip the vault call when we can *prove* there is nothing to
    //    remove; an enumeration failure keeps the strict path.
    let has_keys = gw
        .vault
        .list_keys()
        .map(|entries| entries.iter().any(|e| e.provider == provider))
        .unwrap_or(true);
    if has_keys {
        gw.vault.remove_key(&provider).map_err(|e| {
            ApiError::not_found(&format!("Key not found for provider '{}': {}", provider, e))
        })?;
    }

    // 2. Remove from in-memory provider list.
    resource_cache::remove_provider_from_memory(&mut gw, &provider);

    // 3. Persist to disk.
    let data_dir = get_data_dir_from_gw(&gw);
    resource_cache::persist_provider_cache(&mut gw, &data_dir);
    drop(gw);

    // 4. Hot-push — handled by MQTT publisher trigger below.
    // ADR-033: Trigger MQTT global resource republish after resource change.
    if let Some(ref trigger) = state.mqtt_publisher_trigger {
        trigger.trigger();
    }

    Ok(Json(MessageResponse {
        message: format!("Key removed for provider: {}", provider),
    }))
}

/// `DELETE /api/providers/:provider/keys/:account_id` — remove ONE account
/// (API key) of a provider.
///
/// The provider's other accounts and its provider-level config (`base_url`,
/// `models`, `compact_model`) stay untouched. Removing the last account
/// leaves the provider listed but keyless — `list_providers` iterates the
/// account rows, so it drops out of the configured set and the provider
/// returns to the "not yet configured" list.
pub async fn remove_provider_account(
    State(state): State<AppState>,
    Path((provider, account_id)): Path<(String, String)>,
) -> Result<Json<MessageResponse>, ApiError> {
    let mut gw = state.gateway_state.write().await;

    // Vault::delete removes the on-disk entry as well, so no extra
    // persistence step here (unlike provider_list.json below).
    gw.vault
        .remove_account(&provider, &account_id)
        .map_err(|e| {
            ApiError::not_found(&format!(
                "Account '{}' not found for provider '{}': {}",
                account_id, provider, e
            ))
        })?;
    drop(gw);

    // Hot-push: running agents must stop receiving the removed account.
    // ADR-033: Trigger MQTT global resource republish after resource change.
    if let Some(ref trigger) = state.mqtt_publisher_trigger {
        trigger.trigger();
    }

    Ok(Json(MessageResponse {
        message: format!("Account {} removed for provider: {}", account_id, provider),
    }))
}

/// `PATCH /api/providers/:provider/keys/:account_id` — edit an existing
/// account's `alias` and/or `api_key`. At least one field must be
/// provided. Used by the harness edit dialog so users can rename or
/// rotate keys on already-configured accounts without losing the
/// account's UUID.
pub async fn update_provider_account(
    State(state): State<AppState>,
    Path((provider, account_id)): Path<(String, String)>,
    Json(body): Json<UpdateProviderAccountRequest>,
) -> Result<Json<MessageResponse>, ApiError> {
    if body.alias.is_none() && body.key.is_none() {
        return Err(ApiError::bad_request(
            "at least one of `alias` or `key` must be provided",
        ));
    }

    let mut gw = state.gateway_state.write().await;
    if let Some(ref new_alias) = body.alias {
        gw.vault
            .update_alias(&provider, &account_id, new_alias)
            .map_err(|e| {
                ApiError::not_found(&format!(
                    "Failed to update alias for {}/{}: {}",
                    provider, account_id, e
                ))
            })?;
    }
    if let Some(ref new_key) = body.key {
        gw.vault
            .update_account_key(&provider, &account_id, new_key)
            .map_err(|e| {
                ApiError::not_found(&format!(
                    "Failed to update key for {}/{}: {}",
                    provider, account_id, e
                ))
            })?;
    }
    drop(gw);

    // Hot-push: the account's alias is used by the model menu breadcrumb
    // when drilling into multi-key providers, so we must republish.
    if let Some(ref trigger) = state.mqtt_publisher_trigger {
        trigger.trigger();
    }

    Ok(Json(MessageResponse {
        message: format!("Account {} updated for provider: {}", account_id, provider),
    }))
}

/// Request body for `PATCH /api/providers/:provider/keys/:account_id`.
///
/// Both fields are optional but at least one must be present — enforced
/// in the handler so we reject no-op calls early.
#[derive(Deserialize)]
pub struct UpdateProviderAccountRequest {
    /// New alias for the account. Trimmed server-side; must be non-empty.
    #[serde(default)]
    pub alias: Option<String>,
    /// New API key for the account. Trimmed server-side; must be non-empty.
    #[serde(default)]
    pub key: Option<String>,
}

/// `PUT /api/providers/:provider` — update a provider (key and/or config).
///
/// If `key` is None/empty, the existing Vault key is preserved.
/// If `models` is empty and `default_model` is None, existing models are
/// preserved from provider_list.json.
pub async fn update_provider(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Json(body): Json<UpdateProviderRequest>,
) -> Result<Json<MessageResponse>, ApiError> {
    if let Some(ref url) = body.base_url
        && !url.is_empty()
        && !url.starts_with("http://")
        && !url.starts_with("https://")
    {
        return Err(ApiError::bad_request(
            "base_url must start with http:// or https://",
        ));
    }

    let mut gw = state.gateway_state.write().await;

    // 1a. Legacy single-key update path — kept for backwards
    //     compatibility with pre-multi-account clients. When the caller
    //     provides `keys`, the per-account update still wins: it adds
    //     accounts instead of mutating the first one.
    let has_legacy_key_change = body.key.as_ref().is_some_and(|k| !k.is_empty());
    if has_legacy_key_change && body.keys.is_empty() {
        let api_key = body.key.as_ref().expect("checked above").clone();
        gw.vault
            .store_key(&provider, &api_key)
            .map_err(|e| ApiError::internal(&format!("Failed to update key: {}", e)))?;
    }

    // 1b. Multi-account path — append each entry as a fresh account.
    //     Empty keys are silently dropped so the dialog can keep
    //     stub rows around without contaminating the vault.
    for entry in &body.keys {
        let alias = entry.alias.as_deref();
        let api_key = entry.key.trim();
        if api_key.is_empty() {
            continue;
        }
        gw.vault
            .add_account(&provider, alias, api_key)
            .map_err(|e| ApiError::internal(&format!("Failed to add account: {}", e)))?;
    }

    // 2. Resolve models: provided > default_model > existing from cache.
    let resolved_models: Vec<String> = if !body.models.is_empty() {
        body.models.clone()
    } else if let Some(ref m) = body.default_model {
        vec![m.clone()]
    } else {
        // Preserve existing models from provider_list.json cache.
        gw.resource_cache
            .provider_list
            .providers
            .iter()
            .find(|p| p.id == provider)
            .map(|p| p.models.iter().map(|m| m.id.clone()).collect())
            .unwrap_or_default()
    };

    // 3. Resolve base_url: provided > existing from cache.
    let resolved_base_url = if body.base_url.is_some() {
        body.base_url.clone()
    } else {
        gw.resource_cache
            .provider_list
            .providers
            .iter()
            .find(|p| p.id == provider)
            .and_then(|p| {
                if p.base_url.is_empty() {
                    None
                } else {
                    Some(p.base_url.clone())
                }
            })
    };

    // 4. Resolve compact_model: provided > existing from cache.
    let resolved_compact_model = if body.compact_model.is_some() {
        body.compact_model.clone()
    } else {
        gw.resource_cache
            .provider_list
            .providers
            .iter()
            .find(|p| p.id == provider)
            .and_then(|p| p.compact_model.clone())
    };

    // 5. Rebuild ProviderListItem (capabilities from offline_providers.json).
    let max_output_tokens = gw
        .config
        .as_ref()
        .map(|c| c.max_output_tokens_limit)
        .unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS);
    // Preserve the existing custom flag from the stored provider entry.
    let is_custom = gw
        .resource_cache
        .provider_list
        .providers
        .iter()
        .find(|p| p.id == provider)
        .map(|p| p.custom)
        .unwrap_or(false);
    let mut item = resource_cache::build_provider_list_item(
        &provider,
        resolved_base_url.as_deref(),
        &resolved_models,
        resolved_compact_model.as_deref(),
        max_output_tokens,
        is_custom,
    );

    // 5b. Merge user-provided model_capabilities overrides (e.g. default_reasoning_effort).
    if let Some(ref user_caps) = body.model_capabilities {
        merge_user_capabilities(&mut item, user_caps);
    }

    // 6. Replace in in-memory list.
    resource_cache::remove_provider_from_memory(&mut gw, &provider);
    gw.resource_cache
        .provider_list
        .providers
        .push(item);

    // 7. Persist to disk.
    let data_dir = get_data_dir_from_gw(&gw);
    resource_cache::persist_provider_cache(&mut gw, &data_dir);
    drop(gw);

    // 8. Hot-push — handled by MQTT publisher trigger below.
    // ADR-033: Trigger MQTT global resource republish after resource change.
    if let Some(ref trigger) = state.mqtt_publisher_trigger {
        trigger.trigger();
    }

    Ok(Json(MessageResponse {
        message: format!("Key updated for provider: {}", provider),
    }))
}

// ── Search key handlers ───────────────────────────────────────────────

/// `GET /api/search/keys` — list stored search provider keys (masked)
pub async fn list_search_keys(
    State(state): State<AppState>,
) -> Result<Json<Vec<SearchKeyEntryResponse>>, ApiError> {
    let gw = state.gateway_state.read().await;
    let entries = gw
        .vault
        .list_search_keys()
        .map_err(|e| ApiError::internal(&format!("Failed to list search keys: {}", e)))?;

    let response = entries
        .iter()
        .map(|k| SearchKeyEntryResponse {
            provider: k.provider.clone(),
            key_preview: k.key_preview.clone(),
        })
        .collect();

    Ok(Json(response))
}

/// `POST /api/search/keys` — add a search provider API key
pub async fn add_search_key(
    State(state): State<AppState>,
    Json(body): Json<AddSearchKeyRequest>,
) -> Result<(StatusCode, Json<MessageResponse>), ApiError> {
    if body.provider.is_empty() {
        return Err(ApiError::bad_request("provider must not be empty"));
    }
    if body.key.is_empty() {
        return Err(ApiError::bad_request("key must not be empty"));
    }

    let mut gw = state.gateway_state.write().await;
    gw.vault
        .store_search_key(&body.provider, &body.key)
        .map_err(|e| ApiError::internal(&format!("Failed to store search key: {}", e)))?;

    // Rebuild search_list cache so AgentHello picks up the new provider.
    let data_dir = get_data_dir_from_gw(&gw);
    resource_cache::rebuild_and_save_search_cache(&mut gw, &data_dir);
    drop(gw); // Release write lock before hot-push

    // Hot-push search config change — handled by MQTT publisher trigger below.
    // ADR-033: Trigger MQTT global resource republish after resource change.
    if let Some(ref trigger) = state.mqtt_publisher_trigger {
        trigger.trigger();
    }

    Ok((
        StatusCode::CREATED,
        Json(MessageResponse {
            message: format!("Search key stored for provider: {}", body.provider),
        }),
    ))
}

/// `DELETE /api/search/keys/:provider` — remove a search provider API key
pub async fn remove_search_key(
    State(state): State<AppState>,
    Path(provider): Path<String>,
) -> Result<Json<MessageResponse>, ApiError> {
    let mut gw = state.gateway_state.write().await;
    gw.vault.remove_search_key(&provider).map_err(|e| {
        ApiError::not_found(&format!("Search key not found for '{}': {}", provider, e))
    })?;

    // Rebuild search_list cache after removal.
    let data_dir = get_data_dir_from_gw(&gw);
    resource_cache::rebuild_and_save_search_cache(&mut gw, &data_dir);
    drop(gw);

    // Hot-push handled by MQTT publisher trigger below.
    // ADR-033: Trigger MQTT global resource republish after resource change.
    if let Some(ref trigger) = state.mqtt_publisher_trigger {
        trigger.trigger();
    }

    Ok(Json(MessageResponse {
        message: format!("Search key removed for provider: {}", provider),
    }))
}

/// `PUT /api/search/keys/:provider` — update a search provider API key (partial)
pub async fn update_search_key(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Json(body): Json<UpdateSearchKeyRequest>,
) -> Result<Json<MessageResponse>, ApiError> {
    let mut gw = state.gateway_state.write().await;

    // Resolve the API key: use provided key, or preserve existing key
    let api_key = match body.key {
        Some(ref k) if !k.is_empty() => k.clone(),
        _ => match gw.vault.get_search_key(&provider) {
            Ok(entry) => entry.api_key,
            Err(e) => {
                return Err(ApiError::not_found(&format!(
                    "Search key not found for '{}': {}",
                    provider, e
                )));
            }
        },
    };

    // Remove old entry, store new
    let _ = gw.vault.remove_search_key(&provider);
    gw.vault
        .store_search_key(&provider, &api_key)
        .map_err(|e| ApiError::internal(&format!("Failed to update search key: {}", e)))?;

    // Rebuild search_list cache after update.
    let data_dir = get_data_dir_from_gw(&gw);
    resource_cache::rebuild_and_save_search_cache(&mut gw, &data_dir);
    drop(gw);

    // Hot-push handled by MQTT publisher trigger below.
    // ADR-033: Trigger MQTT global resource republish after resource change.
    if let Some(ref trigger) = state.mqtt_publisher_trigger {
        trigger.trigger();
    }

    Ok(Json(MessageResponse {
        message: format!("Search key updated for provider: {}", provider),
    }))
}

// ── Helpers ───────────────────────────────────────────────────────────

/// Merge user-provided model capabilities overrides into a ProviderListItem.
///
/// For each model ID present in `user_caps`:
/// - If the model already exists in `item.models`, user-set fields override
///   the offline data (only non-None user fields are applied).
/// - If the model is not in `item.models`, the override is silently ignored
///   (only configured models can have overrides).
fn merge_user_capabilities(
    item: &mut acowork_core::protocol::ProviderListItem,
    user_caps: &HashMap<String, ModelCapabilitiesInfo>,
) {
    for model_entry in &mut item.models {
        if let Some(user_cap) = user_caps.get(&model_entry.id) {
            // Core limits — always override (frontend always sends these)
            model_entry.capabilities.context_window = user_cap.context_window;
            model_entry.capabilities.max_output_tokens = user_cap.max_output_tokens;
            model_entry.capabilities.supports_tool_calling = user_cap.supports_tool_calling;

            // Optional fields — override only when the user explicitly set them
            if user_cap.modalities.is_some() {
                model_entry.capabilities.modalities = user_cap.modalities.clone();
            }
            if user_cap.supports_attachment.is_some() {
                model_entry.capabilities.supports_attachment = user_cap.supports_attachment;
            }
            if user_cap.default_reasoning_effort.is_some() {
                model_entry.capabilities.default_reasoning_effort =
                    user_cap.default_reasoning_effort.clone();
            }
            if user_cap.thinking_mode.is_some() {
                model_entry.capabilities.thinking_mode = user_cap.thinking_mode.clone();
            }
            if user_cap.supports_reasoning.is_some() {
                model_entry.capabilities.supports_reasoning = user_cap.supports_reasoning;
            }
            if user_cap.supports_temperature.is_some() {
                model_entry.capabilities.supports_temperature = user_cap.supports_temperature;
            }
        }
    }
}

/// Get data_dir from GatewayState config.
pub(crate) fn get_data_dir_from_gw(gw: &crate::gateway::state::GatewayState) -> PathBuf {
    gw.config
        .as_ref()
        .map(|c| PathBuf::from(&c.data_dir))
        .unwrap_or_else(|| PathBuf::from("./data"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A keyless provider must stay visible in the UI (one empty-account
    /// row) so it can be deleted — otherwise it lingers in
    /// `provider_list.json` and in every Runtime's provider list.
    #[test]
    fn account_rows_surface_keyless_non_local_provider() {
        // Local provider: placeholder row, as before.
        assert_eq!(
            account_rows_for_provider(true, None),
            vec![(String::new(), String::new(), "(local)".to_string())]
        );
        // Non-local provider with no vault account: still one row.
        assert_eq!(
            account_rows_for_provider(false, None),
            vec![(String::new(), String::new(), "(no key)".to_string())]
        );
        // Accounts present: handed through untouched.
        let accounts = vec![("acc-1".to_string(), "alias".to_string(), "sk-...123".to_string())];
        assert_eq!(
            account_rows_for_provider(false, Some(accounts.clone())),
            accounts
        );
    }

    #[test]
    fn test_add_provider_request_deserialization() {
        let json = r#"{"provider": "openai", "key": "sk-12345"}"#;
        let req: AddProviderRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.provider, "openai");
        assert_eq!(req.key, "sk-12345");
        assert!(req.base_url.is_none());
        assert!(req.default_model.is_none());
    }

    #[test]
    fn test_add_provider_request_with_full_config() {
        let json = r#"{"provider": "deepseek", "key": "sk-abc", "base_url": "https://api.deepseek.com/v1", "default_model": "deepseek-chat"}"#;
        let req: AddProviderRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.provider, "deepseek");
        assert_eq!(req.key, "sk-abc");
        assert_eq!(
            req.base_url,
            Some("https://api.deepseek.com/v1".to_string())
        );
        assert_eq!(req.default_model, Some("deepseek-chat".to_string()));
    }

    #[test]
    fn test_update_provider_request_deserialization() {
        let json = r#"{"key": "sk-new-key"}"#;
        let req: UpdateProviderRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.key, Some("sk-new-key".to_string()));
        assert!(req.base_url.is_none());
        assert!(req.default_model.is_none());
    }

    #[test]
    fn test_update_provider_request_with_full_config() {
        let json = r#"{"key": "sk-new", "base_url": "https://api.custom.com/v1", "default_model": "custom-model"}"#;
        let req: UpdateProviderRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.key, Some("sk-new".to_string()));
        assert_eq!(req.base_url, Some("https://api.custom.com/v1".to_string()));
        assert_eq!(req.default_model, Some("custom-model".to_string()));
    }

    #[test]
    fn test_update_provider_request_accepts_keys_array() {
        // The edit dialog sends `keys` to append accounts in one round-trip.
        let json = r#"{
            "keys": [
                {"alias": "work", "key": "sk-work"},
                {"alias": "personal", "key": "sk-personal"}
            ],
            "base_url": "https://api.example.com/v1"
        }"#;
        let req: UpdateProviderRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.keys.len(), 2);
        assert_eq!(req.keys[0].alias.as_deref(), Some("work"));
        assert_eq!(req.keys[0].key, "sk-work");
        assert_eq!(req.keys[1].alias.as_deref(), Some("personal"));
        assert_eq!(req.keys[1].key, "sk-personal");
        assert_eq!(req.base_url.as_deref(), Some("https://api.example.com/v1"));
        // `key` defaults to None when only `keys` is sent.
        assert!(req.key.is_none());
    }

    #[test]
    fn test_update_provider_account_request_alias_only() {
        let json = r#"{"alias": "prod"}"#;
        let req: UpdateProviderAccountRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.alias.as_deref(), Some("prod"));
        assert!(req.key.is_none());
    }

    #[test]
    fn test_update_provider_account_request_key_only() {
        let json = r#"{"key": "sk-rotated"}"#;
        let req: UpdateProviderAccountRequest = serde_json::from_str(json).unwrap();
        assert!(req.alias.is_none());
        assert_eq!(req.key.as_deref(), Some("sk-rotated"));
    }

    #[test]
    fn test_update_provider_account_request_both_fields() {
        let json = r#"{"alias": "prod", "key": "sk-rotated"}"#;
        let req: UpdateProviderAccountRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.alias.as_deref(), Some("prod"));
        assert_eq!(req.key.as_deref(), Some("sk-rotated"));
    }
}
