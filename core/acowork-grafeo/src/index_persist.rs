//! HNSW topology persistence — the read side `grafeo-engine` is missing.
//!
//! `grafeo-engine` 0.5.42 writes a `VectorStore` section into the `.grafeo`
//! container on every checkpoint (`build_sections`) but never consumes it:
//! `load_from_sections` only deserializes that section when the store already
//! has an index registered under the same `"label:property"` key, and
//! `CatalogSection::deserialize` deliberately does not recreate the index
//! definitions (it defers to a post-load step that does not exist in 0.5.42).
//! The section is therefore written to disk and discarded, and every `open`
//! pays a full O(N log N) HNSW rebuild — measured in release at ~0.3 ms per
//! 512-dim vector: 3.7k vectors ≈ 1 s, 55k ≈ 17 s.
//!
//! Every piece needed to consume the section ourselves is public API:
//!
//! - [`GrafeoDB::file_manager`] → `read_section_directory` → `find(VectorStore)`
//! - [`GrafeoDB::store`] → `LpgStore::add_vector_index` — registers an *empty*
//!   index without scanning the graph
//! - `VectorStoreSection::deserialize` — restores the persisted topology into
//!   the registered indexes (matched by key)
//!
//! Restore is best-effort by design. Any mismatch — dimension change, missing
//! section, empty or stale topology — drops the half-restored index so the
//! caller falls back to building from the data, i.e. the previous behaviour.
//!
//! The same engine gap also means nothing on the node-write path ever inserts
//! into a vector index (`vector_indexes` is only written by `add_vector_index`),
//! so a freshly built index only ever covered the data that existed at open
//! time. [`insert_vector`] closes that hole and [`sync_vector_index`] repairs
//! the delta accumulated while a store was closed.

use std::sync::Arc;

use grafeo_common::storage::{Section, SectionType};
use grafeo_common::types::{NodeId, PropertyKey, Value};
use grafeo_core::index::vector::section::VectorStoreSection;
use grafeo_core::index::vector::{
    DistanceMetric, HnswConfig as EngineHnswConfig, HnswIndex, PropertyVectorAccessor,
    VectorIndexKind,
};
use grafeo_engine::GrafeoDB;

use crate::index_config::{HnswConfig, VECTOR_METRIC};

/// Property holding the embedding. Every vector index in this workspace is
/// built on it, and the persisted key is `"{label}:embedding"`.
pub const EMBEDDING_PROPERTY: &str = "embedding";

/// Mirror our [`HnswConfig`] onto the engine's HNSW parameters.
///
/// Only the build-time parameters matter for a restored topology; `ef_search`
/// is applied per query by our own search calls.
fn engine_config(cfg: &HnswConfig) -> EngineHnswConfig {
    let metric = DistanceMetric::from_str(VECTOR_METRIC).unwrap_or(DistanceMetric::Cosine);
    EngineHnswConfig::new(cfg.dim, metric)
        .with_m(cfg.m)
        .with_ef_construction(cfg.ef_construction)
}

/// Raw bytes of the persisted `VectorStore` section, if the container has one.
fn vector_section_bytes(db: &GrafeoDB) -> Option<Vec<u8>> {
    let fm = db.file_manager()?;
    let dir = fm.read_section_directory().ok()??;
    let entry = dir.find(SectionType::VectorStore)?;
    fm.read_section_data(entry).ok()
}

/// Ids of `label` nodes carrying a vector, plus the dimension of the first one.
///
/// One pass over the label. Used both as the dimension guard (the persisted
/// topology was built from vectors of the data's dimension, so a data/config
/// mismatch must disable restore) and as the live-id set for the delta sync.
fn scan_vectors(db: &GrafeoDB, label: &str, property: &str) -> (Vec<NodeId>, Option<usize>) {
    let key = PropertyKey::new(property);
    let graph = db.graph_store();
    let mut ids = Vec::new();
    let mut dim = None;
    for id in graph.nodes_by_label(label) {
        if let Some(Value::Vector(v)) = graph.get_node_property(id, &key) {
            if dim.is_none() {
                dim = Some(v.len());
            }
            ids.push(id);
        }
    }
    (ids, dim)
}

/// Drop `label`'s vector index so the caller rebuilds it from data.
fn drop_index(db: &GrafeoDB, label: &str, property: &str) {
    db.drop_vector_index(label, property);
}

