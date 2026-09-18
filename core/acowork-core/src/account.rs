//! User account types (ADR-076 §决策 1).
//!
//! `UserAccount` upgrades the presentation-only [`UserProfile`] into a
//! first-class identity: a login handle (`username`), a [`Role`], and
//! credential / lifecycle fields. The presentation fields (language,
//! timezone, avatar, …) are preserved so the same account can produce the
//! [`UserProfile`] pushed to Runtime as `last_user_profile`.
//!
//! Persistence (see ADR-076 §决策 2): the account record lives in the
//! Gateway's own store — `user_profiles.json` keeps the *public* view,
//! the credential file keeps the hash. This module is pure types (no I/O,
//! no hashing) so both Gateway and Desktop can share it.

use crate::protocol::UserProfile;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Login role (ADR-076 §决策 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Ordinary user: sees only their own sessions.
    #[default]
    User,
    /// Administrator: sees all sessions, may manage accounts.
    Admin,
}

impl Role {
    /// The canonical lowercase spelling carried in a token claim
    /// (ADR-076 §决策 3) and in the `accounts.json` record.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Admin => "admin",
        }
    }

    /// Parse the token-claim spelling. Unknown values fall back to the
    /// least-privileged role — a malformed claim must never widen access.
    pub fn from_claim(s: &str) -> Self {
        match s {
            "admin" => Self::Admin,
            _ => Self::User,
        }
    }
}

/// Sentinel `password_hash` for accounts that have not yet set a password.
///
/// Used (under `AUTH_MODE=multi_user`) for accounts an admin created but
/// whose owner has not completed first-login via `invite_token`
/// (ADR-076 §决策 6). It is never a valid PHC string, so a verify against
/// it always fails — the account is structurally un-loggable-in rather
/// than merely disabled. `AUTH_MODE=local` never creates accounts at all
/// (ADR-076 §决策 12).
pub const DISABLED_PASSWORD_HASH: &str = "$disabled$";

/// A user account (ADR-076 §决策 1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserAccount {
    // ── Identity ──
    /// Unique user identifier (UUID v4) — same semantics as
    /// `UserProfile.user_id`.
    pub user_id: String,
    /// Login handle: lowercase letters, digits, `-` and `_`.
    pub username: String,
    /// Display name — what the user wants to be called.
    pub display_name: String,
    /// Authorization role.
    #[serde(default)]
    pub role: Role,

    // ── Credentials ──
    /// Argon2id PHC string (`$argon2id$v=19$m=...,t=...,p=...$<salt>$<hash>`),
    /// or [`DISABLED_PASSWORD_HASH`] for local-mode accounts. Stored
    /// independently of the Vault so login verification works while the
    /// Vault is locked.
    pub password_hash: String,
    /// When the password was last changed (ISO 8601).
    #[serde(default)]
    pub password_changed_at: String,
    /// When the password expires (ISO 8601); `None` = never. Recorded but
    /// not enforced in this iteration (see ADR-076 §5.5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password_expires_at: Option<String>,

    // ── Presentation (mirrors `UserProfile`) ──
    pub language: String,
    pub timezone: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub city: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occupation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub builtin_avatar: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub communication_style: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub custom: HashMap<String, String>,

    // ── Lifecycle ──
    /// When the account was created (ISO 8601).
    pub created_at: String,
    /// When the account was last modified (ISO 8601).
    pub updated_at: String,
    /// When the account last logged in (ISO 8601).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_login_at: Option<String>,
    /// Soft-delete marker (ISO 8601). A disabled account cannot log in but
    /// its `user_id` and session ownership are preserved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled_at: Option<String>,
}

impl UserAccount {
    /// Whether this account may log in with a password: it has a real hash
    /// (not the [`DISABLED_PASSWORD_HASH`] sentinel) and is not disabled.
    pub fn is_login_capable(&self) -> bool {
        self.password_hash != DISABLED_PASSWORD_HASH && self.disabled_at.is_none()
    }

    /// Whether this account is an administrator.
    pub fn is_admin(&self) -> bool {
        matches!(self.role, Role::Admin)
    }

    /// Build the public [`UserProfile`] view of this account.
    ///
    /// This is the shape pushed to Runtime as `last_user_profile` — it
    /// carries no credential material. `is_active` marks whether this
    /// account is the active/online user.
    pub fn to_public_profile(&self, is_active: bool) -> UserProfile {
        UserProfile {
            user_id: self.user_id.clone(),
            display_name: self.display_name.clone(),
            language: self.language.clone(),
            timezone: self.timezone.clone(),
            city: self.city.clone(),
            country: self.country.clone(),
            occupation: self.occupation.clone(),
            avatar: self.avatar.clone(),
            builtin_avatar: self.builtin_avatar.clone(),
            communication_style: self.communication_style.clone(),
            custom: self.custom.clone(),
            created_at: self.created_at.clone(),
            updated_at: self.updated_at.clone(),
            is_active,
        }
    }
}

/// Redacted account view (ADR-076 §决策 5 / §决策 6 `/api/auth/me`).
///
/// Everything a client is allowed to see about an account: identity,
/// role and presentation fields — no credential material, no password
/// timestamps. `accounts.json` keeps the full record; this is the shape
/// that crosses the HTTP boundary (and, later, the Tauri IPC boundary).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountView {
    pub user_id: String,
    pub username: String,
    pub display_name: String,
    pub role: Role,
    pub language: String,
    pub timezone: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub city: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occupation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub builtin_avatar: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub communication_style: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub custom: HashMap<String, String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_login_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled_at: Option<String>,
}

