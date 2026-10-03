//! Passphrase-sealed secrets for the export envelope.
//!
//! Every secret in the file is a `{ "$sealed": "<blob>" }` object. The key
//! comes from Argon2id over the operator passphrase (pinned at m=65536,
//! t=3, p=1) mixed with a per-export envelope nonce, and each value seals
//! under XChaCha20-Poly1305 with its own fresh random nonce. Blobs are
//! `base64(nonce || ciphertext)`; the envelope carries the salt and the
//! envelope nonce, never the passphrase or the key.
//!
//! The envelope nonce is KDF context, not a cipher nonce: it feeds key
//! derivation (`SHA-256(argon2id-output || envelope-nonce)`), so blobs from
//! one export never open under another export's key even if the salt and
//! passphrase repeated.

use argon2::{Algorithm, Argon2, Params, Version};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, KeyInit},
};
use sha2::{Digest as _, Sha256};
use thiserror::Error;

use crate::export::envelope::{KdfParams, SecretEnvelope};

/// The only sealing scheme v1 readers accept.
pub const SCHEME: &str = "argon2id+xchacha20poly1305";
/// The only KDF algorithm v1 readers accept.
pub const KDF_ALGO: &str = "argon2id";
/// Pinned Argon2id memory cost in KiB.
pub const M_COST_KIB: u32 = 65536;
/// Pinned Argon2id time cost.
pub const T_COST: u32 = 3;
/// Pinned Argon2id parallelism.
pub const P_COST: u32 = 1;
/// Salt length in bytes. Single-sourced here; the import side re-exports
/// it instead of re-declaring it.
pub const SALT_LEN: usize = 16;
/// Envelope-nonce length in bytes. Single-sourced here; the import side
/// re-exports it instead of re-declaring it.
pub const ENVELOPE_NONCE_LEN: usize = 24;
/// JSON key marking a sealed secret value (`{"$sealed": ...}`). The typed
/// [`SealedValue`](crate::export::envelope::SealedValue) renames to this
/// same literal; attribute macros cannot name a constant, so the two are
/// kept adjacent by convention.
pub const SEALED_KEY: &str = "$sealed";
/// Per-value cipher-nonce length in bytes.
const VALUE_NONCE_LEN: usize = 24;
/// Sealing-key length in bytes.
const KEY_LEN: usize = 32;
/// Poly1305 tag length in bytes.
const TAG_LEN: usize = 16;
/// Minimum sealed blob length: value nonce plus the Poly1305 tag.
/// Single-sourced here; the import side re-exports it.
pub const MIN_SEALED_BLOB_LEN: usize = VALUE_NONCE_LEN + TAG_LEN;

/// Every way sealing or unsealing can fail. Fixed strings only; no variant
/// carries key, passphrase, blob, or plaintext material.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum SealError {
    /// The blob does not open: wrong passphrase, corrupt data, or a blob
    /// from another envelope. Deliberately one variant so callers cannot
    /// distinguish the cases.
    #[error("sealed value does not open under this passphrase")]
    AuthFailed,
    /// The blob is not decodable base64 or is too short to hold a sealed
    /// value. Raised before any key derivation.
    #[error("sealed value is not a valid sealed blob")]
    InvalidBlob,
    /// The secret envelope names an unknown scheme or parameters, or its
    /// salt/nonce fields do not decode.
    #[error("secret envelope is not a supported v1 envelope")]
    InvalidEnvelope,
    /// Key derivation failed.
    #[error("key derivation failed")]
    KdfFailed,
    /// The OS random generator failed.
    #[error("random number generator failed")]
    RandomFailed,
    /// Sealing failed. Unreachable in practice (XChaCha20-Poly1305
    /// encryption only fails on allocation failure) but typed anyway.
    #[error("cannot seal value")]
    SealFailed,
}

impl SealError {
    /// Machine-readable code for reports and CLI exits.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::AuthFailed => "ENVELOPE_AUTH_FAILED",
            Self::InvalidBlob => "INVALID_SEALED_BLOB",
            Self::InvalidEnvelope => "INVALID_SECRET_ENVELOPE",
            Self::KdfFailed => "KDF_FAILED",
            Self::RandomFailed => "RANDOM_FAILED",
            Self::SealFailed => "SEAL_FAILED",
        }
    }
}

/// The spec-pinned sealing parameters, for tests and reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PinnedKdf {
    /// KDF algorithm name.
    pub algo: &'static str,
    /// Memory cost in KiB.
    pub m_cost_kib: u32,
    /// Time cost.
    pub t_cost: u32,
    /// Parallelism.
    pub p_cost: u32,
}

/// The spec-pinned sealing parameters, for tests and reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PinnedParams {
    /// Scheme name.
    pub scheme: &'static str,
    /// KDF pin.
    pub kdf: PinnedKdf,
}

/// The parameters every v1 envelope must carry.
#[must_use]
pub fn secret_envelope_params() -> PinnedParams {
    PinnedParams {
        scheme: SCHEME,
        kdf: PinnedKdf {
            algo: KDF_ALGO,
            m_cost_kib: M_COST_KIB,
            t_cost: T_COST,
            p_cost: P_COST,
        },
    }
}

/// An export's sealing context: the derived key plus the salt and envelope
/// nonce that parameterize it. Built once per export; every [`seal`](Self::seal)
/// call draws a fresh value nonce.
pub struct Sealer {
    key: [u8; KEY_LEN],
    salt: [u8; SALT_LEN],
    envelope_nonce: [u8; ENVELOPE_NONCE_LEN],
}

