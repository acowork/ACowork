//! One-shot diagnosis of a **copy** of the live memory store.
//!
//! Answers, on real data and without any embedding service, the questions the
//! runtime logs cannot:
//!
//! 1. **Coverage** — node count vs nodes carrying an `embedding` vs the vector
//!    index length. If the index counts nodes that carry no vector, they enter
//!    every vector search as zero vectors and poison the ranking.
//! 2. **The probe node** written through `memory_store` — is it in the vector
//!    index at all (query with its own vector; it must come back first)?
//! 3. **Per-source behaviour** for the exact queries that returned 0 online:
//!    `text_search` (BM25), `vector_search`, `hybrid_search`.
//! 4. **Write-after-restore** — a node written now must be searchable
//!    immediately, and a node written *without* a vector must not silently
//!    join the HNSW index.
//!
//! Usage:
//! ```text
//! cargo run --release -p acowork-grafeo --example diagnose_memory -- \
//!     --file=<copy of private.grafeo> [--dim=512] [--probe=PONYTAIL_EMBED_PROBE_7F3A]
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;

use acowork_grafeo::grafeo::GrafeoStore;
use acowork_grafeo::types::{GrafeoConfig, labels};
use grafeo_common::types::{NodeId, Value};

fn config(path: &Path, dim: usize) -> GrafeoConfig {
    GrafeoConfig {
        db_path: path.to_path_buf(),
        embedding_dim: dim,
    }
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

fn index_len(store: &GrafeoStore, label: &str) -> Option<usize> {
    let key = format!("{label}:embedding");
    store.db().store().get_vector_index_by_key(&key).map(|i| i.len())
}

fn main() {
    let mut file = None;
    let mut dim = 512usize;
    let mut probe = "PONYTAIL_EMBED_PROBE_7F3A".to_string();
    for a in std::env::args().skip(1) {
        if let Some(v) = a.strip_prefix("--file=") {
            file = Some(PathBuf::from(v));
        } else if let Some(v) = a.strip_prefix("--dim=") {
            dim = v.parse().unwrap();
        } else if let Some(v) = a.strip_prefix("--probe=") {
            probe = v.to_string();
        } else {
            eprintln!("unknown argument: {a}");
            std::process::exit(2);
        }
    }
    let Some(file) = file else {
        eprintln!("usage: diagnose_memory --file=<private.grafeo> [--dim=512] [--probe=TOKEN]");
        std::process::exit(2);
    };

    let store = GrafeoStore::open(&config(&file, dim)).unwrap();
    eprintln!("opened {file:?} (never write to the live file — pass a copy)\n");

    // ── 1. coverage per label ───────────────────────────────────────────
    let memory_labels = [
        labels::EPISODIC,
        labels::KNOWLEDGE,
        labels::PROCEDURAL,
        labels::AUTOBIOGRAPHICAL,
    ];
    eprintln!("── coverage ──────────────────────────────────────────────");
    eprintln!("  {:<16} {:>6} {:>16} {:>13}", "label", "nodes", "with_embedding", "vector_index");
    for label in memory_labels {
        let ids = store.db().graph_store().nodes_by_label(label);
        let nodes = ids.len();
        let mut with_emb = 0usize;
        for id in &ids {
            if let Some(n) = store.get_node(*id)
                && embedding_of(&n, "embedding").is_some()
            {
                with_emb += 1;
            }
        }
        eprintln!(
            "  {label:<16} {nodes:>6} {with_emb:>16} {:>13}",
            format!("{:?}", index_len(&store, label))
        );
    }

    // ── 2. the probe node ──────────────────────────────────────────────
    eprintln!("\n── probe node ({probe}) ─────────────────────────────────");
    let mut probe_node: Option<(NodeId, String, Option<Vec<f32>>)> = None;
    for id in store.db().graph_store().nodes_by_label(labels::EPISODIC) {
        let Some(n) = store.get_node(id) else { continue };
        let t = text_of(&n, "content").unwrap_or_default();
        if t.contains(&probe) {
            probe_node = Some((id, t, embedding_of(&n, "embedding")));
            break;
        }
    }
    match &probe_node {
        Some((id, text, vec)) => {
            eprintln!(
                "  found id={} has_embedding={} dim={:?} text_len={}",
                id.0,
                vec.is_some(),
                vec.as_ref().map(|v| v.len()),
                text.chars().count()
            );
            eprintln!("  head: {:?}", text.chars().take(64).collect::<String>());
        }
        None => eprintln!("  NOT FOUND — the probe node is not in this store"),
    }

    // ── 3. per-source behaviour ────────────────────────────────────────
    eprintln!("\n── searches on Episodic ─────────────────────────────────");
    let zh = "独角兽冰淇淋彩虹气球".to_string();
    let head: String = probe_node
        .as_ref()
        .map(|(_, t, _)| t.chars().take(24).collect())
        .unwrap_or_default();

    for q in [&probe, &zh, &head] {
        let hits = store.text_search(labels::EPISODIC, q, 5);
        eprintln!(
            "  text_search   (BM25) {q:?} -> {:?}",
            hits.map(|v| v.len()).map_err(|e| e.to_string())
        );
    }

    if let Some((_, _, Some(vec))) = &probe_node {
        let hits = store.vector_search(labels::EPISODIC, vec, 5, None);
        match hits {
            Ok(h) => {
                let self_rank = h.iter().position(|(id, _)| {
                    probe_node.as_ref().is_some_and(|(pid, _, _)| *pid == *id)
                });
                eprintln!(
                    "  vector_search (probe's own vector) -> {} hits, probe_at={self_rank:?}",
                    h.len()
                );
            }
            Err(e) => eprintln!("  vector_search failed: {e}"),
        }
        for q in [&probe, &zh] {
            match store.hybrid_search(labels::EPISODIC, "content", "embedding", q, vec, 5) {
                Ok(h) => eprintln!("  hybrid_search {q:?} -> {} hits", h.len()),
                Err(e) => eprintln!("  hybrid_search {q:?} failed: {e}"),
            }
        }
    } else {
        eprintln!("  (no probe vector — cannot exercise the vector path)");
    }

    // ── 4. cosine floor (the fix) ─────────────────────────────────────
    //
    // `provider_impl.rs::hybrid_search_full` (what the Runtime calls) ranks
    // with RRF but thresholds on the *absolute* cosine similarity recovered
    // from the vector index, then returns it normalized to [0, 1]. The old
    // code thresholded the fused score, which goes negative on the
    // single-source (vector-only) path and silently dropped every hit.
    // `None` / `-1.0` keeps everything; `0.3` drops weakly-related rows.
    if let Some((_, _, Some(vec))) = &probe_node {
        eprintln!("
── cosine floor (relevance scale) ───────────────────────");
        let unrelated: Vec<f32> = (0..dim).map(|i| 0.1 + (i % 7) as f32 * 0.01).collect();
        for (qname, q) in [("zh phrase", &zh), ("ascii token", &probe)] {
            for (name, v) in [("probe vec", vec), ("unrelated vec", &unrelated)] {
                let raw = store
                    .hybrid_search(labels::EPISODIC, "content", "embedding", q, v.as_slice(), 3)
                    .map(|r| r.iter().map(|(_, s)| *s).collect::<Vec<f64>>());
                let kept = store
                    .hybrid_search_full(labels::EPISODIC, q, v.as_slice(), 3, 0.0, 0.0, Some(0.3))
                    .map(|r| r.iter().map(|(_, s)| *s).collect::<Vec<f64>>());
                eprintln!("  [{qname} x {name}] fused={raw:?} | min_cosine=0.3 -> {kept:?}");
            }
        }
    }

    // ── 4. write path, reproduced exactly ──────────────────────────────
    //
    // `memory_store` → `provider.store_episode` → `GrafeoStore::store_episode`
    // (episodic/store.rs), which is a TWO-step write: create the node without
    // an embedding, then `set_node_property(id, "embedding", ..)`. That is a
    // different API from `store_node(..)` with the vector inline, so it gets
    // its own test. Both indexes must pick the node up, or fresh memories are
    // invisible to one of the two retrieval sources.
    eprintln!("\n── write path (memory_store's real sequence) ────────────");
    let token = "DIAGNOSE_WRITE_MARKER_5C1D";
    let marker: Vec<f32> = (0..dim).map(|i| 0.013 + i as f32 * 1e-6).collect();
    let id = store
        .db()
        .create_node_with_props(&[labels::EPISODIC], [("content", Value::from(token))]);
    store.set_node_property(id, "embedding", Value::Vector(Arc::from(marker.as_slice())));
    let v = store.vector_search(labels::EPISODIC, &marker, 5, None).unwrap();
    let t = store.text_search(labels::EPISODIC, token, 5).unwrap();
    eprintln!(
        "  vector_search finds the fresh node = {}",
        v.iter().any(|(h, _)| *h == id)
    );
    eprintln!(
        "  text_search (BM25) finds the fresh node = {}",
        t.iter().any(|(h, _)| *h == id)
    );
    eprintln!("    (vector=true + text=false  =>  the write path does not feed the BM25 index,");
    eprintln!("     so fresh memories are unreachable by any text-side query until a reopen)");

    let before = index_len(&store, labels::EPISODIC);
    let plain = store
        .db()
        .create_node_with_props(&[labels::EPISODIC], [("content", Value::from("diagnose marker, no vector"))]);
    eprintln!(
        "  node WITHOUT vector: index_len {before:?} -> {:?} (node id {})",
        index_len(&store, labels::EPISODIC),
        plain.0
    );
    eprintln!("    (a jump here = vectorless nodes join HNSW as zero vectors)");

    store.close().unwrap();
    eprintln!("\ndone (this ran on a copy; pass a fresh copy next time)");
}
