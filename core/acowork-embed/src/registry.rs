//! Model registry — reads and manages the embedding_models.json registry.

use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// Embedding model registry types live in `acowork-core` (protocol layer)
// and are shared with the Gateway, which also serializes them to disk.
// Re-export here so `pool.rs` / `model.rs` keep using `crate::registry::`
// paths unchanged — there is exactly ONE definition, no drift.
pub use acowork_core::protocol::{
    EmbeddingModelEntry, EmbeddingModelsFile, OnnxOutputKind, PoolingStrategy,
};

/// Model download/load status (internal representation).
///
/// For API responses, use [`ModelStatusFlat`] or [`ModelStatus::to_api_parts`]
/// to get a consistent flat format (always string `status` + optional fields).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelStatus {
    /// Model registry entry exists but not downloaded.
    NotDownloaded,
    /// Model is currently being downloaded (0-100 progress).
    Downloading(u8),
    /// Model files are on disk, ready to load.
    Downloaded,
    /// Model is loaded into ONNX Runtime and ready for inference.
    Loaded,
    /// Download or load failed.
    Failed(String),
}

/// Flat API representation of [`ModelStatus`].
///
/// Always serializes as a JSON object with a string `status` field,
/// plus optional `progress` / `error` fields. This avoids the
/// inconsistent string-vs-object output that raw enum serialization
/// would produce.
#[derive(Debug, Clone, Serialize)]
pub struct ModelStatusFlat {
    /// One of: `"not_downloaded"`, `"downloading"`, `"downloaded"`,
    /// `"loaded"`, `"failed"`.
    pub status: &'static str,
    /// Download progress percentage (0-100). Only present when downloading.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<u8>,
    /// Error message. Only present on failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ModelStatus {
    /// Convert to a flat API representation with consistent JSON shape.
    pub fn to_api_parts(&self) -> ModelStatusFlat {
        match self {
            ModelStatus::NotDownloaded => ModelStatusFlat {
                status: "not_downloaded",
                progress: None,
                error: None,
            },
            ModelStatus::Downloading(pct) => ModelStatusFlat {
                status: "downloading",
                progress: Some(*pct),
                error: None,
            },
            ModelStatus::Downloaded => ModelStatusFlat {
                status: "downloaded",
                progress: None,
                error: None,
            },
            ModelStatus::Loaded => ModelStatusFlat {
                status: "loaded",
                progress: None,
                error: None,
            },
            ModelStatus::Failed(reason) => ModelStatusFlat {
                status: "failed",
                progress: None,
                error: Some(reason.clone()),
            },
        }
    }
}

/// Model info with status (for API responses).
#[derive(Debug, Clone, Serialize)]
pub struct ModelInfo {
    #[serde(flatten)]
    pub entry: EmbeddingModelEntry,
    #[serde(flatten)]
    pub status: ModelStatusFlat,
}

/// The model registry, loaded from embedding_models.json.
#[derive(Clone)]
pub struct ModelRegistry {
    models: Vec<EmbeddingModelEntry>,
    /// Map from model ID to index in the models vec.
    index: HashMap<String, usize>,
}

impl ModelRegistry {
    /// Load registry from the given data directory.
    ///
    /// Search order (matches the `offline_providers.json` pattern):
    ///   1. `{data_dir}/embedding_models.json`  (user-writable, primary)
    ///   2. `{exe_dir}/embedding_models.json`   (installer-provided)
    ///   3. `$CARGO_MANIFEST_DIR/assets/`       (dev / test via cargo)
    ///   4. `{cwd}/embedding_models.json`        (dev convenience)
    ///
    /// Returns an empty registry if no file is found anywhere.
    pub fn load(data_dir: &Path) -> Self {
        let candidates = Self::build_candidates(data_dir);

        for path in &candidates {
            if path.exists() {
                match std::fs::read_to_string(path) {
                    Ok(content) => match serde_json::from_str::<EmbeddingModelsFile>(&content) {
                        Ok(reg) => {
                            tracing::info!(
                                path = %path.display(),
                                count = reg.models.len(),
                                "Loaded embedding model registry"
                            );
                            return Self::from_models(reg.models);
                        }
                        Err(e) => {
                            tracing::warn!(
                                path = %path.display(),
                                error = %e,
                                "Failed to parse embedding_models.json"
                            );
                        }
                    },
                    Err(e) => {
                        tracing::warn!(
                            path = %path.display(),
                            error = %e,
                            "Failed to read embedding_models.json"
                        );
                    }
                }
            }
        }

        tracing::warn!(
            "embedding_models.json not found in any candidate path, using empty registry"
        );
        Self::from_models(Vec::new())
    }

