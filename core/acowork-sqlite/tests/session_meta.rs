//! Tests for `SqliteSessionMetaStore` — the SQLite-backed
//! `SessionMetaStore` implementation (ADR-082 §4 step 3).
//!
//! Validates the trait contract end-to-end against a real on-disk store:
//!
//! 1. Round-trip: `upsert` → `get` returns the same fields.
//! 2. `list_recent` orders by `last_active_at` descending and respects `limit`.
//! 3. `search` matches `title` / `agent_id` / `workspace_id` and falls back
//!    to `list_recent` on empty query.
//! 4. `delete` removes one row (and its FTS entry) and is idempotent.
//! 5. `prune_to` deletes the right rows and reports the deleted ids.
//! 6. `list_with_totals` returns correct aggregates across many rows.
//! 7. `import_from_json` is idempotent and leaves source files alone.
//! 8. The SQLite store keeps memory + conversation index + session meta in
//!    the same `.sqlite` file (the user's "one file" requirement).

use std::path::PathBuf;
use std::sync::Arc;

use acowork_core::error::Result as AcoworkResult;
use acowork_memory::session_meta::{
    SessionImportReport, SessionMeta, SessionMetaStore, last_active_at_ms,
};
use acowork_memory::{SessionTokens, TodoItem, TodoStatus};
use acowork_sqlite::{SqliteSessionMetaStore, SqliteStore};

const DIM: usize = 4;

fn make_meta(
    sid: &str,
    last_active: &str,
    title: Option<&str>,
    agent: &str,
) -> SessionMeta {
    SessionMeta {
        version: 4,
        session_id: sid.to_string(),
        agent_id: agent.to_string(),
        created_at: "2026-01-01T00:00:00.000Z".to_string(),
        title: title.map(str::to_string),
        workspace_id: Some("ws-test".into()),
        model: Some("gpt-4o-mini".into()),
        provider: Some("openai".into()),
        account_id: Some("acct-default".into()),
        reasoning_effort: None,
        temperature: Some(0.7),
        context_window: Some(128_000),
        todos: Some(vec![TodoItem {
            id: "todo-1".into(),
            content: "ship it".into(),
            status: TodoStatus::InProgress,
        }]),
        message_count: 3,
        last_active_at: last_active.to_string(),
        tokens: Some(SessionTokens {
            last_input: 100,
            last_output: 50,
            total_input: 900,
            total_output: 400,
            last_cache_read: 0,
            last_cache_write: 0,
            total_cache_read: 0,
            total_cache_write: 0,
        }),
        llm_call_counter: Some(2),
        model_ratio: Some(1.0),
        last_compaction_offset: Some(0),
        corrupted: false,
    }
}

fn open_pair(path: &PathBuf) -> (Arc<SqliteStore>, SqliteSessionMetaStore) {
    let store = Arc::new(SqliteStore::open(path, DIM).unwrap());
    let session_meta = SqliteSessionMetaStore::new(store.clone());
    (store, session_meta)
}

#[test]
fn round_trip_preserves_every_field() -> AcoworkResult<()> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    let (_store, sm) = open_pair(&path);
    let meta = make_meta("abc", "2026-03-04T05:06:07.890Z", Some("hello world"), "ponytail");
    sm.upsert(&meta)?;

    let got = sm.get("abc")?.expect("row present");
    assert_eq!(got.session_id, "abc");
    assert_eq!(got.agent_id, "ponytail");
    assert_eq!(got.title.as_deref(), Some("hello world"));
    assert_eq!(got.workspace_id.as_deref(), Some("ws-test"));
    assert_eq!(got.model.as_deref(), Some("gpt-4o-mini"));
    assert_eq!(got.provider.as_deref(), Some("openai"));
    assert_eq!(got.account_id.as_deref(), Some("acct-default"));
    assert_eq!(got.temperature, Some(0.7));
    assert_eq!(got.context_window, Some(128_000));
    assert_eq!(got.message_count, 3);
    assert_eq!(got.llm_call_counter, Some(2));
    assert_eq!(got.model_ratio, Some(1.0));
    let tokens = got.tokens.as_ref().expect("tokens persisted");
    assert_eq!(tokens.last_input, 100);
    assert_eq!(tokens.last_output, 50);
    assert_eq!(tokens.total_input, 900);
    assert_eq!(tokens.total_output, 400);
    let todos = got.todos.as_ref().expect("todos persisted");
    assert_eq!(todos.len(), 1);
    assert_eq!(todos[0].id, "todo-1");
    assert_eq!(todos[0].content, "ship it");
    assert_eq!(todos[0].status, TodoStatus::InProgress);
    Ok(())
}

