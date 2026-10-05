//! v2 secret decryption: Fernet under `DATA_ENC_KEY`.
//!
//! v2 encrypted stored secrets with Fernet (AES-128-CBC + HMAC-SHA256,
//! 32-byte urlsafe-base64 key: signing half first, encryption half second).
//! Its `decrypt` treated anything undecryptable as legacy plaintext and
//! handed it back unchanged; [`FernetKey::decrypt_legacy`] keeps exactly
//! those semantics so the exporter resolves every secret the way v2 did
//! (empty passes through, failures pass the input back flagged).
//!
//! [`FernetKey::encrypt`] exists so tests and tooling can build
//! v2-faithful fixtures; the exporter itself only decrypts.

use aes::Aes128;
use base64::{Engine as _, engine::general_purpose::URL_SAFE};
use cbc::cipher::{
    BlockDecryptMut as _, BlockEncryptMut as _, KeyIvInit as _, block_padding::Pkcs7,
};
use cbc::{Decryptor, Encryptor};
use hmac::{Hmac, Mac as _};
use sha2::Sha256;
use thiserror::Error;

/// Fernet key length in bytes (16 signing + 16 encryption).
const KEY_LEN: usize = 32;
/// Fernet token version byte.
const TOKEN_VERSION: u8 = 0x80;
/// Timestamp field length in bytes.
const TIMESTAMP_LEN: usize = 8;
/// IV length in bytes.
const IV_LEN: usize = 16;
/// HMAC-SHA256 length in bytes.
const HMAC_LEN: usize = 32;

/// Every way v2 key handling or token decryption can fail. Fixed strings
/// only; no variant carries key, token, or plaintext material.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum FernetError {
    /// The key is not 32 urlsafe-base64 bytes.
    #[error("v2 key is not 32 urlsafe-base64 bytes")]
    InvalidKey,
    /// The token does not decode, fails authentication, or does not unpad
    /// to UTF-8. One variant on purpose, so callers cannot distinguish
    /// wrong-key from corrupt-token.
    #[error("value is not a valid v2 token")]
    InvalidToken,
    /// The OS random generator failed.
    #[error("random number generator failed")]
    RandomFailed,
    /// The system clock is unusable for token timestamps.
    #[error("system clock failed")]
    ClockFailed,
}

/// A v2 `DATA_ENC_KEY`: 32 raw bytes, first half signing, second half
/// encryption. Never derives from anything else; the exporter loads it from
/// the v2 key file and refuses to mint.
pub struct FernetKey {
    raw: [u8; KEY_LEN],
}

impl std::fmt::Debug for FernetKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FernetKey([redacted])")
    }
}

impl Drop for FernetKey {
    fn drop(&mut self) {
        for byte in self.raw.iter_mut() {
            // Volatile so the wipe survives optimizer dead-store removal.
            unsafe {
                std::ptr::write_volatile(byte, 0);
            }
        }
    }
}

impl FernetKey {
    /// Parse a v2 key from its urlsafe-base64 text form.
    pub fn from_base64(text: &str) -> Result<Self, FernetError> {
        let decoded = URL_SAFE
            .decode(text.trim())
            .map_err(|_| FernetError::InvalidKey)?;
        if decoded.len() != KEY_LEN {
            return Err(FernetError::InvalidKey);
        }
        let mut raw = [0u8; KEY_LEN];
        raw.copy_from_slice(&decoded);
        Ok(Self { raw })
    }

    /// Render the key in its v2 text form (fixture support).
    #[must_use]
    pub fn to_base64(&self) -> String {
        URL_SAFE.encode(self.raw)
    }

    /// Mint a fresh random key (fixture support; the exporter never mints).
    pub fn generate() -> Result<Self, FernetError> {
        let mut raw = [0u8; KEY_LEN];
        getrandom::fill(&mut raw).map_err(|_| FernetError::RandomFailed)?;
        Ok(Self { raw })
    }

