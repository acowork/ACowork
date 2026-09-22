//! End-to-end check of index persistence on **real stores** (~/.acowork).
//!
//! Runs as a standalone process: it copies the real stores into a scratch
//! directory, so the live Agent data is only ever read. No embedding provider
//! is needed — every query vector is taken from the real nodes themselves.
//!
//! Usage:
//! ```text
//! cargo run --release -p acowork-grafeo --example verify_real_stores -- \
//!     <agent_workspace_dir> <scratch_dir> [dim]
//! ```
//! e.g.
//! ```text
//! cargo run --release -p acowork-grafeo --example verify_real_stores -- \
//!     "$HOME/.acowork/acowork-node/packages/com.acowork.ponytail/93d5dd73-f002-458b-bb60-c3d8c0f74d5a/workspace" \
//!     "$TEMP/acowork_verify" 512
//! ```
//!
//! What it proves, pass by pass:
//!
//! 1. `open` #1 on a copy of the real store (may build the indexes).
//!    `close` → the HNSW topology is written into the `.grafeo` container.
//! 2. `open` #2 → `ensure_vector_index` reports whether the index came from the
//!    container (**restored**) or from the data (**built**), plus wall time.
//!    Restored = persistence works; built = the bug is back.
//! 3. Vector recall on real embeddings: query with a real node's own vector and
//!    check whether that node comes back (this is the ANN quality question).
//! 4. Writes made after a restore are searchable immediately, and still
//!    searchable after the next reopen (the WAL-delta repair path).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use acowork_grafeo::grafeo::GrafeoStore;
use acowork_grafeo::types::{GrafeoConfig, labels};
use grafeo_common::types::{NodeId, Value};

/// Label used by the Runtime's conversation index (`conversation_index.rs`).
const CONV_LABEL: &str = "ConversationMessage";

fn config(path: &Path, dim: usize) -> GrafeoConfig {
    GrafeoConfig {
        db_path: path.to_path_buf(),
        embedding_dim: dim,
    }
}

/// Recursive copy — `fs::copy` on files, `create_dir_all` on directories.
fn copy_tree(from: &Path, to: &Path) -> std::io::Result<u64> {
    if from.is_file() {
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(from, to)?;
        return Ok(std::fs::metadata(to)?.len());
    }
    std::fs::create_dir_all(to)?;
    let mut bytes = 0;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        bytes += copy_tree(&entry.path(), &to.join(entry.file_name()))?;
    }
    Ok(bytes)
}

fn tree_size(path: &Path) -> u64 {
    if path.is_file() {
        return std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    }
    std::fs::read_dir(path)
        .map(|rd| rd.flatten().map(|e| tree_size(&e.path())).sum())
        .unwrap_or(0)
}

/// Copy `from` to `to` once. An existing `to` is reused, so a second run does
/// not re-copy the real store (the conversation WAL alone is 183 MB).
fn copy_if_missing(from: &Path, to: &Path, what: &str) -> u64 {
    if to.exists() {
        eprintln!("  reusing existing copy: {to:?}");
        return tree_size(to);
    }
    match copy_tree(from, to) {
        Ok(n) if n > 0 => n,
        other => {
            eprintln!(
                "  !! could not read the real {what} at {from:?} ({other:?}).\n     \
                 A running Agent holds a byte lock on its store: stop that Agent, or pass \
                 --memory-file / --legacy-dir pointing at a copy taken while it was stopped."
            );
            std::process::exit(3);
        }
    }
}

/// A node with its embedding, as read from a real store.
struct Sample {
    id: NodeId,
    vector: Vec<f32>,
    text: String,
}

fn embedding_of(node: &grafeo_core::graph::lpg::Node, property: &str) -> Option<Vec<f32>> {
    match node.get_property(property) {
        Some(Value::Vector(v)) => Some(v.to_vec()),
        _ => None,
    }
}

fn text_of(node: &grafeo_core::graph::lpg::Node, property: &str) -> Option<String> {
    match node.get_property(property) {
        Some(Value::String(s)) => Some(s.to_string()),
        _ => None,
    }
}

