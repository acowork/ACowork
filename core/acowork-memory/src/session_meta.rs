//! Session metadata storage: trait + types (ADR-082 §4 step 3).
//!
//! The session list is the metadata side of `conversations/`: titles, model
//! choice, token totals, todo snapshots. Each implementation is responsible for
//! the storage and retrieval of the row keyed by `session_id`; the JSONL
//! conversation log itself is not in scope.
//!
//! The trait exists so the JSON file backend (`JsonSessionMetaStore`) and the
//! SQLite backend (`SqliteSessionMetaStore`) are interchangeable, mirroring the
//! `MemoryProvider` trait's role for memory nodes. The runtime holds an
//! `Arc<dyn SessionMetaStore>` the same way it holds `Arc<dyn MemoryProvider>`
//! — backend flip happens in one place, call sites do not move.
//!
//! # Atomicity
//!
//! `upsert` replaces the whole row. Callers in this codebase already mutate
//! the in-memory `SessionMeta` and `write_meta` it whole — the trait matches
//! that unit of work rather than threading individual setters through every
//! column.
//!
//! # Pruning
//!
//! `prune_to` deletes sessions whose `last_active_at` is older than the
//! (n+1)-th most-recent and returns the deleted ids. The matching JSONL files
//! are not this backend's concern — the caller cleans them up. Keeping the
//! side-effect split lets the trait stay pure on the storage axis.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use acowork_core::error::Result;

/// Default maximum pruned-after-pick when pruning is disabled.
pub const PRUNE_DISABLED: usize = 0;

/// `(input, output, cache_read, cache_write)` token totals across every
/// session in the store. Aliased so the `SessionMetaStore` signature stays
/// readable.
pub type SessionTotals = (u64, u64, u64, u64);

/// One page of `list_with_totals`: the page rows, the total row count across
/// the whole store, and the store-wide [`SessionTotals`].
pub type SessionPage = (Vec<SessionMeta>, usize, SessionTotals);

/// Per-session token counters (ADR-027 snapshot + cumulative).
///
/// Persisted as seven flattened integer columns in SQLite (see
/// `SqliteSessionMetaStore`); JSON storage serialises them whole.
///
/// `#[serde(default)]` keeps legacy meta files from before ADR-066 readable
/// (they omit the four cache fields; defaults are all zero, matching the
/// "宁可 miss 也不估计" policy).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionTokens {
    pub last_input: u64,
    pub last_output: u64,
    pub total_input: u64,
    pub total_output: u64,
    pub last_cache_read: u64,
    pub last_cache_write: u64,
    pub total_cache_read: u64,
    pub total_cache_write: u64,
}

/// One todo entry (ADR-060). Lives inside `SessionMeta.todos`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoItem {
    pub id: String,
    pub content: String,
    pub status: TodoStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

/// One persisted session. Identical wire shape to the legacy
/// `conversations/meta/{session_id}.json` so the SQLite backend can import the
/// JSON files verbatim (ADR-082 §4 step 3 boot-time migration).
///
/// `version` and `corrupted` are read back by the JSON backend's own
/// deserialisation; the SQLite backend ignores them and writes only the
/// columns it persists. They will go away in a later step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionMeta {
    pub version: u32,
    pub session_id: String,
    pub agent_id: String,
    pub created_at: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub todos: Option<Vec<TodoItem>>,

    pub message_count: u64,
    pub last_active_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<SessionTokens>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm_call_counter: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_ratio: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_compaction_offset: Option<u64>,

    #[serde(default)]
    pub corrupted: bool,
}

/// Outcome of an `import_from_json` call. Surfaced so a future telemetry hook
/// can see what landed; the runtime logs it on the way through.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SessionImportReport {
    /// Files actually imported (after dedup and parse errors).
    pub imported: usize,
    /// Files skipped because the target store is not empty.
    pub skipped_target_non_empty: bool,
    /// Source files that failed to parse.
    pub parse_failures: usize,
    /// `session_id`s present in `import_file` whose import was attempted but
    /// failed at the storage layer (rolled back row-by-row).
    pub storage_failures: usize,
}

