//! Account authentication service (ADR-076 §决策 3, §决策 6).
//!
//! Ties together the pieces that only make sense as a whole at login time:
//! the account store ([`crate::account::store`]), Argon2id verification
//! ([`crate::account::password`]), HS256 token minting ([`super::token`])
//! and refresh-family revocation ([`super::revoked`]).
//!
//! The signing secret and the revocation registry are loaded once at boot
//! and held here; the account list is read from disk per operation (it is
//! the authority, and a stale in-memory copy would let a revoked account
//! log in).
//!
//! **Only constructed under `AUTH_MODE=multi_user`** (ADR-076 §决策 12) —
//! in `local` mode nothing in this module runs and `accounts.json` is
//! never created.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

use acowork_core::account::{AccountListFile, DISABLED_PASSWORD_HASH, Role, UserAccount};
use serde::{Deserialize, Serialize};

use crate::account::{password, store};
use crate::auth::revoked::RevokedFamilies;
use crate::auth::token::{ACCESS_TTL_SECS, Claims, TokenError, TokenKind, TokenSigner};

/// Password policy (ADR-076 §决策 6, `[multi_user].password_policy`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PasswordPolicy {
    pub min_length: usize,
    pub require_digit: bool,
    pub require_mixed_case: bool,
}

impl Default for PasswordPolicy {
    fn default() -> Self {
        Self {
            min_length: 8,
            require_digit: true,
            require_mixed_case: false,
        }
    }
}

impl PasswordPolicy {
    /// Check a candidate password. `Err` carries a user-facing reason.
    pub fn validate(&self, password: &str) -> Result<(), String> {
        if password.chars().count() < self.min_length {
            return Err(format!(
                "password must be at least {} characters",
                self.min_length
            ));
        }
        if self.require_digit && !password.chars().any(|c| c.is_ascii_digit()) {
            return Err("password must contain at least one digit".into());
        }
        if self.require_mixed_case
            && !(password.chars().any(|c| c.is_ascii_lowercase())
                && password.chars().any(|c| c.is_ascii_uppercase()))
        {
            return Err("password must mix upper and lower case".into());
        }
        Ok(())
    }
}

/// Bootstrap administrator from `[multi_user].bootstrap_admin`
/// (ADR-076 §决策 5).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BootstrapAdmin {
    pub username: String,
    pub password: String,
    #[serde(default)]
    pub display_name: Option<String>,
}

/// A freshly minted token pair (ADR-076 §决策 3).
#[derive(Debug, Clone, Serialize)]
pub struct TokenPair {
    pub access_token: String,
    pub refresh_token: String,
    pub token_type: &'static str,
    pub expires_in: i64,
}

/// The authenticated identity extracted from an access token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthPrincipal {
    pub user_id: String,
    pub role: Role,
}

impl AuthPrincipal {
    pub fn is_admin(&self) -> bool {
        matches!(self.role, Role::Admin)
    }
}

/// Why an authentication operation failed. The HTTP layer maps these to
/// status codes; the variants deliberately do not distinguish
/// "unknown user" from "wrong password" (see [`AuthService::login`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    /// Bad credentials, unknown account, or a disabled / unactivated
    /// account — always one indistinguishable outcome.
    InvalidCredentials,
    /// The token itself is unusable (bad signature / expired / wrong kind).
    Token(TokenError),
    /// The refresh family was revoked (logout, password change, replay).
    Revoked,
    /// The new password violates the policy.
    Policy(String),
    /// Storage failure — never the user's fault.
    Store(String),
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidCredentials => f.write_str("invalid credentials"),
            Self::Token(e) => write!(f, "{e}"),
            Self::Revoked => f.write_str("token revoked"),
            Self::Policy(m) => f.write_str(m),
            Self::Store(m) => write!(f, "account store error: {m}"),
        }
    }
}

impl std::error::Error for AuthError {}

/// The account system's runtime handle (ADR-076 §决策 3).
pub struct AuthService {
    data_dir: PathBuf,
    pub(crate) signer: TokenSigner,
    revoked: Mutex<RevokedFamilies>,
    policy: PasswordPolicy,
    bootstrap_admin: Option<BootstrapAdmin>,
}