/// Restore `label`'s vector index from the persisted container.
///
/// Returns `true` when a usable topology was restored (the index is registered
/// and caught up with the live data), `false` when the caller must build it.
pub fn restore_vector_index(db: &GrafeoDB, label: &str, property: &str, cfg: &HnswConfig) -> bool {
    let (live_ids, live_dim) = scan_vectors(db, label, property);
    // No vectors yet, or the data dimension no longer matches this config
    // (provider migration): let the caller build/fail the normal way.
    if live_dim != Some(cfg.dim) {
        return false;
    }
    let Some(bytes) = vector_section_bytes(db) else {
        return false;
    };

    // Register an empty index first — `deserialize` matches by key and cannot
    // create the index itself.
    let index = Arc::new(VectorIndexKind::Hnsw(HnswIndex::new(engine_config(cfg))));
    db.store()
        .add_vector_index(label, property, Arc::clone(&index));

    let entries = db.store().vector_index_entries();
    let mut section = VectorStoreSection::new(entries);
    if let Err(e) = section.deserialize(&bytes) {
        tracing::warn!(label, error = %e, "vector index restore failed; rebuilding from data");
        drop_index(db, label, property);
        return false;
    }

    // Guard against a topology that does not describe this data: empty means
    // the key was not in the section, and more entries than live vectors means
    // the topology references nodes the graph no longer has.
    let restored = index.len();
    if restored == 0 || restored > live_ids.len() {
        tracing::warn!(
            label,
            restored,
            live = live_ids.len(),
            "persisted vector topology does not match live data; rebuilding"
        );
        drop_index(db, label, property);
        return false;
    }

    let added = sync_vector_index(db, label, property);
    tracing::info!(
        label,
        restored,
        added,
        live = live_ids.len(),
        "vector index restored from disk (no rebuild)"
    );
    true
}

/// Insert every live vector missing from `label`'s index, and prune index
/// entries whose node is gone. O(N) hash lookups instead of an O(N log N)
/// build — this is what makes a restored topology usable: the checkpoint is up
/// to `DEFAULT_CHECKPOINT_INTERVAL` old, and WAL replay may have added nodes
/// since.
///
/// Returns the number of vectors inserted.
pub fn sync_vector_index(db: &GrafeoDB, label: &str, property: &str) -> usize {
    let key = format!("{label}:{property}");
    let Some(index) = db.store().get_vector_index_by_key(&key) else {
        return 0;
    };
    let graph = db.graph_store();
    let accessor = PropertyVectorAccessor::new(&*graph, property);
    let pkey = PropertyKey::new(property);
    let nodes = graph.nodes_by_label(label);

    let mut added = 0usize;
    for &id in &nodes {
        if let Some(Value::Vector(v)) = graph.get_node_property(id, &pkey)
            && !index.contains(id)
        {
            index.insert(id, &v, &accessor);
            added += 1;
        }
    }

    // Stale entries (deleted messages, forgotten memories) only show up when
    // the topology is *longer* than the data; pruning needs the full id set,
    // so only pay for it when there is something to prune.
    if index.len() > nodes.len() {
        let (_, _, topo) = index.snapshot_topology();
        let live: std::collections::HashSet<NodeId> = nodes.iter().copied().collect();
        let mut pruned = 0usize;
        for (id, _) in topo {
            if !live.contains(&id) && index.remove(id) {
                pruned += 1;
            }
        }
        if pruned > 0 {
            tracing::info!(label, pruned, "pruned stale vector index entries");
        }
    }
    added
}

/// Insert one vector into `label`'s index, if one is registered.
///
/// Best-effort: a stale or absent index must never fail a write. Returns
/// whether the vector was indexed.
pub fn insert_vector(
    db: &GrafeoDB,
    label: &str,
    property: &str,
    id: NodeId,
    vector: &[f32],
) -> bool {
    let key = format!("{label}:{property}");
    let Some(index) = db.store().get_vector_index_by_key(&key) else {
        return false;
    };
    let graph = db.graph_store();
    let accessor = PropertyVectorAccessor::new(&*graph, property);
    index.insert(id, vector, &accessor);
    true
}

/// Sync a property that was just written into the matching vector index.
///
/// A no-op unless `value` is a vector. Used by [`crate::graph`]'s property
/// writer so every embedding write path (episodic writes, lazy embedding via
/// `set_node_property`, embedding rebuilds, dimension migration) keeps the index
/// current without each caller having to remember.
pub fn sync_written_property(db: &GrafeoDB, id: NodeId, property: &str, value: &Value) {
    let Value::Vector(vector) = value else {
        return;
    };
    let Some(node) = db.get_node(id) else {
        return;
    };
    for label in &node.labels {
        insert_vector(db, label, property, id, vector);
    }
}

/// Drop `id` from every registered vector index (the engine's node-delete path
/// does not touch indexes either).
pub fn remove_vector(db: &GrafeoDB, id: NodeId) {
    for (_, index) in db.store().vector_index_entries() {
        index.remove(id);
    }
}

/// Whether a vector index is registered for `label`.
pub fn has_vector_index(db: &GrafeoDB, label: &str, property: &str) -> bool {
    let key = format!("{label}:{property}");
    db.store().get_vector_index_by_key(&key).is_some()
}
