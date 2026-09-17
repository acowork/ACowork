//! Login-token signing and verification (ADR-076 §决策 3).
//!
//! A minimal HS256 JWT: `base64url(header).base64url(payload).base64url(sig)`.
//! The signing secret is a persistent random 32-byte key
//! (`data_dir/auth/secret`), independent of the Vault master key — so
//! login works while the Vault is locked, and a Vault relock never
//! invalidates live sessions.
//!
//! Two token kinds share the codec:
//! - **access** — short-lived (15 min), carries `sub` + `role`.
//! - **refresh** — long-lived (30 days), carries `sub` + `family`. A family
//!   is rotated on every refresh; revoking a family kills every refresh
//!   token descended from it (see [`crate::auth::revoked`]).

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Access-token lifetime (ADR-076 §决策 3).
pub const ACCESS_TTL_SECS: i64 = 15 * 60;
/// Refresh-token lifetime (ADR-076 §决策 3).
pub const REFRESH_TTL_SECS: i64 = 30 * 24 * 3600;

/// The exact header we emit. Verification never trusts a caller-supplied
/// `alg` (that is the classic `alg=none` forgery vector) — we always
/// verify as HS256.
const HEADER_JSON: &str = r#"{"alg":"HS256","typ":"JWT"}"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Access,
    Refresh,
}

impl TokenKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Access => "access",
            Self::Refresh => "refresh",
        }
    }
}

/// Verified token payload.
#[derive(Debug, Clone, PartialEq)]
pub struct Claims {
    /// Subject — the `user_id`.
    pub sub: String,
    /// Role (`"user"` / `"admin"`); present on access tokens only.
    pub role: Option<String>,
    /// Refresh-token family id; present on refresh tokens only.
    pub family: Option<String>,
    pub kind: TokenKind,
    /// Issued-at (unix seconds).
    pub iat: i64,
    /// Expiry (unix seconds).
    pub exp: i64,
}

impl Claims {
    pub fn is_admin(&self) -> bool {
        self.role.as_deref() == Some("admin")
    }

    /// Whether this claim's refresh `family` is scoped to its subject.
    ///
    /// A family is minted as `{user_id}.{random}`, and revocation uses a
    /// `{user_id}.*` wildcard prefix (see [`crate::auth::revoked`]). If the
    /// family ever belonged to a *different* subject it would match that
    /// subject's wildcard instead of its own — an authorization blur — so
    /// the binding is checked at verify time, not just at sign time.
    pub fn is_family_consistent(&self) -> bool {
        match &self.family {
            Some(f) => f.starts_with(&format!("{}.", self.sub)),
            None => true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenError {
    Malformed,
    BadSignature,
    Expired,
    WrongKind,
}

impl std::fmt::Display for TokenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Malformed => "malformed token",
            Self::BadSignature => "invalid token signature",
            Self::Expired => "token expired",
            Self::WrongKind => "wrong token kind",
        };
        f.write_str(s)
    }
}

impl std::error::Error for TokenError {}

#[derive(Serialize, Deserialize)]
struct Payload {
    sub: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    family: Option<String>,
    kind: String,
    iat: i64,
    exp: i64,
}

/// Signs and verifies login tokens.
pub struct TokenSigner {
    key: Vec<u8>,
}

impl TokenSigner {
    /// Build a signer from a raw key.
    pub fn new(key: Vec<u8>) -> Self {
        Self { key }
    }