impl AuthService {
    /// Build the service: load (or mint) the signing secret, load the
    /// revocation registry. `accounts.json` is *not* touched here — see
    /// [`Self::ensure_bootstrap_admin`].
    pub fn new(
        data_dir: &Path,
        policy: PasswordPolicy,
        bootstrap_admin: Option<BootstrapAdmin>,
    ) -> Result<Self, String> {
        let auth_dir = data_dir.join("auth");
        std::fs::create_dir_all(&auth_dir)
            .map_err(|e| format!("failed to create {}: {e}", auth_dir.display()))?;
        Ok(Self {
            data_dir: data_dir.to_path_buf(),
            signer: TokenSigner::load_or_generate(&auth_dir.join("secret"))?,
            revoked: Mutex::new(RevokedFamilies::load(&auth_dir.join("revoked_families.txt"))),
            policy,
            bootstrap_admin,
        })
    }

    pub fn policy(&self) -> &PasswordPolicy {
        &self.policy
    }

    /// The lock is never held across a panic-prone section; a poisoned
    /// lock cannot be "reset" meaningfully, so recover the inner value
    /// rather than propagate the panic into every later request.
    fn revoked(&self) -> MutexGuard<'_, RevokedFamilies> {
        self.revoked.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn load_accounts(&self) -> Result<AccountListFile, String> {
        store::load_accounts(&self.data_dir)
    }

    pub fn save_accounts(&self, list: &AccountListFile) -> Result<(), String> {
        store::save_accounts(&self.data_dir, list)
    }

    /// Look up one account's public metadata.
    pub fn account(&self, user_id: &str) -> Result<UserAccount, AuthError> {
        self.load_accounts()
            .map_err(AuthError::Store)?
            .find(user_id)
            .cloned()
            .ok_or(AuthError::InvalidCredentials)
    }

    /// Verify credentials and mint a token pair (ADR-076 §决策 6).
    ///
    /// Unknown user, wrong password, `$disabled$` sentinel and a soft-deleted
    /// account all return [`AuthError::InvalidCredentials`] — the caller must
    /// not be able to tell "no such user" from "wrong password".
    pub fn login(&self, username: &str, password: &str, now: i64) -> Result<TokenPair, AuthError> {
        let mut list = self.load_accounts().map_err(AuthError::Store)?;
        let stored = list
            .find_by_username(username)
            .map(|a| a.password_hash.clone());
        let login_capable = list
            .find_by_username(username)
            .map(|a| a.is_login_capable())
            .unwrap_or(false);

        // Always spend one Argon2 verification, even when there is nothing
        // to verify — otherwise the response time separates
        // "user exists with a password" from "does not".
        let hash = match &stored {
            Some(h) if h != DISABLED_PASSWORD_HASH => h.as_str(),
            _ => timing_decoy(),
        };
        let ok = password::verify_password(password, hash).map_err(AuthError::Store)?;
        if !ok || !login_capable {
            return Err(AuthError::InvalidCredentials);
        }

        // Record the login. A failure here must not block an otherwise
        // valid login, so it is logged, not propagated.
        if let Some(acc) = list
            .accounts
            .iter_mut()
            .find(|a| a.username.eq_ignore_ascii_case(username))
        {
            acc.last_login_at = Some(iso(now));
            list.version += 1;
            let user_id = acc.user_id.clone();
            let role = acc.role;
            if let Err(e) = self.save_accounts(&list) {
                tracing::warn!(error = %e, "failed to record last_login_at");
            }
            return Ok(self.mint(&user_id, role, now));
        }
        Err(AuthError::InvalidCredentials)
    }

