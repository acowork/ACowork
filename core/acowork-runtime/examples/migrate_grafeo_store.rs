//! Offline grafeo → SQLite migration runner (ADR-082 §4 step 2).
//!
//! Run this against a **copy** of an agent's workspace to see what the
//! boot-time migration does. Unit tests build their own stores; only real
//! history has the node shapes (and the `skipped` ones) that a synthetic
//! store cannot produce.
//!
//! It copies nothing and refuses to run when the target already exists, so the
//! live store stays read-only to it. It writes `memory/private.sqlite` and
//! `conversation_index.sqlite` into the workspace it is pointed at — give it a
//! scratch copy, never the live workspace.
//!
//! ```text
//! cargo run -p acowork-runtime --example migrate_grafeo_store -- \
//!     --workspace <copy-of-workspace> --dim 512
//! ```
//!
//! Exits non-zero when any source node was skipped or when recall parity fails,
//! so it can gate a rollout instead of being read like a log line.

use std::path::PathBuf;
use std::sync::Arc;

use acowork_grafeo::grafeo::GrafeoStore;
use acowork_grafeo::types::GrafeoConfig;
use acowork_memory::{MemoryProvider, MemoryQuery};
use acowork_runtime::conversation_index::ConversationIndex;
use acowork_runtime::memory::grafeo_import::{detect_embedding_dim, import_grafeo_memory};
use acowork_sqlite::SqliteStore;