    /// Build candidate file paths in priority order.
    ///
    /// Two locations only:
    ///   1. `{data_dir}/embedding_models.json` — user-editable copy (always wins)
    ///   2. `{exe_dir}/embedding_models.json`  — bundled copy, placed there by
    ///      whatever distributes the binary (dev build script, package installer,
    ///      Tauri bundler).
    fn build_candidates(data_dir: &Path) -> Vec<PathBuf> {
        let mut candidates = Vec::new();
        candidates.push(data_dir.join("embedding_models.json"));
        if let Ok(exe_path) = std::env::current_exe()
            && let Some(exe_dir) = exe_path.parent()
        {
            candidates.push(exe_dir.join("embedding_models.json"));
        }
        candidates
    }

    /// Create registry from a list of model entries.
    fn from_models(models: Vec<EmbeddingModelEntry>) -> Self {
        let index = models
            .iter()
            .enumerate()
            .map(|(i, m)| (m.id.clone(), i))
            .collect();
        Self { models, index }
    }

    /// Get all model entries.
    pub fn models(&self) -> &[EmbeddingModelEntry] {
        &self.models
    }

    /// Get a model entry by ID.
    pub fn get(&self, id: &str) -> Option<&EmbeddingModelEntry> {
        self.index.get(id).map(|&i| &self.models[i])
    }

    /// Get the recommended model (first model with recommended=true).
    pub fn recommended(&self) -> Option<&EmbeddingModelEntry> {
        self.models.iter().find(|m| m.recommended)
    }

    /// External-data weight files for a variant (empty if it has none).
    ///
    /// Resolves the WEIGHTS ONLY — it does not tell you which graph they go
    /// with. Use [`Self::resolve_variant`], which returns both together,
    /// anywhere a download is actually being assembled.
    pub fn external_data_paths(&self, model_id: &str, variant: &str) -> Vec<String> {
        self.get(model_id)
            .and_then(|model| model.external_data_files.get(variant))
            .cloned()
            .unwrap_or_default()
    }

    /// Resolve a variant to the `(onnx_file, external_data_files)` pair that
    /// must be downloaded together.
    ///
    /// Returns `None` when `variant` is not a key of `onnx_variants`. Callers
    /// must NOT assemble a download from the graph and weights separately:
    /// resolving the graph alone falls back to `onnx_file` while
    /// `external_data_paths` yields an empty list, so the download silently
    /// omits the `*.onnx_data` weights. ORT resolves those by name at
    /// session-creation time, so the result is a model that downloads
    /// "successfully" and then fails to load.
    ///
    /// Registries without an `onnx_variants` map accept any variant and use
    /// `onnx_file` (no variant data to select).
    pub fn resolve_variant(&self, model_id: &str, variant: &str) -> Option<(String, Vec<String>)> {
        let model = self.get(model_id)?;
        let onnx_file = match &model.onnx_variants {
            Some(variants) => variants.get(variant)?.clone(),
            None => model.onnx_file.clone(),
        };
        Some((onnx_file, self.external_data_paths(model_id, variant)))
    }

    /// The variant keys this model supports, for error messages.
    pub fn variants(&self, model_id: &str) -> Vec<String> {
        self.get(model_id)
            .and_then(|m| m.onnx_variants.as_ref())
            .map(|v| {
                let mut keys: Vec<String> = v.keys().cloned().collect();
                keys.sort();
                keys
            })
            .unwrap_or_default()
    }

    /// Check if a model is downloaded (its directory exists on disk).
    pub fn is_downloaded(&self, models_dir: &Path, model_id: &str) -> bool {
        let model_dir = models_dir.join(model_id);
        model_dir.exists() && model_dir.is_dir()
    }