    /// Exchange a refresh token for a fresh pair (ADR-076 §决策 3).
    ///
    /// Refresh tokens are **single-use** (RFC 9700 §4.14.2 rotation with
    /// reuse detection): a successful refresh retires the presented family
    /// and mints a new one. Presenting an already-spent family is the leak
    /// signal — the token was stolen, or the client raced itself — and the
    /// response is to **kill every family for the user**, so a thief who
    /// refreshed first cannot outlive the victim's next attempt.
    pub fn refresh(&self, refresh_token: &str, now: i64) -> Result<TokenPair, AuthError> {
        let claims = self
            .signer
            .verify_kind(refresh_token, TokenKind::Refresh, now)
            .map_err(AuthError::Token)?;
        let family = claims.family.clone().ok_or(AuthError::Revoked)?;

        if self.revoked().is_rotated(&family) {
            tracing::warn!(
                user_id = %claims.sub,
                "refresh token reuse detected — revoking every family for this user"
            );
            self.revoked()
                .revoke_user(&claims.sub)
                .map_err(AuthError::Store)?;
            return Err(AuthError::Revoked);
        }
        if self.revoked().is_revoked(&claims.sub, &family) {
            return Err(AuthError::Revoked);
        }

        // The account may have been disabled or deleted since the token
        // was issued (15-minute access tokens expire, refresh tokens do
        // not) — refresh is the enforcement point for that.
        let list = self.load_accounts().map_err(AuthError::Store)?;
        let account = list.find(&claims.sub).ok_or(AuthError::InvalidCredentials)?;
        if !account.is_login_capable() {
            return Err(AuthError::InvalidCredentials);
        }

        self.revoked()
            .mark_rotated(&family)
            .map_err(AuthError::Store)?;
        Ok(self.mint(&account.user_id, account.role, now))
    }

    /// Revoke the family behind `refresh_token` (ADR-076 §决策 6).
    ///
    /// Idempotent by construction: an already-expired or unparseable token
    /// is a no-op success, so logout never fails on a dead session.
    pub fn logout(&self, refresh_token: &str, now: i64) -> Result<(), AuthError> {
        match self
            .signer
            .verify_kind(refresh_token, TokenKind::Refresh, now)
        {
            Ok(claims) => {
                if let Some(family) = claims.family {
                    self.revoked()
                        .revoke_family(&family)
                        .map_err(AuthError::Store)?;
                }
                Ok(())
            }
            Err(TokenError::Expired) | Err(TokenError::Malformed) => Ok(()),
            Err(TokenError::WrongKind) | Err(TokenError::BadSignature) => {
                Err(AuthError::Token(TokenError::BadSignature))
            }
        }
    }

    /// Change a password, killing every refresh family for the user
    /// (ADR-076 §决策 6: "改密成功后撤销该 user 的所有 refresh_token").
    pub fn change_password(
        &self,
        user_id: &str,
        old_password: &str,
        new_password: &str,
        now: i64,
    ) -> Result<(), AuthError> {
        let mut list = self.load_accounts().map_err(AuthError::Store)?;
        let idx = list
            .accounts
            .iter()
            .position(|a| a.user_id == user_id)
            .ok_or(AuthError::InvalidCredentials)?;

        let stored = list.accounts[idx].password_hash.clone();
        let ok = password::verify_password(old_password, &stored).map_err(AuthError::Store)?;
        if !ok {
            return Err(AuthError::InvalidCredentials);
        }

        self.policy.validate(new_password).map_err(AuthError::Policy)?;
        let hash = password::hash_password(new_password).map_err(AuthError::Store)?;

        let stamp = iso(now);
        let acc = &mut list.accounts[idx];
        acc.password_hash = hash;
        acc.password_changed_at = stamp.clone();
        acc.updated_at = stamp;
        list.version += 1;
        self.save_accounts(&list).map_err(AuthError::Store)?;

        self.revoked()
            .revoke_user(user_id)
            .map_err(AuthError::Store)
    }

