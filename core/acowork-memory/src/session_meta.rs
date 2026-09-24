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
//! (n+1)-th most-recent **of that session's owner**, and returns the deleted
//! ids. Bucketing by owner is deliberate (ADR-076 §决策 4): a single global cap
//! lets one account's busy week evict another account's history. The matching
//! JSONL files are not this backend's concern — the caller cleans them up.
//! Keeping the side-effect split lets the trait stay pure on the storage axis.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use acowork_core::error::Result;

/// Default maximum pruned-after-pick when pruning is disabled.
pub const PRUNE_DISABLED: usize = 0;

/// `(input, output, cache_read, cache_write)` token totals across every
/// session a caller may read. Aliased so the tuple returned by the runtime's
/// session listing (`scan_sessions_async`) stays readable.
pub type SessionTotals = (u64, u64, u64, u64);


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

/// Who may read a session (ADR-076 §决策 4).
///
/// The field exists so a session can be *shared* rather than owned
/// exclusively: a [`Public`](Self::Public) session is readable by every
/// authenticated user, a [`Private`](Self::Private) one only by its owner
/// (or an administrator).
///
/// **Absent means public** — but only absent *and* ownerless.
///
/// `SessionMeta.visibility` is `Option<SessionVisibility>` with
/// `skip_serializing_if = "Option::is_none"`, so an unset visibility is
/// byte-identical on disk to a pre-ADR-076 row. That is the whole point:
/// upgrading must not retroactively hide every existing session from its
/// user, and those sessions have no owner to restrict them to anyway
/// (`is_readable_by` ignores the flag when `user_id` is absent).
///
/// A session created by an identified account is **not** left absent: the
/// creation path stamps `Private` (ADR-076 §决策 4). `None` therefore
/// means "predates accounts, or local mode" — never "a new session whose
/// creator nobody bothered to ask" — which is why this value stopped
/// being the multi-user default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionVisibility {
    /// Readable by any authenticated user.
    ///
    /// The default only where no owner exists to restrict it to; an owned
    /// session is created `Private` and must be shared deliberately.
    Public,
    /// Readable only by the owning account (and administrators).
    Private,
}

impl SessionVisibility {
    /// Whether a session with this setting is restricted to its owner.
    pub fn is_private(self) -> bool {
        matches!(self, Self::Private)
    }

    /// Stable `TEXT` encoding, used by the SQLite `sessions.visibility` column.
    ///
    /// Deliberately hand-written instead of going through `serde_json`: the
    /// column is plain `TEXT`, and `serde_json` would store the quotes too.
    /// A test pins this in lockstep with the `#[serde(rename_all = "lowercase")]`
    /// representation, so a JSON-sidecar row and a SQLite row agree.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Private => "private",
        }
    }

    /// Inverse of [`Self::as_str`].
    ///
    /// Unrecognised text returns `None`, i.e. "absent" → public. A column
    /// value this code does not know (hand-edited database, a future variant
    /// read by an older binary) must not fail the whole listing, and it must
    /// not silently mean "private" either.
    pub fn from_db_str(value: &str) -> Option<Self> {
        match value {
            "public" => Some(Self::Public),
            "private" => Some(Self::Private),
            _ => None,
        }
    }
}

/// Who is asking for a session (ADR-076 §决策 4).
///
/// Produced by the HTTP layer from the Gateway-injected `x-user-id`
/// header (see `acowork-gateway`'s auth middleware) and consumed by the
/// session read/write paths. Two variants, because "administrator" and
/// "`AUTH_MODE=local`, no accounts at all" (ADR-076 §决策 12) have
/// genuinely identical access — both are unfiltered — and splitting them
/// would mean two code paths that can never diverge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionScope {
    /// No restriction: an administrator, or a local-mode Runtime where
    /// the account system is off.
    Unfiltered,
    /// A signed-in account.
    User(String),
}

impl SessionScope {
    /// Parse the `x-user-id` header value.
    ///
    /// `None` (header absent) means local mode — the Gateway never strips
    /// or injects in that mode, so an absent header is the normal local
    /// case and must NOT be treated as "anonymous, deny everything".
    /// The all-axes sentinel `*` is the administrator's unfiltered view.
    pub fn from_header_value(value: Option<&str>) -> Self {
        match value.map(str::trim) {
            None | Some("") | Some("*") => Self::Unfiltered,
            Some(uid) => Self::User(uid.to_string()),
        }
    }

