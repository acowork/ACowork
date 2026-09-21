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

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

use acowork_core::account::{AccountListFile, DISABLED_PASSWORD_HASH, Role, UserAccount};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::account::{password, store};
use crate::auth::revoked::RevokedFamilies;
use crate::auth::token::{ACCESS_TTL_SECS, Claims, TokenError, TokenKind, TokenSigner};

/// Invite-token lifetime (ADR-076 §决策 6: "一次性 invite_token（24h 过期）").
pub const INVITE_TTL_SECS: i64 = 24 * 3600;

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

/// Display-field patch applied by [`AuthService::update_account`]
/// (ADR-076 §决策 1: the presentation fields live on `UserAccount`, so
/// they are edited through the same authority as the credentials — never
/// through the derived `user_profiles.json` view, which `sync_profiles`
/// rebuilds from `accounts.json` and would clobber).
///
/// `None` = leave the field unchanged. `avatar` / `builtin_avatar` keep
/// the existing wire contract: an empty string clears the field.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ProfilePatch {
    pub display_name: Option<String>,
    pub language: Option<String>,
    pub timezone: Option<String>,
    pub city: Option<String>,
    pub country: Option<String>,
    pub occupation: Option<String>,
    pub avatar: Option<String>,
    pub builtin_avatar: Option<String>,
    pub communication_style: Option<String>,
    pub custom: Option<HashMap<String, String>>,
}