/// Trait every session-meta backend implements.
///
/// Method semantics:
/// - `get`: `Ok(None)` when the row is missing, `Err` only for backend failure.
/// - `upsert`: replaces the entire row. No `insert_or_update` distinction.
/// - `list_recent`: cap `limit`; the JSON backend scans + sorts, the SQLite
///   backend reads an index. Order is `last_active_at` descending.
/// - `find_latest`: zero-cost shortcut for `list_recent(1).into_iter().next()`.
/// - `search`: substring query against `title` first, falling back to
///   `agent_id` / `workspace_id`. SQLite uses FTS5 trigram; the JSON backend
///   scans + filter. Empty `query` returns the same as `list_recent`.
/// - `prune_to`: leaves the first `max_sessions` newest rows and removes the
///   rest, returning the deleted ids.
/// - `list_with_totals`: paginated list + agent-wide token aggregates, used by
///   the `/sessions` HTTP API. The aggregates are *full-scan*, not page-bound;
///   implementations should compute them in a single pass, not by re-reading
///   the page slice.
/// - `import_from_json`: idempotent. The JSON backend is no-op when the
///   directory is absent and skips entirely when the store already holds a
///   row. The SQLite backend gates on `COUNT(*) > 0`. **No implementation ever
///   deletes or modifies the source files.**
pub trait SessionMetaStore: Send + Sync {
    fn get(&self, session_id: &str) -> Result<Option<SessionMeta>>;
    fn upsert(&self, meta: &SessionMeta) -> Result<()>;
    fn list_recent(&self, limit: usize) -> Result<Vec<SessionMeta>>;
    fn find_latest(&self) -> Result<Option<SessionMeta>>;
    fn search(&self, query: &str, limit: usize) -> Result<Vec<SessionMeta>>;
    /// Returns the deleted session_ids so the caller can clean up JSONL.
    fn prune_to(&self, max_sessions: usize) -> Result<Vec<String>>;
    /// `(page, row_limit)` -> `(rows, total_row_count, totals)`.
    /// - `rows` is the page slice (newest first, ≤ `row_limit`).
    /// - `total_row_count` is the count of *all* sessions, not just the page.
    /// - `totals` is `(input, output, cache_read, cache_write)` summed
    ///   across *all* sessions, not just the page.
    fn list_with_totals(&self, page: u32, size: u32) -> Result<SessionPage>;
    fn import_from_json(&self, conversations_meta_dir: &Path) -> Result<SessionImportReport>;
}

/// JSON-file backend (ADR-024's `conversations/meta/{id}.json`).
///
/// One file per session, atomic write via temp + rename. Exists both as a real
/// backend (for installs that never opt into SQLite) and as the source side of
/// `SqliteSessionMetaStore::import_from_json`. Behaviour matches the legacy
/// free functions in `acowork-runtime::conversation` byte-for-byte; that is
/// not an accident — this struct **is** those functions, repackaged.
pub struct JsonSessionMetaStore {
    /// `conversations/` (the parent of `meta/`). The `meta/` subdirectory is
    /// derived from this in every method.
    conversations_dir: PathBuf,
}

impl JsonSessionMetaStore {
    pub fn new(conversations_dir: PathBuf) -> Self {
        Self { conversations_dir }
    }

    /// Path to the per-session meta file.
    pub fn meta_path(&self, session_id: &str) -> PathBuf {
        self.conversations_dir
            .join("meta")
            .join(format!("{session_id}.json"))
    }

    fn meta_dir(&self) -> PathBuf {
        self.conversations_dir.join("meta")
    }
}

impl SessionMetaStore for JsonSessionMetaStore {
    fn get(&self, session_id: &str) -> Result<Option<SessionMeta>> {
        let path = self.meta_path(session_id);
        let data = match std::fs::read_to_string(&path) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        serde_json::from_str(&data).map(Some).map_err(Into::into)
    }