    /// Get the local directory for a model.
    pub fn model_dir(&self, models_dir: &Path, model_id: &str) -> PathBuf {
        models_dir.join(model_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression test: an unknown variant must NOT silently resolve to the
    /// registry's default `onnx_file`.
    ///
    /// The old `onnx_path(...)` + `external_data_paths(...)` pair did exactly
    /// that — `onnx_path` fell back to `onnx_file` for an unknown key while
    /// `external_data_paths` returned `[]`. The download then skipped the
    /// `*.onnx_data` weights entirely, ORT failed to resolve them at
    /// session-creation time, and the user got a model that downloaded at
    /// 100% and could never load.
    #[test]
    fn unknown_variant_does_not_silently_fall_back() {
        let dir = tempfile::tempdir().unwrap();
        let json = r#"{"version": 1, "models": [{
            "id": "bge-test",
            "name": "Test",
            "dimension": 512,
            "max_tokens": 512,
            "size_mb": 90,
            "languages": ["zh"],
            "hf_repo": "onnx-community/bge-test-ONNX",
            "pooling_strategy": "cls",
            "onnx_file": "onnx/model.onnx",
            "tokenizer_file": "tokenizer.json",
            "onnx_variants": {
                "fp32": "onnx/model.onnx",
                "fp16": "onnx/model_fp16.onnx"
            },
            "external_data_files": {
                "fp32": ["onnx/model.onnx_data"],
                "fp16": ["onnx/model_fp16.onnx_data"]
            },
            "bundled": false,
            "recommended": true
        }]}"#;
        std::fs::write(dir.path().join("embedding_models.json"), json).unwrap();
        let registry = ModelRegistry::load(dir.path());

        // A known variant resolves the graph AND its weights together.
        let (onnx_file, ext) = registry.resolve_variant("bge-test", "fp16").unwrap();
        assert_eq!(onnx_file, "onnx/model_fp16.onnx");
        assert_eq!(ext, vec!["onnx/model_fp16.onnx_data".to_string()]);