impl ProfilePatch {
    /// Apply this patch to an account. `display_name` keeps the legacy
    /// contract: trimmed, and an all-whitespace value is ignored rather
    /// than blanking the name.
    fn apply_to(self, account: &mut UserAccount) {
        if let Some(name) = self.display_name {
            let name = name.trim();
            if !name.is_empty() {
                account.display_name = name.to_string();
            }
        }
        if let Some(v) = self.language {
            account.language = v;
        }
        if let Some(v) = self.timezone {
            account.timezone = v;
        }
        if let Some(v) = self.city {
            account.city = Some(v);
        }
        if let Some(v) = self.country {
            account.country = Some(v);
        }
        if let Some(v) = self.occupation {
            account.occupation = Some(v);
        }
        if let Some(v) = self.avatar {
            account.avatar = (!v.is_empty()).then_some(v);
        }
        if let Some(v) = self.builtin_avatar {
            account.builtin_avatar = (!v.is_empty()).then_some(v);
        }
        if let Some(v) = self.communication_style {
            account.communication_style = Some(v);
        }
        if let Some(v) = self.custom {
            account.custom = v;
        }
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
    /// A uniqueness / state precondition failed (duplicate username, last
    /// admin, already-disabled). Distinct from `Policy` because it maps to
    /// 409, not 422.
    Conflict(String),
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
            Self::Conflict(m) => f.write_str(m),
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
            revoked: Mutex::new(RevokedFamilies::load(
                &auth_dir.join("revoked_families.txt"),
            )),
            policy,
            bootstrap_admin,
        })
    }

    pub fn policy(&self) -> &PasswordPolicy {
        &self.policy
    }

    /// Root of the gateway data directory. Account-private stores (chat
    /// history, ADR-076 §决策 8) live here, beside `auth/`.
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
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
        let account = list
            .find(&claims.sub)
            .ok_or(AuthError::InvalidCredentials)?;
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

        self.policy
            .validate(new_password)
            .map_err(AuthError::Policy)?;
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
            invite_token_hash: None,
            invite_expires_at: None,
        };
        tracing::info!(username = %account.username, "created bootstrap administrator");
        list.accounts.push(account);
        list.version += 1;
        self.save_accounts(&list)
    }

    // ── Account CRUD (ADR-076 §决策 5 / §决策 6, `account_api.rs`) ──
    //
    // These are the *authoritative* account mutations. The presentation
    // cache (`user_profiles.json`, the `last_user_profile` source) is
    // derived from this store by the HTTP layer, which owns the resource
    // cache; this service only owns `accounts.json`.

    /// Create an account (ADR-076 §决策 6 `POST /api/users`).
    ///
    /// `password` is optional: `None` mints an [`invite_token`] instead and
    /// stores [`DISABLED_PASSWORD_HASH`], so the owner must complete
    /// first-login to activate. The returned tuple is `(account, invite)`
    /// where `invite` is `Some` only in the passwordless case.
    ///
    /// The username is lowercased and must be unique (case-insensitive) —
    /// duplicates are a 409, not a silent second account.
    pub fn create_account(
        &self,
        username: &str,
        display_name: &str,
        password: Option<&str>,
        role: Role,
        now: i64,
    ) -> Result<(UserAccount, Option<String>), AuthError> {
        let username = username.trim().to_ascii_lowercase();
        if username.is_empty() || !username.bytes().all(is_username_byte) {
            return Err(AuthError::Policy(
                "username must be lowercase letters, digits, '-' or '_'".into(),
            ));
        }

        let mut list = self.load_accounts().map_err(AuthError::Store)?;
        if list.find_by_username(&username).is_some() {
            return Err(AuthError::Conflict(format!(
                "username '{username}' already exists"
            )));
        }

        let stamp = iso(now);
        let (password_hash, invite) = match password {
            Some(p) => {
                self.policy.validate(p).map_err(AuthError::Policy)?;
                (password::hash_password(p).map_err(AuthError::Store)?, None)
            }
            None => (DISABLED_PASSWORD_HASH.to_string(), Some(self.new_invite())),
        };

        let mut account = UserAccount {
            user_id: uuid::Uuid::new_v4().to_string(),
            username,
            display_name: if display_name.trim().is_empty() {
                "User".into()
            } else {
                display_name.trim().to_string()
            },
            role,
            password_hash,
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
            custom: HashMap::new(),
            created_at: stamp.clone(),
            updated_at: stamp,
            last_login_at: None,
            disabled_at: None,
            invite_token_hash: None,
            invite_expires_at: None,
        };
        if let Some((_, hash)) = &invite {
            account.invite_token_hash = Some(hash.clone());
            account.invite_expires_at = Some(iso(now + INVITE_TTL_SECS));
        }

        list.accounts.push(account.clone());
        list.version += 1;
        self.save_accounts(&list).map_err(AuthError::Store)?;
        Ok((account, invite.map(|(t, _)| t)))
    }

    /// Update an account's editable fields (ADR-076 §决策 6).
    ///
    /// `None` leaves a field unchanged. `role` is handled by
    /// [`Self::set_role`] because demoting the last admin must be refused
    /// as a whole-store invariant, not a per-field edit.
    pub fn update_account(
        &self,
        user_id: &str,
        patch: ProfilePatch,
        now: i64,
    ) -> Result<UserAccount, AuthError> {
        let mut list = self.load_accounts().map_err(AuthError::Store)?;
        let idx = self.index_of(&list, user_id)?;
        patch.apply_to(&mut list.accounts[idx]);
        list.accounts[idx].updated_at = iso(now);
        list.version += 1;
        let account = list.accounts[idx].clone();
        self.save_accounts(&list).map_err(AuthError::Store)?;
        Ok(account)
    }

    /// Change an account's role, refusing to remove the last administrator.
    pub fn set_role(&self, user_id: &str, role: Role, now: i64) -> Result<UserAccount, AuthError> {
        let mut list = self.load_accounts().map_err(AuthError::Store)?;
        let idx = self.index_of(&list, user_id)?;
        if list.accounts[idx].role == Role::Admin
            && role != Role::Admin
            && self.admin_count(&list) <= 1
        {
            return Err(AuthError::Conflict(
                "cannot demote the last administrator".into(),
            ));
        }
        list.accounts[idx].role = role;
        list.accounts[idx].updated_at = iso(now);
        list.version += 1;
        let account = list.accounts[idx].clone();
        self.save_accounts(&list).map_err(AuthError::Store)?;
        Ok(account)
    }

    /// Soft-delete an account (ADR-076 §决策 6). `disabled_at = now`; every
    /// refresh family is revoked so no live session outlives the deletion.
    ///
    /// Idempotent: disabling an already-disabled account is a no-op success
    /// (the operator's intent is satisfied either way). Refuses to disable
    /// the last administrator — that would lock everyone out.
    pub fn disable_account(&self, user_id: &str, now: i64) -> Result<(), AuthError> {
        let mut list = self.load_accounts().map_err(AuthError::Store)?;
        let idx = self.index_of(&list, user_id)?;
        if list.accounts[idx].disabled_at.is_some() {
            return Ok(());
        }
        if list.accounts[idx].role == Role::Admin && self.admin_count(&list) <= 1 {
            return Err(AuthError::Conflict(
                "cannot disable the last administrator".into(),
            ));
        }
        list.accounts[idx].disabled_at = Some(iso(now));
        list.accounts[idx].updated_at = iso(now);
        list.version += 1;
        self.save_accounts(&list).map_err(AuthError::Store)?;
        self.revoked()
            .revoke_user(user_id)
            .map_err(AuthError::Store)
    }

    /// Mint a fresh single-use `invite_token` for an account and clear its
    /// password (ADR-076 §决策 6 `POST /api/users/{id}/reset-password`).
    ///
    /// The account becomes [`DISABLED_PASSWORD_HASH`] — the reset flow *is*
    /// first-login. Every refresh family is revoked so the previous holder
    /// is logged out immediately.
    pub fn reset_password(&self, user_id: &str, now: i64) -> Result<String, AuthError> {
        let mut list = self.load_accounts().map_err(AuthError::Store)?;
        let idx = self.index_of(&list, user_id)?;
        let (token, hash) = self.new_invite();
        list.accounts[idx].password_hash = DISABLED_PASSWORD_HASH.to_string();
        list.accounts[idx].invite_token_hash = Some(hash);
        list.accounts[idx].invite_expires_at = Some(iso(now + INVITE_TTL_SECS));
        list.accounts[idx].updated_at = iso(now);
        list.version += 1;
        self.save_accounts(&list).map_err(AuthError::Store)?;
        self.revoked()
            .revoke_user(user_id)
            .map_err(AuthError::Store)?;
        Ok(token)
    }

    /// Complete first-login: consume an `invite_token` and set the initial
    /// password (ADR-076 §决策 6 `POST /api/auth/first-login`).
    ///
    /// Single-use — the invite is burned (hash cleared) on success. An
    /// unknown, expired or already-spent token is one indistinguishable
    /// [`AuthError::InvalidCredentials`], so the endpoint never enumerates
    /// accounts. Returns a token pair (the caller is now logged in).
    pub fn first_login(
        &self,
        invite_token: &str,
        new_password: &str,
        now: i64,
    ) -> Result<TokenPair, AuthError> {
        let hash = invite_hash(invite_token);
        let mut list = self.load_accounts().map_err(AuthError::Store)?;
        let idx = list
            .accounts
            .iter()
            .position(|a| {
                a.invite_token_hash.as_deref() == Some(hash.as_str()) && a.disabled_at.is_none()
            })
            .ok_or(AuthError::InvalidCredentials)?;

        // Expiry is checked against the stored deadline, not the token.
        let expired = list.accounts[idx]
            .invite_expires_at
            .as_deref()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.timestamp() < now)
            .unwrap_or(true);
        if expired {
            return Err(AuthError::InvalidCredentials);
        }

        self.policy
            .validate(new_password)
            .map_err(AuthError::Policy)?;
        let hash = password::hash_password(new_password).map_err(AuthError::Store)?;

        let stamp = iso(now);
        let acc = &mut list.accounts[idx];
        acc.password_hash = hash;
        acc.password_changed_at = stamp.clone();
        acc.updated_at = stamp;
        acc.invite_token_hash = None;
        acc.invite_expires_at = None;
        acc.last_login_at = Some(iso(now));
        list.version += 1;
        let (user_id, role) = (acc.user_id.clone(), acc.role);
        self.save_accounts(&list).map_err(AuthError::Store)?;
        Ok(self.mint(&user_id, role, now))
    }

    fn index_of(&self, list: &AccountListFile, user_id: &str) -> Result<usize, AuthError> {
        list.accounts
            .iter()
            .position(|a| a.user_id == user_id)
            .ok_or(AuthError::InvalidCredentials)
    }

    fn admin_count(&self, list: &AccountListFile) -> usize {
        list.accounts
            .iter()
            .filter(|a| a.role == Role::Admin && a.disabled_at.is_none())
            .count()
    }

    /// Mint an `(invite_token, sha256_hash)` pair. Only the hash is stored
    /// (ADR-076 §决策 6: the token is a bearer secret, the store keeps no
    /// way to recover it).
    fn new_invite(&self) -> (String, String) {
        let token = random_hex16();
        let hash = invite_hash(&token);
        (token, hash)
    }
}

