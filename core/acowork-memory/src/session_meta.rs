//! Session metadata storage: trait + types (ADR-082 §4 step 3).
//!
//! The session list is the metadata side of `conversations/`: titles, model
//! choice, token totals, todo snapshots. Each implementation is responsible for
//! the storage and retrieval of the row keyed by `session_id`; the JSONL
//! conversation log itself is not in scope.
//!
//! `SqliteSessionMetaStore` is the only implementation. The runtime holds an
//! `Arc<dyn SessionMetaStore>` the same way it holds `Arc<dyn MemoryProvider>`,
//! so the storage backend and the call sites stay apart.
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

use std::path::Path;

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
/// - `delete`: removes one session row (and any derived index entry). No-op
///   when the session is absent, so callers do not need a pre-check.
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
    /// Remove a single session row. No-op when the session is absent.
    fn delete(&self, session_id: &str) -> Result<()>;
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

/// Helper that converts the legacy `last_active_at` ISO string to an epoch-ms
/// `i64` for SQLite indexing. Returns 0 for unparseable input (the SQLite
/// backend sorts the row to the bottom, same as the legacy JSON behaviour of
/// putting malformed rows in an unspecified order).
pub fn last_active_at_ms(meta: &SessionMeta) -> i64 {
    DateTime::parse_from_rfc3339(&meta.last_active_at)
        .map(|dt| dt.with_timezone(&Utc).timestamp_millis())
        .unwrap_or(0)
}

// `Send + Sync` sanity: the trait is used behind `Arc<dyn _>` across threads.
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Box<dyn SessionMetaStore>>();
};