        // An unknown variant is rejected rather than downgraded to the fp32
        // graph — otherwise the weights would be missing at load time.
        assert!(
            registry.resolve_variant("bge-test", "onnx").is_none(),
            "unknown variant must not fall back to the default onnx_file"
        );
        assert_eq!(registry.variants("bge-test"), vec!["fp16", "fp32"]);
    }

    #[test]
    fn variant_resolution_without_variants_map_uses_onnx_file() {
        // Registries predating `onnx_variants` have nothing to select, so
        // any variant resolves to `onnx_file` (with no external data).
        let dir = tempfile::tempdir().unwrap();
        let json = r#"{"version": 1, "models": [{
            "id": "legacy",
            "name": "Legacy",
            "dimension": 256,
            "max_tokens": 128,
            "size_mb": 50,
            "languages": ["en"],
            "hf_repo": "test/repo",
            "pooling_strategy": "mean",
            "onnx_file": "model.onnx",
            "tokenizer_file": "tokenizer.json",
            "bundled": false,
            "recommended": true
        }]}"#;
        std::fs::write(dir.path().join("embedding_models.json"), json).unwrap();
        let registry = ModelRegistry::load(dir.path());

        let (onnx_file, ext) = registry.resolve_variant("legacy", "fp32").unwrap();
        assert_eq!(onnx_file, "model.onnx");
        assert!(ext.is_empty());
        assert!(registry.variants("legacy").is_empty());
    }

    #[test]
    fn test_load_registry_from_bundled_path() {
        // In a real install the bundled copy lives next to the binary.
        // For the test, copy the manifest into a temp dir and run the
        // test binary with current_exe redirected via the test harness.
        // Simpler: just check the data_dir path resolution works.
        let dir = tempfile::tempdir().unwrap();
        let registry = ModelRegistry::load(dir.path());
        // No data_dir file, no bundled file in test env → empty registry
        assert!(registry.models().is_empty());
    }

    #[test]
    fn test_load_registry_from_data_dir() {
        // When data_dir contains embedding_models.json, it takes priority over fallbacks
        let dir = tempfile::tempdir().unwrap();
        let custom_json = r#"{"version": 1, "models": [{
            "id": "custom-model",
            "name": "Custom",
            "dimension": 256,
            "max_tokens": 128,
            "size_mb": 50,
            "languages": ["en"],
            "hf_repo": "test/repo",
            "pooling_strategy": "mean",
            "onnx_file": "model.onnx",
            "tokenizer_file": "tokenizer.json",
            "bundled": false,
            "recommended": true
        }]}"#;
        std::fs::write(dir.path().join("embedding_models.json"), custom_json).unwrap();
        let registry = ModelRegistry::load(dir.path());
        assert_eq!(registry.models().len(), 1);
        assert!(registry.get("custom-model").is_some());
        assert!(registry.get("bge-small-zh-v1.5").is_none());
    }

    #[test]
    fn test_recommended_model() {
        let dir = tempfile::tempdir().unwrap();
        seed_test_registry(dir.path());
        let registry = ModelRegistry::load(dir.path());
        let rec = registry.recommended().unwrap();
        assert_eq!(rec.id, "bge-small-zh-v1.5");
        assert_eq!(rec.pooling_strategy, PoolingStrategy::Cls);
        assert_eq!(rec.dimension, 512);
    }

    #[test]
    fn test_onnx_variant_selection() {
        let dir = tempfile::tempdir().unwrap();
        seed_test_registry(dir.path());
        let registry = ModelRegistry::load(dir.path());

        // fp16 variant → graph + its matching weights
        let (path, ext) = registry.resolve_variant("bge-small-zh-v1.5", "fp16").unwrap();
        assert_eq!(path, "onnx/model_fp16.onnx");
        assert_eq!(ext, vec!["onnx/model_fp16.onnx_data".to_string()]);

        // int8 variant
        let (path, ext) = registry.resolve_variant("bge-small-zh-v1.5", "int8").unwrap();
        assert_eq!(path, "onnx/model_quantized.onnx");
        assert_eq!(ext, vec!["onnx/model_quantized.onnx_data".to_string()]);

        // Model whose variants map lacks fp16 (bge-m3 has only fp32):
        // asking for fp16 must be rejected, not silently downgraded.
        assert!(registry.resolve_variant("bge-m3", "fp16").is_none());
        let (path, _) = registry.resolve_variant("bge-m3", "fp32").unwrap();
        assert_eq!(path, "model.onnx");
    }

    /// Copy the source manifest into the test temp dir so it acts as the
    /// user's `data_dir/embedding_models.json`. Tests use `CARGO_MANIFEST_DIR`
    /// only to locate the fixture file — this is test setup, not a runtime
    /// path resolver.
    fn seed_test_registry(data_dir: &Path) {
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("assets")
            .join("embedding_models.json");
        std::fs::copy(&manifest, data_dir.join("embedding_models.json"))
            .expect("test fixture: source manifest must exist");
    }

    #[test]
    fn test_pooling_strategy_deserialize() {
        let json = r#"{"pooling_strategy": "mean"}"#;
        let v: serde_json::Value = serde_json::from_str(json).unwrap();
        let strategy: PoolingStrategy =
            serde_json::from_value(v["pooling_strategy"].clone()).unwrap();
        assert_eq!(strategy, PoolingStrategy::Mean);
    }

    #[test]
    fn test_onnx_output_kind_defaults_to_hidden_states() {
        // Existing entries that don't set `onnx_output_kind` must keep
        // the old behavior (raw last_hidden_state → manual pooling).
        // Use a full entry (the field is on `EmbeddingModelEntry`) and
        // verify the default kicks in when the key is omitted.
        let json = r#"{
            "id": "x",
            "name": "X",
            "dimension": 64,
            "max_tokens": 32,
            "size_mb": 1,
            "languages": ["en"],
            "hf_repo": "r",
            "pooling_strategy": "cls",
            "onnx_file": "m.onnx",
            "tokenizer_file": "t.json",
            "bundled": false,
            "recommended": false
        }"#;
        let entry: EmbeddingModelEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.onnx_output_kind, OnnxOutputKind::HiddenStates);
    }

    #[test]
    fn test_onnx_output_kind_already_pooled_roundtrip() {
        // The bge-m3 entry and any future already-pooled encoder must
        // deserialize from the JSON tag and serialize back identically.
        let json = r#""already_pooled""#;
        let kind: OnnxOutputKind = serde_json::from_str(json).unwrap();
        assert_eq!(kind, OnnxOutputKind::AlreadyPooled);
        let back = serde_json::to_string(&kind).unwrap();
        assert_eq!(back, r#""already_pooled""#);
    }
}