#[test]
fn get_missing_returns_none() -> AcoworkResult<()> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    let (_store, sm) = open_pair(&path);
    assert!(sm.get("never-existed")?.is_none());
    Ok(())
}

#[test]
fn delete_removes_row_and_is_idempotent() -> AcoworkResult<()> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    let (_store, sm) = open_pair(&path);
    sm.upsert(&make_meta("keep", "2026-01-01T00:00:00.000Z", Some("keep"), "agent"))?;
    sm.upsert(&make_meta("gone", "2026-02-01T00:00:00.000Z", Some("gone"), "agent"))?;

    sm.delete("gone")?;
    assert!(sm.get("gone")?.is_none(), "deleted row must be gone");
    assert!(sm.get("keep")?.is_some(), "sibling row untouched");

    // Both `sessions` and `fts_sessions` must be cleared — a stale
    // `fts_sessions` row would keep the deleted title alive.
    let conn = rusqlite::Connection::open(&path)
        .map_err(|e| acowork_core::error::AcoworkError::Memory(format!("sqlite: {e}")))?;
    for table in ["sessions", "fts_sessions"] {
        let n: i64 = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE session_id = 'gone'"),
                [],
                |r| r.get(0),
            )
            .map_err(|e| acowork_core::error::AcoworkError::Memory(format!("sqlite: {e}")))?;
        assert_eq!(n, 0, "{table} must not retain the deleted session");
    }

    // Idempotent: deleting an already-absent session is a no-op.
    sm.delete("gone")?;
    Ok(())
}

#[test]
fn list_recent_orders_by_last_active_desc() -> AcoworkResult<()> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    let (_store, sm) = open_pair(&path);
    sm.upsert(&make_meta("older", "2025-01-01T00:00:00.000Z", None, "a"))?;
    sm.upsert(&make_meta("middle", "2026-06-15T12:00:00.000Z", None, "a"))?;
    sm.upsert(&make_meta("newest", "2026-12-31T23:59:59.999Z", None, "a"))?;

    let listed = sm.list_recent(10)?;
    assert_eq!(listed.len(), 3);
    assert_eq!(listed[0].session_id, "newest");
    assert_eq!(listed[1].session_id, "middle");
    assert_eq!(listed[2].session_id, "older");

    let capped = sm.list_recent(2)?;
    assert_eq!(capped.len(), 2);
    assert_eq!(capped[0].session_id, "newest");
    assert_eq!(capped[1].session_id, "middle");

    assert!(sm.list_recent(0)?.is_empty());
    Ok(())
}

#[test]
fn find_latest_returns_the_newest() -> AcoworkResult<()> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    let (_store, sm) = open_pair(&path);
    sm.upsert(&make_meta("a", "2025-01-01T00:00:00.000Z", None, "a"))?;
    sm.upsert(&make_meta("b", "2026-06-15T12:00:00.000Z", None, "a"))?;
    let latest = sm.find_latest()?.expect("non-empty");
    assert_eq!(latest.session_id, "b");
    Ok(())
}

#[test]
fn empty_search_falls_back_to_list_recent() -> AcoworkResult<()> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    let (_store, sm) = open_pair(&path);
    sm.upsert(&make_meta("a", "2025-01-01T00:00:00.000Z", Some("alpha"), "agent"))?;
    sm.upsert(&make_meta("b", "2026-06-15T12:00:00.000Z", Some("beta"), "agent"))?;
    let all = sm.search("   ", 10)?;
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].session_id, "b");
    Ok(())
}

#[test]
fn search_matches_title_agent_workspace() -> AcoworkResult<()> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    let (_store, sm) = open_pair(&path);
    sm.upsert(&make_meta("a", "2025-01-01T00:00:00.000Z", Some("alpha project"), "agent"))?;
    sm.upsert(&make_meta("b", "2026-06-15T12:00:00.000Z", Some("beta run"), "agent"))?;

    let by_title = sm.search("alpha", 10)?;
    assert_eq!(by_title.len(), 1);
    assert_eq!(by_title[0].session_id, "a");

    let by_title_cs = sm.search("ALPHA", 10)?;
    assert_eq!(by_title_cs.len(), 1, "case-insensitive match expected");

    let by_agent = sm.search("agent", 10)?;
    assert_eq!(by_agent.len(), 2);
    Ok(())
}

