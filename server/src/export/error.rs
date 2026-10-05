//! Typed failures for the v2 export.
//!
//! Every variant maps to a SCREAMING_SNAKE code. Messages carry key names and
//! paths (the operator supplied them) but never secret material, passphrases,
//! or plaintext.

use std::path::PathBuf;

use thiserror::Error;

/// Every way envelope parsing or v2 export can fail.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ExportError {
    /// `format` is not `droppedneedle-export`.
    #[error("unknown export format: expected 'droppedneedle-export'")]
    UnsupportedFormat,
    /// `format_version` is anything but 1.
    #[error("unsupported export format version: expected 1")]
    UnsupportedFormatVersion,
    /// A required top-level key is absent.
    #[error("export is missing required key '{key}'")]
    MissingRequiredKey {
        /// The absent key.
        key: String,
    },
    /// The file is not JSON, not an object, or a section has the wrong shape.
    #[error("export file is not a valid envelope: {reason}")]
    InvalidEnvelope {
        /// What failed, without file contents.
        reason: String,
    },
    /// The v2 key file is absent. The exporter refuses rather than minting:
    /// a fresh key would silently turn stored ciphertext into "legacy
    /// plaintext", which is exactly how v2 corrupted credentials on key loss.
    #[error("v2 key file {} is missing; refusing to export", .path.display())]
    V2KeyNotFound {
        /// Expected key file location.
        path: PathBuf,
    },
    /// The v2 key file exists but holds no usable `DATA_ENC_KEY`.
    #[error("v2 key file holds no usable DATA_ENC_KEY")]
    V2KeyInvalid,
    /// A stored v2 token does not open under the v2 key: the key file holds
    /// a different key than the one v2 encrypted with.
    #[error("v2 secret at {field} does not open under the v2 key; check DATA_ENC_KEY")]
    V2KeyMismatch {
        /// Which stored value failed, never its contents.
        field: String,
    },
    /// The v2 config file cannot be read.
    #[error("cannot read v2 config {}: {reason}", .path.display())]
    V2ConfigUnreadable {
        /// Config file location.
        path: PathBuf,
        /// OS reason.
        reason: String,
    },
    /// The v2 config file is not a JSON object.
    #[error("v2 config is not a JSON object")]
    V2ConfigInvalid,
    /// The v2 config carries no instance id. There is no verbatim value to
    /// export, and minting one would fork the instance identity, so refuse.
    #[error("v2 config has no instance_id")]
    InstanceIdMissing,
    /// A v2 database read failed. The table name is fixed at the call site;
    /// the detail never carries row contents.
    #[error("cannot read v2 table '{table}': {detail}")]
    V2Database {
        /// Table being read.
        table: String,
        /// Driver summary.
        detail: String,
    },
    /// Sealing failed. The cause keeps its own code; this preserves it.
    #[error(transparent)]
    Seal(#[from] crate::export::seal::SealError),
    /// The export document cannot be written out.
    #[error("cannot write export file: {reason}")]
    ExportWrite {
        /// OS or serialization reason.
        reason: String,
    },
    /// The export timestamp cannot be rendered.
    #[error("cannot render export timestamp")]
    Timestamp,
}

impl ExportError {
    /// Machine-readable code for reports and CLI exits.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnsupportedFormat => "UNSUPPORTED_FORMAT",
            Self::UnsupportedFormatVersion => "UNSUPPORTED_FORMAT_VERSION",
            Self::MissingRequiredKey { .. } => "MISSING_REQUIRED_KEY",
            Self::InvalidEnvelope { .. } => "INVALID_ENVELOPE",
            Self::V2KeyNotFound { .. } => "V2_KEY_NOT_FOUND",
            Self::V2KeyInvalid => "V2_KEY_INVALID",
            Self::V2KeyMismatch { .. } => "V2_KEY_MISMATCH",
            Self::V2ConfigUnreadable { .. } => "V2_CONFIG_UNREADABLE",
            Self::V2ConfigInvalid => "V2_CONFIG_INVALID",
            Self::InstanceIdMissing => "MISSING_INSTANCE_ID",
            Self::V2Database { .. } => "V2_DATABASE_ERROR",
            Self::Seal(inner) => inner.code(),
            Self::ExportWrite { .. } => "EXPORT_WRITE_FAILED",
            Self::Timestamp => "TIMESTAMP_FAILED",
        }
    }
}
