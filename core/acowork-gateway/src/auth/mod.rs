//! Authentication subsystem (ADR-076, post-ADR-084).
//!
//! The account system — credentials, roles, login / refresh / logout,
//! revocation — lives in the `acowork-user` process (ADR-084 §决策 1). What
//! remains here is everything the Gateway still needs to *be* the single
//! authentication point without owning any account data:
//!
//! - [`mode`] — deployment auth-mode resolution (`local` vs `multi_user`),
//!   the first-class config that gates the entire ADR-076 account system.
//! - [`token`] — shared Ed25519 login-token contract
//!   (re-export of `acowork_core::auth`, ADR-084 §决策 3). Verify-only:
//!   [`crate::auth::TokenVerifier`] holds a public key, so neither a Gateway
//!   compromise nor a Gateway code path can mint a login.
//!
//! The Gateway's `AuthService` is gone (M4): the two writers it needed were
//! `admin-setup` (now a delegate to the user binary's own subcommand) and
//! the restricted-mode gate (now a snapshot the supervisor polls).

pub mod mode;
pub mod token;

pub use mode::{AuthMode, is_loopback_host, resolve_auth_mode};