    /// Load the signing secret from `path`, generating and persisting a
    /// fresh 32-byte secret on first run.
    ///
    /// On Unix the file is created with `0600` permissions — the secret
    /// mints login tokens, so it must not be world-readable.
    pub fn load_or_generate(path: &std::path::Path) -> Result<Self, String> {
        match std::fs::read(path) {
            Ok(bytes) if bytes.len() >= 32 => Ok(Self::new(bytes)),
            Ok(bytes) => Err(format!(
                "auth secret at {} is too short ({} bytes, need >= 32)",
                path.display(),
                bytes.len()
            )),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let mut key = vec![0u8; 32];
                rand::fill(&mut key[..]);
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| format!("failed to create auth dir: {e}"))?;
                }
                write_secret(path, &key)?;
                tracing::info!(path = %path.display(), "generated new auth signing secret");
                Ok(Self::new(key))
            }
            Err(e) => Err(format!("failed to read auth secret {}: {e}", path.display())),
        }
    }

    /// Sign a short-lived access token.
    pub fn sign_access(&self, user_id: &str, role: &str, now: i64) -> String {
        self.sign(&Payload {
            sub: user_id.to_string(),
            role: Some(role.to_string()),
            family: None,
            kind: TokenKind::Access.as_str().to_string(),
            iat: now,
            exp: now + ACCESS_TTL_SECS,
        })
    }

    /// Sign a long-lived refresh token belonging to `family`.
    ///
    /// The family must be scoped to the subject (`{user_id}.{random}`) —
    /// revocation wildcards depend on it. An unscoped family is a
    /// programming error, so it panics in debug and is rejected at verify
    /// time regardless.
    pub fn sign_refresh(&self, user_id: &str, family: &str, now: i64) -> String {
        debug_assert!(
            family.starts_with(&format!("{user_id}.")),
            "refresh family must be scoped to its subject: {family} for {user_id}"
        );
        self.sign(&Payload {
            sub: user_id.to_string(),
            role: None,
            family: Some(family.to_string()),
            kind: TokenKind::Refresh.as_str().to_string(),
            iat: now,
            exp: now + REFRESH_TTL_SECS,
        })
    }

    fn sign(&self, payload: &Payload) -> String {
        let header = B64.encode(HEADER_JSON);
        let body = B64.encode(serde_json::to_vec(payload).expect("payload serializes"));
        let signing_input = format!("{header}.{body}");
        let sig = self.mac(signing_input.as_bytes());
        format!("{signing_input}.{}", B64.encode(sig))
    }

    fn mac(&self, input: &[u8]) -> Vec<u8> {
        let mut mac =
            HmacSha256::new_from_slice(&self.key).expect("HMAC accepts keys of any length");
        mac.update(input);
        mac.finalize().into_bytes().to_vec()
    }

    /// Verify a token's signature and expiry. Kind is not enforced here —
    /// use [`Self::verify_kind`] to reject a refresh token presented as an
    /// access token (or vice versa).
    pub fn verify(&self, token: &str, now: i64) -> Result<Claims, TokenError> {
        let mut parts = token.split('.');
        let (header, body, sig) = match (parts.next(), parts.next(), parts.next(), parts.next()) {
            (Some(h), Some(b), Some(s), None) => (h, b, s),
            _ => return Err(TokenError::Malformed),
        };

        let sig_bytes = B64.decode(sig).map_err(|_| TokenError::Malformed)?;
        let signing_input = format!("{header}.{body}");
        let mut mac =
            HmacSha256::new_from_slice(&self.key).expect("HMAC accepts keys of any length");
        mac.update(signing_input.as_bytes());
        // `verify_slice` is constant-time.
        mac.verify_slice(&sig_bytes)
            .map_err(|_| TokenError::BadSignature)?;

        let raw = B64.decode(body).map_err(|_| TokenError::Malformed)?;
        let payload: Payload =
            serde_json::from_slice(&raw).map_err(|_| TokenError::Malformed)?;

        if payload.exp < now {
            return Err(TokenError::Expired);
        }

        let kind = match payload.kind.as_str() {
            "access" => TokenKind::Access,
            "refresh" => TokenKind::Refresh,
            _ => return Err(TokenError::Malformed),
        };

        // The family must be scoped to the subject (see
        // `Claims::is_family_consistent`). Checked here, not just at sign
        // time, so a hand-crafted payload can never cross-wire revocation
        // wildcards between users.
        if let Some(family) = &payload.family
            && !family.starts_with(&format!("{}.", payload.sub))
        {
            return Err(TokenError::Malformed);
        }

        Ok(Claims {
            sub: payload.sub,
            role: payload.role,
            family: payload.family,
            kind,
            iat: payload.iat,
            exp: payload.exp,
        })
    }

    /// Like [`Self::verify`] but also requires the token to be of `kind`.
    pub fn verify_kind(
        &self,
        token: &str,
        kind: TokenKind,
        now: i64,
    ) -> Result<Claims, TokenError> {
        let claims = self.verify(token, now)?;
        if claims.kind != kind {
            return Err(TokenError::WrongKind);
        }
        Ok(claims)
    }
}

/// Current unix time in seconds.
pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(unix)]
fn write_secret(path: &std::path::Path, key: &[u8]) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    // Atomic write: temp sibling + rename, mirroring `account::store`.
    // A bare write could leave a truncated secret on crash, which would
    // fail-fast on next boot (availability loss); rename never exposes a
    // partial file.
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(|e| format!("failed to create auth secret: {e}"))?;
    f.write_all(key)
        .map_err(|e| format!("failed to write auth secret: {e}"))?;
    f.sync_all()
        .map_err(|e| format!("failed to sync auth secret: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("failed to commit auth secret: {e}")
    })
}

