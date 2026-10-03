//! Export file parsing and sealed-secret unlocking (import side).
//!
//! The envelope shape and all sealing crypto live in the export slice
//! (`crate::export::envelope`, `crate::export::seal`); this module only
//! adapts them to the importer's needs: parse the file, expose the raw
//! document the validator and the merge walk, and open sealed values
//! under the operator passphrase.
//!
//! Unlock cost note: each sealed value re-derives the Argon2id key
//! (the 64 MiB pinned parameters), so large secret counts take seconds.
//! That is acceptable for a one-shot migration tool and keeps a single
//! KDF implementation.

use serde_json::Value;
use thiserror::Error;

use crate::export::envelope::SecretEnvelope;
use crate::export::seal::{self, SealError};

pub use crate::export::envelope::{
    EXPORT_FORMAT, FORMAT_VERSION as EXPORT_FORMAT_VERSION, RESERVED_SECTIONS,
};

/// Wire constants, single-sourced from the export slice so the two sides
/// cannot drift. Names stay import-flavored for the existing callers.
pub use crate::export::seal::{
    ENVELOPE_NONCE_LEN, MIN_SEALED_BLOB_LEN as MIN_SEALED_LEN, SALT_LEN as ENVELOPE_SALT_LEN,
    SEALED_KEY,
};

/// Every way import-side envelope handling can fail. No variant carries
/// passphrase, key, blob, or plaintext material.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum EnvelopeError {
    /// The file is not valid JSON.
    #[error("export file is not valid JSON: {reason}")]
    InvalidJson {
        /// Serde failure reason.
        reason: String,
    },
    /// The JSON root is not an object.
    #[error("export root must be a JSON object")]
    RootNotObject,
    /// The `secret_envelope` block is missing or malformed.
    #[error("secret_envelope is missing or malformed: {reason}")]
    BadEnvelope {
        /// What was wrong with the block.
        reason: String,
    },
    /// A sealed value does not open: wrong passphrase or corrupt data.
    #[error("cannot unlock sealed value at {path}: wrong passphrase or corrupt data")]
    UnlockFailed {
        /// JSON path of the value that failed to open.
        path: String,
    },
    /// A sealed value is malformed (not an object, bad base64, too short).
    #[error("sealed value at {path} is malformed")]
    MalformedSealed {
        /// JSON path of the bad value.
        path: String,
    },
}

/// A parsed export file: the raw document plus its secret envelope.
#[derive(Debug, Clone)]
pub struct ExportFile {
    /// The full parsed document; the validator and the merge read views.
    pub root: Value,
    /// The secret-envelope parameters for unlocking.
    pub envelope: SecretEnvelope,
}

impl ExportFile {
    /// Parse raw export bytes. Structural and semantic checks belong to
    /// the validator; this only proves the JSON parses and the envelope
    /// block deserializes. This plus `validate_export` is the import path;
    /// `export::parse_export` is a stricter typed convenience for tests.
    pub fn parse(bytes: &[u8]) -> Result<Self, EnvelopeError> {
        let root: Value =
            serde_json::from_slice(bytes).map_err(|error| EnvelopeError::InvalidJson {
                reason: error.to_string(),
            })?;
        if !root.is_object() {
            return Err(EnvelopeError::RootNotObject);
        }
        let envelope_value =
            root.get("secret_envelope")
                .ok_or_else(|| EnvelopeError::BadEnvelope {
                    reason: "missing secret_envelope".to_owned(),
                })?;
        let envelope: SecretEnvelope =
            serde_json::from_value(envelope_value.clone()).map_err(|error| {
                EnvelopeError::BadEnvelope {
                    reason: error.to_string(),
                }
            })?;
        Ok(Self { root, envelope })
    }
}

/// Open one sealed value under the operator passphrase. `sealed` is the
/// `{ "$sealed": ... }` object; `path` names it for error reporting only.
pub fn unseal_value(
    passphrase: &str,
    envelope: &SecretEnvelope,
    sealed: &Value,
    path: &str,
) -> Result<String, EnvelopeError> {
    let blob = sealed
        .get(SEALED_KEY)
        .and_then(Value::as_str)
        .ok_or_else(|| EnvelopeError::MalformedSealed {
            path: path.to_owned(),
        })?;
    seal::unseal(passphrase, envelope, blob).map_err(|error| match error {
        SealError::AuthFailed => EnvelopeError::UnlockFailed {
            path: path.to_owned(),
        },
        SealError::InvalidBlob => EnvelopeError::MalformedSealed {
            path: path.to_owned(),
        },
        SealError::InvalidEnvelope
        | SealError::KdfFailed
        | SealError::RandomFailed
        | SealError::SealFailed => EnvelopeError::BadEnvelope {
            reason: "secret envelope failed before unlock".to_owned(),
        },
    })
}

/// True when `value` is a sealed-secret object (`{"$sealed": ...}`).
#[must_use]
pub fn is_sealed(value: &Value) -> bool {
    value
        .as_object()
        .is_some_and(|object| object.contains_key(SEALED_KEY))
}

/// Collect every sealed object under `node`, paired with its JSON path.
/// Paths use dotted keys with `[n]` indexes (`settings.indexers[0]`).
pub fn collect_sealed(node: &Value, path: String, out: &mut Vec<(String, Value)>) {
    if is_sealed(node) {
        out.push((path, node.clone()));
        return;
    }
    match node {
        Value::Object(object) => {
            for (key, child) in object {
                let child_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                collect_sealed(child, child_path, out);
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                collect_sealed(child, format!("{path}[{index}]"), out);
            }
        }
        _ => {}
    }
}
