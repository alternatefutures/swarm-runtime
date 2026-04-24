//! Encryption at rest for agent state records.
//!
//! Design: docs/architecture/design/state-schema.md §8
//! Issue: alternatefutures/admin#134
//!
//! Two schemes are implemented:
//!   - AES-256-GCM for SENSITIVE-tier records (per-tenant key from Infisical)
//!   - FHE (TFHE-rs) for RESTRICTED-tier records (per-tenant FHE key from argus #133)
//!
//! The KeyProvider trait is the interface to the key management system.
//! Production implementation: InfisicalKeyProvider (backed by v3.0 §5.2 S4).
//! Test implementation: InMemoryKeyProvider.
//!
//! ⚠️ COORDINATION REQUIRED — argus (#133):
//!   This module defines the KeyProvider interface but does NOT implement the
//!   key derivation hierarchy, key storage, or rotation strategy. Those are
//!   argus's responsibility. See OQ-S2 in state-schema.md.

use async_trait::async_trait;
use bytes::Bytes;

use crate::error::{StateError, StateResult};
use crate::proto::{AadBinding, EncryptionEnvelope, EncryptionScheme};

// ────────────────────────────────────────────────────────────────────────────
// KeyProvider trait
// ────────────────────────────────────────────────────────────────────────────

/// The key material returned by a KeyProvider.
pub enum EncryptionKey {
    /// 256-bit AES key for AES-256-GCM (SENSITIVE tier).
    Aes256Gcm([u8; 32]),
    /// TFHE-rs client key for FHE operations (RESTRICTED tier).
    /// Box<> because ClientKey is large and we want to avoid stack overflow.
    // NOTE: tfhe::ClientKey type will be available once tfhe crate is wired in.
    // Using a placeholder bytes type for the skeleton phase.
    FheTfheRs(Box<Vec<u8>>), // TODO: replace Vec<u8> with tfhe::ClientKey in Phase 6
}

/// Interface to the key management system.
///
/// Implementations:
///   - `InMemoryKeyProvider` (test/dev — keys not persisted)
///   - `InfisicalKeyProvider` (production — keys in Infisical, Phase 6+)
///
/// ⚠️ argus (#133) owns the production implementation. This trait is the
/// coordination boundary between this crate and the key management design.
#[async_trait]
pub trait KeyProvider: Send + Sync {
    /// Retrieve the encryption key material for a given opaque key ID.
    async fn get_key(&self, key_id: &str) -> StateResult<EncryptionKey>;

    /// Get the active key ID for a given tenant and sensitivity scheme.
    /// Called at write time to determine which key to use for new records.
    async fn current_key_id(
        &self,
        tenant_id: &str,
        scheme: EncryptionScheme,
    ) -> StateResult<String>;

    /// Initiate key rotation for a tenant. Returns the new key ID.
    /// Does NOT re-encrypt existing records — that is a separate rotation job.
    ///
    /// ⚠️ See OQ-S4: key rotation interacts with compliance retention and GDPR
    /// erasure. argus must define the policy before this is implemented.
    async fn rotate_key(
        &self,
        tenant_id: &str,
        scheme: EncryptionScheme,
    ) -> StateResult<String>;
}

// ────────────────────────────────────────────────────────────────────────────
// InMemoryKeyProvider (test/dev only)
// ────────────────────────────────────────────────────────────────────────────

/// Test key provider with static in-memory keys. NOT for production use.
pub struct InMemoryKeyProvider {
    aes_key: [u8; 32],
    key_id: String,
}

impl InMemoryKeyProvider {
    /// Create a provider with a fixed test key.
    /// In production this would be replaced by InfisicalKeyProvider.
    pub fn new_test() -> Self {
        Self {
            // Static test key — never use real data with this provider
            aes_key: [0x42u8; 32],
            key_id: "test-key-v1".to_string(),
        }
    }
}

#[async_trait]
impl KeyProvider for InMemoryKeyProvider {
    async fn get_key(&self, _key_id: &str) -> StateResult<EncryptionKey> {
        Ok(EncryptionKey::Aes256Gcm(self.aes_key))
    }

    async fn current_key_id(
        &self,
        _tenant_id: &str,
        _scheme: EncryptionScheme,
    ) -> StateResult<String> {
        Ok(self.key_id.clone())
    }

