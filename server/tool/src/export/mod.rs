//! v2 export: the versioned v2-to-v3 envelope, the passphrase-sealed
//! secrets inside it, the data bundle beside it, and the exporter that
//! builds both from a v2 instance dir.
//!
//! The validator (full section rules) and the importer (settings replace,
//! re-encryption, conflict merge, section carry, machine-readable report)
//! in [`crate::import`] build on these parsed types.

pub mod bundle;
pub mod envelope;
pub mod error;
pub mod exporter;
pub mod fernet;
pub mod inventory;
pub mod library_keys;
pub mod seal;
pub mod sections;
pub mod v2dir;

pub use envelope::{
    ApprovalRecord, Attachment, ConnectionRecord, EXPORT_FORMAT, EnvelopeWarning, ExportDoc,
    FORMAT_VERSION, FollowRecord, HashScheme, LeftBehind, MIN_FORMAT_VERSION, ParsedExport,
    ProviderRecord, REQUIRED_KEYS, RESERVED_SECTIONS, RecoveryCode, SealedValue, SecretEnvelope,
    UserRecord, derive_hash_scheme, parse_export,
};
pub use error::ExportError;
pub use exporter::{ExportRequest, bundle_path_for, export_v2, export_v2_to_file, utc_now_rfc3339};
pub use seal::{Opener, SealError, Sealer, secret_envelope_params, unseal};
