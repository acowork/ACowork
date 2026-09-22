//! SQLite schema for the memory store (ADR-082 D1).
//!
//! One `nodes` row per memory node. `props` is the JSON serialization of the
//! node struct (every type-specific field — see the crate docs for the exact
//! contract); `status` / `created_at` / `updated_at` are projected columns so
//! filtering and ordering never have to parse JSON. Embeddings live in
//! `vectors` as little-endian `f32` BLOBs. Each memory label gets its own FTS5
//! table using the `trigram` tokenizer (ADR-082 D3) so CJK text matches as
//! substrings instead of collapsing into one token under `unicode61`.
//!
//! The DDL is idempotent: opening an existing database re-runs it harmlessly.

use acowork_memory::labels;

/// DDL applied on every [`crate::SqliteStore::open`]. `IF NOT EXISTS` throughout.
pub const SCHEMA_SQL: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;
PRAGMA synchronous = NORMAL;

CREATE TABLE IF NOT EXISTS nodes (
    id         INTEGER PRIMARY KEY,
    label      TEXT NOT NULL,
    status     TEXT NOT NULL DEFAULT 'Active',
    props      JSON NOT NULL,
    created_at TEXT NOT NULL DEFAULT '',
    updated_at TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_nodes_label ON nodes(label);

-- Store-level metadata (currently just the embedding dimension). Keeping it in
-- the database means a reopen reports the dimension actually stored rather
-- than whatever the caller guessed.
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS vectors (
    node_id   INTEGER PRIMARY KEY REFERENCES nodes(id) ON DELETE CASCADE,
    dim       INTEGER NOT NULL,
    embedding BLOB NOT NULL
);

CREATE VIRTUAL TABLE IF NOT EXISTS fts_episodic USING fts5(content, node_id UNINDEXED, tokenize = 'trigram');
CREATE VIRTUAL TABLE IF NOT EXISTS fts_knowledge USING fts5(content, node_id UNINDEXED, tokenize = 'trigram');
CREATE VIRTUAL TABLE IF NOT EXISTS fts_procedural USING fts5(content, node_id UNINDEXED, tokenize = 'trigram');
CREATE VIRTUAL TABLE IF NOT EXISTS fts_autobiographical USING fts5(content, node_id UNINDEXED, tokenize = 'trigram');

-- Conversation index (ADR-082 §4 step 2). Separate from the memory labels: it
-- is a different store file driven by a different subsystem (ADR-081), but it
-- reuses the same `nodes` / `vectors` / FTS machinery so `/search` gets the
-- same trigram CJK matching and exact vector scan as memory recall.
CREATE VIRTUAL TABLE IF NOT EXISTS fts_conversation USING fts5(content, node_id UNINDEXED, tokenize = 'trigram');

-- Forgetting archive (ADR-082 D1, replacing the grafeo PurgeLog).
-- Decay is the one path that can destroy memory data, so an expired node is
-- copied here *before* deletion instead of being dropped outright. Keeping the
-- whole row (props + text + embedding) makes recovery a re-insert.
CREATE TABLE IF NOT EXISTS purge_log (
    id        INTEGER PRIMARY KEY,
    node_id   INTEGER NOT NULL,
    label     TEXT NOT NULL,
    props     JSON NOT NULL,
    content   TEXT NOT NULL DEFAULT '',
    embedding BLOB,
    reason    TEXT NOT NULL DEFAULT '',
    purged_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_purge_log_purged_at ON purge_log(purged_at);
"#;

/// Map a node label to its FTS5 table name.
///
/// Returns `None` for labels without a text index (e.g. `Session`,
/// `ToolInvocation`) — those never reach the memory retrieval paths.
pub(crate) fn fts_table(label: &str) -> Option<&'static str> {
    if label == labels::EPISODIC {
        Some("fts_episodic")
    } else if label == labels::KNOWLEDGE {
        Some("fts_knowledge")
    } else if label == labels::PROCEDURAL {
        Some("fts_procedural")
    } else if label == labels::AUTOBIOGRAPHICAL {
        Some("fts_autobiographical")
    } else if label == crate::conversation::LABEL {
        Some("fts_conversation")
    } else {
        None
    }
}