    async fn rotate_key(
        &self,
        _tenant_id: &str,
        _scheme: EncryptionScheme,
    ) -> StateResult<String> {
        // In-memory provider cannot rotate keys
        Err(StateError::InvalidInput(
            "InMemoryKeyProvider does not support key rotation".into(),
        ))
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Encryption / Decryption
// ────────────────────────────────────────────────────────────────────────────

/// Encrypt `plaintext` bytes using the given `EncryptionKey` and return an
/// `EncryptionEnvelope` ready to be embedded in a state record.
///
/// The `aad_binding` is encoded and included as Additional Authenticated Data
/// for AES-256-GCM (prevents ciphertext replay under different keys).
pub async fn encrypt(
    plaintext: Bytes,
    key: &EncryptionKey,
    aad_binding: AadBinding,
) -> StateResult<EncryptionEnvelope> {
    match key {
        EncryptionKey::Aes256Gcm(raw_key) => {
            use aes_gcm::{
                aead::{Aead, KeyInit, OsRng},
                Aes256Gcm, Nonce,
            };
            use rand::RngCore;
            use prost::Message;

            // Generate a random 96-bit nonce
            let mut nonce_bytes = [0u8; 12];
            OsRng.fill_bytes(&mut nonce_bytes);
            let nonce = Nonce::from_slice(&nonce_bytes);

            let cipher = Aes256Gcm::new_from_slice(raw_key)
                .map_err(|_| StateError::AesGcmError)?;

            // Encode AAD
            let aad_bytes = aad_binding.encode_to_vec();

            // AES-GCM with AAD
            let ciphertext = cipher
                .encrypt(nonce, aes_gcm::aead::Payload { msg: &plaintext, aad: &aad_bytes })
                .map_err(|_| StateError::AesGcmError)?;

            Ok(EncryptionEnvelope {
                scheme: EncryptionScheme::Aes256Gcm as i32,
                key_id: String::new(), // caller fills this in from KeyProvider
                iv_or_nonce: nonce_bytes.to_vec(),
                ciphertext,
                aad: aad_bytes,
            })
        }
        EncryptionKey::FheTfheRs(_fhe_key) => {
            // TODO (Phase 6): Implement FHE encryption using TFHE-rs primitives
            // from v3.0 §5.4.2. Specifically fhe_compare / fhe_aggregate / fhe_search
            // for RESTRICTED data.
            //
            // For the design skeleton phase, FHE encryption is not yet implemented.
            // RESTRICTED-tier agents will block at Phase 1; FHE unblocks in Phase 6.
            Err(StateError::InvalidInput(
                "FHE encryption not yet implemented — tracked for Phase 6".into(),
            ))
        }
    }
}

/// Decrypt an `EncryptionEnvelope` and return the plaintext bytes.
pub async fn decrypt(
    envelope: &EncryptionEnvelope,
    key: &EncryptionKey,
) -> StateResult<Bytes> {
    match (EncryptionScheme::try_from(envelope.scheme), key) {
        (Ok(EncryptionScheme::Plaintext), _) => {
            // Should not arrive here with a non-empty ciphertext
            Ok(Bytes::from(envelope.ciphertext.clone()))
        }
        (Ok(EncryptionScheme::Aes256Gcm), EncryptionKey::Aes256Gcm(raw_key)) => {
            use aes_gcm::{aead::{Aead, KeyInit}, Aes256Gcm, Nonce};

            let nonce = Nonce::from_slice(&envelope.iv_or_nonce);
            let cipher = Aes256Gcm::new_from_slice(raw_key)
                .map_err(|_| StateError::AesGcmError)?;

            let plaintext = cipher
                .decrypt(nonce, aes_gcm::aead::Payload {
                    msg: &envelope.ciphertext,
                    aad: &envelope.aad,
                })
                .map_err(|_| StateError::AesGcmError)?;

            Ok(Bytes::from(plaintext))
        }
        (Ok(EncryptionScheme::FheTfheRs), EncryptionKey::FheTfheRs(_)) => {
            // TODO (Phase 6): FHE decryption
            Err(StateError::InvalidInput(
                "FHE decryption not yet implemented — tracked for Phase 6".into(),
            ))
        }
        _ => Err(StateError::InvalidInput(
            "scheme/key type mismatch in encryption envelope".into(),
        )),
    }
}
