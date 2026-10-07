//! v3 secret encryption at rest.
//!
//! v2 kept a Fernet key as `DATA_ENC_KEY` in `<config>/config/.env` (a
//! different `.env` from the app one) and, worse, treated undecryptable
//! ciphertext as legacy plaintext: deleting the key file minted a fresh key
//! and every stored Fernet token silently became the credential. v3 keeps
//! the two halves of that apart:
//!
//! - The v3 key lives in `<config_dir>/data_enc.key` (base64, mode 0600),
//!   never in the environment. `load_or_generate` mints it on first boot;
//!   `load` refuses to mint, for contexts that must fail closed.
//! - `decrypt` fails closed with a typed error on any undecryptable input.
//!   There is no legacy-plaintext passthrough here; that lives only in the
//!   v2 importer, which resolves v2 values through v2 semantics and
//!   immediately re-encrypts under this key.
//!
//! Ciphertext shape is `v3:<base64 nonce||ciphertext>` (ChaCha20-Poly1305,
//! random 96-bit nonce per encryption). Empty strings pass through both
//! directions, matching v2: empty is absence, never failure.

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use chacha20poly1305::{
    ChaCha20Poly1305, Nonce,
    aead::{Aead, KeyInit},
};
use thiserror::Error;

/// Name of the v3 key file inside the config directory.
pub const KEY_FILE_NAME: &str = "data_enc.key";
/// Prefix marking v3 ciphertext in stored secret fields.
pub const CIPHER_PREFIX: &str = "v3:";
/// Raw key length in bytes (ChaCha20-Poly1305 key).
const KEY_LEN: usize = 32;
/// Nonce length in bytes.
const NONCE_LEN: usize = 12;

/// Every way key handling or secret decryption can fail. Fixed strings
/// only; no variant carries key or plaintext material.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum CryptoError {
    /// The key file is absent and this context refuses to mint one (the
    /// v2 exporter/importer use this to refuse rather than silently
    /// re-keying, which is what corrupted v2 credentials on key loss).
    #[error("key file {} is missing; refusing to mint a fresh key here", .path.display())]
    KeyMissing {
        /// Expected key file location.
        path: PathBuf,
    },
    /// The key file cannot be read.
    #[error("cannot read key file {}: {reason}", .path.display())]
    KeyRead {
        /// Key file location.
        path: PathBuf,
        /// OS reason.
        reason: String,
    },
    /// The key file cannot be written.
    #[error("cannot write key file {}: {reason}", .path.display())]
    KeyWrite {
        /// Key file location.
        path: PathBuf,
        /// OS reason.
        reason: String,
    },
    /// The key file does not hold 32 base64 bytes. Never minted over;
    /// fail closed so bad key material cannot silently orphan ciphertext.
    #[error("key file holds an invalid key: expected 32 base64 bytes")]
    KeyInvalid,
    /// Stored input is neither empty nor `v3:` ciphertext.
    #[error("value is not v3 ciphertext")]
    UnknownFormat,
    /// Ciphertext does not open under this key (wrong key or corrupt data).
    #[error("cannot decrypt value: key mismatch or corrupt data")]
    DecryptFailed,
    /// Encryption failed. Unreachable in practice (ChaCha20-Poly1305
    /// encryption only fails on allocation failure) but typed anyway.
    #[error("cannot encrypt value")]
    EncryptFailed,
    /// The OS random generator failed.
    #[error("random number generator failed")]
    RandomFailed,
}

/// Authenticated encryption under the v3 data key. Built once at startup
/// and injected by constructor; never a global.
pub struct Crypto {
    key: [u8; KEY_LEN],
}

impl std::fmt::Debug for Crypto {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Crypto([redacted])")
    }
}

impl Drop for Crypto {
    fn drop(&mut self) {
        for byte in self.key.iter_mut() {
            // Volatile so the wipe survives optimizer dead-store removal.
            unsafe {
                std::ptr::write_volatile(byte, 0);
            }
        }
    }
}

