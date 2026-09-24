//! Retrieval fusion layer (ADR-082 D5): per-source gating + rank fusion.
//!
//! The engine-era fusion API mixed three score domains into a single `score`
//! field (ADR-082 §1.4), which silently dropped every Chinese `memory_recall`
//! hit: a BM25 miss pushed the query onto the vector-only path, whose score
//! (`cos - 1`, non-positive) could never clear a `>= 0.0` gate. Fusion and
//! gating are therefore implemented here, so the score domains are structurally
//! separate:
//!
//! * **vector source** gates on `cos >= min_cosine` — the absolute cosine
//!   domain. Callers may pass `None` to disable the gate; `memory_recall`
//!   currently passes `None` and relies on the result count (`k`) as the
//!   quality knob instead, because anisotropic embeddings make an absolute
//!   cosine floor unreliable as a relevance signal.
//! * **text source** has no fixed threshold: BM25 carries IDF and drifts with
//!   corpus statistics, so a lexical hit is independent evidence and survives
//!   even when its embedding sits far away.
//! * **fusion** is equal-weight RRF (k = 60) over the union of the two ranked
//!   lists.
//!
//! The returned `score` is the RRF fused score (`Σ w / (60 + rank + 1)`).
//! RRF encodes rank position only and is not comparable across queries — do
//! not threshold on its absolute value. Use the result count (`k`) for
//! recall-quality control; the previous `(1 + cos) / 2` re-mapping was
//! removed because it conflated fusion rank with similarity and made
//! `score == 0.5` mean both "orthogonal vector hit" and "BM25-only hit",
//! breaking downstream thresholds for both interpretations.

use std::collections::HashMap;

use crate::{Result, SqliteStore};

/// RRF rank constant (ADR-082 D5).
pub const RRF_K: f64 = 60.0;

impl SqliteStore {
    /// Hybrid search over `label`: vector + text, gated per source, fused by RRF.
    ///
    /// * `text_weight` / `vector_weight` scale each source's RRF contribution.
    ///   `(0.0, 0.0)` means "unspecified" and falls back to equal weight.
    /// * `min_cosine` is an absolute cosine floor applied to the **vector source
    ///   only**; `None` disables it. Callers that care about recall quality
    ///   should pass `None` and rely on `k` as the quality knob, since
    ///   anisotropic embeddings make an absolute cosine floor unreliable.
    ///
    /// Results come back ordered by fused rank. The returned score is the
    /// RRF fused score (`Σ w / (60 + rank + 1)`) — *not* the cosine. RRF is
    /// a rank-position signal, so absolute values are not comparable across
    /// queries and must not be thresholded on.
    #[allow(clippy::too_many_arguments)]
    pub fn hybrid_search_full(
        &self,
        label: &str,
        query: &str,
        embedding: &[f32],
        k: usize,
        text_weight: f64,
        vector_weight: f64,
        min_cosine: Option<f32>,
    ) -> Result<Vec<(u64, f64)>> {
        if k == 0 {
            return Ok(Vec::new());
        }
        // Both rankings are fetched deeper than the final `k`: RRF is a function
        // of rank, so truncating a source before fusion would bake that source's
        // own ordering into the fused result.
        let pool = k.saturating_mul(2);

        let floor = f64::from(min_cosine.unwrap_or(-1.0_f32));
        let mut vector_ranked: Vec<u64> = Vec::new();
        if !embedding.is_empty() {
            for (id, cos) in self.vector_search(label, embedding, pool)? {
                if cos >= floor {
                    vector_ranked.push(id);
                }
            }
        }

        let text_ranked: Vec<u64> = if query.trim().is_empty() {
            Vec::new()
        } else {
            self.text_search(label, query, pool)?
                .into_iter()
                .map(|(id, _)| id)
                .collect()
        };

        // ponytail: `(0.0, 0.0)` means "unspecified", not "mute both sources" —
        // the trait signature cannot express the latter, and equal weight is
        // what D5 asks for anyway.
        let (text_weight, vector_weight) = match (text_weight, vector_weight) {
            (0.0, 0.0) => (1.0, 1.0),
            weights => weights,
        };

        let mut fused: HashMap<u64, f64> = HashMap::new();
        for (rank, id) in text_ranked.iter().enumerate() {
            *fused.entry(*id).or_default() += text_weight / (RRF_K + rank as f64 + 1.0);
        }
        for (rank, id) in vector_ranked.iter().enumerate() {
            *fused.entry(*id).or_default() += vector_weight / (RRF_K + rank as f64 + 1.0);
        }

        let mut ranked: Vec<(u64, f64)> = fused.into_iter().collect();
        // Node id as tie-break keeps the order stable across runs (HashMap
        // iteration order is not deterministic).
        ranked.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        ranked.truncate(k);

        Ok(ranked
            .into_iter()
            // Return the fused score itself. The previous `(1 + cos) / 2`
            // re-mapping conflated rank fusion with cosine similarity and made
            // `score == 0.5` carry two incompatible meanings; downstream
            // `min_score` / `min_cosine` / NRR thresholds were silently
            // working against a non-existent semantic for text-only hits.
            .collect())
    }

    /// Hybrid search with equal weights and no cosine floor.
    ///
    /// `text_prop` / `vec_prop` exist for `GrafeoStore` API parity and are
    /// ignored: the SQLite schema indexes one text column (`content`) and one
    /// vector column per label (ADR-082 D2/D3).
    pub fn hybrid_search_filtered(
        &self,
        label: &str,
        _text_prop: &str,
        _vec_prop: &str,
        query: &str,
        embedding: &[f32],
        k: usize,
    ) -> Result<Vec<(u64, f64)>> {
        self.hybrid_search_full(label, query, embedding, k, 1.0, 1.0, None)
    }

    /// Text search with `GrafeoStore` API parity; the field selector is ignored
    /// because only `content` is indexed.
    pub fn text_search_with_filter(
        &self,
        label: &str,
        _field: &str,
        query: &str,
        k: usize,
    ) -> Result<Vec<(u64, f64)>> {
        self.text_search(label, query, k)
    }
}
