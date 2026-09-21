//! User↔user chat persistence (ADR-076 §决策 8).
//!
//! One canonical directory per pair, so a conversation has exactly one
//! location and is never duplicated:
//!
//! ```text
//! {data_dir}/users/{min(a,b)}/chats/{max(a,b)}/
//!   conversation.json   participants, last_active_at, unread, version
//!   messages.jsonl      append-only messages
//!   files/{id}          attachment blob (ADR-076 §决策 9)
//!   files/{id}.json     its metadata
//! ```
//!
//! `a`/`b` are compared as byte strings; the order is arbitrary but stable,
//! and `chat_id` is the wire form `min__max`. User ids are UUIDv4
//! (ADR-076 §决策 1), so `__` can never appear inside an id.
//!
//! Attachments live under the pair directory, not per user: a file is
//! addressed by its conversation and only ever read by its two participants,
//! so a second namespace would add a lookup without adding a boundary.
//!
//! The on-disk name is an opaque id, not the uploaded filename — the ADR's
//! sketch (`files/{message_id}_{filename}`) presumes a message id that does
//! not exist yet at upload time, and splicing a user string into a path
//! invites both traversal and name collisions. Metadata is a sidecar, and
//! the blob is written **first**: a crash between the two leaves an orphan
//! blob nobody can reference, never a metadata row pointing at nothing.
//!
//! ponytail: orphan blobs are never reaped. Finite ceiling (a failed upload
//! after a successful blob write), and uploads are authenticated; add a
//! sweep keyed on "blob with no sidecar, older than N days" if a deployment
//! ever cares to reclaim the space.
//!
//! Only reachable under `AUTH_MODE=multi_user` (ADR-076 §决策 12) — nothing
//! here runs, and `data_dir/users/` is never created, in `local` mode.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Separator inside `chat_id` (`min__max`).
pub const SEP: &str = "__";

/// `last_message_preview` length, in characters (ADR-076 §决策 8).
const PREVIEW_CHARS: usize = 80;

/// Upload ceilings, in bytes (ADR-076 §决策 9: images 25 MB, documents
/// 100 MB, no virus scan).
pub const MAX_IMAGE_BYTES: u64 = 25 * 1024 * 1024;
pub const MAX_DOCUMENT_BYTES: u64 = 100 * 1024 * 1024;

/// Cap on the stored filename, in characters. Cosmetic only — the name never
/// reaches a path — but it does reach a response header.
const MAX_FILENAME_CHARS: usize = 200;

/// The size ceiling for a given mime type.
pub fn limit_for(mime: &str) -> u64 {
    if mime.starts_with("image/") {
        MAX_IMAGE_BYTES
    } else {
        MAX_DOCUMENT_BYTES
    }
}

/// One attachment, as stored in `files/{id}.json` and inside a message.
///
/// Every field is written by [`store_attachment`] and read back verbatim by
/// [`append_message`] — a client only ever sends the `id`, so it cannot claim
/// a size, a mime type, or a name it did not actually upload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Attachment {
    pub id: String,
    pub filename: String,
    pub mime: String,
    pub size: u64,
}

impl Attachment {
    /// Message `kind` implied by the payload: ADR-076 §决策 9 keeps only
    /// `image` and `document` (no voice / video / reaction — YAGNI).
    pub fn kind(&self) -> &'static str {
        if self.mime.starts_with("image/") {
            "image"
        } else {
            "document"
        }
    }
}

/// Collapse a client-supplied `Content-Type` to a bare, safe `type/subtype`.
///
/// The value is stored and later echoed in a response header, so an unvalidated
/// one would be a header-injection primitive. Anything that is not a
/// parameter-free token pair becomes `application/octet-stream`.
fn normalize_mime(raw: &str) -> String {
    fn is_token_char(c: char) -> bool {
        c.is_ascii_alphanumeric() || "!#$&^_.+-".contains(c)
    }
    let bare = raw.split(';').next().unwrap_or("").trim();
    let valid = bare.len() <= 64
        && bare
            .split_once('/')
            .map(|(t, s)| {
                !t.is_empty()
                    && !s.is_empty()
                    && t.chars().all(is_token_char)
                    && s.chars().all(is_token_char)
            })
            .unwrap_or(false);
    if valid {
        bare.to_ascii_lowercase()
    } else {
        "application/octet-stream".to_string()
    }
}

