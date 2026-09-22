//! Associative diffusion retrieval.

use std::collections::HashMap;

use grafeo_common::types::NodeId;

use crate::error::Result;
use crate::grafeo::GrafeoStore;
use crate::index_config::validate_embedding_dim;

/// Convert cosine distance to similarity score.
///
/// Assumes cosine distance ∈ [0, 2], converts to [0, 1] similarity:
/// - distance = 0 (identical) → similarity = 1.0
/// - distance = 2 (opposite) → similarity = 0.0
/// - distance = 1 (orthogonal) → similarity = 0.5
#[inline]
pub fn cosine_distance_to_similarity(dist: f32) -> f64 {
    (2.0 - f64::from(dist)) / 2.0
}

impl GrafeoStore {
    /// Vector similarity search using the HNSW index.
    ///
    /// Returns up to `k` results as `(NodeId, distance)` pairs sorted by
    /// ascending distance (lower = more similar).
    pub fn vector_search(
        &self,
        label: &str,
        embedding: &[f32],
        k: usize,
        ef: Option<usize>,
    ) -> Result<Vec<(NodeId, f32)>> {
        let results = self
            .db
            .vector_search(label, "embedding", embedding, k, ef, None)?;
        Ok(results)
    }

    /// Full-text search using the BM25 index.
    ///
    /// Returns up to `k` results as `(NodeId, score)` pairs sorted by
    /// descending score (higher = more relevant).
    pub fn text_search(&self, label: &str, query: &str, k: usize) -> Result<Vec<(NodeId, f64)>> {
        let results = self.db.text_search(label, "content", query, k)?;
        Ok(results)
    }

    /// Hybrid search combining BM25 text relevance and vector similarity.
    ///
    /// Uses Reciprocal Rank Fusion (RRF) by default.
    /// Returns up to `k` results as `(NodeId, fused_score)` pairs sorted by
    /// descending fused score.
    pub fn hybrid_search(
        &self,
        label: &str,
        text_prop: &str,
        vec_prop: &str,
        query: &str,
        embedding: &[f32],
        k: usize,
    ) -> Result<Vec<(NodeId, f64)>> {
        let results =
            self.db
                .hybrid_search(label, text_prop, vec_prop, query, Some(embedding), k, None)?;
        // Diagnostic: an empty fusion result on a non-empty index means at least
        // one source came back empty, and RRF cannot tell us which. Probe both
        // sources so the log names the culprit (usize::MAX = the probe failed).
        if results.is_empty() {
            let text_hits = self
                .db
                .text_search(label, text_prop, query, k)
                .map(|v| v.len())
                .unwrap_or(usize::MAX);
            let vector_hits = self
                .vector_search(label, embedding, k, None)
                .map(|v| v.len())
                .unwrap_or(usize::MAX);
            tracing::info!(
                label,
                text_prop,
                vec_prop,
                query_len = query.len(),
                embedding_dim = embedding.len(),
                k,
                text_hits,
                vector_hits,
                "hybrid_search returned empty; per-source probe"
            );
        }
        Ok(results)
    }

    /// Maximal Marginal Relevance (MMR) search.
    ///
    /// Balances relevance (similarity to query) with diversity
    /// (dissimilarity among selected results).
    ///
    /// `lambda` controls the trade-off:
    /// - `1.0` = pure relevance
    /// - `0.0` = pure diversity
    pub fn mmr_search(
        &self,
        label: &str,
        embedding: &[f32],
        k: usize,
        lambda: Option<f32>,
    ) -> Result<Vec<(NodeId, f32)>> {
        let results =
            self.db
                .mmr_search(label, "embedding", embedding, k, None, lambda, None, None)?;
        Ok(results)
    }

    /// Graph expansion (simple): traverse from a start node up to `max_hops` away.
    ///
    /// Returns a deduplicated list of reachable [`NodeId`]s (excluding the
    /// start node itself). The `threshold` parameter is reserved for future
    /// score-based pruning and currently has no effect.
    ///
    /// For the full-featured expansion with scoring and early stopping,
    /// see [`crate::spreading::graph_expand`].
    /// S5.3: Uses parameterized query (`$start_id`) for node ID
    /// to follow safe query practices, even though node IDs are numeric.
    pub fn graph_expand_simple(
        &self,
        start_id: NodeId,
        max_hops: usize,
        _threshold: f32,
    ) -> Result<Vec<NodeId>> {
        let session = self.db.session();
        let gql = format!(
            "MATCH (m)-[r*1..{}]-(other) WHERE id(m) = $start_id RETURN DISTINCT id(other)",
            max_hops
        );
        let mut params = std::collections::HashMap::new();
        params.insert(
            "start_id".to_string(),
            grafeo_common::types::Value::from(start_id.as_u64() as i64),
        );
        let result = session.execute_with_params(&gql, params)?;

        let mut nodes = Vec::new();
        for row in result.rows() {
            if let Some(grafeo_common::types::Value::Int64(id)) = row.first() {
                let node_id = NodeId::new(*id as u64);
                if node_id != start_id {
                    nodes.push(node_id);
                }
            }
        }
        Ok(nodes)
    }