/// RFC 3339 timestamp for `now` (unix seconds).
fn iso(now: i64) -> String {
    chrono::DateTime::from_timestamp(now, 0)
        .map(|d| d.to_rfc3339())
        .unwrap_or_default()
}

/// 16 random bytes as lowercase hex — refresh-family / invite entropy.
fn random_hex16() -> String {
    let mut bytes = [0u8; 16];
    rand::fill(&mut bytes[..]);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// SHA-256 hex of an `invite_token` — the only form persisted
/// (ADR-076 §决策 6). Kept short deliberately: the invite is a 24h
/// single-use bearer secret, so a fast digest is enough (no KDF) —
/// the token itself is 128 bits of CSPRNG output, not a guessable
/// password.
fn invite_hash(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Whether a byte is allowed in a username (ADR-076 §决策 1:
/// "lowercase letters, digits, `-` and `_`").
fn is_username_byte(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_'
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
            invite_token_hash: None,
            invite_expires_at: None,
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
        assert_eq!(
            svc.verify_access(&second.access_token, now + 1)
                .unwrap()
                .user_id,
            uid
        );

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
            Err(AuthError::Token(
                TokenError::BadSignature | TokenError::Malformed
            ))
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

    /// ADR-076 §决策 6: the invite is 24h-bound and single-use. Expiry is
    /// checked against the stored deadline, and an unparseable deadline is
    /// treated as expired (fail closed) — never as "no deadline".
    #[test]
    fn invite_expiry_is_enforced() {
        let dir = tmp_dir("invite-expiry");
        let svc = AuthService::new(&dir, PasswordPolicy::default(), None).unwrap();
        let now = 1_700_000_000;
        let (_, invite) = svc
            .create_account("alice", "Alice", None, Role::User, now)
            .unwrap();
        let invite = invite.expect("passwordless create mints an invite");

        // Just inside the window → activates.
        assert!(
            svc.first_login(&invite, "s3cret123", now + INVITE_TTL_SECS - 1)
                .is_ok()
        );

        // Re-mint and step past the deadline → refused, indistinguishable
        // from any other bad invite.
        let (bob, invite2) = svc
            .create_account("bob", "Bob", None, Role::User, now)
            .unwrap();
        let invite2 = invite2.expect("invite minted");
        let err = svc
            .first_login(&invite2, "s3cret123", now + INVITE_TTL_SECS + 1)
            .unwrap_err();
        assert!(matches!(err, AuthError::InvalidCredentials), "got {err:?}");

        // Fail-closed: a garbage stored deadline must not read as "no expiry".
        let mut list = svc.load_accounts().unwrap();
        let acc = list
            .accounts
            .iter_mut()
            .find(|a| a.user_id == bob.user_id)
            .unwrap();
        acc.invite_expires_at = Some("not-a-timestamp".into());
        svc.save_accounts(&list).unwrap();
        let err = svc.first_login(&invite2, "s3cret123", now).unwrap_err();
        assert!(matches!(err, AuthError::InvalidCredentials), "got {err:?}");
    }

    /// ADR-076 §决策 6: `reset-password` mints a fresh single-use invite,
    /// clears the password (the reset flow *is* first-login), and logs the
    /// previous holder out by revoking every refresh family.
    #[test]
    fn reset_password_mints_single_use_invite_and_kills_families() {
        let (svc, uid) = service_with_user("reset-pwd", "alice", "oldpass12");
        let now = 1_700_000_000;
        let pair = svc.login("alice", "oldpass12", now).unwrap();

        let invite = svc.reset_password(&uid, now).unwrap();

        // The old refresh token is dead (all families revoked).
        assert!(matches!(
            svc.refresh(&pair.refresh_token, now).unwrap_err(),
            AuthError::Revoked
        ));
        // The old password no longer works — the hash was cleared.
        assert!(matches!(
            svc.login("alice", "oldpass12", now).unwrap_err(),
            AuthError::InvalidCredentials
        ));
        // The invite activates a new password, and is then burned.
        assert!(svc.first_login(&invite, "newpass12", now).is_ok());
        assert!(matches!(
            svc.first_login(&invite, "again1234", now).unwrap_err(),
            AuthError::InvalidCredentials
        ));
        assert!(svc.login("alice", "newpass12", now).is_ok());
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

    /// ADR-076 §决策 1: the display fields live on `UserAccount`, so
    /// `update_account` persists them into `accounts.json` — the authority
    /// `sync_profiles` re-derives the public view from. `None` fields are
    /// untouched, an empty `avatar` clears, and a whitespace `display_name`
    /// is ignored rather than blanking the name.
    #[test]
    fn update_account_applies_the_display_patch() {
        let (svc, uid) = service_with_user("profile-patch", "alice", "alicepass1");
        let now = 1_700_000_000;

        let account = svc
            .update_account(
                &uid,
                ProfilePatch {
                    display_name: Some("  Alice A  ".into()),
                    language: Some("zh-CN".into()),
                    timezone: Some("Asia/Shanghai".into()),
                    city: Some("上海".into()),
                    avatar: Some("assets/avatar-01.png".into()),
                    custom: Some([("theme".into(), "dark".into())].into()),
                    ..Default::default()
                },
                now,
            )
            .unwrap();
        assert_eq!(account.display_name, "Alice A");
        assert_eq!(account.language, "zh-CN");
        assert_eq!(account.timezone, "Asia/Shanghai");
        assert_eq!(account.city.as_deref(), Some("上海"));
        assert_eq!(account.avatar.as_deref(), Some("assets/avatar-01.png"));
        assert_eq!(
            account.custom.get("theme").map(String::as_str),
            Some("dark")
        );
        // Untouched fields keep their seeded values.
        assert_eq!(account.country, None);
        assert_eq!(account.builtin_avatar, None);

        // Everything is in the store (the authority), not just the return value.
        let stored = svc.load_accounts().unwrap().find(&uid).unwrap().clone();
        assert_eq!(stored.language, "zh-CN");
        assert_eq!(stored.avatar.as_deref(), Some("assets/avatar-01.png"));

        // Empty avatar clears; whitespace display_name is ignored.
        let account = svc
            .update_account(
                &uid,
                ProfilePatch {
                    display_name: Some("   ".into()),
                    avatar: Some(String::new()),
                    ..Default::default()
                },
                now,
            )
            .unwrap();
        assert_eq!(account.display_name, "Alice A");
        assert_eq!(account.avatar, None);
    }
}
