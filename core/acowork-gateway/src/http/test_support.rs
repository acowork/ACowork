//! Test-only scaffolding for the Gateway's token gate (ADR-084 §决策 2).
//!
//! Before the extraction, a test that needed an authenticated request built
//! an `AuthService`, seeded an account, and called `login`. The Gateway no
//! longer owns any of that: it verifies access tokens with the Ed25519
//! public key the user service publishes ([`crate::lifecycle::user_supervisor`]).
//!
//! So a test now needs a *key pair*, not an account store — and specifically
//! the same key pair on both sides, or the token will not verify. Threading
//! an issuer through every call site would be noise, so the pair is derived
//! from one fixed seed here.
//!
//! **The seed is a fixture, not a secret.** It is only ever used to mint
//! tokens for tests running in-process; nothing outside `cfg(test)` can
//! reach it, and it signs nothing that any deployed Gateway would accept.

use std::sync::Arc;

use acowork_core::auth::{TokenIssuer, TokenVerifier, now_unix};
use ed25519_dalek::SigningKey;

/// Fixed Ed25519 seed (32 bytes). Arbitrary, and deliberately not derived
/// from anything — it must be stable so a token minted by [`access_token`]
/// verifies against [`verifier`] in a different test module.
const TEST_SEED: [u8; 32] = [
    0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0x00,
    0x0f, 0x1e, 0x2d, 0x3c, 0x4b, 0x5a, 0x69, 0x78, 0x87, 0x96, 0xa5, 0xb4, 0xc3, 0xd2, 0xe1, 0xf0,
];

fn issuer() -> TokenIssuer {
    TokenIssuer::new(SigningKey::from_bytes(&TEST_SEED))
}

/// The public half, to install in `GatewayState.user_verifier` — what the
/// supervisor does at runtime once the user service is up.
pub fn verifier() -> Arc<TokenVerifier> {
    Arc::new(issuer().verifier())
}

/// A valid access token; `user_id`/`role` are whatever the test needs the
/// gate to derive.
pub fn access_token(user_id: &str, role: &str) -> String {
    issuer().sign_access(user_id, role, now_unix())
}
