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

pub use claims::{ACCESS_TTL_SECS, REFRESH_TTL_SECS, Claims, TokenError, TokenKind, now_unix};
pub use issuer::TokenIssuer;
pub use verifier::TokenVerifier;
