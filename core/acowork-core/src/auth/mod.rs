//! Shared login-token contract (ADR-084 §决策 3).
//!
//! Extracted from the Gateway (`acowork-gateway/src/auth/token.rs`, formerly
//! HS256) so that the two halves of the split can share one codec without
//! depending on each other:
//!
//! - [`TokenIssuer`] — holds the Ed25519 **private** key. The credential
//!   authority lives in `acowork-user`.
//! - [`TokenVerifier`] — holds the Ed25519 **public** key. Loaded by the
//!   Gateway `auth_middleware` for the per-request check.
//!
//! Asymmetric on purpose: the Gateway verifies but cannot mint, so neither a
//! Gateway compromise nor a Gateway code path can hand out a login.
//!
//! ## Wire format
//!
//! A minimal JWT: `base64url(header).base64url(payload).base64url(sig)` with
//! header `{"alg":"EdDSA","typ":"JWT"}`. The header is **never read back** —
//! verification is always Ed25519 over the signing input, which closes the
//! classic `alg=none` forgery vector. The signature covers the header bytes,
//! so tampering with them still fails verification.
//!
//! Two token kinds share the codec:
//! - **access** — short-lived (15 min), carries `sub` + `role`.
//! - **refresh** — long-lived (30 days), carries `sub` + `family`. A family
//!   is rotated on every refresh; revoking a family kills every refresh token
//!   descended from it.

mod claims;
mod issuer;
mod verifier;

/// Trusted-identity headers the Gateway injects for the user service
/// (ADR-084 §决策 7).
///
/// Defined here, not in either process, because the two sides are separate
/// binaries and a one-sided rename would otherwise be a silent wire break.
/// The Gateway is their **only** trusted writer; both the Gateway's proxy and
/// the service's identity layer re-check that they agree.
pub const AUTH_USER_HEADER: &str = "x-auth-user";
/// Authenticated role (`admin` / `user`).
pub const AUTH_ROLE_HEADER: &str = "x-auth-role";
/// Admin-only "view as user X" scope (ADR-076 §决策 4).
pub const AUTH_AS_USER_HEADER: &str = "x-auth-as-user";

pub use claims::{ACCESS_TTL_SECS, REFRESH_TTL_SECS, Claims, TokenError, TokenKind, now_unix};
pub use issuer::TokenIssuer;
pub use verifier::TokenVerifier;