    /// Encrypt one value exactly the way v2 stored it (fixture support).
    /// Empty passes through, matching v2.
    pub fn encrypt(&self, plaintext: &str) -> Result<String, FernetError> {
        if plaintext.is_empty() {
            return Ok(String::new());
        }
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| FernetError::ClockFailed)?
            .as_secs();
        let mut iv = [0u8; IV_LEN];
        getrandom::fill(&mut iv).map_err(|_| FernetError::RandomFailed)?;
        let ciphertext = Encryptor::<Aes128>::new_from_slices(&self.raw[16..32], &iv)
            .map_err(|_| FernetError::InvalidKey)?
            .encrypt_padded_vec_mut::<Pkcs7>(plaintext.as_bytes());
        let mut msg = Vec::with_capacity(1 + TIMESTAMP_LEN + IV_LEN + ciphertext.len());
        msg.push(TOKEN_VERSION);
        msg.extend_from_slice(&timestamp.to_be_bytes());
        msg.extend_from_slice(&iv);
        msg.extend_from_slice(&ciphertext);
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.raw[0..16])
            .map_err(|_| FernetError::InvalidKey)?;
        mac.update(&msg);
        let mut token = msg;
        token.extend_from_slice(&mac.finalize().into_bytes());
        Ok(URL_SAFE.encode(&token))
    }

    /// Decrypt one v2 token, failing closed on anything invalid. No TTL
    /// check: v2 called `decrypt` without one.
    pub fn decrypt(&self, token: &str) -> Result<String, FernetError> {
        if token.is_empty() {
            return Ok(String::new());
        }
        let bytes = URL_SAFE
            .decode(token.trim())
            .map_err(|_| FernetError::InvalidToken)?;
        if bytes.len() < 1 + TIMESTAMP_LEN + IV_LEN + HMAC_LEN + 16 || bytes[0] != TOKEN_VERSION {
            return Err(FernetError::InvalidToken);
        }
        let (msg, signature) = bytes.split_at(bytes.len() - HMAC_LEN);
        // Ciphertext must hold at least one AES block.
        if msg.len() < 1 + TIMESTAMP_LEN + IV_LEN + 16
            || (msg.len() - (1 + TIMESTAMP_LEN + IV_LEN)) % 16 != 0
        {
            return Err(FernetError::InvalidToken);
        }
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.raw[0..16])
            .map_err(|_| FernetError::InvalidKey)?;
        mac.update(msg);
        mac.verify_slice(signature)
            .map_err(|_| FernetError::InvalidToken)?;
        let iv = &msg[1 + TIMESTAMP_LEN..1 + TIMESTAMP_LEN + IV_LEN];
        let ciphertext = &msg[1 + TIMESTAMP_LEN + IV_LEN..];
        let plaintext = Decryptor::<Aes128>::new_from_slices(&self.raw[16..32], iv)
            .map_err(|_| FernetError::InvalidKey)?
            .decrypt_padded_vec_mut::<Pkcs7>(ciphertext)
            .map_err(|_| FernetError::InvalidToken)?;
        String::from_utf8(plaintext).map_err(|_| FernetError::InvalidToken)
    }

    /// v2 `decrypt()` semantics: empty passes through unflagged, valid
    /// tokens decrypt, and anything else comes back unchanged flagged as
    /// legacy plaintext. The exporter seals both successes and legacy
    /// values; the flag is only informational. Edge: a valid-MAC token
    /// decoding to non-UTF8 counts as legacy passthrough here while v2
    /// raised; unreachable from v2-produced data, which is always UTF-8.
    #[must_use]
    pub fn decrypt_legacy(&self, stored: &str) -> (String, bool) {
        if stored.is_empty() {
            return (String::new(), false);
        }
        match self.decrypt(stored) {
            Ok(plaintext) => (plaintext, false),
            Err(_) => (stored.to_owned(), true),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Independent golden vector: token assembled with `openssl enc
    // -aes-128-cbc` (ciphertext) plus Python's stdlib HMAC-SHA256, with no
    // code shared with this module.
    const GOLDEN_KEY: &str = "MDEyMzQ1Njc4OWFiY2RlZmZlZGNiYTk4NzY1NDMyMTA=";
    const GOLDEN_TOKEN: &str = "gAAAAABoyPOAAAECAwQFBgcICQoLDA0OD9Dm7Q99jdGjWIDM23oxtU4pqC6gZHCIkJ1k1uI8HIhkCwTF6gnBz50axjeIceMv-fxH7F6Mr2XNlb8ulVJ3hxc=";
    const GOLDEN_PLAINTEXT: &str = "slskd-api-key-123";

    #[test]
    fn decrypts_the_independent_golden_vector() {
        let key = FernetKey::from_base64(GOLDEN_KEY).unwrap();
        assert_eq!(key.decrypt(GOLDEN_TOKEN).unwrap(), GOLDEN_PLAINTEXT);
    }

    #[test]
    fn encrypt_round_trips_with_fresh_iv_each_time() {
        let key = FernetKey::generate().unwrap();
        let first = key.encrypt("sab-key").unwrap();
        let second = key.encrypt("sab-key").unwrap();
        assert_ne!(first, second);
        assert_eq!(key.decrypt(&first).unwrap(), "sab-key");
        assert_eq!(key.decrypt(&second).unwrap(), "sab-key");
        assert_eq!(key.encrypt("").unwrap(), "");
    }

    #[test]
    fn wrong_key_garbage_and_tampering_fail_closed() {
        let key = FernetKey::from_base64(GOLDEN_KEY).unwrap();
        let other = FernetKey::generate().unwrap();
        assert_eq!(other.decrypt(GOLDEN_TOKEN), Err(FernetError::InvalidToken));
        assert_eq!(key.decrypt("not-a-token"), Err(FernetError::InvalidToken));
        let mut tampered = GOLDEN_TOKEN.to_owned();
        tampered.pop();
        tampered.push('A');
        assert_eq!(key.decrypt(&tampered), Err(FernetError::InvalidToken));
    }

    #[test]
    fn legacy_passthrough_matches_v2() {
        let key = FernetKey::from_base64(GOLDEN_KEY).unwrap();
        assert_eq!(key.decrypt_legacy(""), (String::new(), false));
        assert_eq!(
            key.decrypt_legacy(GOLDEN_TOKEN),
            (GOLDEN_PLAINTEXT.to_owned(), false)
        );
        assert_eq!(
            key.decrypt_legacy("plaintext-secret"),
            ("plaintext-secret".to_owned(), true)
        );
    }

    #[test]
    fn debug_hides_the_key() {
        let key = FernetKey::from_base64(GOLDEN_KEY).unwrap();
        assert_eq!(format!("{key:?}"), "FernetKey([redacted])");
    }
}