impl Crypto {
    /// Load the key, minting and persisting a fresh one on first boot.
    pub fn load_or_generate(config_dir: &Path) -> Result<Self, CryptoError> {
        let path = config_dir.join(KEY_FILE_NAME);
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::from_key_text(&text),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let key = random_bytes()?;
                write_key_file(&path, &key)?;
                Ok(Self { key })
            }
            Err(error) => Err(CryptoError::KeyRead {
                path,
                reason: error.to_string(),
            }),
        }
    }

    /// Load the key, refusing to mint when absent.
    pub fn load(config_dir: &Path) -> Result<Self, CryptoError> {
        let path = config_dir.join(KEY_FILE_NAME);
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::from_key_text(&text),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(CryptoError::KeyMissing { path })
            }
            Err(error) => Err(CryptoError::KeyRead {
                path,
                reason: error.to_string(),
            }),
        }
    }

    /// Build from raw key bytes (tests and the v2 importer, which
    /// holds the v3 key in memory after unlocking the export envelope).
    pub fn from_key_bytes(bytes: &[u8]) -> Result<Self, CryptoError> {
        if bytes.len() != KEY_LEN {
            return Err(CryptoError::KeyInvalid);
        }
        let mut key = [0u8; KEY_LEN];
        key.copy_from_slice(bytes);
        Ok(Self { key })
    }

    fn from_key_text(text: &str) -> Result<Self, CryptoError> {
        let decoded = STANDARD
            .decode(text.trim())
            .map_err(|_| CryptoError::KeyInvalid)?;
        Self::from_key_bytes(&decoded)
    }

    fn cipher(&self) -> Result<ChaCha20Poly1305, CryptoError> {
        ChaCha20Poly1305::new_from_slice(&self.key).map_err(|_| CryptoError::KeyInvalid)
    }

    /// HMAC-SHA256 over `message` under a key derived from the data key
    /// for `purpose`, so tokens one feature signs never verify in another.
    #[must_use]
    pub fn mac(&self, purpose: &str, message: &[u8]) -> [u8; 32] {
        use hmac::digest::KeyInit;
        use hmac::{Hmac, Mac as _};
        // Keys are 32 bytes; HMAC zero-pads them to the 64-byte block, so
        // passing the padded block is the same key with no fallible path.
        let derive = |key: &[u8; 32], data: &[u8]| -> [u8; 32] {
            let mut block = [0u8; 64];
            block[..32].copy_from_slice(key);
            let mut mac = <Hmac<sha2::Sha256> as KeyInit>::new(&block.into());
            mac.update(data);
            mac.finalize().into_bytes().into()
        };
        let purpose_key = derive(&self.key, purpose.as_bytes());
        derive(&purpose_key, message)
    }

    /// Encrypt one secret. Empty passes through; anything else becomes
    /// `v3:` ciphertext with a fresh random nonce.
    pub fn encrypt(&self, plaintext: &str) -> Result<String, CryptoError> {
        if plaintext.is_empty() {
            return Ok(String::new());
        }
        let nonce_bytes = random_nonce()?;
        let nonce = *Nonce::from_slice(&nonce_bytes);
        let mut blob = Vec::with_capacity(NONCE_LEN + plaintext.len() + 16);
        blob.extend_from_slice(&nonce_bytes);
        let ciphertext = self
            .cipher()?
            .encrypt(&nonce, plaintext.as_bytes())
            .map_err(|_| CryptoError::EncryptFailed)?;
        blob.extend_from_slice(&ciphertext);
        Ok(format!("{CIPHER_PREFIX}{}", STANDARD.encode(&blob)))
    }

    /// Decrypt one stored secret. Empty passes through; non-`v3:` input
    /// and undecryptable ciphertext both fail closed with typed errors.
    pub fn decrypt(&self, stored: &str) -> Result<String, CryptoError> {
        if stored.is_empty() {
            return Ok(String::new());
        }
        let encoded = stored
            .strip_prefix(CIPHER_PREFIX)
            .ok_or(CryptoError::UnknownFormat)?;
        let blob = STANDARD
            .decode(encoded)
            .map_err(|_| CryptoError::UnknownFormat)?;
        if blob.len() <= NONCE_LEN {
            return Err(CryptoError::UnknownFormat);
        }
        let nonce = *Nonce::from_slice(&blob[..NONCE_LEN]);
        let plaintext = self
            .cipher()?
            .decrypt(&nonce, &blob[NONCE_LEN..])
            .map_err(|_| CryptoError::DecryptFailed)?;
        String::from_utf8(plaintext).map_err(|_| CryptoError::DecryptFailed)
    }
}