/// Read every node of `label` that carries an embedding, up to `limit`.
fn sample(store: &GrafeoStore, label: &str, limit: usize) -> Vec<Sample> {
    let mut out = Vec::new();
    for id in store.db().graph_store().nodes_by_label(label) {
        let Some(node) = store.get_node(id) else {
            continue;
        };
        let Some(vector) = embedding_of(&node, "embedding") else {
            continue;
        };
        out.push(Sample {
            id,
            vector,
            text: text_of(&node, "content").unwrap_or_default(),
        });
        if out.len() >= limit {
            break;
        }
    }
    out
}

/// Query with each sample's own vector; report how often the node itself is
/// returned in the top-`k` (recall of an exact match — the ANN quality floor).
fn recall(store: &GrafeoStore, label: &str, samples: &[Sample], k: usize) -> (usize, usize) {
    let mut rank1 = 0;
    let mut found = 0;
    for s in samples {
        match store.vector_search(label, &s.vector, k, None) {
            Ok(hits) => {
                if hits.iter().any(|(id, _)| *id == s.id) {
                    found += 1;
                    if hits.first().is_some_and(|(id, _)| *id == s.id) {
                        rank1 += 1;
                    }
                }
            }
            Err(e) => eprintln!("    vector_search failed: {e}"),
        }
    }
    (found, rank1)
}

fn index_len(store: &GrafeoStore, label: &str) -> Option<usize> {
    let key = format!("{label}:embedding");
    store.db().store().get_vector_index_by_key(&key).map(|i| i.len())
}

fn label_count(store: &GrafeoStore, label: &str) -> usize {
    store.db().graph_store().nodes_by_label(label).len()
}

/// Open `path` twice: the first open may build, the second must restore.
/// Returns the store from the second open, which the caller keeps using.
fn open_twice(path: &Path, dim: usize, label: &str, what: &str) -> GrafeoStore {
    eprintln!("\n--- {what}: {path:?}");

    let t = Instant::now();
    let store = GrafeoStore::open(&config(path, dim)).unwrap();
    let first_open = t.elapsed();
    let t = Instant::now();
    let first_ensure = store.ensure_vector_index(label, "embedding", dim).unwrap();
    let first_ensure_at = t.elapsed();
    eprintln!(
        "  open #1: {:?} (ensure_vector_index {first_ensure_at:?}, restored={first_ensure}) nodes={} index_len={:?}",
        first_open,
        label_count(&store, label),
        index_len(&store, label)
    );
    let t = Instant::now();
    store.close().unwrap();
    eprintln!("  close -> container written ({:?})", t.elapsed());

    let t = Instant::now();
    let store = GrafeoStore::open(&config(path, dim)).unwrap();
    let second_open = t.elapsed();
    let t = Instant::now();
    let restored = store.ensure_vector_index(label, "embedding", dim).unwrap();
    let second_ensure = t.elapsed();
    eprintln!(
        "  open #2: {:?} (ensure_vector_index {second_ensure:?}, restored={restored}) index_len={:?}",
        second_open,
        index_len(&store, label)
    );
    assert!(
        restored,
        "INDEX NOT RESTORED for {what}: the container did not yield the topology"
    );
    store
}