    /// Full-text search over the `content` property.
    pub fn text_search_filtered(
        &self,
        label: &str,
        query: &str,
        k: usize,
    ) -> Result<Vec<(NodeId, f64)>> {
        Ok(self.db.text_search(label, "content", query, k)?)
    }

    /// Hybrid search across the given text/vector properties, ranked by RRF.
    #[allow(clippy::too_many_arguments)]
    pub fn hybrid_search_filtered(
        &self,
        label: &str,
        text_prop: &str,
        vec_prop: &str,
        query: &str,
        embedding: &[f32],
        k: usize,
    ) -> Result<Vec<(NodeId, f64)>> {
        Ok(self
            .db
            .hybrid_search(label, text_prop, vec_prop, query, Some(embedding), k, None)?)
    }

    /// Vector search with configurable `ef`.
    ///
    /// Validates the embedding dimension before searching.
    /// Returns `(NodeId, similarity)` pairs in [0, 1], sorted by descending
    /// similarity.
    pub fn vector_search_with_params(
        &self,
        label: &str,
        embedding: &[f32],
        k: usize,
        ef_search: usize,
    ) -> Result<Vec<(NodeId, f64)>> {
        validate_embedding_dim(embedding, self.hnsw_config.dim)?;
        let raw = self
            .db
            .vector_search(label, "embedding", embedding, k, Some(ef_search), None)?;
        Ok(raw
            .into_iter()
            .map(|(id, dist)| (id, cosine_distance_to_similarity(dist)))
            .collect())
    }

    /// Text search on a specific field.
    ///
    /// Searches the BM25 index for `field` on the given `label`. No score
    /// threshold: BM25 scores drift with corpus statistics.
    pub fn text_search_with_filter(
        &self,
        label: &str,
        field: &str,
        query: &str,
        k: usize,
    ) -> Result<Vec<(NodeId, f64)>> {
        Ok(self.db.text_search(label, field, query, k)?)
    }

    /// Full hybrid search: BM25 + HNSW, ranked by RRF, thresholded by cosine.
    ///
    /// Fusion decides the *order* but yields no relevance magnitude (RRF
    /// scores are rank-only). The absolute cosine similarity is recovered from
    /// the vector index and used both for the `min_cosine` threshold and as
    /// the returned score, normalized to [0, 1] (higher = more relevant).
    #[allow(clippy::too_many_arguments)]
    pub fn hybrid_search_full(
        &self,
        label: &str,
        query: &str,
        embedding: &[f32],
        k: usize,
        _text_weight: f32,
        _vector_weight: f32,
        min_cosine: Option<f32>,
    ) -> Result<Vec<(NodeId, f64)>> {
        validate_embedding_dim(embedding, self.hnsw_config.dim)?;
        let fused = self.db.hybrid_search(
            label,
            "content",
            "embedding",
            query,
            Some(embedding),
            k,
            None,
        )?;

        let distances: HashMap<NodeId, f32> = self
            .db
            .vector_search(label, "embedding", embedding, k.saturating_mul(2), None, None)?
            .into_iter()
            .collect();

        // `min_cosine` lives in cosine space ([-1, 1]); -1.0 keeps everything.
        let floor = f64::from(min_cosine.unwrap_or(-1.0));
        Ok(fused
            .into_iter()
            .filter_map(|(id, _fused)| match distances.get(&id) {
                Some(&dist) => {
                    let cosine = 1.0 - f64::from(dist);
                    (cosine >= floor).then(|| (id, cosine_distance_to_similarity(dist)))
                }
                None => Some((id, 0.5)),
            })
            .collect())
    }

