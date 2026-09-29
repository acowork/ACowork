//! Settings HTTP API — global runtime toggles that are not tied to a
//! specific provider, MCP server, or search engine.
//!
//! ADR-056: Hosts the `default_compact_models` endpoint. Lives in its own
//! module so future global settings (e.g. global embedding default,
//! auto-compaction thresholds) can be added alongside without polluting
//! `provider_api.rs`.

use axum::{Json, Router, extract::State, routing::get};
use serde::{Deserialize, Serialize};

use acowork_core::protocol::CompactModelRef;

use crate::http::routes::{ApiError, AppState};
use crate::resource_cache;

/// Build the settings router. Mounted at `/api/settings`.
pub fn settings_routes() -> Router<AppState> {
    Router::new().route(
        "/api/settings/default-compact-model",
        get(get_default_compact_model).put(put_default_compact_model),
    )
}

// ── Response / request DTOs ──────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct DefaultCompactModelResponse {
    /// Ordered candidate list, empty when not configured.
    pub default_compact_models: Vec<CompactModelRef>,
}

#[derive(Debug, Deserialize)]
pub struct PutDefaultCompactModelRequest {
    /// Ordered candidate list. `[]` clears the global override.
    ///
    /// `Option` so "field absent" stays distinguishable from "field
    /// present but empty" — a `Vec` alone cannot express an explicit
    /// clear that must beat the legacy field below.
    #[serde(default)]
    pub default_compact_models: Option<Vec<CompactModelRef>>,
    /// Legacy single-value form, still accepted for older Desktop builds:
    /// `Some(r)` → `[r]`, `null` → `[]` (only when the list field is absent).
    #[serde(default)]
    #[allow(deprecated)]
    pub default_compact_model: Option<Option<CompactModelRef>>,
}

impl PutDefaultCompactModelRequest {
    /// Effective list: the new field wins whenever it is *present*; the
    /// legacy field is folded in only when the new one was omitted entirely.
    ///
    /// Presence, not emptiness, is the rule: a client that sends both
    /// dialects at once (so an old Gateway also understands it) must still
    /// be able to clear the setting with an explicit `[]`.
    fn effective_list(&self) -> Vec<CompactModelRef> {
        if let Some(list) = &self.default_compact_models {
            return list.clone();
        }
        match &self.default_compact_model {
            Some(Some(r)) => vec![r.clone()],
            _ => Vec::new(),
        }
    }
}

// ── Handlers ─────────────────────────────────────────────────────────

/// `GET /api/settings/default-compact-model` — read current list.
pub async fn get_default_compact_model(
    State(state): State<AppState>,
) -> Result<Json<DefaultCompactModelResponse>, ApiError> {
    let gw = state.gateway_state.read().await;
    let current = gw
        .resource_cache
        .provider_list
        .default_compact_models
        .clone();
    Ok(Json(DefaultCompactModelResponse {
        default_compact_models: current,
    }))
}