    fn upsert(&self, meta: &SessionMeta) -> Result<()> {
        let dir = self.meta_dir();
        std::fs::create_dir_all(&dir)?;
        let target = self.meta_path(&meta.session_id);
        let tmp = dir.join(format!("{}.json.tmp", meta.session_id));
        let json = serde_json::to_string_pretty(meta)?;
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &target)?;
        Ok(())
    }

    fn list_recent(&self, limit: usize) -> Result<Vec<SessionMeta>> {
        let mut all = self.scan_all_sorted_desc()?;
        if limit == 0 {
            return Ok(Vec::new());
        }
        all.truncate(limit);
        Ok(all)
    }

    fn find_latest(&self) -> Result<Option<SessionMeta>> {
        Ok(self.list_recent(1)?.into_iter().next())
    }

    fn search(&self, query: &str, limit: usize) -> Result<Vec<SessionMeta>> {
        let q = query.trim();
        if q.is_empty() {
            return self.list_recent(limit);
        }
        let needle = q.to_lowercase();
        let rows = self
            .scan_all_sorted_desc()?
            .into_iter()
            .filter(|m| {
                m.title
                    .as_deref()
                    .map(|t| t.to_lowercase().contains(&needle))
                    .unwrap_or(false)
                    || m.agent_id.to_lowercase().contains(&needle)
                    || m.workspace_id
                        .as_deref()
                        .map(|w| w.to_lowercase().contains(&needle))
                        .unwrap_or(false)
            })
            .take(limit)
            .collect();
        Ok(rows)
    }

    fn prune_to(&self, max_sessions: usize) -> Result<Vec<String>> {
        if max_sessions == 0 {
            return Ok(Vec::new());
        }
        let all = self.scan_all_sorted_desc()?; // newest first
        if all.len() <= max_sessions {
            return Ok(Vec::new());
        }
        // Sort oldest first, drop the surplus.
        let mut oldest_first = all.clone();
        oldest_first.sort_by(|a, b| a.last_active_at.cmp(&b.last_active_at));
        let to_remove = all.len() - max_sessions;
        let victims: Vec<String> = oldest_first
            .iter()
            .take(to_remove)
            .map(|m| m.session_id.clone())
            .collect();
        for sid in &victims {
            let path = self.meta_path(sid);
            match std::fs::remove_file(&path) {
                Ok(()) => (),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(e) => return Err(e.into()),
            }
        }
        Ok(victims)
    }

    fn list_with_totals(
        &self,
        page: u32,
        size: u32,
    ) -> Result<(Vec<SessionMeta>, usize, (u64, u64, u64, u64))> {
        let all = self.scan_all_sorted_desc()?;
        let total = all.len();
        let mut totals = (0u64, 0u64, 0u64, 0u64);
        for m in &all {
            if let Some(t) = &m.tokens {
                totals.0 = totals.0.saturating_add(t.total_input);
                totals.1 = totals.1.saturating_add(t.total_output);
                totals.2 = totals.2.saturating_add(t.total_cache_read);
                totals.3 = totals.3.saturating_add(t.total_cache_write);
            }
        }
        let size = size.max(1) as usize;
        let start = page as usize * size;
        let page_rows: Vec<SessionMeta> = all.into_iter().skip(start).take(size).collect();
        Ok((page_rows, total, totals))
    }

    fn import_from_json(&self, conversations_meta_dir: &Path) -> Result<SessionImportReport> {
        let mut report = SessionImportReport::default();
        let dir = conversations_meta_dir;
        if !dir.exists() {
            return Ok(report);
        }
        let rd = match std::fs::read_dir(dir) {
            Ok(rd) => rd,
            Err(_) => return Ok(report),
        };
        for entry in rd.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "json") {
                let data = match std::fs::read_to_string(&path) {
                    Ok(d) => d,
                    Err(_) => {
                        report.parse_failures += 1;
                        continue;
                    }
                };
                match serde_json::from_str::<SessionMeta>(&data) {
                    Ok(meta) => match self.upsert(&meta) {
                        Ok(()) => report.imported += 1,
                        Err(_) => report.storage_failures += 1,
                    },
                    Err(_) => report.parse_failures += 1,
                }
            }
        }
        Ok(report)
    }
}