    /// Verify an access token (the middleware path, ADR-076 §决策 3).
    ///
    /// Deliberately **stateless**: signature + expiry only, no account
    /// read. That is the point of a 15-minute access token — a token
    /// stays usable for at most [`ACCESS_TTL_SECS`] after the account is
    /// disabled, and [`Self::refresh`] is the enforcement point that
    /// actually consults the store. A per-request disk read here would
    /// add a file parse to every proxied request for a ceiling of 15
    /// minutes.
    ///
    /// ponytail: bounded staleness of `role` / `disabled_at` == access
    /// token TTL. If that ever needs to be instant, add an in-memory
    /// `user_id → revoked_at` set consulted here (no disk I/O).
    pub fn verify_access(&self, token: &str, now: i64) -> Result<AuthPrincipal, AuthError> {
        let claims: Claims = self
            .signer
            .verify_kind(token, TokenKind::Access, now)
            .map_err(AuthError::Token)?;
        Ok(AuthPrincipal {
            user_id: claims.sub,
            role: Role::from_claim(claims.role.as_deref().unwrap_or("user")),
        })
    }

    /// Mint an access + refresh pair, starting a fresh refresh family.
    fn mint(&self, user_id: &str, role: Role, now: i64) -> TokenPair {
        let family = format!("{user_id}.{}", random_hex16());
        TokenPair {
            access_token: self.signer.sign_access(user_id, role.as_str(), now),
            refresh_token: self.signer.sign_refresh(user_id, &family, now),
            token_type: "Bearer",
            expires_in: ACCESS_TTL_SECS,
        }
    }

    /// Create the first administrator when the account store is empty
    /// (ADR-076 §决策 5 / §决策 12).
    ///
    /// *Empty store, no `bootstrap_admin`* → `Err`: a `multi_user` Gateway
    /// with no accounts has no way in, so the boot must fail loudly
    /// (ADR-076 §决策 12 "缺则拒启动").
    ///
    /// *Non-empty store* → `bootstrap_admin` is ignored. Keeping the
    /// bootstrap password alive as a second permanent admin credential
    /// would be a standing vulnerability; once created, the account is
    /// governed by the normal change-password flow. (This is also what
    /// Gitea / Jenkins / GitLab do — the bootstrap credential is
    /// first-boot-only.)
    pub fn ensure_bootstrap_admin(&self) -> Result<(), String> {
        let mut list = self.load_accounts()?;
        if !list.accounts.is_empty() {
            if self.bootstrap_admin.is_some() {
                tracing::warn!(
                    "AUTH_MODE=multi_user: [multi_user].bootstrap_admin is configured but the \
                     account store is non-empty — ignoring it (bootstrap is first-boot-only)"
                );
            }
            return Ok(());
        }

        let cfg = self.bootstrap_admin.as_ref().ok_or_else(|| {
            "AUTH_MODE=multi_user requires [multi_user].bootstrap_admin in gateway.toml when the \
             account store is empty — refusing to start with no way to log in"
                .to_string()
        })?;
        self.policy.validate(&cfg.password).map_err(|e| {
            format!("[multi_user].bootstrap_admin.password violates the password policy: {e}")
        })?;

        let now = crate::auth::token::now_unix();
        let stamp = iso(now);
        let account = UserAccount {
            user_id: uuid::Uuid::new_v4().to_string(),
            username: cfg.username.to_ascii_lowercase(),
            display_name: cfg
                .display_name
                .clone()
                .unwrap_or_else(|| cfg.username.clone()),
            role: Role::Admin,
            password_hash: password::hash_password(&cfg.password)?,
            password_changed_at: stamp.clone(),
            password_expires_at: None,
            language: "zh-CN".into(),
            timezone: "Asia/Shanghai".into(),
            city: None,
            country: None,
            occupation: None,
            avatar: None,
            builtin_avatar: None,
            communication_style: None,
            custom: Default::default(),
            created_at: stamp.clone(),
            updated_at: stamp,
            last_login_at: None,
            disabled_at: None,
        };
        tracing::info!(username = %account.username, "created bootstrap administrator");
        list.accounts.push(account);
        list.version += 1;
        self.save_accounts(&list)
    }
}

/// RFC 3339 timestamp for `now` (unix seconds).
fn iso(now: i64) -> String {
    chrono::DateTime::from_timestamp(now, 0)
        .map(|d| d.to_rfc3339())
        .unwrap_or_default()
}

