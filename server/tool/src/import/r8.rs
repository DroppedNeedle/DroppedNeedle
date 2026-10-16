//! One-shot scan schedule carry: `sync_frequency` → `scan_frequency`.
//!
//! The v2 `library_sync_settings` section is dropped: it
//! never appears in the export stream. Instead the importer reads the v2
//! `config.json` directly and, whenever the export lacks
//! `library_scan_schedule`, carries the old interval across. Judging on
//! the export alone keeps re-imports convergent. This mirrors the v2
//! getter shim in its preferences service (`get_library_scan_schedule`).

use std::path::Path;

use serde_json::Value;
use thiserror::Error;

/// v2 `sync_frequency` values; a strict subset of v3 `scan_frequency`.
const KNOWN_FREQUENCIES: &[&str] = &[
    "manual", "5min", "10min", "30min", "1hr", "6hr", "12hr", "24hr", "3d", "7d",
];

/// Every way the schedule carry can fail. A missing or unreadable v2 file is
/// not an error: the carry simply does not happen.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum R8Error {
    /// The v2 config file is present but unparseable.
    #[error("v2 config is not valid JSON: {reason}")]
    InvalidJson {
        /// Serde failure reason.
        reason: String,
    },
    /// The v2 config file cannot be read.
    #[error("cannot read v2 config: {reason}")]
    ReadFailed {
        /// OS reason.
        reason: String,
    },
    /// The v2 config names a different instance than the export file.
    #[error(
        "INSTANCE_MISMATCH: export names {export_instance:?} but the v2 config holds {v2_instance:?}"
    )]
    InstanceMismatch {
        /// `instance_id` from the export file.
        export_instance: String,
        /// `instance_id` from the v2 config.
        v2_instance: String,
    },
}

/// Read the v2 interval from an already-parsed v2 `config.json` value.
/// Returns `None` when the section or key is absent, or when the value
/// is not a known interval (an unknown value must never poison the
/// schedule; the v3 default stands).
#[must_use]
pub fn read_sync_frequency(v2_config: &Value) -> Option<String> {
    let frequency = v2_config
        .get("library_sync_settings")?
        .get("sync_frequency")?
        .as_str()?;
    if KNOWN_FREQUENCIES.contains(&frequency) {
        Some(frequency.to_owned())
    } else {
        None
    }
}

/// Carry the v2 interval when the export file lacks the new schedule
/// key. Judged on the export alone so re-imports converge instead of
/// flip-flopping; the export wins whenever it carries the section.
/// A missing v2 file returns `None`. A v2 config naming a different
/// instance than the export is refused: the frequency would belong to
/// someone else's schedule. A v2 config without an `instance_id` skips
/// the check (minimal hand-made carry files).
pub fn carry_frequency(
    v2_config_path: &Path,
    schedule_present: bool,
    export_instance_id: &str,
) -> Result<Option<String>, R8Error> {
    if schedule_present {
        return Ok(None);
    }
    let text = match std::fs::read_to_string(v2_config_path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(R8Error::ReadFailed {
                reason: error.to_string(),
            });
        }
    };
    let parsed: Value = serde_json::from_str(&text).map_err(|error| R8Error::InvalidJson {
        reason: error.to_string(),
    })?;
    if let Some(v2_instance) = parsed.get("instance_id").and_then(Value::as_str)
        && !v2_instance.is_empty()
        && v2_instance != export_instance_id
    {
        return Err(R8Error::InstanceMismatch {
            export_instance: export_instance_id.to_owned(),
            v2_instance: v2_instance.to_owned(),
        });
    }
    Ok(read_sync_frequency(&parsed))
}