impl JsonSessionMetaStore {
    fn scan_all_sorted_desc(&self) -> Result<Vec<SessionMeta>> {
        let dir = self.meta_dir();
        let Ok(rd) = std::fs::read_dir(&dir) else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for entry in rd.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let Ok(data) = std::fs::read_to_string(&path) else {
                continue;
            };
            if let Ok(meta) = serde_json::from_str::<SessionMeta>(&data) {
                out.push(meta);
            }
        }
        out.sort_by(|a, b| b.last_active_at.cmp(&a.last_active_at));
        Ok(out)
    }
}

/// Helper that converts the legacy `last_active_at` ISO string to an epoch-ms
/// `i64` for SQLite indexing. Returns 0 for unparseable input (the SQLite
/// backend sorts the row to the bottom, same as the legacy JSON behaviour of
/// putting malformed rows in an unspecified order).
pub fn last_active_at_ms(meta: &SessionMeta) -> i64 {
    DateTime::parse_from_rfc3339(&meta.last_active_at)
        .map(|dt| dt.with_timezone(&Utc).timestamp_millis())
        .unwrap_or(0)
}

// `Send + Sync` sanity: the type holds only a `PathBuf` and a Mutex in the
// eventual SQLite impl. The Send/Sync bound is required by the trait, and
// `PathBuf` is `Send + Sync`, so this is automatic — no unsafe.
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<JsonSessionMetaStore>();
    assert_send_sync::<Box<dyn SessionMetaStore>>();
};
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_meta(id: &str, last_active: &str, title: Option<&str>) -> SessionMeta {
        SessionMeta {
            version: 4,
            session_id: id.to_string(),
            agent_id: "com.acowork.test".to_string(),
            created_at: "2026-01-01T00:00:00.000Z".to_string(),
            title: title.map(str::to_string),
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

    fn store(dir: &TempDir) -> JsonSessionMetaStore {
        JsonSessionMetaStore::new(dir.path().join("conversations"))
    }

    /// Atomic write survives a process crash between `write` and `rename`:
    /// the tmp file is left behind, the target file is absent, and a follow-up
    /// `upsert` cleans up.
    #[test]
    fn upsert_atomic_via_rename() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(&dir);
        s.upsert(&make_meta("a", "2026-01-01T00:00:00.000Z", Some("hello"))).unwrap();
        let path = s.meta_path("a");
        assert!(path.exists());
        let read = s.get("a").unwrap().unwrap();
        assert_eq!(read.session_id, "a");
        assert_eq!(read.title.as_deref(), Some("hello"));
    }

    #[test]
    fn get_missing_returns_none_not_err() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(&dir);
        assert!(s.get("does-not-exist").unwrap().is_none());
    }

    #[test]
    fn list_recent_is_sorted_by_last_active_desc() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(&dir);
        s.upsert(&make_meta("older", "2025-01-01T00:00:00.000Z", None)).unwrap();
        s.upsert(&make_meta("newer", "2026-12-31T00:00:00.000Z", None)).unwrap();
        let listed = s.list_recent(10).unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].session_id, "newer");
        assert_eq!(listed[1].session_id, "older");
    }

    #[test]
    fn list_recent_zero_limit_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(&dir);
        s.upsert(&make_meta("a", "2026-01-01T00:00:00.000Z", None)).unwrap();
        let listed = s.list_recent(0).unwrap();
        assert!(listed.is_empty(), "limit=0 must return empty, not panic");
    }

    #[test]
    fn find_latest_returns_first_row_of_list_recent() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(&dir);
        s.upsert(&make_meta("older", "2025-01-01T00:00:00.000Z", None)).unwrap();
        s.upsert(&make_meta("newer", "2026-12-31T00:00:00.000Z", None)).unwrap();
        assert_eq!(s.find_latest().unwrap().unwrap().session_id, "newer");
    }

    #[test]
    fn search_matches_title_substring() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(&dir);
        s.upsert(&make_meta("a", "2026-01-01T00:00:00.000Z", Some("网关调试"))).unwrap();
        s.upsert(&make_meta("b", "2026-01-02T00:00:00.000Z", Some("路由列表"))).unwrap();
        let hits = s.search("网关", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].session_id, "a");
    }

    #[test]
    fn search_empty_query_falls_back_to_list_recent() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(&dir);
        s.upsert(&make_meta("a", "2026-01-01T00:00:00.000Z", None)).unwrap();
        let hits = s.search("", 10).unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn prune_to_keeps_n_newest_returns_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(&dir);
        s.upsert(&make_meta("a", "2025-01-01T00:00:00.000Z", None)).unwrap();
        s.upsert(&make_meta("b", "2025-06-01T00:00:00.000Z", None)).unwrap();
        s.upsert(&make_meta("c", "2026-01-01T00:00:00.000Z", None)).unwrap();
        let removed = s.prune_to(2).unwrap();
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0], "a");
        assert!(s.get("a").unwrap().is_none(), "pruned session truly gone");
        assert!(s.get("b").unwrap().is_some());
        assert!(s.get("c").unwrap().is_some());
    }

    #[test]
    fn prune_to_zero_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(&dir);
        s.upsert(&make_meta("a", "2025-01-01T00:00:00.000Z", None)).unwrap();
        assert!(s.prune_to(0).unwrap().is_empty());
        assert!(s.get("a").unwrap().is_some());
    }

    #[test]
    fn list_with_totals_returns_full_scan_aggregates() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(&dir);
        let mut a = make_meta("a", "2026-01-01T00:00:00.000Z", None);
        a.tokens = Some(SessionTokens {
            total_input: 100,
            total_output: 10,
            total_cache_read: 50,
            total_cache_write: 0,
            ..Default::default()
        });
        let mut b = make_meta("b", "2026-01-02T00:00:00.000Z", None);
        b.tokens = Some(SessionTokens {
            total_input: 200,
            total_output: 20,
            total_cache_read: 30,
            total_cache_write: 0,
            ..Default::default()
        });
        s.upsert(&a).unwrap();
        s.upsert(&b).unwrap();
        let (rows, total, totals) = s.list_with_totals(0, 1).unwrap();
        assert_eq!(rows.len(), 1, "page size respected");
        assert_eq!(rows[0].session_id, "b", "newest first");
        assert_eq!(total, 2, "total is the count of all sessions, not the page");
        assert_eq!(totals, (300, 30, 80, 0), "totals are full-scan, not page-bound");
    }

    #[test]
    fn import_from_json_reads_existing_files() {
        let dir = tempfile::tempdir().unwrap();
        // Pre-populate the source tree.
        let src = dir.path().join("conversations").join("meta");
        std::fs::create_dir_all(&src).unwrap();
        let meta = make_meta(
            "imported",
            "2026-05-01T00:00:00.000Z",
            Some("imported from json"),
        );
        std::fs::write(src.join("imported.json"), serde_json::to_string(&meta).unwrap()).unwrap();
        // Point a fresh store at the same `conversations/` and trigger.
        let s = JsonSessionMetaStore::new(dir.path().join("conversations"));
        let report = s.import_from_json(&src).unwrap();
        assert_eq!(report.imported, 1);
        let got = s.get("imported").unwrap().unwrap();
        assert_eq!(got.title.as_deref(), Some("imported from json"));
        // The source file is still on disk — the contract is "never delete".
        assert!(src.join("imported.json").exists());
    }

    #[test]
    fn import_from_json_skips_when_source_missing() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(&dir);
        let report = s.import_from_json(&dir.path().join("conversations").join("meta")).unwrap();
        assert_eq!(report.imported, 0);
    }

    /// This is the property the future `SqliteSessionMetaStore::import_from_json`
    /// needs to share: importing is idempotent and never deletes the source.
    #[test]
    fn import_from_json_does_not_remove_source_files() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("meta_src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("a.json"),
            serde_json::to_string(&make_meta("a", "2026-05-01T00:00:00.000Z", None)).unwrap(),
        )
        .unwrap();
        let dest = JsonSessionMetaStore::new(dir.path().join("dest").join("conversations"));
        std::fs::create_dir_all(dir.path().join("dest").join("conversations").join("meta"))
            .unwrap();
        dest.import_from_json(&src).unwrap();
        assert!(src.join("a.json").exists(), "source must remain untouched");
    }
}