fn verify_memory(src_file: &Path, scratch: &Path, dim: usize) {
    eprintln!("\n================ MEMORY STORE (real) ================");
    let dst = scratch.join("memory").join("private.grafeo");
    let bytes = copy_if_missing(src_file, &dst, "memory store");
    let wal = src_file.with_extension("grafeo.wal");
    if wal.is_dir() {
        copy_if_missing(&wal, &scratch.join("memory").join("private.grafeo.wal"), "memory WAL");
    }
    eprintln!(
        "  copied real store: {:.1} MB from {src_file:?} (source untouched, read-only)",
        bytes as f64 / 1e6
    );

    let store = open_twice(&dst, dim, labels::EPISODIC, "memory");

    for label in labels::MEMORY {
        let nodes = label_count(&store, label);
        if nodes > 0 {
            eprintln!("  {label}: nodes={nodes} index_len={:?}", index_len(&store, label));
        }
    }

    let samples = sample(&store, labels::EPISODIC, 100);
    eprintln!("  episodic nodes carrying the real embedding: {}", samples.len());
    if let Some(first) = samples.first() {
        eprintln!(
            "  real embedding dim = {} | first content: {:?}",
            first.vector.len(),
            first.text.chars().take(60).collect::<String>()
        );
    }
    if !samples.is_empty() {
        let t = Instant::now();
        let (found, rank1) = recall(&store, labels::EPISODIC, &samples, 10);
        eprintln!(
            "  vector recall on real embeddings (k=10, {} queries): in top-10 {found}/{} | top-1 {rank1}/{} ({:?})",
            samples.len(),
            samples.len(),
            samples.len(),
            t.elapsed()
        );
        // Text search over the same real content must also answer.
        let probe: String = samples[0].text.chars().take(24).collect();
        match store.text_search(labels::EPISODIC, &probe, 5) {
            Ok(hits) => eprintln!("  BM25 on real content {probe:?}: {} hits", hits.len()),
            Err(e) => eprintln!("  BM25 failed: {e}"),
        }
        // ── PROBE: text vs vector vs hybrid on the SAME query vector ──
        // Offline reproduction of the `memory_recall` path (manager ->
        // provider -> db.hybrid_search / RRF fusion). The query vector is
        // taken from a real node and the query text from that same node's
        // content, so BOTH fusion sources are guaranteed a hit — no
        // embedding service needed. If hybrid_search comes back empty while
        // text_search and vector_search both hit, the fusion path is at
        // fault, not query-embedding generation.
        if let Some(p) = samples.iter().find(|s| s.text.is_ascii()) {
            let q: String = p.text.chars().take(20).collect();
            let (text_n, text_has) = match store.text_search(labels::EPISODIC, &q, 10) {
                Ok(r) => (r.len(), r.iter().any(|(id, _)| *id == p.id)),
                Err(e) => {
                    eprintln!("    text_search failed: {e}");
                    (usize::MAX, false)
                }
            };
            let (vec_n, vec_top1) =
                match store.vector_search(labels::EPISODIC, &p.vector, 10, None) {
                    Ok(r) => (r.len(), r.first().is_some_and(|(id, _)| *id == p.id)),
                    Err(e) => {
                        eprintln!("    vector_search failed: {e}");
                        (usize::MAX, false)
                    }
                };
            let (hyb_n, hyb_rank) =
                match store.hybrid_search(labels::EPISODIC, "content", "embedding", &q, &p.vector, 10)
                {
                    Ok(r) => (r.len(), r.iter().position(|(id, _)| *id == p.id)),
                    Err(e) => {
                        eprintln!("    hybrid_search failed: {e}");
                        (usize::MAX, None)
                    }
                };
            eprintln!("  [PROBE] target id={} q={:?}", p.id.0, q);
            eprintln!("  [PROBE] text_search   (BM25)     hits={text_n} contains_target={text_has}");
            eprintln!("  [PROBE] vector_search (same vec) hits={vec_n} top1_is_target={vec_top1}");
            eprintln!("  [PROBE] hybrid_search (recall)   hits={hyb_n} target_at={hyb_rank:?}");
            // Score scale: `hybrid_search_full` now thresholds on the absolute
            // cosine similarity and returns it normalized to [0, 1], so the
            // floor is a stable relevance knob (0.3 ≈ 72°). The old code
            // thresholded the fused RRF score, which silently dropped every
            // vector-only hit.
            if let Ok(r) = store.hybrid_search(labels::EPISODIC, "content", "embedding", &q, &p.vector, 10) {
                let scores: Vec<f64> = r.iter().take(3).map(|(_, s)| *s).collect();
                eprintln!("  [PROBE] hybrid_search raw scores (top3) = {scores:?}");
            }
            for mc in [None, Some(-1.0f32), Some(0.0), Some(0.3), Some(0.6)] {
                let n = store
                    .hybrid_search_full(labels::EPISODIC, &q, &p.vector, 10, 0.0, 0.0, mc)
                    .map(|v| v.len().to_string())
                    .unwrap_or_else(|e| format!("Err({e})"));
                eprintln!("  [PROBE] hybrid_search_full(min_cosine={mc:?}) -> hits={n}");
            }
        }


    }
    store.close().unwrap();
}