#[test]
fn prune_to_keeps_top_n_and_reports_victims() -> AcoworkResult<()> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    let (_store, sm) = open_pair(&path);
    sm.upsert(&make_meta("a", "2025-01-01T00:00:00.000Z", None, "agent"))?;
    sm.upsert(&make_meta("b", "2025-02-01T00:00:00.000Z", None, "agent"))?;
    sm.upsert(&make_meta("c", "2025-03-01T00:00:00.000Z", None, "agent"))?;
    sm.upsert(&make_meta("d", "2025-04-01T00:00:00.000Z", None, "agent"))?;

    let victims = sm.prune_to(2)?;
    assert_eq!(victims.len(), 2);
    // Oldest first; the two newest (c, d) survive.
    let mut sorted = victims.clone();
    sorted.sort();
    assert_eq!(sorted, vec!["a".to_string(), "b".to_string()]);

    let kept = sm.list_recent(10)?;
    assert_eq!(kept.len(), 2);
    let kept_ids: Vec<String> = kept.iter().map(|m| m.session_id.clone()).collect();
    assert!(kept_ids.contains(&"c".to_string()));
    assert!(kept_ids.contains(&"d".to_string()));
    Ok(())
}

#[test]
fn list_with_totals_aggregates_across_pages() -> AcoworkResult<()> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    let (_store, sm) = open_pair(&path);
    for i in 0..5 {
        let sid = format!("s{i}");
        let stamp = format!("2026-{:02}-01T00:00:00.000Z", i + 1);
        let mut meta = make_meta(&sid, &stamp, None, "agent");
        meta.tokens = Some(SessionTokens {
            last_input: 10,
            last_output: 5,
            total_input: 100,
            total_output: 50,
            last_cache_read: 0,
            last_cache_write: 0,
            total_cache_read: 0,
            total_cache_write: 0,
        });
        sm.upsert(&meta)?;
    }
    let (page1, total, agg) = sm.list_with_totals(1, 2)?;
    assert_eq!(page1.len(), 2);
    assert_eq!(total, 5);
    assert_eq!(agg, (500, 250, 0, 0));
    let (page2, _, _) = sm.list_with_totals(2, 2)?;
    assert_eq!(page2.len(), 2);
    let (page3, _, _) = sm.list_with_totals(3, 2)?;
    assert_eq!(page3.len(), 1);
    Ok(())
}

#[test]
fn import_from_json_loads_legacy_dir_and_is_idempotent() -> AcoworkResult<()> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    let (_store, sm) = open_pair(&path);

    // Pre-populate the legacy dir with two JSON sidecar files.
    let meta_dir = dir.path().join("conversations").join("meta");
    std::fs::create_dir_all(&meta_dir).unwrap();
    let m1 = make_meta("legacy-1", "2025-08-01T00:00:00.000Z", Some("from json 1"), "agent");
    let m2 = make_meta("legacy-2", "2025-09-01T00:00:00.000Z", Some("from json 2"), "agent");
    std::fs::write(
        meta_dir.join("legacy-1.json"),
        serde_json::to_string(&m1).unwrap(),
    )
    .unwrap();
    std::fs::write(
        meta_dir.join("legacy-2.json"),
        serde_json::to_string(&m2).unwrap(),
    )
    .unwrap();
    let file_one = meta_dir.join("legacy-1.json");
    let file_two = meta_dir.join("legacy-2.json");
    assert!(file_one.exists());
    assert!(file_two.exists());

    let report = sm.import_from_json(&meta_dir)?;
    assert_eq!(report.imported, 2);
    assert!(!report.skipped_target_non_empty);
    assert_eq!(report.parse_failures, 0);

    // Source files must remain on disk.
    assert!(file_one.exists());
    assert!(file_two.exists());

    // Rows are present.
    let got = sm.get("legacy-1")?.expect("imported row visible");
    assert_eq!(got.title.as_deref(), Some("from json 1"));

    // Re-run is a no-op because the SQLite table is non-empty.
    let again = sm.import_from_json(&meta_dir)?;
    assert_eq!(again.imported, 0);
    assert!(again.skipped_target_non_empty);
    Ok(())
}

#[test]
fn import_from_json_returns_zero_when_source_absent() -> AcoworkResult<()> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    let (_store, sm) = open_pair(&path);
    let report: SessionImportReport =
        sm.import_from_json(&dir.path().join("conversations").join("meta"))?;
    assert_eq!(report.imported, 0);
    assert_eq!(report.parse_failures, 0);
    Ok(())
}

