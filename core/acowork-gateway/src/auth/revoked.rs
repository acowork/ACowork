//! Revoked refresh-token families (ADR-076 §决策 3, §5.2).
//!
//! Two revocation scopes share one flat file:
//! - an **exact** family id — revokes one device's refresh chain (logout);
//! - a **`{user_id}.*` wildcard** — revokes every family for a user
//!   (password change, account disable).
//!
//! A family id is minted as `{user_id}.{random}` at login, so the wildcard
//! prefix is a plain string match.
//!
//! ponytail: the file grows without bound and is never GC'd. Fine for the
//! small-team scale this ADR targets (< 100 users); past that, move to a
//! SQLite table with an expiry column.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Suffix marking a per-user wildcard entry.
const USER_WILDCARD: &str = ".*";

pub struct RevokedFamilies {
    path: PathBuf,
    entries: HashSet<String>,
}

impl RevokedFamilies {
    /// Load the registry, tolerating a missing file (nothing revoked yet).
    pub fn load(path: &Path) -> Self {
        let entries = match std::fs::read_to_string(path) {
            Ok(raw) => raw
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(str::to_string)
                .collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashSet::new(),
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "Failed to read revoked_families; treating as empty"
                );
                HashSet::new()
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

    /// Whether `family` (belonging to `user_id`) has been revoked.
    pub fn is_revoked(&self, user_id: &str, family: &str) -> bool {
        self.entries.contains(family) || self.entries.contains(&format!("{user_id}{USER_WILDCARD}"))
    }

    /// Revoke a single family (logout on one device).
    pub fn revoke_family(&mut self, family: &str) -> Result<(), String> {
        self.insert(family.to_string())
    }

    /// Revoke every family for a user (password change / disable).
    pub fn revoke_user(&mut self, user_id: &str) -> Result<(), String> {
        self.insert(format!("{user_id}{USER_WILDCARD}"))
    }

    fn insert(&mut self, entry: String) -> Result<(), String> {
        if self.entries.insert(entry) {
            self.persist()?;
        }
        Ok(())
    }

    fn persist(&self) -> Result<(), String> {
        let mut sorted: Vec<&String> = self.entries.iter().collect();
        sorted.sort();
        let body: String = sorted.iter().map(|s| format!("{s}\n")).collect();
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

    #[test]
    fn missing_file_is_empty() {
        let r = RevokedFamilies::load(&tmp_path("missing"));
        assert!(!r.is_revoked("u-1", "u-1.abc"));
    }

    #[test]
    fn exact_family_revocation() {
        let p = tmp_path("exact");
        let mut r = RevokedFamilies::load(&p);
        r.revoke_family("u-1.abc").unwrap();

        assert!(r.is_revoked("u-1", "u-1.abc"));
        assert!(!r.is_revoked("u-1", "u-1.def"));
        assert!(!r.is_revoked("u-2", "u-2.abc"));

        // Persisted across reload.
        let reloaded = RevokedFamilies::load(&p);
        assert!(reloaded.is_revoked("u-1", "u-1.abc"));
    }

    #[test]
    fn user_wildcard_revokes_all_families() {
        let p = tmp_path("wildcard");
        let mut r = RevokedFamilies::load(&p);
        r.revoke_user("u-1").unwrap();

        assert!(r.is_revoked("u-1", "u-1.abc"));
        assert!(r.is_revoked("u-1", "u-1.def"));
        // A different user is untouched, even with a similar id.
        assert!(!r.is_revoked("u-10", "u-10.abc"));
        assert!(!r.is_revoked("u-2", "u-2.abc"));
    }

    #[test]
    fn idempotent_revoke_does_not_grow() {
        let p = tmp_path("idem");
        let mut r = RevokedFamilies::load(&p);
        r.revoke_family("u-1.abc").unwrap();
        r.revoke_family("u-1.abc").unwrap();
        let body = std::fs::read_to_string(&p).unwrap();
        assert_eq!(body.lines().count(), 1);
        // Atomic write: no temp sibling left behind.
        assert!(!p.with_extension("tmp").exists());
    }
}