impl From<&UserAccount> for AccountView {
    fn from(a: &UserAccount) -> Self {
        Self {
            user_id: a.user_id.clone(),
            username: a.username.clone(),
            display_name: a.display_name.clone(),
            role: a.role,
            language: a.language.clone(),
            timezone: a.timezone.clone(),
            city: a.city.clone(),
            country: a.country.clone(),
            occupation: a.occupation.clone(),
            avatar: a.avatar.clone(),
            builtin_avatar: a.builtin_avatar.clone(),
            communication_style: a.communication_style.clone(),
            custom: a.custom.clone(),
            created_at: a.created_at.clone(),
            updated_at: a.updated_at.clone(),
            last_login_at: a.last_login_at.clone(),
            disabled_at: a.disabled_at.clone(),
        }
    }
}

/// Versioned account list persisted to disk (ADR-076 §决策 2).
///
/// Follows the same pattern as [`crate::protocol::UserProfileListFile`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AccountListFile {
    /// Monotonic version counter — bumped on every mutation.
    #[serde(default)]
    pub version: u64,
    /// All accounts (including soft-deleted, `disabled_at` set).
    #[serde(default)]
    pub accounts: Vec<UserAccount>,
}

impl AccountListFile {
    /// Look up an account by `user_id`.
    pub fn find(&self, user_id: &str) -> Option<&UserAccount> {
        self.accounts.iter().find(|a| a.user_id == user_id)
    }

    /// Look up an account by `username` (case-insensitive).
    pub fn find_by_username(&self, username: &str) -> Option<&UserAccount> {
        self.accounts
            .iter()
            .find(|a| a.username.eq_ignore_ascii_case(username))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> UserAccount {
        UserAccount {
            user_id: "u-1".into(),
            username: "alice".into(),
            display_name: "Alice".into(),
            role: Role::User,
            password_hash: "$argon2id$v=19$m=65536,t=3,p=4$c2FsdA$aGFzaA".into(),
            password_changed_at: "2026-10-15T00:00:00Z".into(),
            password_expires_at: None,
            language: "zh-CN".into(),
            timezone: "Asia/Shanghai".into(),
            city: Some("上海".into()),
            country: None,
            occupation: None,
            avatar: None,
            builtin_avatar: None,
            communication_style: None,
            custom: HashMap::new(),
            created_at: "2026-10-15T00:00:00Z".into(),
            updated_at: "2026-10-15T00:00:00Z".into(),
            last_login_at: None,
            disabled_at: None,
        }
    }

    #[test]
    fn sentinel_hash_is_not_login_capable() {
        let mut a = sample();
        assert!(a.is_login_capable());
        a.password_hash = DISABLED_PASSWORD_HASH.into();
        assert!(!a.is_login_capable());
    }

    #[test]
    fn disabled_account_is_not_login_capable() {
        let mut a = sample();
        a.disabled_at = Some("2026-10-16T00:00:00Z".into());
        assert!(!a.is_login_capable());
    }

    #[test]
    fn role_serde_roundtrip() {
        let json = serde_json::to_string(&Role::Admin).unwrap();
        assert_eq!(json, "\"admin\"");
        let back: Role = serde_json::from_str("\"user\"").unwrap();
        assert_eq!(back, Role::User);
        // Missing role defaults to User (old data / local mode).
        let a: UserAccount = serde_json::from_str(
            r#"{"user_id":"x","username":"y","display_name":"Y",
                "password_hash":"$disabled$","language":"en","timezone":"UTC",
                "created_at":"t","updated_at":"t"}"#,
        )
        .unwrap();
        assert_eq!(a.role, Role::User);
        assert_eq!(a.password_changed_at, "");
    }

    #[test]
    fn public_profile_carries_no_credentials() {
        let a = sample();
        let p = a.to_public_profile(true);
        assert_eq!(p.user_id, a.user_id);
        assert_eq!(p.display_name, a.display_name);
        assert!(p.is_active);
        // Serialized profile must not leak username / role / hash.
        let json = serde_json::to_string(&p).unwrap();
        assert!(!json.contains("password"));
        assert!(!json.contains("alice"));
        assert!(!json.contains("username"));
    }

    #[test]
    fn account_view_carries_no_credentials() {
        let a = sample();
        let view = AccountView::from(&a);
        assert_eq!(view.user_id, a.user_id);
        assert_eq!(view.username, a.username);
        assert_eq!(view.role, a.role);
        // The serialized view must not leak the hash or its timestamps.
        let json = serde_json::to_string(&view).unwrap();
        assert!(!json.contains("password"), "leaked password field: {json}");
        assert!(!json.contains("argon2"), "leaked hash: {json}");
    }

    #[test]
    fn account_list_lookup() {
        let list = AccountListFile {
            version: 1,
            accounts: vec![sample()],
        };
        assert!(list.find("u-1").is_some());
        assert!(list.find("nope").is_none());
        assert!(list.find_by_username("ALICE").is_some());
    }
}
