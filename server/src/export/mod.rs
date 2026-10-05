//! v2 export: the versioned v2-to-v3 envelope, the passphrase-sealed
//! secrets inside it, and the exporter that builds one from a v2 instance
//! dir.
//!
//! The validator (full section rules) and the importer (settings replace,
//! re-encryption, conflict merge, atomic commit, machine-readable report)
//! in [`crate::import`] build on these parsed types.

pub mod envelope;
pub mod error;
pub mod exporter;
pub mod fernet;
pub mod seal;
pub mod v2dir;

pub use envelope::{
    ApprovalRecord, EXPORT_FORMAT, EnvelopeWarning, ExportDoc, FORMAT_VERSION, FollowRecord,
    HashScheme, ParsedExport, ProviderRecord, REQUIRED_KEYS, RESERVED_SECTIONS, RecoveryCode,
    SealedValue, SecretEnvelope, UserRecord, derive_hash_scheme, parse_export,
};
pub use error::ExportError;
pub use exporter::{ExportRequest, export_v2, export_v2_to_file, utc_now_rfc3339};
pub use seal::{Opener, SealError, Sealer, secret_envelope_params, unseal};
