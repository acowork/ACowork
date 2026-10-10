//! Revoked refresh-token families (ADR-076 §决策 3, §5.2).
//!
//! Three entry scopes share one flat file. Each line is `{key}\t{ts}`,
//! where `ts` is a unix-seconds stamp whose meaning depends on the scope:
//! - `r:{family}\t{rotated_at}` — the family was **rotated** (spent by a
//!   successful refresh). Presenting it again is either a replay of a
//!   stolen token or a client race; `AuthService::refresh` only treats it
//!   as a leak once the replay lands outside the grace window (RFC 9700
//!   §4.14.2 reuse detection).
//! - `x:{family}\t{revoked_at}` — the family was **explicitly revoked**
//!   (logout). A replay is just a dead session; `revoked_at` is recorded
//!   for auditing only, revocation itself is absolute.
//! - `{user_id}.*\t{cutoff}` — every family **issued at or before**
//!   `cutoff` is dead (password change, disable, reset, reuse detection).
//!   Time-bounded on purpose: an unbounded "all families forever" entry
//!   also locks out the *next* login, so a single misjudged replay becomes
//!   a permanent, unrecoverable account lock — the user cannot get back in
//!   without hand-editing this file. Scoping the entry to `iat` keeps the
//!   blast radius to the sessions that actually existed at revoke time.
//!
//! The two scopes must stay distinct: a logged-out token being replayed
//! must not nuke the user's *other* sessions, while a rotated token being
//! replayed (outside the grace window) must.
//!
//! A family id is minted as `{user_id}.{random}` at login, so the
//! wildcard prefix is a plain string match.
//!
//! Lines written before timestamps existed carry no `\t{ts}`. They inherit
//! the file's mtime, which is the closest available bound and is always
//! **after** the entry was created — so a legacy wildcard revokes the
//! sessions that existed when it was written, and self-heals for anything
//! minted since. A legacy `r:` entry reads as ancient, so replays against
//! it stay strict (no accidental grace window for old data).
//!
//! ponytail: the file grows without bound and is never GC'd. Fine for the
//! small-team scale this ADR targets (< 100 users, one entry per refresh);
//! past that, move to a SQLite table with an expiry column.
//!
//! ponytail: revocation resolution is one second, so `revoke_user` cannot
//! tell a same-second session apart from an earlier one. It fails closed —
//! a login landing in the same second as a password change is revoked with
//! the rest and needs one more login. The upgrade path is millisecond
//! stamps, not a looser comparison.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Suffix marking a per-user wildcard entry.
const USER_WILDCARD: &str = ".*";
/// Prefix marking a family spent by rotation.
const ROTATED: &str = "r:";
/// Prefix marking a family revoked outright.
const REVOKED: &str = "x:";

pub struct RevokedFamilies {
    path: PathBuf,
    /// Entry key -> unix-seconds stamp. See the module docs for what the
    /// stamp means per scope.
    entries: HashMap<String, i64>,
}

/// Separator between an entry key and its timestamp.
const SEP: char = '\t';

impl RevokedFamilies {
    /// Load the registry, tolerating a missing file (nothing revoked yet).
    pub fn load(path: &Path) -> Self {
        // Pre-timestamp entries carry no stamp; the file's own mtime is the
        // tightest bound available for them.
        let legacy_ts = std::fs::metadata(path)
            .and_then(|m| m.modified())
            .map(|t| {
                t.duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0)
            })
            .unwrap_or(0);