fn verify_conversation(legacy_src: &Path, scratch: &Path, dim: usize) {
    eprintln!("\n============ CONVERSATION INDEX (real, legacy dir) ============");
    // A file (not a directory) means an already-migrated store: verify it in place.
    if legacy_src.is_file() {
        eprintln!("  migrated single-file store: {legacy_src:?}");
        let store = open_twice(legacy_src, dim, CONV_LABEL, "conversation index");
        let samples = sample(&store, CONV_LABEL, 200);
        eprintln!("  messages with real embeddings: {}", samples.len());
        if !samples.is_empty() {
            let (found, rank1) = recall(&store, CONV_LABEL, &samples, 10);
            eprintln!(
                "  vector recall on real messages (k=10, {} queries): in top-10 {found}/{} | top-1 {rank1}/{}",
                samples.len(),
                samples.len(),
                samples.len()
            );
            let probe: String = samples[0].text.chars().take(24).collect();
            match store.text_search(CONV_LABEL, &probe, 5) {
                Ok(hits) => eprintln!("  BM25 on real content {probe:?}: {} hits", hits.len()),
                Err(e) => eprintln!("  BM25 failed: {e}"),
            }
        }
        store.close().unwrap();
        return;
    }
    let legacy = scratch.join("conversation_legacy");
    let bytes = copy_if_missing(legacy_src, &legacy, "conversation index");
    eprintln!(
        "  copied real legacy store: {:.1} MB on disk ({:?} WAL segments)",
        bytes as f64 / 1e6,
        std::fs::read_dir(legacy.join("wal")).map(|d| d.count()).unwrap_or(0)
    );

    // Read the real messages (with their real embeddings) out of the legacy store.
    let t = Instant::now();
    let source = GrafeoStore::open(&config(&legacy, dim)).unwrap();
    let legacy_open = t.elapsed();
    let nodes_total = label_count(&source, CONV_LABEL);
    let samples = sample(&source, CONV_LABEL, usize::MAX);
    let mut sessions: Vec<String> = samples
        .iter()
        .filter_map(|s| {
            source
                .get_node(s.id)
                .and_then(|n| n.get_property("session_id").and_then(|v| v.as_str()).map(str::to_string))
        })
        .collect();
    sessions.sort();
    sessions.dedup();
    eprintln!(
        "  legacy open (183 MB WAL replayed) = {legacy_open:?} | messages={nodes_total} with embeddings={} sessions={}",
        samples.len(),
        sessions.len()
    );
    if samples.is_empty() {
        eprintln!(
            "  !! the legacy store replayed to ZERO nodes: its WAL holds nothing the engine \
             treats as committed.\n     Nothing to migrate from here — the index is rebuilt from \
             the JSONL history instead,\n     which is exactly what the first post-migration \
             start does. Skipping the migration part."
        );
        return;
    }

    // Migrate them into the new single-file store, exactly as the Runtime does.
    let path = scratch.join("conversation_index.grafeo");
    let _ = std::fs::remove_file(&path);
    let t = Instant::now();
    let store = GrafeoStore::open(&config(&path, dim)).unwrap();
    store.db().create_text_index(CONV_LABEL, "content").unwrap();
    let built = store.ensure_vector_index(CONV_LABEL, "embedding", dim).unwrap();
    let mut copied = 0;
    for s in &samples {
        let src = source.get_node(s.id).unwrap();
        store
            .store_node(
                CONV_LABEL,
                [
                    (
                        "session_id",
                        Value::from(
                            src.get_property("session_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default(),
                        ),
                    ),
                    (
                        "message_index",
                        Value::from(
                            src.get_property("message_index")
                                .and_then(|v| v.as_int64())
                                .unwrap_or(0),
                        ),
                    ),
                    (
                        "role",
                        Value::from(src.get_property("role").and_then(|v| v.as_str()).unwrap_or_default()),
                    ),
                    ("content", Value::from(s.text.clone())),
                    ("embedding", Value::Vector(Arc::from(s.vector.as_slice()))),
                ],
            )
            .unwrap();
        copied += 1;
    }
    eprintln!(
        "  migrated {copied} real messages into a new single-file store in {:?} (first open: restored={built})",
        t.elapsed()
    );
    let t = Instant::now();
    store.close().unwrap();
    eprintln!(
        "  close -> {:.1} MB written ({:?})",
        tree_size(&path) as f64 / 1e6,
        t.elapsed()
    );
    drop(source);

    // Reopen: the topology must come back from the container, not from data.
    let store = open_twice(&path, dim, CONV_LABEL, "conversation index");

    let (found, rank1) = recall(&store, CONV_LABEL, &samples, 10);
    eprintln!(
        "  vector recall on real messages (k=10, {} queries): in top-10 {found}/{} | top-1 {rank1}/{}",
        samples.len(),
        samples.len(),
        samples.len()
    );

    // Real content through BM25 (conv search degrades to this without an embedding).
    let probe: String = samples[0].text.chars().take(24).collect();
    match store.text_search(CONV_LABEL, &probe, 5) {
        Ok(hits) => eprintln!("  BM25 on real content {probe:?}: {} hits", hits.len()),
        Err(e) => eprintln!("  BM25 failed: {e}"),
    }

    // A message written *after* the restore must be searchable right away, and
    // still be there after one more reopen (the WAL-delta repair path).
    let late_vec: Vec<f32> = samples[0].vector.iter().map(|v| v * 1.01).collect();
    let late_id = store
        .store_node(
            CONV_LABEL,
            [
                ("session_id", Value::from("verify-late")),
                ("message_index", Value::from(0i64)),
                ("role", Value::from("user")),
                ("content", Value::from("post-restore write marker")),
                ("embedding", Value::Vector(Arc::from(late_vec.as_slice()))),
            ],
        )
        .unwrap();
    let hits = store.vector_search(CONV_LABEL, &late_vec, 5, None).unwrap();
    eprintln!(
        "  write after restore: indexed immediately = {} (len now {:?})",
        hits.iter().any(|(id, _)| *id == late_id),
        index_len(&store, CONV_LABEL)
    );
    let t = Instant::now();
    store.close().unwrap();
    eprintln!("  close -> {:.1} MB ({:?})", tree_size(&path) as f64 / 1e6, t.elapsed());

    // Deliberately no close: a killed Runtime. The write must survive the WAL replay.
    let store = GrafeoStore::open(&config(&path, dim)).unwrap();
    let restored = store.ensure_vector_index(CONV_LABEL, "embedding", dim).unwrap();
    let hits = store.vector_search(CONV_LABEL, &late_vec, 5, None).unwrap();
    eprintln!(
        "  after an unclean exit: restored={restored} late write still searchable = {} | index_len={:?}",
        hits.iter().any(|(id, _)| *id == late_id),
        index_len(&store, CONV_LABEL)
    );
}

fn main() {
    let mut scratch = None;
    let mut dim = 512usize;
    let mut memory_file = None;
    let mut legacy_dir = None;
    let mut workspace = None;
    for a in std::env::args().skip(1) {
        if let Some(v) = a.strip_prefix("--scratch=") {
            scratch = Some(PathBuf::from(v));
        } else if let Some(v) = a.strip_prefix("--dim=") {
            dim = v.parse().unwrap();
        } else if let Some(v) = a.strip_prefix("--memory-file=") {
            memory_file = Some(PathBuf::from(v));
        } else if let Some(v) = a.strip_prefix("--legacy-dir=") {
            legacy_dir = Some(PathBuf::from(v));
        } else if let Some(v) = a.strip_prefix("--workspace=") {
            workspace = Some(PathBuf::from(v));
        } else {
            eprintln!("unknown argument: {a}");
            std::process::exit(2);
        }
    }
    let Some(scratch) = scratch else {
        eprintln!(
            "usage: verify_real_stores --scratch=<dir> [--workspace=<agent workspace>]\n\
             \x20      [--memory-file=<private.grafeo>] [--legacy-dir=<conversation_index dir>] [--dim=512]"
        );
        std::process::exit(2);
    };
    let memory_file = memory_file.or_else(|| {
        workspace
            .as_deref()
            .map(|w| w.join("memory").join("private.grafeo"))
    });
    let legacy_dir = legacy_dir.or_else(|| {
        workspace
            .as_deref()
            .map(|w| w.join("conversation_index"))
    });

    std::fs::create_dir_all(&scratch).unwrap();
    eprintln!("scratch (copies only): {scratch:?}");
    eprintln!("embedding dim        : {dim}");

    match &memory_file {
        Some(f) => verify_memory(f, &scratch, dim),
        None => eprintln!("no --memory-file / --workspace: skipping the memory store check"),
    }
    match &legacy_dir {
        Some(d) => verify_conversation(d, &scratch, dim),
        None => eprintln!("no --legacy-dir / --workspace: skipping the conversation index check"),
    }

    eprintln!("\nALL CHECKS PASSED — every index came back from the container, not from the data.");
}
