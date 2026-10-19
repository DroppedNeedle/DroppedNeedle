//! Typed failures for the v3 settings store.
//!
//! Every variant names the section and field involved so a settings handler
//! can render a precise 4xx without ever interpolating secret material. No
//! variant carries plaintext secrets: crypto failures are fixed strings.

use std::path::PathBuf;

use thiserror::Error;

/// Every way typed-config load, validation, or save can fail.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConfigError {
    /// The `config.json` a first start would copy in cannot be read, or is
    /// not a JSON object. Nothing was written.
    #[error(
        "cannot read the settings file {}: {reason}. Fix it, or move it away to start with \
         defaults",
        .path.display()
    )]
    LegacyFile {
        /// The file.
        path: PathBuf,
        /// OS or decoder reason, never file contents.
        reason: String,
    },
    /// A present section does not match its schema.
    #[error("section {section:?} has the wrong shape: {reason}")]
    SectionDecode {
        /// Section key from the config file.
        section: &'static str,
        /// Decoder reason, never secret material.
        reason: String,
    },
    /// A submitted value fails schema validation. Returned, never panicked.
    #[error("invalid value for {section}.{field}: {reason}")]
    Validation {
        /// Section key being saved.
        section: &'static str,
        /// Field name within the section.
        field: &'static str,
        /// Plain-language reason, safe for 4xx bodies.
        reason: String,
    },
    /// The database was too busy, or shutting down, to take the write.
    /// Retryable; settings routes answer it as the server-busy 503.
    #[error("settings are busy: {0}")]
    Busy(String),
    /// Reading or writing the stored settings failed.
    #[error("settings storage failed: {0}")]
    Database(String),
    /// Secret decryption failed. Fixed strings only, never key material.
    #[error(transparent)]
    Crypto(#[from] super::crypto::CryptoError),
}

impl From<crate::db::DbError> for ConfigError {
    fn from(error: crate::db::DbError) -> Self {
        use crate::db::DbError;
        match error {
            DbError::Busy { .. } | DbError::TxTimeout { .. } | DbError::LaneClosed => {
                Self::Busy(error.to_string())
            }
            other => Self::Database(other.to_string()),
        }
    }
}

impl ConfigError {
    /// Whether the database was too busy (or stopping) to take the change:
    /// a retry may succeed, and a request answers it as the busy 503.
    #[must_use]
    pub fn is_busy(&self) -> bool {
        matches!(self, Self::Busy(_))
    }
}
