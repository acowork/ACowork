//! End-to-end wiring test for ADR-082 §4 step 3: the runtime's session-meta
//! free functions must serve from the SQLite store once the backend is
//! installed, after a one-shot import of the legacy JSON side-car.
//!
//! Lives in its own integration-test binary because
//! `conversation::install_session_meta_backend` is a process-wide `OnceLock`
//! — a second install in the same process is ignored, so sharing a binary
//! with other session-meta tests would make them order-dependent.

use std::sync::Arc;

use acowork_core::error::AcoworkError;
use acowork_memory::SessionMetaStore;
use acowork_memory::session_meta::SessionMeta;
use acowork_sqlite::{SqliteSessionMetaStore, SqliteStore};

use acowork_runtime::conversation::{
    install_session_meta_backend, read_session_meta, scan_sessions_from_meta, write_session_meta,
};

const DIM: usize = 8;

fn make_meta(sid: &str, last_active: &str, title: &str) -> SessionMeta {
    SessionMeta {
        version: 4,
        session_id: sid.to_string(),
        agent_id: "test-agent".to_string(),
        created_at: "2026-01-01T00:00:00.000Z".to_string(),
        title: Some(title.to_string()),
        workspace_id: None,
        model: Some("test-model".into()),
        provider: Some("test-provider".into()),
        account_id: None,
        reasoning_effort: None,
        temperature: None,
        context_window: None,
        todos: None,
        message_count: 1,
        last_active_at: last_active.to_string(),
        tokens: None,
        llm_call_counter: None,
        model_ratio: None,
        last_compaction_offset: None,
        corrupted: false,
    }
}

#[test]
fn legacy_json_sidecars_import_then_serve_from_sqlite() -> Result<(), AcoworkError> {
    let ws = tempfile::tempdir().unwrap();
    let meta_dir = ws.path().join("conversations").join("meta");
    std::fs::create_dir_all(&meta_dir).unwrap();

    // Two legacy JSON side-car files, as an install predating ADR-082 would
    // have on disk.
    for (sid, stamp, title) in [
        ("s1", "2025-01-01T00:00:00.000Z", "first session"),
        ("s2", "2025-02-01T00:00:00.000Z", "second session"),
    ] {
        std::fs::write(
            meta_dir.join(format!("{sid}.json")),
            serde_json::to_string(&make_meta(sid, stamp, title)).unwrap(),
        )
        .unwrap();
    }
    let legacy_s1 = meta_dir.join("s1.json");
    let legacy_s2 = meta_dir.join("s2.json");

    // Open the SQLite store in the memory dir — the same file the runtime
    // uses by default (one file for memory + session meta + conversation
    // vectors).
    let store = Arc::new(SqliteStore::open(
        ws.path().join("memory").join("private.sqlite"),
        DIM,
    )?);
    let sm = Arc::new(SqliteSessionMetaStore::new(store));

    // Import before installing so the backend is populated when the free
    // functions first consult it (mirrors the runtime boot order).
    let report = sm.import_from_json(&meta_dir)?;
    assert_eq!(report.imported, 2, "both sidecars imported");

    install_session_meta_backend(sm.clone());

    // Reads now come from SQLite.
    let got = read_session_meta(&ws.path().join("conversations"), "s1").unwrap();
    assert_eq!(got.title.as_deref(), Some("first session"));
    assert_eq!(got.model.as_deref(), Some("test-model"));

    // A brand-new write lands in SQLite only — no new JSON file appears.
    let s3 = make_meta("s3", "2026-06-01T00:00:00.000Z", "third session");
    write_session_meta(&ws.path().join("conversations"), &s3).unwrap();
    assert!(
        !meta_dir.join("s3.json").exists(),
        "sqlite backend must not write a JSON sidecar"
    );
    let round = read_session_meta(&ws.path().join("conversations"), "s3").unwrap();
    assert_eq!(round.title.as_deref(), Some("third session"));

    // The import never deletes its source.
    assert!(legacy_s1.exists(), "legacy JSON sidecar must survive the import");
    assert!(legacy_s2.exists(), "legacy JSON sidecar must survive the import");

    // The list path sees all three rows, newest first.
    let all = scan_sessions_from_meta(&ws.path().join("conversations"));
    assert_eq!(all.len(), 3);
    assert_eq!(all[0].0, "s3", "newest session first");

    // Re-running the import is a no-op (table is populated).
    let again = sm.import_from_json(&meta_dir)?;
    assert_eq!(again.imported, 0);
    assert!(again.skipped_target_non_empty);
    Ok(())
}
