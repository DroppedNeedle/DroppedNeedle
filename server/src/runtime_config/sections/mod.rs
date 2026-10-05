//! Typed config sections without secrets (tier 2 of 2).
//!
//! Every user-editable runtime value lives in exactly one section struct
//! here (or in `secret_sections` when it holds secrets). Sections are plain
//! serde data with two hooks:
//!
//! - `validate()` runs on the save path and rejects bad submitted values
//!   with typed errors. It never heals: a save either lands or fails.
//! - `normalize()` runs on both paths and heals stored drift the way v2
//!   `__post_init__` did (unknown tier labels fall back, out-of-range
//!   optional bounds clear to `None`). Loading an old or hand-edited file
//!   never bricks the boot.
//!
//! Unknown JSON fields inside a section decode leniently (forward
//! compatibility); unknown section keys at the top level are reported by
//! `ConfigStore::unknown_top_level_keys` for boot diagnostics.

use super::error::ConfigError;

mod access;
mod acquisition;
mod library;
mod listening;
mod management;
mod musicbrainz;
mod plugins;

pub use access::*;
pub use acquisition::*;
pub use library::*;
pub use listening::*;
pub use management::*;
pub use musicbrainz::*;
pub use plugins::*;

/// One user-editable runtime section stored under a config-file key.
pub trait Section: Default + serde::Serialize + serde::de::DeserializeOwned {
    /// Config-file key, kept byte-identical to v2 for import fidelity.
    const KEY: &'static str;

    /// Reject bad submitted values with typed errors. Secret fields are
    /// never validated here: on the save path they may hold the mask.
    fn validate(&self) -> Result<(), ConfigError> {
        Ok(())
    }

    /// Heal stored drift and apply v2 `__post_init__` sanitization. Must be
    /// infallible. Must not touch secret fields, with one exception: the
    /// AudioDB blank-to-"123" default healing (v2 does it on every
    /// construction). The store runs `normalize` before masking on reads,
    /// so the healed default still masks.
    fn normalize(&mut self) {}
}

/// Marker for sections with no secret fields. Only these may use the plain
/// `ConfigStore::save`; secret sections must go through `save_secret` (and
/// indexers/plugins through their dedicated methods), so storing a secret
/// as plaintext is a compile error rather than a code-review catch.
/// `Plugins` is not plain, on purpose: its secret-flagged values need the
/// manifest's secret-key set at save time.
pub trait PlainSection: Section {}

impl PlainSection for UserPreferences {}
impl PlainSection for LibraryScanSchedule {}
impl PlainSection for FilesystemWatcher {}
impl PlainSection for WantedWatcher {}
impl PlainSection for SourcePriority {}
impl PlainSection for UsenetBackendSetting {}
impl PlainSection for ScrobbleSettings {}
impl PlainSection for PrimaryMusicSource {}
impl PlainSection for FreeMusic {}
impl PlainSection for GetIt {}
impl PlainSection for SecuritySettings {}
impl PlainSection for ConnectApps {}
impl PlainSection for DownloadPolicy {}
impl PlainSection for MusicBrainzSettings {}
impl PlainSection for LastFmSettings {}
impl PlainSection for LyricsSettings {}
impl PlainSection for InternalState {}
impl PlainSection for LibraryManagement {}

/// Reject `value` outside `min..=max` with a typed validation error.
pub fn check_range<T>(
    section: &'static str,
    field: &'static str,
    value: T,
    min: T,
    max: T,
) -> Result<(), ConfigError>
where
    T: PartialOrd + std::fmt::Display + Copy,
{
    if value < min || value > max {
        return Err(ConfigError::Validation {
            section,
            field,
            reason: format!("must be between {min} and {max}, got {value}"),
        });
    }
    Ok(())
}

/// `HH:MM` server-local time (`^([01]\d|2[0-3]):[0-5]\d$`), no regex crate.
#[must_use]
pub fn is_valid_hhmm(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 5 || bytes[2] != b':' {
        return false;
    }
    let digit = |index: usize| bytes[index].is_ascii_digit();
    if !(digit(0) && digit(1) && digit(3) && digit(4)) {
        return false;
    }
    let hour = (bytes[0] - b'0') * 10 + (bytes[1] - b'0');
    let minute = (bytes[3] - b'0') * 10 + (bytes[4] - b'0');
    hour <= 23 && minute <= 59
}

/// v2 URL normalization: strip, prepend `default_scheme` when schemeless,
/// drop trailing slashes.
pub fn normalize_http_url(url: &mut String, default_scheme: &str) {
    let trimmed = url.trim().to_owned();
    let mut owned = if trimmed.is_empty()
        || trimmed.starts_with("http://")
        || trimmed.starts_with("https://")
    {
        trimmed
    } else {
        format!("{default_scheme}{trimmed}")
    };
    while owned.ends_with('/') && owned.len() > 1 {
        owned.pop();
    }
    *url = owned;
}

/// Strip one pasted `/api/v1` or `/api` suffix (Prowlarr/Lidarr precedent).
pub fn strip_api_suffix(url: &mut String) {
    for suffix in ["/api/v1", "/api"] {
        if let Some(prefix) = url.strip_suffix(suffix) {
            let mut owned = prefix.to_owned();
            while owned.ends_with('/') && owned.len() > 1 {
                owned.pop();
            }
            *url = owned;
            return;
        }
    }
}

/// Keep only safe relative components (slskd subpath precedent): drop "",
/// ".", "..", and any drive/anchor, so the result can never escape the
/// mount it joins onto.
#[must_use]
pub fn sanitize_subpath(raw: &str) -> String {
    raw.split(['/', '\\'])
        .filter(|part| !part.is_empty() && *part != "." && *part != "..")
        .collect::<Vec<_>>()
        .join("/")
}

/// Absolute-container-path hygiene (slskd incomplete-mount precedent):
/// relative input or a bare "/" has no safe meaning, so it becomes "".
#[must_use]
pub fn sanitize_absolute_mount(raw: &str) -> String {
    let trimmed = raw.trim();
    if !trimmed.starts_with('/') {
        return String::new();
    }
    let cleaned = sanitize_subpath(trimmed);
    if cleaned.is_empty() {
        String::new()
    } else {
        format!("/{cleaned}")
    }
}

/// One typed validation failure.
fn validation(section: &'static str, field: &'static str, reason: String) -> ConfigError {
    ConfigError::Validation {
        section,
        field,
        reason,
    }
}