fn main() {
    let mut workspace: Option<PathBuf> = None;
    let mut fallback_dim = 512usize;
    let mut memory_only = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--workspace" => workspace = args.next().map(PathBuf::from),
            "--dim" => {
                fallback_dim = args.next().and_then(|v| v.parse().ok()).unwrap_or(512);
            }
            "--memory-only" => memory_only = true,
            other => {
                eprintln!("unexpected argument: {other}");
                std::process::exit(2);
            }
        }
    }
    let Some(workspace) = workspace else {
        eprintln!("usage: --workspace <copy-of-an-agent-workspace> [--dim <fallback>]");
        std::process::exit(2);
    };

    let memory_dir = workspace.join("memory");
    let db_path = memory_dir.join("private.sqlite");
    if db_path.exists() {
        eprintln!(
            "refusing to run: {} already exists — this runner only imports into a fresh target",
            db_path.display()
        );
        std::process::exit(2);
    }
    println!("workspace : {}", workspace.display());
    // The dimension decides the width SQLite records for this store, so it has
    // to come from the data. Guessing wrong would not corrupt the migrated
    // vectors, but it would leave the runtime skipping every one of them.
    let dim = detect_embedding_dim(&memory_dir);
    match dim {
        Some(dim) => println!("dimension : {dim} (read from a source embedding)"),
        None => println!(
            "dimension : {fallback_dim} (ASSUMED — no source embedding found to confirm it)"
        ),
    }
    let dim = dim.unwrap_or(fallback_dim);

    let mut failures = 0usize;

    // ── Memory store ─────────────────────────────────────────────────────
    let target = SqliteStore::open(&db_path, dim).expect("open sqlite target");
    let report = import_grafeo_memory(&memory_dir, &target, dim);
    match &report {
        Some(r) => {
            println!(
                "memory import   : episodes={} knowledge={} procedural={} autobiographical={} \
                 skipped={} remapped={} dropped={}",
                r.episodes,
                r.knowledge,
                r.procedural,
                r.autobiographical,
                r.skipped,
                r.references_remapped,
                r.references_dropped
            );
            if r.skipped > 0 {
                println!(
                    "FAIL: {} node(s) could not be converted. They are still in the grafeo \
                     store, but they are not in this migration:",
                    r.skipped
                );
                for (why, count) in &r.skipped_reasons {
                    println!("  {count:>6} x {why}");
                }
                for (why, detail) in &r.skipped_examples {
                    let detail: String = detail.chars().take(200).collect();
                    println!("  first {why}: {detail}");
                }
            }
            if !r.normalized.is_empty() {
                println!("  filled in for legacy nodes (not data loss):");
                for (what, count) in &r.normalized {
                    println!("  {count:>6} x {what}");
                }
            }
            if r.skipped > 0 {
                failures += 1;
            }
        }
        None => println!("memory import   : nothing to migrate (no source, or target not empty)"),
    }

    // ── Recall parity, memory ────────────────────────────────────────────
    // Probes are taken out of the migrated data itself: ask each store for a
    // snippet of an episode and it must hand that episode back. A hardcoded
    // query list would report a green run on a store that happens to contain
    // none of the words, which proves nothing.
    if report.as_ref().is_some_and(|r| !r.is_empty()) {
        let source: Arc<dyn MemoryProvider> = Arc::new(
            GrafeoStore::open(&GrafeoConfig {
                db_path: memory_dir.join("private.grafeo"),
                embedding_dim: dim,
            })
            .expect("open grafeo source"),
        );
        let episodes = target
            .get_episodes(None, 200)
            .expect("read back migrated episodes");
        if episodes.is_empty() {
            println!("recall parity   : no episodes to probe");
        }
        let mut probed = 0usize;
        for episode in episodes.iter().take(5) {
            let Some(snippet) = snippet_of(&episode.content) else {
                continue;
            };
            probed += 1;
            let before = contents(source.as_ref(), &snippet);
            let after = contents(&target, &snippet);
            let hit = |hits: &[String]| hits.iter().any(|c| c == &episode.content);
            println!(
                "recall {snippet:>12} : grafeo={} sqlite={} self-hit grafeo={} sqlite={}",
                before.len(),
                after.len(),
                hit(&before),
                hit(&after)
            );
            if !hit(&after) {
                let head: String = episode.content.chars().take(60).collect();
                println!("  FAIL: sqlite cannot find its own episode by snippet: {head}");
                failures += 1;
            }
            // The snapshot is the baseline: if even grafeo misses a self-hit,
            // this store's ranking is the thing to look at, not the migration.
            if !hit(&before) {
                println!("  note: grafeo also missed this self-hit — weak baseline");
            }
        }
        if probed == 0 {
            println!("FAIL: no episode was long enough to probe — recall is unproven");
            failures += 1;
        }
    }

    // ── Conversation index ───────────────────────────────────────────────
    if memory_only {
        println!("conversation idx: skipped (--memory-only)");
    } else if workspace.join("conversation_index.grafeo").exists()
        || workspace.join("conversation_index").is_dir()
    {
        let count = {
            let index = ConversationIndex::open(&workspace, dim).expect("open conversation index");
            let count = index.message_count();
            println!("conversation idx: {count} messages indexed");
            // The JSONL log is what the indexer tails, so a message out of it
            // is a message the index is supposed to be able to find.
            let probe = first_jsonl_snippet(index.conversations_dir());
            match &probe {
                Some(probe) => {
                    let hits = index.search(probe, None, 5);
                    println!("  self-hit probe  : {probe:?} -> {} hit(s)", hits.len());
                    if !hits.iter().any(|h| h.content.contains(probe)) {
                        println!("  FAIL: the conversation index cannot find its own message");
                        failures += 1;
                    }
                }
                None => println!("  self-hit probe  : no conversations/*.jsonl to probe"),
            }
            count
        };
        // Reopening must not re-import and must still see the messages: this is
        // the one-time guard running a second time, exactly as a real restart.
        let reopened = ConversationIndex::open(&workspace, dim).expect("reopen conversation index");
        println!("  after reopen    : {} messages", reopened.message_count());
        if count == 0 {
            println!("FAIL: the conversation index imported nothing");
            failures += 1;
        } else if reopened.message_count() != count {
            println!("FAIL: message count changed on reopen — import is not one-shot");
            failures += 1;
        }
    } else {
        println!("conversation idx: no source index found");
    }

    println!();
    if failures == 0 {
        println!("OK: migration is complete and recall parity holds on this store");
    } else {
        println!("{failures} problem(s) — do not switch this agent yet");
        std::process::exit(1);
    }
}

/// Episode contents returned for `query`, for cross-backend comparison.
fn contents(provider: &dyn MemoryProvider, query: &str) -> Vec<String> {
    let mut q = MemoryQuery::new(query);
    q.limit = 10;
    provider
        .search_episodes(&q)
        .map(|hits| hits.into_iter().map(|h| h.content).collect())
        .unwrap_or_default()
}

/// A short, distinctive window out of `content`, or `None` when the text is too
/// short to say anything about. It starts past the beginning to skip the
/// boilerplate that opens most of these messages.
fn snippet_of(content: &str) -> Option<String> {
    let chars: Vec<char> = content.chars().collect();
    if chars.len() < 24 {
        return None;
    }
    let start = chars.len() / 4;
    let snippet: String = chars[start..start + 12].iter().copied().collect();
    let snippet = snippet.trim().to_string();
    (snippet.chars().count() >= 8).then_some(snippet)
}

/// Content snippet of the first message in the JSONL log the indexer tails.
fn first_jsonl_snippet(dir: &std::path::Path) -> Option<String> {
    let entry = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "jsonl"))?;
    let text = std::fs::read_to_string(entry).ok()?;
    text.lines().find_map(|line| {
        let value: serde_json::Value = serde_json::from_str(line).ok()?;
        snippet_of(value.get("content")?.as_str()?)
    })
}
