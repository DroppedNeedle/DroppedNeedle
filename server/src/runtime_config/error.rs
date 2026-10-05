//! Typed failures for the v3 config system.
//!
//! Every variant names the section and field involved so a settings handler
//! can render a precise 4xx without ever interpolating secret material. No
//! variant carries plaintext secrets: crypto failures are fixed strings.

use std::path::PathBuf;

use thiserror::Error;

/// Every way typed-config load, validation, or save can fail.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConfigError {
    /// The config file cannot be read (missing file is not this error; a
    /// missing file means every section reads as its default).
    #[error("cannot read config file {}: {reason}", .path.display())]
    ReadFailed {
        /// File that could not be read.
        path: PathBuf,
        /// OS reason, never secret material.
        reason: String,
    },
    /// The config file cannot be written durably.
    #[error("cannot write config file {}: {reason}", .path.display())]
    WriteFailed {
        /// File that could not be written.
        path: PathBuf,
        /// OS reason, never secret material.
        reason: String,
    },
    /// The config file is not a JSON object.
    #[error("config file is not a JSON object: {reason}")]
    InvalidJson {
        /// Decoder reason, never secret material.
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
    /// The store mutex is poisoned (a previous holder panicked while holding
    /// it). Unrecoverable without restart; surfaced, never swallowed.
    #[error("config lock unavailable: {0}")]
    LockUnavailable(&'static str),
    /// Secret decryption failed. Fixed strings only, never key material.
    #[error(transparent)]
    Crypto(#[from] super::crypto::CryptoError),
}