    /// The account id, or `None` for an unfiltered scope.
    pub fn user_id(&self) -> Option<&str> {
        match self {
            Self::Unfiltered => None,
            Self::User(uid) => Some(uid.as_str()),
        }
    }
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

    /// ADR-076 §决策 4: the account that owns this session.
    ///
    /// `None` = created before the account system existed, or under
    /// `AUTH_MODE=local` (no accounts at all) — only an unfiltered
    /// (administrator) reader sees those. Immutable after creation: the
    /// metadata builder never invents one, and the only writer is the
    /// creation path (see `ConversationSession::set_user_id`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    /// ADR-076 §决策 4: who may read this session. **Absent = public**
    /// (see [`SessionVisibility`]). Mutable — the owner can flip it at
    /// any time via `ConversationSession::set_visibility`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility: Option<SessionVisibility>,

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

impl SessionMeta {
    /// May `scope` **read** this session (ADR-076 §决策 4)?
    ///
    /// An unset (`None`) visibility is public: every signed-in account
    /// may read it. Only an explicit `private` restricts reading to the
    /// owning account.
    ///
    /// In practice `None` now shows up only on sessions with no owner
    /// (pre-ADR-076 data, or local mode): the creation path stamps
    /// `Private` on anything it can attribute to an account.
    pub fn is_readable_by(&self, scope: &SessionScope) -> bool {
        match scope {
            SessionScope::Unfiltered => true,
            SessionScope::User(uid) => match self.user_id.as_deref() {
                // Ownerless sessions are a two-way split, and the split is
                // the whole reason `None` and `Some(Private)` must not be
                // collapsed:
                //
                //   `None` / `Public` → readable by every account.
                //     This is pre-ADR-076 data (no owner to restrict it to)
                //     and local-mode sessions. Hiding them on upgrade would
                //     strand users outside their own history.
                //
                //   `Private` → readable by nobody but an administrator.
                //     An *unclaimed* session. The agent creates one at cold
                //     start, before any account has touched the process;
                //     handing that to every account as a shared session is
                //     how two users end up typing into the same
                //     conversation. "Private with no owner" honestly means
                //     "belongs to no one", so no account gets it.
                //
                // Note this is the *explicit* flag, set by the creation
                // path or an administrator — not something a user can
                // reach (see `is_writable_by` and `put_session_visibility`).
                None => !self.visibility.is_some_and(SessionVisibility::is_private),
                Some(owner) => {
                    !self.visibility.is_some_and(SessionVisibility::is_private) || owner == uid
                }
            },
        }
    }

    /// May `scope` **modify** this session (open / close / delete /
    /// retitle / re-share)?
    ///
    /// Deliberately stricter than [`Self::is_readable_by`]: a public
    /// session is *visible* to every account but *owned* by exactly one,
    /// so sharing is not the same as handing over the delete button.
    ///
    /// Ownerless sessions keep the pre-ADR-076 rule — modifiable by any
    /// signed-in account — for the same reason [`Self::is_readable_by`]
    /// keeps them readable: that is the behaviour data predating accounts
    /// (and local mode) depends on, and locking them to admins would
    /// strand users outside their own history.
    ///
    /// The exception is an ownerless session marked `Private`: an
    /// unclaimed session nobody may read is also one nobody may write, or
    /// the first user to touch it could flip the flag back and re-share
    /// it with everyone.
    pub fn is_writable_by(&self, scope: &SessionScope) -> bool {
        match scope {
            SessionScope::Unfiltered => true,
            SessionScope::User(uid) => match self.user_id.as_deref() {
                None => !self.visibility.is_some_and(SessionVisibility::is_private),
                Some(owner) => owner == uid,
            },
        }
    }
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
/// - `prune_to`: leaves the first `max_sessions` newest rows **per owner** and
///   removes the rest, returning the deleted ids. Bucketing by owner is what
///   keeps one account's busy week from evicting another account's history
///   (ADR-076 §决策 4).
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