impl std::fmt::Debug for Sealer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Sealer([redacted])")
    }
}

impl Drop for Sealer {
    fn drop(&mut self) {
        for byte in self.key.iter_mut() {
            // Volatile so the wipe survives optimizer dead-store removal.
            unsafe {
                std::ptr::write_volatile(byte, 0);
            }
        }
    }
}

impl Sealer {
    /// Derive a sealing context from the operator passphrase with a fresh
    /// random salt and envelope nonce. Passphrase strength is the
    /// operator's job; empty passphrases derive like any other.
    pub fn generate(passphrase: &str) -> Result<Self, SealError> {
        let mut salt = [0u8; SALT_LEN];
        let mut envelope_nonce = [0u8; ENVELOPE_NONCE_LEN];
        getrandom::fill(&mut salt).map_err(|_| SealError::RandomFailed)?;
        getrandom::fill(&mut envelope_nonce).map_err(|_| SealError::RandomFailed)?;
        let key = derive_key(passphrase, &salt, &envelope_nonce)?;
        Ok(Self {
            key,
            salt,
            envelope_nonce,
        })
    }

    /// The public envelope parameters for the export file.
    #[must_use]
    pub fn secret_envelope(&self) -> SecretEnvelope {
        SecretEnvelope {
            scheme: SCHEME.to_owned(),
            kdf: KdfParams {
                algo: KDF_ALGO.to_owned(),
                m: M_COST_KIB,
                t: T_COST,
                p: P_COST,
                salt_b64: STANDARD.encode(self.salt),
            },
            nonce_b64: STANDARD.encode(self.envelope_nonce),
        }
    }

    /// Seal one secret value. Empty seals like any other value and unseals
    /// back to empty; absence stays absence at the JSON layer instead.
    pub fn seal(&self, plaintext: &str) -> Result<String, SealError> {
        let mut nonce_bytes = [0u8; VALUE_NONCE_LEN];
        getrandom::fill(&mut nonce_bytes).map_err(|_| SealError::RandomFailed)?;
        let nonce = *XNonce::from_slice(&nonce_bytes);
        let ciphertext = cipher(&self.key)
            .encrypt(&nonce, plaintext.as_bytes())
            .map_err(|_| SealError::SealFailed)?;
        let mut blob = Vec::with_capacity(VALUE_NONCE_LEN + ciphertext.len());
        blob.extend_from_slice(&nonce_bytes);
        blob.extend_from_slice(&ciphertext);
        Ok(STANDARD.encode(&blob))
    }
}

/// Open one sealed blob under the operator passphrase. Wrong passphrases,
/// tampered blobs, and blobs from another envelope all fail as
/// [`SealError::AuthFailed`] before any write can happen.
pub fn unseal(
    passphrase: &str,
    envelope: &SecretEnvelope,
    blob_b64: &str,
) -> Result<String, SealError> {
    if envelope.scheme != SCHEME {
        return Err(SealError::InvalidEnvelope);
    }
    if envelope.kdf.algo != KDF_ALGO
        || envelope.kdf.m != M_COST_KIB
        || envelope.kdf.t != T_COST
        || envelope.kdf.p != P_COST
    {
        return Err(SealError::InvalidEnvelope);
    }
    let salt = STANDARD
        .decode(envelope.kdf.salt_b64.trim())
        .map_err(|_| SealError::InvalidEnvelope)?;
    let envelope_nonce = STANDARD
        .decode(envelope.nonce_b64.trim())
        .map_err(|_| SealError::InvalidEnvelope)?;
    if salt.len() != SALT_LEN || envelope_nonce.len() != ENVELOPE_NONCE_LEN {
        return Err(SealError::InvalidEnvelope);
    }
    let blob = STANDARD
        .decode(blob_b64.trim())
        .map_err(|_| SealError::InvalidBlob)?;
    if blob.len() < VALUE_NONCE_LEN + TAG_LEN {
        return Err(SealError::InvalidBlob);
    }

    let key = derive_key(passphrase, &salt, &envelope_nonce)?;
    let nonce = *XNonce::from_slice(&blob[..VALUE_NONCE_LEN]);
    let plaintext = cipher(&key)
        .decrypt(&nonce, &blob[VALUE_NONCE_LEN..])
        .map_err(|_| SealError::AuthFailed)?;
    String::from_utf8(plaintext).map_err(|_| SealError::InvalidBlob)
}

fn derive_key(
    passphrase: &str,
    salt: &[u8],
    envelope_nonce: &[u8],
) -> Result<[u8; KEY_LEN], SealError> {
    let params =
        Params::new(M_COST_KIB, T_COST, P_COST, Some(KEY_LEN)).map_err(|_| SealError::KdfFailed)?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut raw = [0u8; KEY_LEN];
    argon2
        .hash_password_into(passphrase.as_bytes(), salt, &mut raw)
        .map_err(|_| SealError::KdfFailed)?;
    let mut hasher = Sha256::new();
    hasher.update(raw);
    hasher.update(envelope_nonce);
    for byte in raw.iter_mut() {
        // Volatile so the wipe survives optimizer dead-store removal.
        unsafe {
            std::ptr::write_volatile(byte, 0);
        }
    }
    Ok(hasher.finalize().into())
}

fn cipher(key: &[u8; KEY_LEN]) -> XChaCha20Poly1305 {
    XChaCha20Poly1305::new(key.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_hides_key_material() {
        let shown = format!("{:?}", Sealer::generate("x").unwrap());
        assert!(shown.contains("[redacted]"));
        assert!(!shown.contains('x'));
    }
}
