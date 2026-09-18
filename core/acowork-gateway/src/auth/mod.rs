//! Authentication subsystem (ADR-076).
//!
//! - [`mode`] — deployment auth-mode resolution (`local` vs `multi_user`),
//!   the first-class config that gates the entire ADR-076 account system.
//! - [`token`] — HS256 login-token signing / verification.
//! - [`revoked`] — refresh-token family revocation registry.
//! - [`service`] — the assembled account service (login / refresh /
//!   logout / change-password), built only under `AUTH_MODE=multi_user`.

pub mod mode;
pub mod revoked;
pub mod service;
pub mod token;

pub use mode::{AuthMode, is_loopback_host, resolve_auth_mode};
pub use service::{AuthError, AuthPrincipal, AuthService, BootstrapAdmin, PasswordPolicy};
