//! Dropped-section rejection.
//!
//! Six config keys from v2 have no v3 counterpart: the legacy catalog sync
//! (one-shot import only), transient scan hints (runtime-only), the
//! vestigial local-files section, the dead home section, the legacy Lidarr
//! backup (whose plaintext key never carries forward), and the top-level
//! Jellyfin URL mirror (the section URL wins). Addressing any of them is a
//! [`SettingsError::Dropped`] (410 Gone), never a silent ignore: a client
//! holding a v2-era key must learn it is gone, not believe it saved.

use super::error::SettingsError;

/// One dropped key with the reason it is gone.
pub struct DroppedKey {
    /// Former config-file key.
    pub key: &'static str,
    /// Why v3 has no such section; shown in the error message.
    pub reason: &'static str,
}

/// Every dropped section key.
pub const DROPPED: &[DroppedKey] = &[
    DroppedKey {
        key: "library_sync_settings",
        reason: "the v2 catalog sync is a one-shot import now",
    },
    DroppedKey {
        key: "library_scan_dirty_scopes",
        reason: "scan hints are runtime state",
    },
    DroppedKey {
        key: "local_files_settings",
        reason: "it had no effect",
    },
    DroppedKey {
        key: "home_settings",
        reason: "it had no effect",
    },
    DroppedKey {
        key: "_legacy_lidarr",
        reason: "the Lidarr backup key does not carry forward",
    },
    DroppedKey {
        key: "jellyfin_url",
        reason: "jellyfin_settings holds the Jellyfin URL",
    },
];

/// Reject a dropped section key with 410 Gone. Kept keys pass through.
pub fn ensure_kept_section(key: &str) -> Result<(), SettingsError> {
    match DROPPED.iter().find(|dropped| dropped.key == key) {
        Some(dropped) => Err(SettingsError::Dropped {
            message: format!(
                "The {:?} section was removed from v3 ({}); it cannot be read or saved.",
                dropped.key, dropped.reason
            ),
        }),
        None => Ok(()),
    }
}

/// True when `key` names a dropped section.
#[must_use]
pub fn is_dropped_section(key: &str) -> bool {
    DROPPED.iter().any(|dropped| dropped.key == key)
}
