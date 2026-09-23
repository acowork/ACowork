//! Conversation index store layout + watermark recovery (ADR-081 §4.2).
//!
//! Guards two properties that a boot-time check cannot see:
//!
//! 1. the index lives in a `.grafeo` **single file** (the suffix selects the
//!    engine's checkpointed layout; a suffix-less path is treated as the
//!    legacy `WalDirectory` layout, whose WAL grew to 182 MB and was replayed
//!    in full on every boot), and the legacy directory is migrated away;
//! 2. re-opening the store recovers the per-session watermark, so the tailer
//!    resumes instead of re-embedding the whole JSONL history (the watermark
//!    is in-memory, so it must be rebuilt from the persisted nodes).

use std::sync::Arc;

use acowork_grafeo::grafeo::GrafeoStore;
use acowork_grafeo::types::GrafeoConfig;
use acowork_runtime::conversation_index::ConversationIndex;
use grafeo_common::types::Value;

const DIM: usize = 8;

fn embedding(seed: f32) -> Vec<f32> {
    (0..DIM).map(|k| seed + k as f32 / 100.0).collect()
}

#[test]
fn store_is_a_single_file_and_reopen_recovers_watermarks() {
    let dir = tempfile::tempdir().unwrap();

    {
        let index = ConversationIndex::open(dir.path(), DIM).unwrap();
        index
            .index_message("s1", 0, "user", "what is grafeo", &embedding(0.1))
            .unwrap();
        index
            .index_message("s1", 1, "assistant", "a graph database", &embedding(0.2))
            .unwrap();
        index
            .index_message("s2", 7, "user", "unrelated session", &embedding(0.3))
            .unwrap();
        assert_eq!(index.next_line("s1"), 2);
        // No close(): a Runtime is normally killed by the Node Agent, so the
        // nodes live in the WAL only. Reopen must recover them by replaying it.
    }

    assert!(
        dir.path().join("conversation_index.grafeo").is_file(),
        "the index must be a single .grafeo file (checkpointed WAL), not a directory"
    );

    let index = ConversationIndex::open(dir.path(), DIM).unwrap();
    // Without recovery this is 0 for every session and the tailer re-embeds
    // and re-inserts all history on every restart.
    assert_eq!(index.next_line("s1"), 2, "watermark must survive a reopen");
    assert_eq!(index.next_line("s2"), 8, "watermark must survive a reopen");

    // The restored store is still searchable (nodes + text index came back).
    let hits = index.search("graph database", None, 5);
    assert!(
        hits.iter().any(|h| h.session_id == "s1"),
        "text search over the reopened store found nothing: {hits:?}"
    );
}

#[test]
fn legacy_directory_is_migrated_into_the_single_file_store() {
    let dir = tempfile::tempdir().unwrap();
    let legacy = dir.path().join("conversation_index"); // suffix-less -> legacy WalDirectory
    let label = "ConversationMessage";

    // Build a legacy store the way pre-migration builds did, embeddings included.
    {
        let store = GrafeoStore::open(&GrafeoConfig {
            db_path: legacy.clone(),
            embedding_dim: DIM,
        })
        .unwrap();
        let _ = store.db().create_text_index(label, "content");
        let _ = store.ensure_vector_index(label, "embedding", DIM);
        for (line, role, content) in [
            (0usize, "user", "old user turn"),
            (1, "assistant", "old assistant turn"),
        ] {
            store
                .store_node(
                    label,
                    [
                        ("session_id", Value::from("s1")),
                        ("message_index", Value::from(line as i64)),
                        ("role", Value::from(role)),
                        ("content", Value::from(content)),
                        ("embedding", Value::Vector(Arc::from(embedding(0.1 + line as f32).as_slice()))),
                    ],
                )
                .unwrap();
        }
        store.close().unwrap();
    }
    assert!(legacy.is_dir());

    let index = ConversationIndex::open(dir.path(), DIM).unwrap();

    // The old store is gone, its messages — and their embeddings — live on.
    assert!(!legacy.exists(), "the legacy unbounded-WAL store must be dropped");
    assert!(dir.path().join("conversation_index.grafeo").is_file());
    assert_eq!(
        index.next_line("s1"),
        2,
        "migrated messages must set the watermark, or the tailer re-embeds the history"
    );
    let hits = index.search("old assistant", None, 5);
    assert!(
        hits.iter().any(|h| h.content.contains("old assistant")),
        "migrated messages must be searchable: {hits:?}"
    );
}