        let entries = match std::fs::read_to_string(path) {
            Ok(raw) => raw
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .filter_map(|l| {
                    let (key, stamp) = match l.split_once(SEP) {
                        Some((k, v)) => (k.trim().to_string(), v.trim().parse::<i64>().ok()),
                        None => (l.to_string(), None),
                    };
                    if key.is_empty() {
                        return None;
                    }
                    Some((key, stamp.unwrap_or(legacy_ts)))
                })
                .collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "Failed to read revoked_families; treating as empty"
                );
                HashMap::new()
            }
        };
        Self {
            path: path.to_path_buf(),
            entries,
        }
    }

    /// Path of the revocation file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether `family` (belonging to `user_id`) is dead, given that it was
    /// minted at `issued_at`.
    ///
    /// `issued_at` is the refresh token's `iat`. It only narrows the
    /// wildcard scope: an exact `r:`/`x:` hit is absolute regardless of
    /// when the family was issued.
    pub fn is_revoked(&self, user_id: &str, family: &str, issued_at: i64) -> bool {
        self.entries.contains_key(family)
            || self.entries.contains_key(&format!("{ROTATED}{family}"))
            || self.entries.contains_key(&format!("{REVOKED}{family}"))
            || self
                .entries
                .get(&format!("{user_id}{USER_WILDCARD}"))
                .is_some_and(|cutoff| issued_at <= *cutoff)
    }

    /// When this family was spent by rotation, if at all (reuse signal).
    pub fn rotated_at(&self, family: &str) -> Option<i64> {
        self.entries.get(&format!("{ROTATED}{family}")).copied()
    }

    /// Whether this family was already spent by a refresh (reuse signal).
    pub fn is_rotated(&self, family: &str) -> bool {
        self.rotated_at(family).is_some()
    }

    /// Record a family as spent by rotation (single-use refresh token).
    ///
    /// Last write wins, unlike the revocation markers: each rotation of a
    /// family issues a successor in that *same* family, so the marker has to
    /// track the most recent spend. Freezing it at the first rotation would
    /// judge every later duplicate against a 13-minute-old timestamp and
    /// revoke a chain that is behaving perfectly normally — the exact false
    /// positive this grace window exists to prevent.
    pub fn mark_rotated(&mut self, family: &str, now: i64) -> Result<(), String> {
        let key = format!("{ROTATED}{family}");
        if self.entries.get(&key).is_some_and(|t| *t >= now) {
            return Ok(());
        }
        self.entries.insert(key, now);
        self.persist()
    }

    /// Revoke a single family outright (logout on one device).
    pub fn revoke_family(&mut self, family: &str, now: i64) -> Result<(), String> {
        self.insert_if_absent(format!("{REVOKED}{family}"), now)
    }

    /// Revoke every family this user held **as of `now`** (password change /
    /// disable / reset / reuse detection). Sessions minted afterwards are
    /// untouched — see the module docs on why the entry is time-bounded.
    pub fn revoke_user(&mut self, user_id: &str, now: i64) -> Result<(), String> {
        let key = format!("{user_id}{USER_WILDCARD}");
        // Repeated revocations widen the cutoff rather than shrink it, so a
        // second revoke can never resurrect a session the first one killed.
        if self.entries.get(&key).is_some_and(|c| *c >= now) {
            return Ok(());
        }
        self.entries.insert(key, now);
        self.persist()
    }

    fn insert_if_absent(&mut self, key: String, now: i64) -> Result<(), String> {
        // First write wins: a later revoke must not shrink an earlier cutoff.
        if let std::collections::hash_map::Entry::Vacant(e) = self.entries.entry(key) {
            e.insert(now);
            self.persist()?;
        }
        Ok(())
    }

    fn persist(&self) -> Result<(), String> {
        let mut lines: Vec<String> = self
            .entries
            .iter()
            .map(|(k, ts)| format!("{k}{SEP}{ts}"))
            .collect();
        lines.sort();
        let body: String = lines.iter().map(|s| format!("{s}\n")).collect();
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create auth dir: {e}"))?;
        }
        // Atomic write: a torn revocation file would lose entries and
        // silently let a revoked family back in. Temp sibling + rename.
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, &body)
            .map_err(|e| format!("failed to write revocation tmp: {e}"))?;
        std::fs::rename(&tmp, &self.path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("failed to commit revocation file: {e}")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("acowork-revoked-{tag}-{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("revoked_families.txt")
    }

    /// A stamp comfortably inside "now" for tests that mint sessions.
    const T0: i64 = 1_700_000_000;

    #[test]
    fn missing_file_is_empty() {
        let r = RevokedFamilies::load(&tmp_path("missing"));
        assert!(!r.is_revoked("u-1", "u-1.abc", T0));
    }

    #[test]
    fn exact_family_revocation() {
        let p = tmp_path("exact");
        let mut r = RevokedFamilies::load(&p);
        r.revoke_family("u-1.abc", T0).unwrap();

        assert!(r.is_revoked("u-1", "u-1.abc", T0));
        assert!(!r.is_revoked("u-1", "u-1.def", T0));
        assert!(!r.is_revoked("u-2", "u-2.abc", T0));

        // Persisted across reload.
        let reloaded = RevokedFamilies::load(&p);
        assert!(reloaded.is_revoked("u-1", "u-1.abc", T0));
    }

    #[test]
    fn user_wildcard_revokes_all_families() {
        let p = tmp_path("wildcard");
        let mut r = RevokedFamilies::load(&p);
        r.revoke_user("u-1", T0).unwrap();

        assert!(r.is_revoked("u-1", "u-1.abc", T0));
        assert!(r.is_revoked("u-1", "u-1.def", T0));
        // A different user is untouched, even with a similar id.
        assert!(!r.is_revoked("u-10", "u-10.abc", T0));
        assert!(!r.is_revoked("u-2", "u-2.abc", T0));
    }

    #[test]
    fn user_wildcard_spares_sessions_minted_after_it() {
        // The whole point of the cutoff: a revocation must not lock out the
        // *next* login, or one misjudged replay is a permanent account lock.
        let p = tmp_path("wildcard-forward");
        let mut r = RevokedFamilies::load(&p);
        r.revoke_user("u-1", T0).unwrap();

        assert!(r.is_revoked("u-1", "u-1.old", T0 - 1));
        assert!(!r.is_revoked("u-1", "u-1.new", T0 + 1));

        // Survives the file round-trip.
        let reloaded = RevokedFamilies::load(&p);
        assert!(reloaded.is_revoked("u-1", "u-1.old", T0 - 1));
        assert!(!reloaded.is_revoked("u-1", "u-1.new", T0 + 1));
    }

    #[test]
    fn repeated_revocation_widens_the_cutoff() {
        // A second revoke must never shrink the first one's reach.
        let p = tmp_path("wildcard-widen");
        let mut r = RevokedFamilies::load(&p);
        r.revoke_user("u-1", T0 + 100).unwrap();
        r.revoke_user("u-1", T0).unwrap();

        assert!(r.is_revoked("u-1", "u-1.mid", T0 + 50));
    }

    #[test]
    fn rotation_and_explicit_revocation_are_distinguishable() {
        let p = tmp_path("rotate");
        let mut r = RevokedFamilies::load(&p);
        r.mark_rotated("u-1.spent", T0).unwrap();
        r.revoke_family("u-1.loggedout", T0).unwrap();

        // Both block the token...
        assert!(r.is_revoked("u-1", "u-1.spent", T0));
        assert!(r.is_revoked("u-1", "u-1.loggedout", T0));
        // ...but only rotation is a reuse signal, and it carries its stamp.
        assert_eq!(r.rotated_at("u-1.spent"), Some(T0));
        assert_eq!(r.rotated_at("u-1.loggedout"), None);
        assert!(r.is_rotated("u-1.spent"));
        assert!(!r.is_rotated("u-1.loggedout"));
        // An untouched family is neither.
        assert!(!r.is_revoked("u-1", "u-1.fresh", T0));
        assert!(!r.is_rotated("u-1.fresh"));
    }

    #[test]
    fn rotation_stamp_tracks_the_latest_spend() {
        // Each rotation issues a successor in the SAME family, so the marker
        // must move forward. Freezing it at the first spend would judge every
        // later duplicate against a stale stamp and revoke a chain that is
        // behaving normally — the false positive the grace window exists to
        // prevent.
        //
        // This cannot widen the window forever: the grace is 10s and a
        // legitimate rotation is ~13 minutes apart, so an attacker replaying
        // an old token still lands outside it and trips the revocation.
        let p = tmp_path("stamp-advances");
        let mut r = RevokedFamilies::load(&p);
        r.mark_rotated("u-1.abc", T0).unwrap();
        r.mark_rotated("u-1.abc", T0 + 780).unwrap();
        assert_eq!(r.rotated_at("u-1.abc"), Some(T0 + 780));

        // A backwards stamp (clock skew / replayed write) must not regress it.
        r.mark_rotated("u-1.abc", T0).unwrap();
        assert_eq!(r.rotated_at("u-1.abc"), Some(T0 + 780));
    }

    #[test]
    fn legacy_entries_without_a_stamp_are_tolerated() {
        // Pre-timestamp format: bare key, no '\t'. A legacy wildcard must
        // still revoke what existed when it was written (fail closed on
        // `issued_at <= cutoff` with the file mtime as the cutoff) but must
        // not lock out a session minted after it.
        let p = tmp_path("legacy");
        std::fs::write(&p, "u-1.*\nr:u-1.spent\n").unwrap();

        let r = RevokedFamilies::load(&p);
        assert!(r.is_rotated("u-1.spent"));
        // Legacy stamp = file mtime ≈ now, so an old session is covered...
        assert!(r.is_revoked("u-1", "u-1.anything", 1));
        // ...and one minted in the future is not.
        assert!(!r.is_revoked("u-1", "u-1.anything", i64::MAX));
    }

    #[test]
    fn malformed_lines_are_skipped() {
        let p = tmp_path("malformed");
        std::fs::write(&p, "# comment\n\n  \nu-1.abc\tnot-a-number\n").unwrap();
        let r = RevokedFamilies::load(&p);
        // The unparseable stamp falls back to the legacy bound, but the
        // entry itself still revokes — a bad stamp must not resurrect a token.
        assert!(r.is_revoked("u-1", "u-1.abc", 1));
    }

    #[test]
    fn idempotent_revoke_does_not_grow() {
        let p = tmp_path("idem");
        let mut r = RevokedFamilies::load(&p);
        r.revoke_family("u-1.abc", T0).unwrap();
        r.revoke_family("u-1.abc", T0).unwrap();
        let body = std::fs::read_to_string(&p).unwrap();
        assert_eq!(body.lines().count(), 1);
        // Atomic write: no temp sibling left behind.
        assert!(!p.with_extension("tmp").exists());
    }
}