fn random_bytes() -> Result<[u8; KEY_LEN], CryptoError> {
    let mut key = [0u8; KEY_LEN];
    getrandom::fill(&mut key).map_err(|_| CryptoError::RandomFailed)?;
    Ok(key)
}

fn random_nonce() -> Result<[u8; NONCE_LEN], CryptoError> {
    let mut nonce = [0u8; NONCE_LEN];
    getrandom::fill(&mut nonce).map_err(|_| CryptoError::RandomFailed)?;
    Ok(nonce)
}

fn write_key_file(path: &Path, key: &[u8; KEY_LEN]) -> Result<(), CryptoError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| CryptoError::KeyWrite {
            path: path.to_path_buf(),
            reason: error.to_string(),
        })?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(path).map_err(|error| CryptoError::KeyWrite {
        path: path.to_path_buf(),
        reason: error.to_string(),
    })?;
    use std::io::Write as _;
    file.write_all(STANDARD.encode(key).as_bytes())
        .and_then(|()| file.write_all(b"\n"))
        .and_then(|()| file.sync_all())
        .map_err(|error| CryptoError::KeyWrite {
            path: path.to_path_buf(),
            reason: error.to_string(),
        })?;
    #[cfg(unix)]
    std::fs::File::open(path.parent().unwrap_or(Path::new(".")))
        .and_then(|dir| dir.sync_all())
        .map_err(|error| CryptoError::KeyWrite {
            path: path.to_path_buf(),
            reason: error.to_string(),
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_crypto() -> Crypto {
        Crypto::from_key_bytes(&[7u8; KEY_LEN]).unwrap()
    }

    #[test]
    fn round_trip_with_fresh_nonce_each_time() {
        let crypto = test_crypto();
        let first = crypto.encrypt("slskd-api-key").unwrap();
        let second = crypto.encrypt("slskd-api-key").unwrap();
        assert!(first.starts_with(CIPHER_PREFIX));
        assert_ne!(first, second);
        assert_eq!(crypto.decrypt(&first).unwrap(), "slskd-api-key");
    }

    #[test]
    fn empty_passes_through_both_ways() {
        let crypto = test_crypto();
        assert_eq!(crypto.encrypt("").unwrap(), "");
        assert_eq!(crypto.decrypt("").unwrap(), "");
    }

    #[test]
    fn wrong_key_and_garbage_fail_closed() {
        let crypto = test_crypto();
        let other = Crypto::from_key_bytes(&[9u8; KEY_LEN]).unwrap();
        let sealed = crypto.encrypt("secret").unwrap();
        assert_eq!(other.decrypt(&sealed), Err(CryptoError::DecryptFailed));
        assert_eq!(
            crypto.decrypt("not-ciphertext"),
            Err(CryptoError::UnknownFormat)
        );
        assert_eq!(crypto.decrypt("v3:!!!"), Err(CryptoError::UnknownFormat));
    }

    #[test]
    fn debug_hides_the_key() {
        let shown = format!("{:?}", test_crypto());
        assert!(shown.contains("[redacted]"));
    }

    #[test]
    fn key_file_round_trip_and_missing_refusal() {
        let dir = std::env::temp_dir().join(format!(
            "dn-crypto-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let crypto = Crypto::load_or_generate(&dir).unwrap();
        let sealed = crypto.encrypt("x").unwrap();
        let reloaded = Crypto::load(&dir).unwrap();
        assert_eq!(reloaded.decrypt(&sealed).unwrap(), "x");
        assert!(matches!(
            Crypto::load(&dir.join("absent")),
            Err(CryptoError::KeyMissing { .. })
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