/// 16 random bytes as lowercase hex — refresh-family entropy.
fn random_hex16() -> String {
    let mut bytes = [0u8; 16];
    rand::fill(&mut bytes[..]);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A hash of a throwaway random password, computed once, verified against
/// when there is no real hash to check (unknown user, `$disabled$`
/// sentinel). See [`AuthService::login`].
fn timing_decoy() -> &'static str {
    static DECOY: OnceLock<String> = OnceLock::new();
    DECOY.get_or_init(|| {
        password::hash_password(&random_hex16())
            .unwrap_or_else(|_| DISABLED_PASSWORD_HASH.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use argon2::Params;

    /// Weak Argon2 params — the real ones cost ~100 ms per call and these
    /// tests hash a dozen times.
    fn weak() -> Params {
        Params::new(8, 1, 1, Some(32)).unwrap()
    }

    fn tmp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("acowork-authsvc-{tag}-{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn service_with_user(tag: &str, username: &str, pwd: &str) -> (AuthService, String) {
        let dir = tmp_dir(tag);
        let svc = AuthService::new(&dir, PasswordPolicy::default(), None).unwrap();
        let now = 1_700_000_000;
        let stamp = iso(now);
        let account = UserAccount {
            user_id: format!("u-{username}"),
            username: username.into(),
            display_name: username.into(),
            role: Role::User,
            password_hash: password::hash_password_with(pwd, weak()).unwrap(),
            password_changed_at: stamp.clone(),
            password_expires_at: None,
            language: "en".into(),
            timezone: "UTC".into(),
            city: None,
            country: None,
            occupation: None,
            avatar: None,
            builtin_avatar: None,
            communication_style: None,
            custom: Default::default(),
            created_at: stamp.clone(),
            updated_at: stamp,
            last_login_at: None,
            disabled_at: None,
        };
        let list = AccountListFile {
            version: 1,
            accounts: vec![account],
        };
        svc.save_accounts(&list).unwrap();
        (svc, format!("u-{username}"))
    }

    #[test]
    fn login_mints_usable_pair_and_stamps_last_login() {
        let (svc, uid) = service_with_user("login-ok", "alice", "s3cret123");
        let now = 1_700_000_000;
        let pair = svc.login("alice", "s3cret123", now).unwrap();

        let principal = svc.verify_access(&pair.access_token, now).unwrap();
        assert_eq!(principal.user_id, uid);
        assert_eq!(principal.role, Role::User);

        let stored = svc.account(&uid).unwrap();
        assert!(stored.last_login_at.is_some());
    }

    #[test]
    fn login_username_is_case_insensitive_and_wrong_password_fails() {
        let (svc, _) = service_with_user("login-case", "alice", "s3cret123");
        assert!(svc.login("ALICE", "s3cret123", 0).is_ok());
        assert_eq!(
            svc.login("alice", "nope", 0).unwrap_err(),
            AuthError::InvalidCredentials
        );
        assert_eq!(
            svc.login("nobody", "s3cret123", 0).unwrap_err(),
            AuthError::InvalidCredentials
        );
    }

    #[test]
    fn disabled_or_unset_password_cannot_log_in() {
        let (svc, uid) = service_with_user("login-disabled", "bob", "s3cret123");
        let mut list = svc.load_accounts().unwrap();
        list.accounts[0].password_hash = DISABLED_PASSWORD_HASH.into();
        svc.save_accounts(&list).unwrap();
        assert_eq!(
            svc.login("bob", "s3cret123", 0).unwrap_err(),
            AuthError::InvalidCredentials
        );

        // Guard the other branch too: a disabled_at stamp alone must block.
        let mut list = svc.load_accounts().unwrap();
        list.accounts[0].password_hash = password::hash_password_with("s3cret123", weak()).unwrap();
        list.accounts[0].disabled_at = Some("2026-01-01T00:00:00Z".into());
        svc.save_accounts(&list).unwrap();
        assert!(!svc.account(&uid).unwrap().is_login_capable());
        assert_eq!(
            svc.login("bob", "s3cret123", 0).unwrap_err(),
            AuthError::InvalidCredentials
        );
    }

    #[test]
    fn refresh_rotates_and_reuse_kills_the_whole_chain() {
        let (svc, uid) = service_with_user("refresh-rot", "carol", "s3cret123");
        let now = 1_700_000_000;
        let first = svc.login("carol", "s3cret123", now).unwrap();

        let second = svc.refresh(&first.refresh_token, now + 1).unwrap();
        assert_ne!(first.refresh_token, second.refresh_token);
        assert_eq!(svc.verify_access(&second.access_token, now + 1).unwrap().user_id, uid);

        // Replaying the spent token is the leak signal: it kills every
        // family for the user, including the live descendant.
        assert_eq!(
            svc.refresh(&first.refresh_token, now + 2).unwrap_err(),
            AuthError::Revoked
        );
        assert_eq!(
            svc.refresh(&second.refresh_token, now + 3).unwrap_err(),
            AuthError::Revoked
        );
    }

    #[test]
    fn a_rotated_token_is_distinct_from_a_logged_out_one() {
        // Logging out on one device must NOT kill the user's other
        // devices — only reuse detection is allowed to do that.
        let (svc, _) = service_with_user("logout-scope", "nina", "s3cret123");
        let now = 1_700_000_000;
        let phone = svc.login("nina", "s3cret123", now).unwrap();
        let laptop = svc.login("nina", "s3cret123", now).unwrap();

        svc.logout(&phone.refresh_token, now + 1).unwrap();
        assert_eq!(
            svc.refresh(&phone.refresh_token, now + 2).unwrap_err(),
            AuthError::Revoked
        );
        // The laptop session is untouched.
        assert!(svc.refresh(&laptop.refresh_token, now + 2).is_ok());
    }

    #[test]
    fn expired_access_token_is_rejected() {
        let (svc, _) = service_with_user("access-exp", "dave", "s3cret123");
        let pair = svc.login("dave", "s3cret123", 1_000).unwrap();
        assert_eq!(
            svc.verify_access(&pair.access_token, 1_000 + ACCESS_TTL_SECS + 1)
                .unwrap_err(),
            AuthError::Token(TokenError::Expired)
        );
    }

    #[test]
    fn access_token_is_not_accepted_as_refresh_and_vice_versa() {
        let (svc, _) = service_with_user("kind-mix", "erin", "s3cret123");
        let pair = svc.login("erin", "s3cret123", 1_000).unwrap();
        assert_eq!(
            svc.refresh(&pair.access_token, 1_001).unwrap_err(),
            AuthError::Token(TokenError::WrongKind)
        );
        assert_eq!(
            svc.verify_access(&pair.refresh_token, 1_001).unwrap_err(),
            AuthError::Token(TokenError::WrongKind)
        );
    }

    #[test]
    fn logout_revokes_the_family_and_is_idempotent() {
        let (svc, _) = service_with_user("logout", "frank", "s3cret123");
        let pair = svc.login("frank", "s3cret123", 1_000).unwrap();
        svc.logout(&pair.refresh_token, 1_001).unwrap();
        assert_eq!(
            svc.refresh(&pair.refresh_token, 1_002).unwrap_err(),
            AuthError::Revoked
        );
        // Second logout on the same (now dead) token still succeeds.
        svc.logout(&pair.refresh_token, 1_003).unwrap();
        // Garbage is a no-op, not a crash.
        svc.logout("not-a-token", 1_004).unwrap();
    }

    #[test]
    fn change_password_requires_old_and_kills_all_families() {
        let (svc, uid) = service_with_user("chpwd", "grace", "s3cret123");
        let now = 1_000;
        let pair = svc.login("grace", "s3cret123", now).unwrap();

        assert_eq!(
            svc.change_password(&uid, "wrong-old", "newpass123", now)
                .unwrap_err(),
            AuthError::InvalidCredentials
        );
        assert_eq!(
            svc.change_password(&uid, "s3cret123", "short1", now)
                .unwrap_err(),
            AuthError::Policy("password must be at least 8 characters".into())
        );
        svc.change_password(&uid, "s3cret123", "newpass123", now)
            .unwrap();

        // Old password stops working, new one works.
        assert!(svc.login("grace", "s3cret123", now).is_err());
        assert!(svc.login("grace", "newpass123", now).is_ok());
        // Every refresh family from before the change is dead.
        assert_eq!(
            svc.refresh(&pair.refresh_token, now + 1).unwrap_err(),
            AuthError::Revoked
        );
    }

    #[test]
    fn tampered_access_token_is_rejected() {
        let (svc, _) = service_with_user("tamper", "heidi", "s3cret123");
        let pair = svc.login("heidi", "s3cret123", 1_000).unwrap();
        let mut chars: Vec<char> = pair.access_token.chars().collect();
        let last = chars.len() - 1;
        chars[last] = if chars[last] == 'A' { 'B' } else { 'A' };
        let tampered: String = chars.into_iter().collect();
        assert!(matches!(
            svc.verify_access(&tampered, 1_001),
            Err(AuthError::Token(TokenError::BadSignature | TokenError::Malformed))
        ));
    }

    #[test]
    fn bootstrap_requires_config_when_store_is_empty() {
        let dir = tmp_dir("boot-missing");
        let svc = AuthService::new(&dir, PasswordPolicy::default(), None).unwrap();
        let err = svc.ensure_bootstrap_admin().unwrap_err();
        assert!(err.contains("bootstrap_admin"), "unexpected: {err}");
    }

    #[test]
    fn bootstrap_creates_admin_once_then_is_ignored() {
        let dir = tmp_dir("boot-once");
        let cfg = BootstrapAdmin {
            username: "root".into(),
            password: "rootpass1".into(),
            display_name: Some("Root".into()),
        };
        let svc = AuthService::new(&dir, PasswordPolicy::default(), Some(cfg)).unwrap();
        svc.ensure_bootstrap_admin().unwrap();

        let list = svc.load_accounts().unwrap();
        assert_eq!(list.accounts.len(), 1);
        assert_eq!(list.accounts[0].role, Role::Admin);
        assert_eq!(list.accounts[0].username, "root");

        // Second boot: a changed bootstrap password must NOT resurrect or
        // overwrite the live admin account.
        let cfg2 = BootstrapAdmin {
            username: "root".into(),
            password: "different1".into(),
            display_name: None,
        };
        let svc2 = AuthService::new(&dir, PasswordPolicy::default(), Some(cfg2)).unwrap();
        svc2.ensure_bootstrap_admin().unwrap();
        assert_eq!(svc2.load_accounts().unwrap().accounts.len(), 1);
        assert!(svc2.login("root", "different1", 0).is_err());
        assert!(svc2.login("root", "rootpass1", 0).is_ok());
    }

    #[test]
    fn bootstrap_rejects_a_password_that_violates_policy() {
        let dir = tmp_dir("boot-policy");
        let cfg = BootstrapAdmin {
            username: "root".into(),
            password: "short".into(),
            display_name: None,
        };
        let svc = AuthService::new(&dir, PasswordPolicy::default(), Some(cfg)).unwrap();
        let err = svc.ensure_bootstrap_admin().unwrap_err();
        assert!(err.contains("password policy"), "unexpected: {err}");
    }

    #[test]
    fn password_policy_checks() {
        let p = PasswordPolicy::default();
        assert!(p.validate("abcd1234").is_ok());
        assert!(p.validate("abcdefgh").is_err()); // no digit
        let mixed = PasswordPolicy {
            require_mixed_case: true,
            ..Default::default()
        };
        assert!(mixed.validate("abcd1234").is_err());
        assert!(mixed.validate("Abcd1234").is_ok());
    }

    #[test]
    fn signing_secret_survives_a_restart() {
        let dir = tmp_dir("secret-persist");
        let svc = AuthService::new(&dir, PasswordPolicy::default(), None).unwrap();
        let pair = svc.mint("u-1", Role::Admin, 1_000);

        // A fresh service on the same data_dir reads the persisted secret,
        // so a token minted before the restart still verifies.
        let restarted = AuthService::new(&dir, PasswordPolicy::default(), None).unwrap();
        let principal = restarted.verify_access(&pair.access_token, 1_001).unwrap();
        assert_eq!(principal.user_id, "u-1");
        assert!(principal.is_admin());
    }
}