/// Last path segment, free of control characters, quotes and backslashes.
///
/// Path separators are stripped because multipart clients are allowed to send
/// them in `file_name`; the quotes and control characters because this string
/// is interpolated into `Content-Disposition`.
fn safe_filename(name: &str) -> String {
    let last = name.rsplit(['/', '\\']).next().unwrap_or("").trim();
    let cleaned: String = last
        .chars()
        .map(|c| {
            if c.is_control() || c == '"' || c == '\\' {
                '_'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim();
    let capped: String = cleaned.chars().take(MAX_FILENAME_CHARS).collect();
    if capped.is_empty() {
        "file".to_string()
    } else {
        capped
    }
}

/// RFC 6266/5987 `Content-Disposition`, carrying the real name twice: an
/// ASCII-mangled `filename=` for old clients and a percent-encoded
/// `filename*=` (UTF-8) so CJK names survive the round trip.
pub fn content_disposition(filename: &str) -> String {
    let ascii: String = filename
        .chars()
        .map(|c| {
            if c.is_ascii_graphic() && c != '"' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let mut pct = String::new();
    for byte in filename.as_bytes() {
        let c = *byte as char;
        if c.is_ascii_alphanumeric() || "!#$&+-.^_`|~".contains(c) {
            pct.push(c);
        } else {
            pct.push_str(&format!("%{byte:02X}"));
        }
    }
    format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{pct}")
}

/// One `messages.jsonl` line.
///
/// `from` is forced by the API to the caller's own id — it is never taken
/// from a request body, so a client cannot forge a sender.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatMessage {
    pub ts: i64,
    pub from: String,
    /// `"text"` / `"image"` / `"document"`, derived from the attachments.
    #[serde(default = "default_kind")]
    pub kind: String,
    pub body: String,
    /// Resolved server-side from attachment ids (ADR-076 §决策 9).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
}

fn default_kind() -> String {
    "text".to_string()
}

/// `conversation.json` — conversation metadata, rewritten on every change.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Conversation {
    pub chat_id: String,
    /// Exactly two ids, byte-sorted `[min, max]`.
    pub participants: Vec<String>,
    pub created_at: i64,
    pub last_active_at: i64,
    #[serde(default)]
    pub last_message_preview: String,
    /// `user_id -> unread count`. Keyed by id rather than by `a`/`b` so a
    /// participant's count cannot drift when the pair order is recomputed.
    #[serde(default)]
    pub unread: BTreeMap<String, u32>,
    #[serde(default)]
    pub version: u64,
}

impl Conversation {
    fn new(chat_id: String, lo: &str, hi: &str, now: i64) -> Self {
        Self {
            chat_id,
            participants: vec![lo.to_string(), hi.to_string()],
            created_at: now,
            last_active_at: now,
            last_message_preview: String::new(),
            unread: BTreeMap::new(),
            version: 1,
        }
    }

    pub fn unread_for(&self, user_id: &str) -> u32 {
        self.unread.get(user_id).copied().unwrap_or(0)
    }

    /// The other participant, as seen by `user_id`.
    pub fn peer_of(&self, user_id: &str) -> Option<&str> {
        self.participants
            .iter()
            .find(|p| p.as_str() != user_id)
            .map(String::as_str)
    }
}

// ── Naming ─────────────────────────────────────────────────────────────

/// Byte-sorted pair `(min, max)`.
pub fn ordered<'a>(a: &'a str, b: &'a str) -> (&'a str, &'a str) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

/// Canonical `chat_id` for a pair — order-independent.
pub fn chat_id(a: &str, b: &str) -> String {
    let (lo, hi) = ordered(a, b);
    format!("{lo}{SEP}{hi}")
}

/// Decode a wire `chat_id`. `None` for anything non-canonical, which callers
/// surface as 404 rather than leaking whether an id exists.
pub fn parse_chat_id(id: &str) -> Option<(String, String)> {
    let (lo, hi) = id.split_once(SEP)?;
    if lo.is_empty() || hi.is_empty() || lo >= hi {
        return None;
    }
    Some((lo.to_string(), hi.to_string()))
}

/// Directory holding one conversation.
pub fn pair_dir(data_dir: &Path, lo: &str, hi: &str) -> PathBuf {
    data_dir.join("users").join(lo).join("chats").join(hi)
}

fn dir_for(data_dir: &Path, chat_id: &str) -> Option<PathBuf> {
    let (lo, hi) = parse_chat_id(chat_id)?;
    Some(pair_dir(data_dir, &lo, &hi))
}

fn conversation_path(dir: &Path) -> PathBuf {
    dir.join("conversation.json")
}

fn messages_path(dir: &Path) -> PathBuf {
    dir.join("messages.jsonl")
}

fn files_dir(dir: &Path) -> PathBuf {
    dir.join("files")
}

/// Attachment blob. `id` is always a UUID (checked at every trust boundary),
/// so it cannot climb out of `files/`.
fn attachment_path(dir: &Path, id: &str) -> PathBuf {
    files_dir(dir).join(id)
}

fn attachment_meta_path(dir: &Path, id: &str) -> PathBuf {
    files_dir(dir).join(format!("{id}.json"))
}

// ── Reads ──────────────────────────────────────────────────────────────

/// Load one conversation by `chat_id`.
pub fn load(data_dir: &Path, chat_id: &str) -> Result<Conversation, String> {
    let dir = dir_for(data_dir, chat_id).ok_or_else(|| "invalid chat_id".to_string())?;
    let path = conversation_path(&dir);
    let raw = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&raw).map_err(|e| format!("{}: {e}", path.display()))
}

/// Every conversation `user_id` participates in.
///
/// The pair directory lives under the *smaller* id, so half of a user's
/// conversations are not under `users/{user_id}/`. This walks all pairs and
/// filters on the authoritative `participants` list.
///
/// ponytail: O(pairs) directory scan per call, fine for a single-gateway
/// fleet; if history outgrows it, maintain a per-user `chats.index` file
/// updated in `save` and read that instead.
pub fn list_for(data_dir: &Path, user_id: &str) -> Result<Vec<Conversation>, String> {
    let users = data_dir.join("users");
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(&users) else {
        return Ok(out); // no `users/` yet: no conversations
    };
    for entry in entries.flatten() {
        let chats = entry.path().join("chats");
        let Ok(peers) = fs::read_dir(&chats) else {
            continue;
        };
        for peer in peers.flatten() {
            let path = conversation_path(&peer.path());
            let Ok(raw) = fs::read_to_string(&path) else {
                continue; // peer dir without metadata: skip, never fail the list
            };
            match serde_json::from_str::<Conversation>(&raw) {
                Ok(c) if c.participants.iter().any(|p| p == user_id) => out.push(c),
                Ok(_) => {}
                Err(e) => tracing::warn!(path = %path.display(), error = %e, "skipping unreadable conversation"),
            }
        }
    }
    out.sort_by_key(|c| std::cmp::Reverse(c.last_active_at));
    Ok(out)
}

/// Read a page of messages, newest-page-first: `offset` counts *back* from
/// the tail (0 = the most recent message), matching ADR-076 §决策 8.
///
/// Returns `(page_in_chronological_order, total_messages)`.
///
/// ponytail: reads the whole JSONL to page it — fine to a few hundred
/// thousand lines; beyond that, seek from the end in fixed-size chunks.
pub fn read_messages(
    data_dir: &Path,
    chat_id: &str,
    offset: usize,
    limit: usize,
) -> Result<(Vec<ChatMessage>, usize), String> {
    let dir = dir_for(data_dir, chat_id).ok_or_else(|| "invalid chat_id".to_string())?;
    let path = messages_path(&dir);
    let file = match fs::File::open(&path) {
        Ok(f) => f,
        // A conversation created but never written has metadata only.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((Vec::new(), 0)),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };

    let mut all: Vec<ChatMessage> = Vec::new();
    for (i, line) in BufReader::new(file).lines().enumerate() {
        let line = line.map_err(|e| format!("{}: {e}", path.display()))?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<ChatMessage>(&line) {
            Ok(m) => all.push(m),
            // One corrupt line must not brick the history (ADR-076 §决策 8).
            Err(e) => tracing::warn!(path = %path.display(), line = i + 1, error = %e, "skipping corrupt message line"),
        }
    }

    let total = all.len();
    let end = total.saturating_sub(offset);
    let start = end.saturating_sub(limit.max(1));
    Ok((all[start..end].to_vec(), total))
}

// ── Attachments (ADR-076 §决策 9) ──────────────────────────────────────

/// Write `bytes` as an attachment of the pair `chat_id`, as uploaded by
/// `from`.
///
/// `from` is checked against the pair itself rather than against
/// `conversation.json`: the conversation file only appears with the first
/// message, and uploading before sending is the normal flow. `chat_id` is
/// canonical, so being one of its two halves *is* participation.
///
/// `filename` and `mime` are presented by the client; both are sanitised here
/// and the result is authoritative from this point on.
pub fn store_attachment(
    data_dir: &Path,
    chat_id: &str,
    from: &str,
    filename: &str,
    mime: &str,
    bytes: &[u8],
) -> Result<Attachment, String> {
    let (lo, hi) = parse_chat_id(chat_id).ok_or_else(|| "invalid chat_id".to_string())?;
    if from != lo && from != hi {
        return Err("not a participant".to_string());
    }
    if bytes.is_empty() {
        return Err("attachment is empty".to_string());
    }
    let mime = normalize_mime(mime);
    let limit = limit_for(&mime);
    if bytes.len() as u64 > limit {
        return Err(format!("attachment exceeds {limit} bytes"));
    }

    let attachment = Attachment {
        id: uuid::Uuid::new_v4().to_string(),
        filename: safe_filename(filename),
        mime,
        size: bytes.len() as u64,
    };

    let dir = pair_dir(data_dir, &lo, &hi);
    let fdir = files_dir(&dir);
    fs::create_dir_all(&fdir).map_err(|e| format!("{}: {e}", fdir.display()))?;

    // Blob first, metadata second — see the module doc for why.
    let blob = attachment_path(&dir, &attachment.id);
    fs::write(&blob, bytes).map_err(|e| format!("{}: {e}", blob.display()))?;

    let meta = attachment_meta_path(&dir, &attachment.id);
    let json = serde_json::to_string(&attachment).map_err(|e| format!("serialize: {e}"))?;
    fs::write(&meta, json).map_err(|e| {
        let _ = fs::remove_file(&blob); // do not leave a blob we cannot describe
        format!("{}: {e}", meta.display())
    })?;
    Ok(attachment)
}

/// Read one attachment back: metadata plus bytes.
///
/// `Ok(None)` covers both "no such attachment" and "not a well-formed id" —
/// the caller answers 404 either way, so a probe cannot tell them apart and
/// learn which attachments a conversation holds.
///
/// `id` comes off a URL path, so it is validated as a UUID before it is
/// joined into a filename — that single check is what keeps `../` out.
pub fn load_attachment(
    data_dir: &Path,
    chat_id: &str,
    id: &str,
) -> Result<Option<(Attachment, Vec<u8>)>, String> {
    let dir = dir_for(data_dir, chat_id).ok_or_else(|| "invalid chat_id".to_string())?;
    if uuid::Uuid::parse_str(id).is_err() {
        return Ok(None);
    }
    let meta = attachment_meta_path(&dir, id);
    let raw = match fs::read_to_string(&meta) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", meta.display())),
    };
    let attachment: Attachment =
        serde_json::from_str(&raw).map_err(|e| format!("{}: {e}", meta.display()))?;
    let blob = attachment_path(&dir, id);
    match fs::read(&blob) {
        Ok(bytes) => Ok(Some((attachment, bytes))),
        // Metadata without a blob: the window between the two writes, or a
        // manual delete. Either way there is nothing to serve.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", blob.display())),
    }
}

/// Whether `id` addresses an attachment of this conversation.
///
/// `append_message` resolves ids itself (so the invariant holds wherever it
/// is called from); this exists only so the API can answer a made-up id with
/// a 4xx instead of letting it read as a server fault.
pub fn attachment_exists(data_dir: &Path, chat_id: &str, id: &str) -> bool {
    dir_for(data_dir, chat_id)
        .map(|dir| uuid::Uuid::parse_str(id).is_ok() && attachment_meta_path(&dir, id).is_file())
        .unwrap_or(false)
}

/// Resolve client-supplied ids to the metadata actually on disk.
///
/// A message can only reference attachments of its own conversation, and only
/// ones that really exist — the caller sends ids, never names or sizes.
fn resolve_attachments(dir: &Path, ids: &[String]) -> Result<Vec<Attachment>, String> {
    ids.iter()
        .map(|id| {
            if uuid::Uuid::parse_str(id).is_err() {
                return Err("invalid attachment id".to_string());
            }
            let meta = attachment_meta_path(dir, id);
            let raw = fs::read_to_string(&meta).map_err(|_| "unknown attachment".to_string())?;
            serde_json::from_str(&raw).map_err(|e| format!("{}: {e}", meta.display()))
        })
        .collect()
}

// ── Writes ─────────────────────────────────────────────────────────────

/// Append a message and bump the recipient's unread count.
///
/// The conversation is created on first write. `attachment_ids` are resolved
/// against this conversation's `files/`; `body` may be empty only when there
/// is something else to show. Returns the stored message (with the
/// server-assigned `ts`).
pub fn append_message(
    data_dir: &Path,
    from: &str,
    to: &str,
    body: &str,
    attachment_ids: &[String],
    now: i64,
) -> Result<ChatMessage, String> {
    if from == to {
        return Err("cannot message yourself".to_string());
    }
    let (lo, hi) = ordered(from, to);
    let dir = pair_dir(data_dir, lo, hi);
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;

    let chat_id = format!("{lo}{SEP}{hi}");
    let mut convo = match load(data_dir, &chat_id) {
        Ok(c) => c,
        Err(_) => Conversation::new(chat_id, lo, hi, now),
    };

    let attachments = resolve_attachments(&dir, attachment_ids)?;
    let kind = attachments
        .first()
        .map(Attachment::kind)
        .unwrap_or("text")
        .to_string();

    let message = ChatMessage {
        ts: now,
        from: from.to_string(),
        kind,
        body: body.to_string(),
        attachments,
    };
    let mut line =
        serde_json::to_string(&message).map_err(|e| format!("serialize message: {e}"))?;
    line.push('\n');
    let path = messages_path(&dir);
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    file.write_all(line.as_bytes())
        .map_err(|e| format!("{}: {e}", path.display()))?;

    convo.last_active_at = now;
    convo.last_message_preview = preview(&message);
    convo.version += 1;
    *convo.unread.entry(to.to_string()).or_insert(0) += 1;
    save(&dir, &convo)?;
    Ok(message)
}

/// Clear `user_id`'s unread counter for a conversation.
pub fn mark_read(data_dir: &Path, chat_id: &str, user_id: &str) -> Result<Conversation, String> {
    let dir = dir_for(data_dir, chat_id).ok_or_else(|| "invalid chat_id".to_string())?;
    let mut convo = load(data_dir, chat_id)?;
    if !convo.participants.iter().any(|p| p == user_id) {
        return Err("not a participant".to_string());
    }
    if convo.unread_for(user_id) != 0 {
        convo.unread.insert(user_id.to_string(), 0);
        convo.version += 1;
        save(&dir, &convo)?;
    }
    Ok(convo)
}

/// What the conversation list shows for this message: the body, or the first
/// attachment's name when there is no body (an image with no caption still
/// needs a row in the list). Truncated on a char boundary so a multi-byte
/// string cannot panic.
fn preview(message: &ChatMessage) -> String {
    let source = if message.body.trim().is_empty() {
        match message.attachments.first() {
            Some(a) if a.kind() == "image" => format!("[image] {}", a.filename),
            Some(a) => format!("[file] {}", a.filename),
            None => String::new(),
        }
    } else {
        message.body.clone()
    };
    let trimmed = source.trim();
    if trimmed.chars().count() <= PREVIEW_CHARS {
        return trimmed.to_string();
    }
    let mut s: String = trimmed.chars().take(PREVIEW_CHARS).collect();
    s.push('…');
    s
}

/// tmp + rename, same durability story as the account store.
fn save(dir: &Path, convo: &Conversation) -> Result<(), String> {
    let path = conversation_path(dir);
    let json =
        serde_json::to_string_pretty(convo).map_err(|e| format!("serialize conversation: {e}"))?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, json).map_err(|e| format!("{}: {e}", tmp.display()))?;
    fs::rename(&tmp, &path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("{}: {e}", path.display())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_dir() -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "acowork-test-chat-{}-{}",
            std::process::id(),
            unique
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    const A: &str = "aaaaaaaa-0000-4000-8000-000000000001";
    const B: &str = "bbbbbbbb-0000-4000-8000-000000000002";
    /// A third account, for the cross-conversation test.
    const C: &str = "cccccccc-0000-4000-8000-000000000003";

    #[test]
    fn attachment_writes_are_confined_to_participants() {
        let dir = temp_dir();
        let id = chat_id(A, B);
        assert!(store_attachment(&dir, &id, C, "x.png", "image/png", b"hi").is_err());
        // A non-canonical pair is not a conversation at all.
        assert!(store_attachment(&dir, &format!("{B}{SEP}{A}"), A, "x", "image/png", b"hi").is_err());
        assert!(store_attachment(&dir, &id, A, "x.png", "image/png", b"").is_err());
    }

    #[test]
    fn an_attachment_cannot_be_borrowed_by_another_conversation() {
        let dir = temp_dir();
        let ab = chat_id(A, B);
        let stored = store_attachment(&dir, &ab, A, "secret.png", "image/png", b"png").unwrap();

        // Same uploader, different peer: the id resolves inside `A-B/files/`
        // only, so `A-C` must refuse it rather than link another chat's blob.
        let ac = chat_id(A, C);
        assert!(!attachment_exists(&dir, &ac, &stored.id));
        assert!(append_message(&dir, A, C, "look", std::slice::from_ref(&stored.id), 1).is_err());
        // And the conversation it does belong to accepts it.
        let msg = append_message(&dir, A, B, "look", std::slice::from_ref(&stored.id), 1).unwrap();
        assert_eq!(msg.kind, "image");
        assert_eq!(msg.attachments[0].filename, "secret.png");
    }

    #[test]
    fn stored_metadata_is_the_gateways_own_record() {
        let dir = temp_dir();
        let id = chat_id(A, B);
        // A path and a quote come from the client's `file_name`; neither may
        // survive into storage, because both end up in a response header.
        let a = store_attachment(
            &dir,
            &id,
            A,
            "../../etc/pa\"sswd.html",
            "text/html; charset=utf-8",
            b"bytes",
        )
        .unwrap();
        assert_eq!(a.filename, "pa_sswd.html");
        // Parameters are dropped; the bare type is kept because it is a valid
        // token pair. Serving it is made safe at the other end (attachment +
        // nosniff), not by refusing to remember it.
        assert_eq!(a.mime, "text/html");
        assert_eq!(a.kind(), "document");
        assert_eq!(a.size, 5);

        let (read, bytes) = load_attachment(&dir, &id, &a.id).unwrap().unwrap();
        assert_eq!(read, a);
        assert_eq!(bytes, b"bytes");
    }

    #[test]
    fn an_injectable_mime_becomes_octet_stream() {
        assert_eq!(normalize_mime("image/png\r\nX-Evil: 1"), "application/octet-stream");
        assert_eq!(normalize_mime("nope"), "application/octet-stream");
        assert_eq!(normalize_mime(""), "application/octet-stream");
        assert_eq!(normalize_mime("image/svg+xml"), "image/svg+xml");
        assert_eq!(normalize_mime("IMAGE/PNG"), "image/png");
    }

    #[test]
    fn attachment_paths_cannot_be_traversed_or_probed() {
        let dir = temp_dir();
        let id = chat_id(A, B);
        store_attachment(&dir, &id, A, "x.png", "image/png", b"png").unwrap();
        // A well-formed pair whose conversation.json exists, so the only
        // thing rejecting these is the id check itself.
        append_message(&dir, A, B, "hi", &[], 1).unwrap();
        for probe in [
            "../conversation.json",
            "..",
            "00000000-0000-4000-8000-000000000000",
            "",
        ] {
            assert!(
                load_attachment(&dir, &id, probe).unwrap().is_none(),
                "probe {probe:?} should resolve to nothing"
            );
        }
    }

    #[test]
    fn ceilings_split_on_the_mime_they_were_uploaded_as() {
        assert_eq!(limit_for("image/png"), MAX_IMAGE_BYTES);
        assert_eq!(limit_for("application/pdf"), MAX_DOCUMENT_BYTES);
        const { assert!(MAX_IMAGE_BYTES < MAX_DOCUMENT_BYTES) };

        let dir = temp_dir();
        let id = chat_id(A, B);
        let too_big = vec![0u8; MAX_IMAGE_BYTES as usize + 1];
        assert!(store_attachment(&dir, &id, A, "big.png", "image/png", &too_big).is_err());
        // The same bytes are a fine *document* — the ceiling follows the
        // declared type, which is why the type is validated first.
        assert!(store_attachment(&dir, &id, A, "big.bin", "application/zip", &too_big).is_ok());
    }

    #[test]
    fn content_disposition_survives_a_cjk_filename() {
        let d = content_disposition("设计稿 v2.png");
        // Header-safe ASCII fallback, and the real name percent-encoded.
        assert!(!d.contains('\u{8bbe}'));
        assert!(d.contains("filename*=UTF-8''%E8%AE%BE%E8%AE%A1%E7%A8%BF%20v2.png"));
        assert!(d.starts_with("attachment; filename=\""));
        // The stored name already has quotes and controls stripped, but the
        // formatter must not be the only thing standing between them and a
        // response header.
        assert!(!content_disposition("a\"b\nc").contains("a\"b"));
    }

    #[test]
    fn a_bodyless_message_previews_its_attachment() {
        let dir = temp_dir();
        let id = chat_id(A, B);
        let a = store_attachment(&dir, &id, A, "shot.png", "image/png", b"png").unwrap();
        let msg = append_message(&dir, A, B, "   ", &[a.id], 1).unwrap();
        assert_eq!(msg.body, "   ");
        let convo = load(&dir, &id).unwrap();
        assert_eq!(convo.last_message_preview, "[image] shot.png");
    }

    #[test]
    fn chat_id_is_order_independent() {
        assert_eq!(chat_id(A, B), chat_id(B, A));
        assert_eq!(chat_id(A, B), format!("{A}{SEP}{B}"));
        assert_eq!(parse_chat_id(&chat_id(B, A)), Some((A.into(), B.into())));
        // Non-canonical / malformed ids decode to `None` (callers 404).
        assert_eq!(parse_chat_id(&format!("{B}{SEP}{A}")), None);
        assert_eq!(parse_chat_id(A), None);
        assert_eq!(parse_chat_id(SEP), None);
    }

    #[test]
    fn append_then_read_round_trips_and_bumps_unread() {
        let dir = temp_dir();
        let id = chat_id(A, B);

        append_message(&dir, A, B, "hello", &[], 100).unwrap();
        append_message(&dir, B, A, "hi there", &[], 200).unwrap();

        let (page, total) = read_messages(&dir, &id, 0, 50).unwrap();
        assert_eq!(total, 2);
        assert_eq!(page.len(), 2);
        assert_eq!(page[0].from, A);
        assert_eq!(page[1].body, "hi there");
        assert_eq!(page[0].kind, "text");

        let convo = load(&dir, &id).unwrap();
        assert_eq!(convo.participants, vec![A.to_string(), B.to_string()]);
        assert_eq!(convo.unread_for(B), 1); // A's message is unread for B
        assert_eq!(convo.unread_for(A), 1); // B's reply is unread for A
        assert_eq!(convo.last_active_at, 200);
        assert_eq!(convo.last_message_preview, "hi there");

        mark_read(&dir, &id, B).unwrap();
        let convo = load(&dir, &id).unwrap();
        assert_eq!(convo.unread_for(B), 0);
        assert_eq!(convo.unread_for(A), 1); // untouched
    }

    #[test]
    fn both_participants_see_the_same_conversation() {
        let dir = temp_dir();
        append_message(&dir, A, B, "one", &[], 10).unwrap();

        for me in [A, B] {
            let list = list_for(&dir, me).unwrap();
            assert_eq!(list.len(), 1, "{me} should see one conversation");
            assert_eq!(list[0].chat_id, chat_id(A, B));
            let peer = list[0].peer_of(me).unwrap();
            assert_eq!(peer, if me == A { B } else { A });
        }
        // A third party sees nothing.
        assert!(list_for(&dir, "cccccccc-0000-4000-8000-000000000003")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn paging_walks_back_from_the_tail() {
        let dir = temp_dir();
        let id = chat_id(A, B);
        for i in 0..10 {
            append_message(&dir, A, B, &format!("m{i}"), &[], 100 + i).unwrap();
        }

        let (page, total) = read_messages(&dir, &id, 0, 3).unwrap();
        assert_eq!(total, 10);
        assert_eq!(
            page.iter().map(|m| m.body.as_str()).collect::<Vec<_>>(),
            vec!["m7", "m8", "m9"]
        );

        let (page, _) = read_messages(&dir, &id, 8, 5).unwrap();
        assert_eq!(
            page.iter().map(|m| m.body.as_str()).collect::<Vec<_>>(),
            vec!["m0", "m1"]
        );

        // Paging past the head is empty, not an error.
        let (page, _) = read_messages(&dir, &id, 99, 5).unwrap();
        assert!(page.is_empty());
    }

    #[test]
    fn corrupt_line_is_skipped_not_fatal() {
        let dir = temp_dir();
        let id = chat_id(A, B);
        append_message(&dir, A, B, "first", &[], 1).unwrap();
        let path = messages_path(&pair_dir(&dir, A, B));
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"{ not json\n").unwrap();
        drop(f);
        append_message(&dir, B, A, "third", &[], 3).unwrap();

        let (page, total) = read_messages(&dir, &id, 0, 50).unwrap();
        assert_eq!(total, 2);
        assert_eq!(page[1].body, "third");
    }

    #[test]
    fn preview_truncates_on_char_boundaries() {
        let dir = temp_dir();
        let long = "话".repeat(200);
        let msg = append_message(&dir, A, B, &long, &[], 1).unwrap();
        assert_eq!(msg.body, long); // stored in full
        let convo = load(&dir, &chat_id(A, B)).unwrap();
        assert_eq!(convo.last_message_preview.chars().count(), PREVIEW_CHARS + 1);
        assert!(convo.last_message_preview.ends_with('…'));
    }

    #[test]
    fn self_message_is_rejected() {
        let dir = temp_dir();
        assert!(append_message(&dir, A, A, "hey me", &[], 1).is_err());
        assert!(load(&dir, &chat_id(A, A)).is_err());
    }

    #[test]
    fn mark_read_requires_participation() {
        let dir = temp_dir();
        let id = chat_id(A, B);
        append_message(&dir, A, B, "hi", &[], 1).unwrap();
        assert!(mark_read(&dir, &id, "cccccccc-0000-4000-8000-000000000003").is_err());
        assert!(load(&dir, "not-a-chat-id").is_err());
    }
}
