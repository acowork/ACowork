//! Ed25519 token issuance (ADR-084 §决策 3).
//!
//! The codec lives in [`acowork_core::auth`] so the issuer (here) and the
//! verifier (the Gateway's `auth_middleware`) share one definition without
//! depending on each other. This module is the user service's entry point to
//! it: the private key is this process's exclusive property, so the Gateway
//! can verify a token but never mint one.

use std::path::Path;

pub use acowork_core::auth::{TokenIssuer, TokenVerifier};

pub use acowork_core::auth::{
    ACCESS_TTL_SECS, REFRESH_TTL_SECS, Claims, TokenError, TokenKind, now_unix,
};

/// File name (under `{data_dir}/auth/`) of the Ed25519 private key.
pub const SIGNING_KEY_FILE: &str = "ed25519.key";
/// File name (under `{data_dir}/auth/`) of the matching public key.
pub const PUBLIC_KEY_FILE: &str = "ed25519.pub";

/// Load (or, on first run, generate) the signing key under `data_dir`.
///
/// Writes `{data_dir}/auth/ed25519.key` (`0600` on Unix) and the sibling
/// `ed25519.pub` the Gateway reads.
pub fn load_issuer(data_dir: &Path) -> Result<TokenIssuer, String> {
    let auth_dir = data_dir.join("auth");
    std::fs::create_dir_all(&auth_dir)
        .map_err(|e| format!("failed to create {}: {e}", auth_dir.display()))?;
    TokenIssuer::load_or_generate(&auth_dir.join(SIGNING_KEY_FILE))
}

/// Load the public key under `data_dir` into a verifier.
///
/// Not used by the serving path (the Gateway verifies, not this service) —
/// it exists so tests and diagnostics can check the pair is consistent.
pub fn load_verifier(data_dir: &Path) -> Result<TokenVerifier, String> {
    TokenVerifier::from_public_key_file(&data_dir.join("auth").join(PUBLIC_KEY_FILE))
}
