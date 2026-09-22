//! Vector index persistence across reopens — the acceptance check for
//! [`acowork_grafeo::index_persist`].
//!
//! `grafeo-engine` 0.5.42 writes the HNSW topology into the `.grafeo` container
//! on every checkpoint but never reads it back, so before `index_persist` a
//! reopen left the store with `GRAFEO-X001: No vector index found` and every
//! open rebuilt the index from the data (measured in release at ~0.3 ms per
//! 512-dim vector — 55k vectors ≈ 17 s).
//!
//! Assertions are on index state (`len`, membership, elapsed time) rather than
//! on approximate-search ranking: the engine's ANN recall on this synthetic,
//! heavily-tied vector set is noisy, so a `top-1 == node` assertion would flap.
//! These tests run in debug, where a rebuild of `NODES` vectors takes seconds
//! while a restore takes milliseconds, making the elapsed-time bound a real
//! discriminator.

use std::sync::Arc;
use std::time::{Duration, Instant};

use acowork_grafeo::grafeo::GrafeoStore;
use acowork_grafeo::index_config::HnswConfig;
use acowork_grafeo::types::{GrafeoConfig, labels};
use grafeo_common::types::{NodeId, Value};

const DIM: usize = 512;
/// Background vectors plus one probe node.
const NODES: usize = 600;
const INDEX_KEY: &str = "Episodic:embedding";

/// Deterministic background vector.
fn embedding(i: usize) -> Vec<f32> {
    (0..DIM).map(|k| (((i * 7 + k) % 97) as f32) / 97.0).collect()
}

/// The probe vector: far from every background vector.
fn probe_embedding() -> Vec<f32> {
    vec![1.0f32; DIM]
}

fn config(path: &std::path::Path, dim: usize) -> GrafeoConfig {
    GrafeoConfig {
        db_path: path.to_path_buf(),
        embedding_dim: dim,
    }
}

fn vector(v: &[f32]) -> Value {
    Value::Vector(Arc::from(v))
}

/// Seed `NODES` background nodes plus one probe node; returns the probe id.
fn seed(store: &GrafeoStore) -> NodeId {
    for i in 0..NODES {
        store
            .store_node(
                labels::EPISODIC,
                [
                    ("content", Value::from(format!("note {i}"))),
                    ("embedding", vector(&embedding(i))),
                ],
            )
            .unwrap();
    }
    store
        .store_node(
            labels::EPISODIC,
            [
                ("content", Value::from("probe")),
                ("embedding", vector(&probe_embedding())),
            ],
        )
        .unwrap()
}

/// The live HNSW index for [`INDEX_KEY`].
fn index_len(store: &GrafeoStore) -> usize {
    store
        .db()
        .store()
        .get_vector_index_by_key(INDEX_KEY)
        .expect("vector index must exist")
        .len()
}

fn index_contains(store: &GrafeoStore, id: NodeId) -> bool {
    store
        .db()
        .store()
        .get_vector_index_by_key(INDEX_KEY)
        .expect("vector index must exist")
        .contains(id)
}

fn temp_store(name: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(name);
    (dir, path)
}

#[test]
fn writes_are_indexed_immediately() {
    let (_dir, path) = temp_store("memory.grafeo");
    let store = GrafeoStore::open(&config(&path, DIM)).unwrap();

    let probe = seed(&store);

    // The engine's node-write path never touches `vector_indexes`, so before
    // the sync in `GrafeoStore::store_node` the index stayed empty (and a
    // search was answered from whatever existed at open time).
    assert_eq!(index_len(&store), NODES + 1);
    assert!(index_contains(&store, probe));
    store.close().unwrap();
}

#[test]
fn reopen_restores_the_index_instead_of_rebuilding_it() {
    let (_dir, path) = temp_store("memory.grafeo");

    let probe = {
        let store = GrafeoStore::open(&config(&path, DIM)).unwrap();
        let probe = seed(&store);
        store.close().unwrap();
        probe
    };

    let opened = Instant::now();
    let store = GrafeoStore::open(&config(&path, DIM)).unwrap();
    let elapsed = opened.elapsed();

    // The persisted topology came back whole: same size, same nodes.
    assert_eq!(index_len(&store), NODES + 1);
    assert!(index_contains(&store, probe));
    assert!(
        elapsed < Duration::from_secs(2),
        "open took {elapsed:?}: the index was rebuilt rather than restored"
    );
}

#[test]
fn writes_after_a_restore_are_indexed() {
    let (_dir, path) = temp_store("memory.grafeo");
    {
        let store = GrafeoStore::open(&config(&path, DIM)).unwrap();
        seed(&store);
        store.close().unwrap();
    }

    let store = GrafeoStore::open(&config(&path, DIM)).unwrap();
    let late = store
        .store_node(
            labels::EPISODIC,
            [
                ("content", Value::from("written after the restore")),
                ("embedding", vector(&probe_embedding())),
            ],
        )
        .unwrap();

    assert_eq!(index_len(&store), NODES + 2);
    assert!(index_contains(&store, late));
}

#[test]
fn dimension_change_does_not_serve_a_stale_index() {
    let (_dir, path) = temp_store("memory.grafeo");
    {
        let store = GrafeoStore::open(&config(&path, DIM)).unwrap();
        seed(&store);
        store.close().unwrap();
    }

    // Reopening at the provider's new dimension must not restore a topology
    // built from the old one: callers see "no index" (and migrate) instead of
    // silently wrong neighbours.
    let store = GrafeoStore::open(&config(&path, DIM / 2)).unwrap();
    let result = store.vector_search(labels::EPISODIC, &vec![0.0f32; DIM / 2], 5, None);
    assert!(
        result.is_err(),
        "a mismatched-dimension topology must not be served: {result:?}"
    );
}

/// A fresh in-memory store has no persisted section, so every index is built
/// from data — the fallback path must stay intact.
#[test]
fn in_memory_store_still_builds_from_data() {
    let store = GrafeoStore::new_in_memory_with_config(HnswConfig {
        dim: DIM,
        ..Default::default()
    })
    .unwrap();
    let probe = seed(&store);
    assert_eq!(index_len(&store), NODES + 1);
    assert!(index_contains(&store, probe));
}
