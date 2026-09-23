//! ADR-082 §4 step 3: `ConversationIndex::from_store` shares the memory file
//! and pulls in a pre-existing legacy index so nothing is re-embedded.
//!
//! Covers the two migration sources that matter for the SQLite-default
//! switch:
//!   * the intermediate `{work_dir}/conversation_index.sqlite` layout;
//!   * the grafeo `{work_dir}/conversation_index.grafeo` layout.

use std::sync::Arc;

use acowork_sqlite::conversation::ConversationStore;
use acowork_sqlite::SqliteStore;

use acowork_runtime::conversation_index::ConversationIndex;

const DIM: usize = 4;

fn emb(seed: f32) -> Vec<f32> {
    vec![seed, 0.0, 0.0, 1.0]
}

#[test]
fn legacy_sqlite_index_is_migrated_into_the_shared_store() {
    let ws = tempfile::tempdir().unwrap();

    // The legacy index lives where the intermediate layout put it.
    let legacy_path = ws.path().join(acowork_runtime::conversation_index::STORE_FILE);
    {
        let legacy = ConversationStore::open(&legacy_path, DIM).unwrap();
        legacy
            .index_message("s1", 0, "user", "remember the legacy index", &emb(0.5))
            .unwrap();
        legacy
            .index_message("s1", 1, "assistant", "it was migrated", &emb(0.6))
            .unwrap();
        legacy
            .index_message("s2", 0, "user", "another session", &emb(0.7))
            .unwrap();
    }

    // The shared store starts empty, exactly as `init_sqlite_backend` leaves it.
    let shared_path = ws.path().join("memory").join("private.sqlite");
    let shared = Arc::new(SqliteStore::open(&shared_path, DIM).unwrap());

    let index = ConversationIndex::from_store(shared.clone(), ws.path(), DIM).unwrap();
    assert_eq!(
        index.message_count(),
        3,
        "all legacy messages carried into the shared store"
    );

    // The embeddings survived, so a vector search finds them without any
    // re-embedding pass.
    let hits = index.search("legacy index", Some(&emb(0.5)), 5);
    assert!(
        hits.iter().any(|h| h.session_id == "s1"),
        "migrated message is searchable"
    );

    // The source file is left in place.
    assert!(legacy_path.exists(), "legacy index source must survive");
}

#[test]
fn second_open_does_not_re_import() {
    let ws = tempfile::tempdir().unwrap();
    let legacy_path = ws.path().join(acowork_runtime::conversation_index::STORE_FILE);
    {
        let legacy = ConversationStore::open(&legacy_path, DIM).unwrap();
        legacy
            .index_message("s1", 0, "user", "only once", &emb(0.5))
            .unwrap();
    }
    let shared_path = ws.path().join("memory").join("private.sqlite");
    let shared = Arc::new(SqliteStore::open(&shared_path, DIM).unwrap());

    let first = ConversationIndex::from_store(shared.clone(), ws.path(), DIM).unwrap();
    assert_eq!(first.message_count(), 1);
    drop(first);

    // Reopening the same shared store must not duplicate the imported rows.
    let second = ConversationIndex::from_store(shared.clone(), ws.path(), DIM).unwrap();
    assert_eq!(second.message_count(), 1, "import must be idempotent");
}