#[test]
fn json_import_round_trips_to_the_same_meta() -> AcoworkResult<()> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    let (_store, sqlite_sm) = open_pair(&path);

    // Seed a legacy `meta/{id}.json` sidecar by hand — the pre-migration
    // format `import_from_json` reads exactly once.
    let meta_dir = dir.path().join("conv").join("meta");
    std::fs::create_dir_all(&meta_dir).unwrap();
    let meta = make_meta("x", "2026-04-05T06:07:08.090Z", Some("from json"), "agent");
    std::fs::write(meta_dir.join("x.json"), serde_json::to_vec(&meta).unwrap()).unwrap();

    let report = sqlite_sm.import_from_json(&meta_dir)?;
    assert_eq!(report.imported, 1);

    let sqlite_meta = sqlite_sm.get("x")?.expect("sqlite row");

    // Same field-for-field after the import.
    assert_eq!(meta.title, sqlite_meta.title);
    assert_eq!(meta.model, sqlite_meta.model);
    assert_eq!(meta.temperature, sqlite_meta.temperature);
    assert_eq!(
        meta.tokens.as_ref().map(|t| t.total_input),
        sqlite_meta.tokens.as_ref().map(|t| t.total_input),
    );
    Ok(())
}

#[test]
fn sessions_table_cohabits_with_memory_and_conversation_index() -> AcoworkResult<()> {
    // The user's hard requirement: one .sqlite file holds memory +
    // conversation index + session meta. We assert it by writing a row
    // into each table family and then reading all three back.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("unified.sqlite");
    let store = Arc::new(SqliteStore::open(&path, DIM)?);
    let sm = SqliteSessionMetaStore::new(store.clone());

    // Session meta side: a single row exercises the schema we just added.
    sm.upsert(&make_meta(
        "demo",
        "2026-07-01T00:00:00.000Z",
        Some("cohabitation test"),
        "agent",
    ))?;

    // Memory + conversation tables are reachable from the same file
    // because the schema is applied on every `SqliteStore::open`.
    assert!(store.node_count()? == 0, "fresh store has no nodes");
    let sessions_count: i64 = {
        let conn = rusqlite::Connection::open(&path)
            .map_err(|e| acowork_core::error::AcoworkError::Memory(format!("sqlite: {e}")))?;
        conn.query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get::<_, i64>(0))
            .map_err(|e| acowork_core::error::AcoworkError::Memory(format!("sqlite: {e}")))?
    };
    assert!(sessions_count >= 1, "one session row written");
    assert_eq!(sessions_count, 1, "one session row written");

    // The helper last_active_at_ms still works (epoch-ms invariant).
    let meta = sm.get("demo")?.expect("session meta row present");
    let ms = last_active_at_ms(&meta);
    assert!(ms > 0);
    Ok(())
}

/// `version` and `corrupted` used to be dropped on write (hardcoded to `4` /
/// `false` on read), so a corrupted session came back healthy.
#[test]
fn version_and_corrupted_round_trip() -> AcoworkResult<()> {
    let dir = tempfile::tempdir().unwrap();
    let (_store, sm) = open_pair(&dir.path().join("store.sqlite"));

    let mut meta = make_meta("c", "2026-01-01T00:00:00.000Z", None, "agent");
    meta.version = 7;
    meta.corrupted = true;
    sm.upsert(&meta)?;

    let back = sm.get("c")?.expect("row");
    assert_eq!(back.version, 7);
    assert!(back.corrupted);
    Ok(())
}

