//! `user_profiles.json` — the derived public view of user profiles.
//!
//! This service is the **only writer** (ADR-084 §决策 4b): the Gateway reads
//! the same file (later, a pulled snapshot) to feed `last_user_profile` to
//! Runtime (ADR-042), and never writes it. Keeping a single writer is what
//! makes the file safe to read without coordination.
//!
//! Carried over from the Gateway's `resource_cache.rs`; the store shape,
//! version-bump semantics and the empty-on-corrupt fallback are unchanged.

use std::path::{Path, PathBuf};

use acowork_core::protocol::UserProfileListFile;

use crate::state::SharedState;

/// `{data_dir}/user_profiles.json`.
pub fn user_profile_list_path(data_dir: &Path) -> PathBuf {
    data_dir.join("user_profiles.json")
}

/// Read the profile list, falling back to an empty list.
///
/// A corrupt file must not take the service down: the file is a *derived*
/// view, so the next account mutation rebuilds it from `accounts.json` — the
/// authority. `version: 0` also stops a stale list from looking newer than
/// the rebuild.
pub fn load_user_profile_list(data_dir: &Path) -> UserProfileListFile {
    let path = user_profile_list_path(data_dir);
    let empty = || UserProfileListFile {
        version: 0,
        users: Vec::new(),
    };
    match std::fs::read_to_string(&path) {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_else(|e| {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "failed to parse user_profiles.json, using empty list"
            );
            empty()
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::info!("user_profiles.json not found, initializing empty");
            empty()
        }
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "failed to read user_profiles.json");
            empty()
        }
    }
}

/// Persist the profile list.
pub fn save_user_profile_list(data_dir: &Path, list: &UserProfileListFile) -> Result<(), String> {
    let json = serde_json::to_string_pretty(list)
        .map_err(|e| format!("failed to serialize user profile list: {e}"))?;
    std::fs::write(user_profile_list_path(data_dir), json)
        .map_err(|e| format!("failed to write user_profiles.json: {e}"))?;
    tracing::info!(
        version = list.version,
        count = list.users.len(),
        "user profile list saved"
    );
    Ok(())
}

/// Bump the in-memory version, then persist it.
///
/// Callers mutate `resource_cache.user_profile_list.users` first; this keeps
/// the "version changes on every mutation" invariant (ADR-059 §7.3) in one
/// place so no handler can forget it.
pub fn rebuild_and_save_user_profile_cache(state: &mut SharedState) {
    let list = &mut state.resource_cache.user_profile_list;
    list.version = list.version.wrapping_add(1);
    let snapshot = list.clone();
    if let Err(e) = save_user_profile_list(&state.data_dir, &snapshot) {
        // The in-memory list is already updated and served; only durability
        // is lost, and the next mutation rewrites the whole file.
        tracing::error!(error = %e, "failed to save user_profiles.json after profile change");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acowork_core::protocol::UserProfile;

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("acowork-user-{tag}-{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn profile(id: &str) -> UserProfile {
        UserProfile {
            user_id: id.to_string(),
            display_name: id.to_string(),
            language: "en-US".into(),
            timezone: "UTC".into(),
            city: None,
            country: None,
            occupation: None,
            avatar: None,
            builtin_avatar: None,
            communication_style: None,
            custom: Default::default(),
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
            is_active: true,
        }
    }

    #[test]
    fn missing_file_loads_empty_not_error() {
        let dir = temp_dir("profiles-missing");
        let list = load_user_profile_list(&dir);
        assert_eq!(list.version, 0);
        assert!(list.users.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A corrupt file must not be fatal — it is a derived view and the next
    /// account mutation rebuilds it.
    #[test]
    fn corrupt_file_falls_back_to_empty() {
        let dir = temp_dir("profiles-corrupt");
        std::fs::write(user_profile_list_path(&dir), b"{ not json").unwrap();
        let list = load_user_profile_list(&dir);
        assert_eq!(list.version, 0);
        assert!(list.users.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_then_load_roundtrips_and_bumps_version() {
        let dir = temp_dir("profiles-roundtrip");
        let mut state = SharedState::new(dir.clone(), false);
        state.resource_cache.user_profile_list.users.push(profile("u-1"));

        rebuild_and_save_user_profile_cache(&mut state);
        assert_eq!(state.resource_cache.user_profile_list.version, 1);

        let on_disk = load_user_profile_list(&dir);
        assert_eq!(on_disk.version, 1);
        assert_eq!(on_disk.users.len(), 1);
        assert_eq!(on_disk.users[0].user_id, "u-1");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
