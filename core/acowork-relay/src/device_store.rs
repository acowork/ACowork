//! Persisted device records (design doc 24 §5.4, §7.3).
//!
//! The relay stores **only public material** per device: the gw-id and its
//! pinned Ed25519 public key (plus bookkeeping). Private keys never leave
//! the Gateway's vault — a relay compromise leaks no secrets.
//!
//! Storage: a single JSON file under the data dir, rewritten atomically
//! (temp sibling + rename). The expected scale (thousands of devices, rare
//! writes) needs nothing heavier.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ed25519_dalek::VerifyingKey;

use acowork_core::relay::proto::{decode_pubkey, encode_pubkey};

/// One persisted device.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DeviceRecord {
    /// Pinned Ed25519 public key (URL-safe base64, raw 32 bytes).
    pub pubkey: String,
    /// Unix seconds of first (TOFU) registration.
    pub created_at: i64,
    /// Unix seconds of the last successful tunnel registration.
    pub last_seen_at: Option<i64>,
}

/// On-disk shape (versioned for forward migrations).
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct StoreFile {
    version: u32,
    devices: BTreeMap<String, DeviceRecord>,
}

/// Errors for device-store operations.
#[derive(Debug, thiserror::Error)]
pub enum DeviceStoreError {
    #[error("device '{0}' is not registered")]
    UnknownDevice(String),
    #[error("device '{0}' already registered")]
    AlreadyRegistered(String),
    #[error("invalid pubkey for device '{0}': {1}")]
    InvalidPubkey(String, String),
    #[error("device store io: {0}")]
    Io(#[from] std::io::Error),
    #[error("device store corrupt: {0}")]
    Corrupt(String),
}

/// The persisted device registry.
pub struct DeviceStore {
    path: PathBuf,
    devices: parking_lot::RwLock<BTreeMap<String, DeviceRecord>>,
}

impl DeviceStore {
    /// Load (or start empty when the file is absent) and keep `path` for
    /// atomic persistence on every mutation.
    pub fn load(data_dir: &Path) -> Result<Self, DeviceStoreError> {
        std::fs::create_dir_all(data_dir)?;
        let path = data_dir.join("devices.json");
        let devices = match std::fs::read_to_string(&path) {
            Ok(text) => {
                let file: StoreFile = serde_json::from_str(&text)
                    .map_err(|e| DeviceStoreError::Corrupt(e.to_string()))?;
                if file.version != 1 {
                    return Err(DeviceStoreError::Corrupt(format!(
                        "unsupported store version {}",
                        file.version
                    )));
                }
                file.devices
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(e.into()),
        };
        Ok(Self {
            path,
            devices: parking_lot::RwLock::new(devices),
        })
    }

    fn persist(&self, devices: &BTreeMap<String, DeviceRecord>) -> Result<(), DeviceStoreError> {
        let file = StoreFile {
            version: 1,
            devices: devices.clone(),
        };
        let text = serde_json::to_string_pretty(&file)
            .map_err(|e| DeviceStoreError::Corrupt(e.to_string()))?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, &self.path)?;
        // Best-effort hardening on Unix (vault-like key files elsewhere in
        // the workspace do the same).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }

    /// TOFU-enroll a new device (fails if already registered) or pin-check
    /// an existing one. Returns the pinned verifying key on success.
    pub fn enroll_or_verify(
        &self,
        gw_id: &str,
        pubkey_b64: &str,
    ) -> Result<VerifyingKey, DeviceStoreError> {
        // Validate the key material before taking any lock.
        let verifying = decode_pubkey(pubkey_b64)
            .map_err(|e| DeviceStoreError::InvalidPubkey(gw_id.to_string(), e))?;
        if encode_pubkey(&verifying) != pubkey_b64 {
            return Err(DeviceStoreError::InvalidPubkey(
                gw_id.to_string(),
                "non-canonical encoding".into(),
            ));
        }

        let mut devices = self.devices.write();
        match devices.get(gw_id) {
            Some(existing) => {
                // Key pinning: a later registration must present the exact
                // same public key (rotation goes through RotateKey with an
                // old-key signature, not through a fresh claim).
                if existing.pubkey != pubkey_b64 {
                    return Err(DeviceStoreError::UnknownDevice(format!(
                        "{gw_id}: pubkey does not match the pinned key"
                    )));
                }
                let mut updated = existing.clone();
                updated.last_seen_at = Some(chrono::Utc::now().timestamp());
                devices.insert(gw_id.to_string(), updated);
            }
            None => {
                devices.insert(
                    gw_id.to_string(),
                    DeviceRecord {
                        pubkey: pubkey_b64.to_string(),
                        created_at: chrono::Utc::now().timestamp(),
                        last_seen_at: Some(chrono::Utc::now().timestamp()),
                    },
                );
            }
        }
        self.persist(&devices)?;
        Ok(verifying)
    }

    /// Rotate the pinned key after the caller has verified the old-key
    /// signature (the store only records the decision).
    pub fn rotate_key(
        &self,
        gw_id: &str,
        new_pubkey_b64: &str,
    ) -> Result<(), DeviceStoreError> {
        let verifying = decode_pubkey(new_pubkey_b64)
            .map_err(|e| DeviceStoreError::InvalidPubkey(gw_id.to_string(), e))?;
        let mut devices = self.devices.write();
        let existing = devices
            .get_mut(gw_id)
            .ok_or_else(|| DeviceStoreError::UnknownDevice(gw_id.to_string()))?;
        existing.pubkey = encode_pubkey(&verifying);
        self.persist(&devices)?;
        Ok(())
    }

    /// Admin pre-registration (enterprise mode): upsert a device record
    /// without TOFU. Later tunnel registrations pin-check against it.
    pub fn register(&self, gw_id: &str, pubkey_b64: &str) -> Result<(), DeviceStoreError> {
        let verifying = decode_pubkey(pubkey_b64)
            .map_err(|e| DeviceStoreError::InvalidPubkey(gw_id.to_string(), e))?;
        let mut devices = self.devices.write();
        devices.insert(
            gw_id.to_string(),
            DeviceRecord {
                pubkey: encode_pubkey(&verifying),
                created_at: chrono::Utc::now().timestamp(),
                last_seen_at: None,
            },
        );
        self.persist(&devices)?;
        Ok(())
    }

    /// The pinned verifying key for a device, if registered.
    pub fn get_pubkey(&self, gw_id: &str) -> Result<Option<VerifyingKey>, DeviceStoreError> {
        let devices = self.devices.read();
        match devices.get(gw_id) {
            Some(record) => Ok(Some(
                decode_pubkey(&record.pubkey)
                    .map_err(|e| DeviceStoreError::InvalidPubkey(gw_id.to_string(), e))?,
            )),
            None => Ok(None),
        }
    }

    /// Whether the device exists (pre-registration check for enterprise
    /// mode where TOFU is disabled).
    pub fn contains(&self, gw_id: &str) -> bool {
        self.devices.read().contains_key(gw_id)
    }

    /// Delete a device (admin revocation). Returns `true` when a record was
    /// removed.
    pub fn revoke(&self, gw_id: &str) -> Result<bool, DeviceStoreError> {
        let mut devices = self.devices.write();
        let removed = devices.remove(gw_id).is_some();
        if removed {
            self.persist(&devices)?;
        }
        Ok(removed)
    }

    /// Snapshot for the admin listing API.
    pub fn list(&self) -> Vec<(String, DeviceRecord)> {
        self.devices
            .read()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// Number of registered devices.
    pub fn len(&self) -> usize {
        self.devices.read().len()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.devices.read().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acowork_core::relay::proto::{encode_pubkey, generate_device_seed};
    use ed25519_dalek::SigningKey;

    fn keypair() -> (SigningKey, String) {
        let key = SigningKey::from_bytes(&generate_device_seed());
        let pubkey = encode_pubkey(&key.verifying_key());
        (key, pubkey)
    }

    #[test]
    fn tofu_then_pinning() {
        let dir = tempfile::tempdir().unwrap();
        let store = DeviceStore::load(dir.path()).unwrap();
        let gw = "0f1e2d3c-4b5a-4678-9abc-def012345678";
        let (_k1, pk1) = keypair();
        let (_k2, pk2) = keypair();

        // TOFU enroll.
        store.enroll_or_verify(gw, &pk1).unwrap();
        assert!(store.contains(gw));

        // Same key verifies.
        store.enroll_or_verify(gw, &pk1).unwrap();

        // A different key is refused (pinning).
        let err = store.enroll_or_verify(gw, &pk2).unwrap_err();
        assert!(err.to_string().contains("does not match"), "{err}");

        // Garbage keys are rejected before enrollment.
        assert!(store.enroll_or_verify("11111111-2222-4333-8444-555555555555", "??").is_err());
    }

    #[test]
    fn rotation_updates_pin() {
        let dir = tempfile::tempdir().unwrap();
        let store = DeviceStore::load(dir.path()).unwrap();
        let gw = "0f1e2d3c-4b5a-4678-9abc-def012345678";
        let (_old, old_pk) = keypair();
        let (_new, new_pk) = keypair();
        store.enroll_or_verify(gw, &old_pk).unwrap();
        store.rotate_key(gw, &new_pk).unwrap();
        // New key now verifies, old key no longer matches.
        store.enroll_or_verify(gw, &new_pk).unwrap();
        assert!(store.enroll_or_verify(gw, &old_pk).is_err());
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn revoke_and_persistence_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let gw = "0f1e2d3c-4b5a-4678-9abc-def012345678";
        let (_k, pk) = keypair();

        {
            let store = DeviceStore::load(dir.path()).unwrap();
            store.enroll_or_verify(gw, &pk).unwrap();
        }
        // Reload from disk — record survives.
        {
            let store = DeviceStore::load(dir.path()).unwrap();
            assert!(store.contains(gw));
            assert!(store.revoke(gw).unwrap());
            assert!(!store.contains(gw));
            assert!(!store.revoke(gw).unwrap());
        }
        // Reload again — revocation persisted.
        let store = DeviceStore::load(dir.path()).unwrap();
        assert!(!store.contains(gw));
    }
}
