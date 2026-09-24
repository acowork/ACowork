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
/// `SqliteSessionMetaStore`).
///
/// `#[serde(default)]` keeps a row written before ADR-066 — which omits the
/// four cache fields — deserialisable; the defaults are all zero, matching the
/// "宁可 miss 也不估计" policy.
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

/// One persisted session, mapped 1:1 onto the `sessions` row.
///
/// `version` is the `conversations/{id}.jsonl` format version the row was
/// written by (the runtime's `CONVERSATION_FORMAT_VERSION`), `corrupted` is set
/// when the JSONL had to be salvaged. Both round-trip through
/// `SqliteSessionMetaStore` and are served verbatim by the `/sessions` API.
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

/// Trait every session-meta backend implements.
///
/// Method semantics:
/// - `get`: `Ok(None)` when the row is missing, `Err` only for backend failure.
/// - `upsert`: replaces the entire row. No `insert_or_update` distinction.
/// - `delete`: removes one session row (and any derived index entry). No-op
///   when the session is absent, so callers do not need a pre-check.
/// - `list_recent`: cap `limit`. Order is `last_active_at` descending, read off
///   the `last_active_at` index.
/// - `find_latest`: zero-cost shortcut for `list_recent(1).into_iter().next()`.
/// - `search`: substring query against `title` first, falling back to
///   `agent_id` / `workspace_id` (FTS5 trigram). Empty `query` returns the same
///   as `list_recent`.
/// - `prune_to`: leaves the first `max_sessions` newest rows and removes the
///   rest, returning the deleted ids.
/// - `list_with_totals`: paginated list + agent-wide token aggregates, used by
///   the `/sessions` HTTP API. The aggregates are *full-scan*, not page-bound;
///   implementations should compute them in a single pass, not by re-reading
///   the page slice.
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
}

/// Converts `last_active_at` (RFC3339 string) to the epoch-ms `i64` the
/// `last_active_at` column indexes. Returns 0 for unparseable input, which
/// sorts the row to the bottom (SQLite compares `INTEGER` numerically).
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