/// `PUT /api/settings/default-compact-model` — set or clear.
///
/// Body: `{ "default_compact_models": [ { "provider_id": "...", "model_id": "..." }, ... ] }`
/// or `{ "default_compact_models": [] }` to clear. The legacy single-object
/// form `{ "default_compact_model": {...} | null }` is still accepted.
///
/// 422 on invalid (provider_id unknown, model_id not in that provider).
pub async fn put_default_compact_model(
    State(state): State<AppState>,
    Json(body): Json<PutDefaultCompactModelRequest>,
) -> Result<Json<DefaultCompactModelResponse>, ApiError> {
    let data_dir = {
        let gw = state.gateway_state.read().await;
        gw.config
            .as_ref()
            .map(|c| std::path::PathBuf::from(&c.data_dir))
            .unwrap_or_else(|| std::path::PathBuf::from("./data"))
    };

    let mut gw = state.gateway_state.write().await;

    // In-memory mutation with validation. `set_default_compact_models` bumps
    // `version` on success; we persist right after. Validation failure
    // (unknown provider_id / model_id not in that provider) → 422 per
    // ADR-056 §4.1.
    let prev = resource_cache::set_default_compact_models(
        &mut gw.resource_cache.provider_list,
        body.effective_list(),
    )
    .map_err(|e| ApiError::unprocessable_entity(&e))?;

    // Persist to disk (the in-memory version bump is sufficient; we don't
    // call `persist_provider_cache` here because that would bump the version
    // a *second* time).
    if let Err(e) = resource_cache::save_provider_list(&data_dir, &gw.resource_cache.provider_list)
    {
        // Roll back in-memory mutation on disk failure so on-disk + memory
        // stay consistent. The setter already mutated `default_compact_models`
        // and bumped `version`; revert those.
        gw.resource_cache.provider_list.default_compact_models = prev.clone();
        // The version bump is monotonic and cannot be trivially reversed
        // without racing with concurrent updates, so we leave it. The next
        // legitimate save will replace it.
        return Err(ApiError::internal(&format!(
            "Failed to persist provider_list.json: {}",
            e
        )));
    }

    let current = gw
        .resource_cache
        .provider_list
        .default_compact_models
        .clone();

    // Trigger MQTT retained republish so Runtimes pick up the new value
    // immediately (no need to wait for the next periodic publish).
    if let Some(ref trigger) = state.mqtt_publisher_trigger {
        trigger.trigger();
    }

    tracing::info!(
        new = ?current,
        prev = ?prev,
        "default_compact_models updated via HTTP"
    );

    Ok(Json(DefaultCompactModelResponse {
        default_compact_models: current,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_deserialization_with_list() {
        let json = r#"{"default_compact_models":[{"provider_id":"ollama","model_id":"qwen2.5:0.5b"},{"provider_id":"deepseek","model_id":"dsv"}]}"#;
        let req: PutDefaultCompactModelRequest = serde_json::from_str(json).unwrap();
        let list = req.effective_list();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].provider_id, "ollama");
        assert_eq!(list[1].model_id, "dsv");
    }

    #[test]
    fn request_deserialization_empty_list_clears() {
        let json = r#"{"default_compact_models":[]}"#;
        let req: PutDefaultCompactModelRequest = serde_json::from_str(json).unwrap();
        assert!(req.effective_list().is_empty());
    }

    #[test]
    fn request_deserialization_legacy_single_still_accepted() {
        let json = r#"{"default_compact_model":{"provider_id":"ollama","model_id":"qwen2.5:0.5b"}}"#;
        let req: PutDefaultCompactModelRequest = serde_json::from_str(json).unwrap();
        let list = req.effective_list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].provider_id, "ollama");
    }

    #[test]
    fn request_deserialization_legacy_null_clears() {
        let json = r#"{"default_compact_model":null}"#;
        let req: PutDefaultCompactModelRequest = serde_json::from_str(json).unwrap();
        assert!(req.effective_list().is_empty());
    }

    #[test]
    fn request_deserialization_explicit_empty_list_wins_over_legacy_field() {
        // A client that speaks both dialects (sends the new list AND the
        // legacy single value, so an old Gateway also understands it) must
        // still be able to CLEAR the setting. "New field wins" is a
        // presence rule, not a non-emptiness rule: an explicit
        // `[]` has to win over a stale legacy value, otherwise clearing
        // silently resurrects the legacy pick.
        let json = r#"{"default_compact_models":[],"default_compact_model":{"provider_id":"ollama","model_id":"qwen2.5:0.5b"}}"#;
        let req: PutDefaultCompactModelRequest = serde_json::from_str(json).unwrap();
        assert!(
            req.effective_list().is_empty(),
            "explicit empty list must clear, not fold the legacy value back in"
        );
    }

    #[test]
    fn request_deserialization_new_list_wins_over_legacy_field() {
        let json = r#"{"default_compact_models":[{"provider_id":"ds","model_id":"flash"}],"default_compact_model":{"provider_id":"ollama","model_id":"qwen2.5:0.5b"}}"#;
        let req: PutDefaultCompactModelRequest = serde_json::from_str(json).unwrap();
        let list = req.effective_list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].provider_id, "ds");
    }

    #[test]
    fn request_deserialization_missing_field_is_empty() {
        // Omitted fields → empty list via #[serde(default)].
        let json = r#"{}"#;
        let req: PutDefaultCompactModelRequest = serde_json::from_str(json).unwrap();
        assert!(req.effective_list().is_empty());
    }
}