    /// Perform a hybrid search and collect retrieval metrics.
    ///
    /// Computes statistics about the result set, including whether abstention
    /// was triggered (empty result set).
    pub fn search_with_metrics(
        &self,
        label: &str,
        query: &str,
        embedding: &[f32],
        k: usize,
    ) -> Result<(Vec<(NodeId, f64)>, acowork_memory::RetrievalMetrics)> {
        let results = self.db.hybrid_search(
            label,
            "content",
            "embedding",
            query,
            Some(embedding),
            k,
            None,
        )?;

        let filtered = results;
        let filtered_count = 0;

        let result_count = filtered.len();
        let max_score = filtered
            .iter()
            .map(|(_, s)| *s as f32)
            .fold(0.0_f32, f32::max);
        let avg_score = if result_count > 0 {
            filtered.iter().map(|(_, s)| *s as f32).sum::<f32>() / result_count as f32
        } else {
            0.0
        };
        let abstention_triggered = result_count == 0;

        let metrics = acowork_memory::RetrievalMetrics {
            result_count,
            avg_score,
            max_score,
            abstention_triggered,
            filtered_count,
            retrieval_level: 0,
            graph_expand_nodes: 0,
            hint_type: acowork_memory::HintType::Semantic,
        };

        Ok((filtered, metrics))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index_config::{HnswConfig, validate_embedding_dim};
    use crate::types::{DEFAULT_EMBEDDING_DIM, labels};
    use grafeo_common::types::Value;

    /// Helper: create an in-memory GrafeoStore for testing.
    fn test_store() -> GrafeoStore {
        GrafeoStore::new_in_memory().unwrap()
    }

    /// Helper: generate a test embedding vector of the expected dimension.
    fn test_embedding() -> Vec<f32> {
        vec![0.1f32; DEFAULT_EMBEDDING_DIM]
    }

    /// Helper: store an Episodic node with both content and embedding.
    ///
    /// Creates the node with content first, then sets the embedding through
    /// [`GrafeoStore::set_node_property`], which keeps the vector index current.
    fn store_episode(store: &GrafeoStore, content: &str, embedding: &[f32]) -> NodeId {
        let id = store
            .store_node(labels::EPISODIC, [("content", Value::from(content))])
            .unwrap();
        store.set_node_property(
            id,
            "embedding",
            Value::Vector(std::sync::Arc::from(embedding.to_vec().into_boxed_slice())),
        );
        id
    }

    // =====================================================================
    // Test 1: HNSW config defaults
    // =====================================================================

    #[test]
    fn test_hnsw_config_default_values() {
        let config = HnswConfig::default();
        assert_eq!(config.m, 16);
        assert_eq!(config.ef_construction, 100);
        assert_eq!(config.ef_search, 64);
        assert_eq!(config.dim, DEFAULT_EMBEDDING_DIM);
    }

    // =====================================================================
    // Test 2: HNSW config builder pattern
    // =====================================================================

    #[test]
    fn test_hnsw_config_builder() {
        let config = HnswConfig::new(768)
            .with_m(32)
            .with_ef_construction(200)
            .with_ef_search(128);
        assert_eq!(config.m, 32);
        assert_eq!(config.ef_construction, 200);
        assert_eq!(config.ef_search, 128);
        assert_eq!(config.dim, 768);
    }

    // =====================================================================
    // Test 3: vector_search_with_params returns results for indexed data
    // =====================================================================

    #[test]
    fn test_vector_search_with_params_basic() {
        let store = test_store();
        let emb = test_embedding();
        store_episode(&store, "test content", &emb);

        let results = store
            .vector_search_with_params(labels::EPISODIC, &emb, 5, 64)
            .unwrap();
        assert_eq!(results.len(), 1);
        // Cosine similarity of identical vectors should be ~1.0
        let (_, score) = results[0];
        assert!(score > 0.9, "expected similarity > 0.9, got {score}");
    }

    // =====================================================================
    // Test 4: vector_search_with_params rejects wrong dimension
    // =====================================================================

    #[test]
    fn test_vector_search_with_params_wrong_dim() {
        let store = test_store();
        let bad_emb = vec![0.1f32; 128];

        let result = store.vector_search_with_params(labels::EPISODIC, &bad_emb, 5, 64);
        assert!(result.is_err(), "expected error for wrong dimension");
    }

    // =====================================================================
    // Test 5: text_search_with_filter returns results
    // =====================================================================

    #[test]
    fn test_text_search_with_filter_basic() {
        let store = test_store();
        let emb = test_embedding();
        store_episode(&store, "the quick brown fox", &emb);
        store_episode(&store, "the lazy dog", &emb);

        let results = store
            .text_search_with_filter(labels::EPISODIC, "content", "quick fox", 5)
            .unwrap();
        assert!(!results.is_empty(), "expected at least one result");
    }

    // =====================================================================
    // Test 7: hybrid_search_full basic functionality
    // =====================================================================

    #[test]
    fn test_hybrid_search_full_basic() {
        let store = test_store();
        let emb = test_embedding();
        store_episode(&store, "machine learning algorithms", &emb);

        let results = store
            .hybrid_search_full(
                labels::EPISODIC,
                "machine learning",
                &emb,
                5,
                0.5,
                0.5,
                None,
            )
            .unwrap();
        assert!(!results.is_empty(), "expected at least one result");
    }

    // =====================================================================
    // Test 7b: hybrid_search_full applies the floor in cosine space
    // =====================================================================

    #[test]
    fn test_hybrid_search_full_cosine_floor() {
        // Regression: the floor used to be applied to the *fused* score, which
        // goes negative on the single-source (vector-only) path — so a query
        // that only the vector index matched was filtered down to nothing. The
        // floor now lives in cosine space and the returned score is the
        // normalized similarity in [0, 1].
        let store = test_store();
        let emb = test_embedding();
        store_episode(&store, "engineering meeting notes", &emb);

        // Query text matches no BM25 term -> only the vector source returns
        // rows (sources.len() == 1). This is the case the old code dropped.
        let all = store
            .hybrid_search_full(labels::EPISODIC, "zzz-no-text-match", &emb, 5, 0.0, 0.0, None)
            .unwrap();
        assert!(!all.is_empty(), "vector-only hits must not be dropped");
        for (_, s) in &all {
            assert!((0.0..=1.0).contains(s), "score {s} outside [0, 1]");
        }

        // A floor above the cosine maximum drops everything.
        let none = store
            .hybrid_search_full(labels::EPISODIC, "zzz-no-text-match", &emb, 5, 0.0, 0.0, Some(2.0))
            .unwrap();
        assert!(none.is_empty(), "floor above 1.0 must drop everything");
    }

    // =====================================================================
    // Test 8: hybrid_search_full rejects wrong dimension
    // =====================================================================

    #[test]
    fn test_hybrid_search_full_wrong_dim() {
        let store = test_store();
        let bad_emb = vec![0.1f32; 128];

        let result =
            store.hybrid_search_full(labels::EPISODIC, "test query", &bad_emb, 5, 0.5, 0.5, None);
        assert!(
            result.is_err(),
            "expected error for wrong embedding dimension"
        );
    }

    // =====================================================================
    // Test 9: validate_embedding_dim correctness
    // =====================================================================

    #[test]
    fn test_validate_embedding_dim_ok_and_err() {
        use crate::types::DEFAULT_EMBEDDING_DIM;
        // Valid
        let ok_emb = vec![0.0f32; DEFAULT_EMBEDDING_DIM];
        assert!(validate_embedding_dim(&ok_emb, DEFAULT_EMBEDDING_DIM).is_ok());

        // Wrong dimension
        let bad_emb = vec![0.0f32; 128];
        let err = validate_embedding_dim(&bad_emb, DEFAULT_EMBEDDING_DIM).unwrap_err();
        match err {
            crate::error::GrafeoError::InvalidDimension { expected, got } => {
                assert_eq!(expected, DEFAULT_EMBEDDING_DIM);
                assert_eq!(got, 128);
            }
            other => panic!("expected InvalidDimension, got: {other}"),
        }

        // Empty
        let empty: Vec<f32> = Vec::new();
        let err = validate_embedding_dim(&empty, DEFAULT_EMBEDDING_DIM).unwrap_err();
        match err {
            crate::error::GrafeoError::InvalidDimension { expected, got } => {
                assert_eq!(expected, DEFAULT_EMBEDDING_DIM);
                assert_eq!(got, 0);
            }
            other => panic!("expected InvalidDimension, got: {other}"),
        }
    }

    // =====================================================================
    // Test 10: index recovery after close and reopen
    // =====================================================================

    #[test]
    fn test_index_recovery_after_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("recovery_test.grafeo");

        let emb = test_embedding();

        // Phase 1: create store, add data, verify search works
        {
            let store = GrafeoStore::open_with_default_config(&db_path).unwrap();
            store_episode(&store, "persistent memory content", &emb);

            // Verify vector search works before close
            let results = store
                .vector_search_with_params(labels::EPISODIC, &emb, 5, 64)
                .unwrap();
            assert_eq!(results.len(), 1);

            store.close().unwrap();
        }

        // Phase 2: reopen and verify index is still usable
        {
            let store = GrafeoStore::open_with_default_config(&db_path).unwrap();

            // Rebuild vector index since HNSW is not persisted automatically
            store
                .db()
                .rebuild_vector_index(labels::EPISODIC, "embedding")
                .unwrap();
            store
                .db()
                .rebuild_text_index(labels::EPISODIC, "content")
                .unwrap();

            let results = store
                .vector_search_with_params(labels::EPISODIC, &emb, 5, 64)
                .unwrap();
            assert_eq!(results.len(), 1, "index should recover after reopen");

            // Text search should also work
            let text_results = store
                .text_search_with_filter(labels::EPISODIC, "content", "persistent memory", 5)
                .unwrap();
            assert!(
                !text_results.is_empty(),
                "text index should recover after reopen"
            );
        }
    }
}
