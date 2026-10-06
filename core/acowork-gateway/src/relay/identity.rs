//! Gateway relay identity (design doc 24 §7.3).
//!
//! Machine-scoped device credential for the relay tunnel: a random UUID
//! v4 gw-id plus an Ed25519 seed, generated on first enable and pinned by
//! the relay thereafter (TOFU).
//!
//! Persisted as a plain `0600` JSON file next to the Gateway config —
//! deliberately NOT in the password-locked vault: the tunnel must be up
//! before any remote user can authenticate (login/refresh requests
//! themselves travel through the tunnel), so the credential cannot
//! depend on a vault unlock that only a local user can perform. Same
//! trust model as the Node Agent's `identity.json` (ADR-075): a
//! machine-scoped secret whose compromise affects exactly one device's
//! tunnel, not any user account.

use std::path::Path;

use acowork_core::relay::proto::{generate_device_seed, is_valid_gw_id};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};

/// On-disk shape (versioned for forward migrations).
#[derive(Debug, Serialize, Deserialize)]
struct RelayIdentityFile {
    version: u32,
    gw_id: String,
    /// Ed25519 seed, URL-safe base64 (raw 32 bytes).
    seed: String,
}

/// The loaded (or freshly minted) device identity.
pub struct RelayIdentity {
    pub gw_id: String,
    key: SigningKey,
}

impl RelayIdentity {
    /// Load the identity from `path`, creating it on first use.
    ///
    /// Creation is idempotent under concurrent callers being unlikely
    /// (the enable API is serialized by the RelayClient's supervision
    /// lock); the atomic tmp+rename write means a torn state can never
    /// be observed — readers see either the old file or the new one.
    pub fn load_or_create(path: &Path) -> Result<Self, String> {
        if path.exists() {
            return Self::load(path);
        }
        Self::create(path)
    }

    fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("reading relay identity '{}': {e}", path.display()))?;
        let file: RelayIdentityFile = serde_json::from_str(&text)
            .map_err(|e| format!("parsing relay identity '{}': {e}", path.display()))?;
        if file.version != 1 {
            return Err(format!(
                "unsupported relay identity version {} in {}",
                file.version,
                path.display()
            ));
        }
        if !is_valid_gw_id(&file.gw_id) {
            return Err(format!(
                "invalid gw_id in relay identity '{}'",
                path.display()
            ));
        }
        let seed: [u8; 32] = B64
            .decode(&file.seed)
            .map_err(|e| format!("invalid seed in relay identity '{}': {e}", path.display()))?
            .try_into()
            .map_err(|v: Vec<u8>| {
                format!(
                    "relay identity seed must be 32 bytes, got {}",
                    v.len()
                )
            })?;
        Ok(Self {
            gw_id: file.gw_id,
            key: SigningKey::from_bytes(&seed),
        })
    }

    fn create(path: &Path) -> Result<Self, String> {
        let seed = generate_device_seed();
        let gw_id = uuid::Uuid::new_v4().to_string();
        let file = RelayIdentityFile {
            version: 1,
            gw_id: gw_id.clone(),
            seed: B64.encode(seed),
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("creating relay identity dir: {e}"))?;
        }
        let tmp = path.with_extension("json.tmp");
        let text =
            serde_json::to_string_pretty(&file).map_err(|e| format!("encoding identity: {e}"))?;
        std::fs::write(&tmp, text).map_err(|e| format!("writing relay identity: {e}"))?;
        // Restrict BEFORE publishing: renaming first would leave a window
        // where the private key sits at the umask default (typically
        // world-readable) until a post-rename chmod lands.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
        }
        std::fs::rename(&tmp, path).map_err(|e| format!("publishing relay identity: {e}"))?;
        tracing::info!(gw_id = %gw_id, path = %path.display(), "minted relay device identity");
        Ok(Self {
            gw_id,
            key: SigningKey::from_bytes(&seed),
        })
    }

    /// The device signing key (proofs for the relay challenge-response,
    /// §5.4). Never leaves the Gateway process.
    pub fn signing_key(&self) -> &SigningKey {
        &self.key
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_or_create_is_stable_across_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("relay_identity.json");

        let first = RelayIdentity::load_or_create(&path).unwrap();
        assert!(is_valid_gw_id(&first.gw_id));

        // Reload yields the SAME identity (same key, same gw_id).
        let second = RelayIdentity::load_or_create(&path).unwrap();
        assert_eq!(first.gw_id, second.gw_id);
        assert_eq!(
            first.signing_key().verifying_key(),
            second.signing_key().verifying_key()
        );

        // 0600 on Unix.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn corrupted_identity_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("relay_identity.json");
        std::fs::write(&path, "{\"version\": 2}").unwrap();
        assert!(RelayIdentity::load_or_create(&path).is_err());
    }
}