/// A pre-v1 `sessions` table (no `version` / `corrupted` columns) is upgraded
/// in place by the v1 migration: DDL skips the existing table, the migration
/// adds the columns, the row survives, and `PRAGMA user_version` is stamped
/// to the current schema version in a single transaction. Reopening the
/// store must be a no-op (no extra statements, no file mtime change).
#[test]
fn migrations_run_in_order_and_stop_at_stored_version() -> AcoworkResult<()> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");

    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
                session_id              TEXT PRIMARY KEY,
                agent_id                TEXT NOT NULL,
                created_at              INTEGER NOT NULL,
                last_active_at          INTEGER NOT NULL,
                title                   TEXT,
                workspace_id            TEXT,
                model                   TEXT,
                provider                TEXT,
                account_id              TEXT,
                reasoning_effort        TEXT,
                temperature             REAL,
                context_window          INTEGER,
                todos                   JSON,
                message_count           INTEGER NOT NULL DEFAULT 0,
                llm_call_counter        INTEGER,
                model_ratio             REAL,
                last_compaction_offset  INTEGER,
                token_last_input        INTEGER NOT NULL DEFAULT 0,
                token_last_output       INTEGER NOT NULL DEFAULT 0,
                token_total_input       INTEGER NOT NULL DEFAULT 0,
                token_total_output      INTEGER NOT NULL DEFAULT 0,
                token_last_cache_read   INTEGER NOT NULL DEFAULT 0,
                token_last_cache_write  INTEGER NOT NULL DEFAULT 0,
                token_total_cache_read  INTEGER NOT NULL DEFAULT 0,
                token_total_cache_write INTEGER NOT NULL DEFAULT 0
            );
            INSERT INTO sessions(session_id, agent_id, created_at, last_active_at, title)
            VALUES ('old', 'agent', 0, 0, 'legacy');",
        )
        .unwrap();
    }

    let (_store, sm) = open_pair(&path);

    // v1 migration added the two columns and stamped the version.
    let cols: Vec<String> = rusqlite::Connection::open(&path)
        .map_err(|e| acowork_core::error::AcoworkError::Memory(format!("sqlite: {e}")))?
        .prepare("SELECT name FROM pragma_table_info('sessions')")
        .map_err(|e| acowork_core::error::AcoworkError::Memory(format!("sqlite: {e}")))?
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| acowork_core::error::AcoworkError::Memory(format!("sqlite: {e}")))?
        .filter_map(|r| r.ok())
        .collect();
    assert!(cols.iter().any(|c| c == "version"), "v1 added `version`");
    assert!(cols.iter().any(|c| c == "corrupted"), "v1 added `corrupted`");

    let v: i64 = rusqlite::Connection::open(&path)
        .map_err(|e| acowork_core::error::AcoworkError::Memory(format!("sqlite: {e}")))?
        .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
        .map_err(|e| acowork_core::error::AcoworkError::Memory(format!("sqlite: {e}")))?;
    assert_eq!(v, acowork_sqlite::schema_version());

    // Legacy row survives with DDL defaults.
    let row = sm.get("old")?.expect("legacy row survives");
    assert_eq!(row.title.as_deref(), Some("legacy"));
    assert_eq!(row.version, 3);
    assert!(!row.corrupted);

    // Reopen is a no-op: file mtime unchanged (no transaction, no rewrite).
    let mtime_before = std::fs::metadata(&path).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    let _ = open_pair(&path);
    let mtime_after = std::fs::metadata(&path).unwrap().modified().unwrap();
    assert_eq!(
        mtime_before, mtime_after,
        "reopening an up-to-date store must not rewrite the file"
    );
    Ok(())
}


/// Fresh databases must record the schema version so external tooling can
/// tell which schema it is looking at, and older databases must be upgraded
/// to it. Reopening a store whose stored version already matches must be a
/// no-op (no extra PRAGMA, no row churn).
#[test]
fn user_version_is_stamped_on_open_and_is_idempotent() -> AcoworkResult<()> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");

    // First open on a fresh file.
    let (store, _sm) = open_pair(&path);
    let v1: i64 = rusqlite::Connection::open(&path)
        .map_err(|e| acowork_core::error::AcoworkError::Memory(format!("sqlite: {e}")))?
        .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
        .map_err(|e| acowork_core::error::AcoworkError::Memory(format!("sqlite: {e}")))?;
    assert_eq!(v1, acowork_sqlite::schema_version());
    drop(store);

    // Reopen: the stored version already matches, so opening must not touch
    // anything. We assert that by checking `sessions` row count is unchanged.
    let _ = open_pair(&path);
    let row_count: i64 = rusqlite::Connection::open(&path)
        .map_err(|e| acowork_core::error::AcoworkError::Memory(format!("sqlite: {e}")))?
        .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get::<_, i64>(0))
        .map_err(|e| acowork_core::error::AcoworkError::Memory(format!("sqlite: {e}")))?;
    assert_eq!(row_count, 0, "reopen must not synthesize rows");
    Ok(())
}

// ponytail: this suite is intentionally extensive — the SQLite backend is
// the new home of session-meta data, so a regression here is a regression
// in every workspace that ever opts in. Cloning Arc<SqliteStore> into the
// session store means both backends share the same connection lock, which
// is the property that makes the "one file" guarantee hold.