//! Shared login-token contract — re-export (ADR-084 §决策 3).
//!
//! The codec moved to [`acowork_core::auth`] so that `acowork-user` (issuer)
//! and the Gateway (verifier) share one definition without depending on each
//! other. Gateway callers keep importing `crate::auth::token::*`, so this
//! stays a drop-in during the migration.
//!
//! Was: a local HS256 implementation with a shared symmetric secret
//! (`data_dir/auth/secret`). Signing and verification must now use different
//! keys, which is why the codec is asymmetric.
//!
//! After M1 the account system leaves the Gateway and this module shrinks to
//! the verifier surface only (`auth_middleware`); the remaining re-exports
//! can then be deleted.

pub use acowork_core::auth::{
    ACCESS_TTL_SECS, Claims, TokenError, TokenIssuer, TokenKind, TokenVerifier, now_unix,
};