#[cfg(not(unix))]
fn write_secret(path: &std::path::Path, key: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, key).map_err(|e| format!("failed to write auth secret: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("failed to commit auth secret: {e}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signer() -> TokenSigner {
        TokenSigner::new(vec![7u8; 32])
    }

    #[test]
    fn access_roundtrip() {
        let t = signer().sign_access("u-1", "admin", 1000);
        let c = signer().verify(&t, 1001).unwrap();
        assert_eq!(c.sub, "u-1");
        assert_eq!(c.role.as_deref(), Some("admin"));
        assert!(c.is_admin());
        assert_eq!(c.kind, TokenKind::Access);
        assert_eq!(c.exp, 1000 + ACCESS_TTL_SECS);
    }

    #[test]
    fn refresh_roundtrip_and_kind_check() {
        // Family is minted as `{user_id}.{random}` — scoped to the subject.
        let t = signer().sign_refresh("u-1", "u-1.fam-9", 1000);
        let c = signer().verify(&t, 1001).unwrap();
        assert_eq!(c.family.as_deref(), Some("u-1.fam-9"));
        assert_eq!(c.kind, TokenKind::Refresh);
        assert!(c.is_family_consistent());

        // A refresh token must not pass an access check.
        assert_eq!(
            signer().verify_kind(&t, TokenKind::Access, 1001),
            Err(TokenError::WrongKind)
        );
    }

    #[test]
    fn refresh_family_must_match_subject() {
        let s = signer();
        // Hand-craft a refresh token whose family belongs to a *different*
        // user. If accepted, `u-2` would match the `u-1.*` wildcard when
        // user 1's password changes. Verify must reject it.
        let cross_user = s.sign(&Payload {
            sub: "u-2".into(),
            role: None,
            family: Some("u-1.abc".into()),
            kind: "refresh".into(),
            iat: 1000,
            exp: 1000 + REFRESH_TTL_SECS,
        });
        assert_eq!(s.verify(&cross_user, 1001), Err(TokenError::Malformed));

        // Same-subject family verifies; an adjacent id (`u-10`) must not
        // accidentally satisfy the `u-1.` prefix.
        assert!(s.verify(&s.sign_refresh("u-1", "u-1.abc", 1000), 1001).is_ok());
        let adjacent = s.sign(&Payload {
            sub: "u-10".into(),
            role: None,
            family: Some("u-1.abc".into()),
            kind: "refresh".into(),
            iat: 1000,
            exp: 1000 + REFRESH_TTL_SECS,
        });
        assert_eq!(s.verify(&adjacent, 1001), Err(TokenError::Malformed));
    }

    #[test]
    fn expired_token_rejected() {
        let t = signer().sign_access("u-1", "user", 1000);
        assert_eq!(
            signer().verify(&t, 1000 + ACCESS_TTL_SECS + 1),
            Err(TokenError::Expired)
        );
    }

    #[test]
    fn tampered_payload_rejected() {
        let t = signer().sign_access("u-1", "user", 1000);
        let parts: Vec<&str> = t.split('.').collect();
        // Swap in a payload claiming admin, keeping the original signature.
        let forged = B64.encode(
            serde_json::to_vec(&Payload {
                sub: "u-1".into(),
                role: Some("admin".into()),
                family: None,
                kind: "access".into(),
                iat: 1000,
                exp: 1000 + ACCESS_TTL_SECS,
            })
            .unwrap(),
        );
        let forged_token = format!("{}.{}.{}", parts[0], forged, parts[2]);
        assert_eq!(
            signer().verify(&forged_token, 1001),
            Err(TokenError::BadSignature)
        );
    }

    #[test]
    fn wrong_key_rejected() {
        let t = signer().sign_access("u-1", "user", 1000);
        let other = TokenSigner::new(vec![9u8; 32]);
        assert_eq!(other.verify(&t, 1001), Err(TokenError::BadSignature));
    }

    #[test]
    fn malformed_rejected() {
        let s = signer();
        assert_eq!(s.verify("not-a-token", 1000), Err(TokenError::Malformed));
        assert_eq!(s.verify("a.b", 1000), Err(TokenError::Malformed));
        assert_eq!(s.verify("a.b.c.d", 1000), Err(TokenError::Malformed));
    }

    #[test]
    fn secret_is_persisted_and_reused() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("acowork-token-test-{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("secret");

        let t = signer().sign_access("u-1", "user", 1000);
        let s1 = TokenSigner::load_or_generate(&path).unwrap();
        let s2 = TokenSigner::load_or_generate(&path).unwrap();
        // Reusing the on-disk secret keeps a token minted by s1 valid under s2.
        let t1 = s1.sign_access("u-1", "user", 1000);
        assert!(s2.verify(&t1, 1001).is_ok());
        // Sanity: the unrelated test signer still rejects it.
        assert!(signer().verify(&t, 1001).is_ok());
        assert!(s2.verify(&t, 1001).is_err());

        // Atomic write: no temp sibling left behind.
        assert!(!path.with_extension("tmp").exists());
        // Unix: the persisted secret must be owner-only (0600).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "auth secret must be 0600");
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
