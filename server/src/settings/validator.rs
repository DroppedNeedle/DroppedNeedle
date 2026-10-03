//! Dropped-section rejection (D2/D3/D4/D5/D8/D15).
//!
//! Six config keys from v2 have no v3 counterpart: the legacy catalog sync
//! (one-shot import only), transient scan hints (runtime-only), the
//! vestigial local-files section, the dead home section, the legacy Lidarr
//! backup (whose plaintext key never carries forward), and the top-level
//! Jellyfin URL mirror (the section URL wins). Addressing any of them is a
//! [`SettingsError::Dropped`] (410 Gone), never a silent ignore: a client
//! holding a v2-era key must learn it is gone, not believe it saved.

use super::error::SettingsError;

/// One dropped key with its decision ref.
pub struct DroppedKey {
    /// Former config-file key.
    pub key: &'static str,
    /// Decision ref (D-list, R-answer, or export spec section).
    pub decision: &'static str,
}

/// Every dropped section key. Mirrors the stage-2 registry; the message
/// names the decision so the caller can look it up.
pub const DROPPED: &[DroppedKey] = &[
    DroppedKey {
        key: "library_sync_settings",
        decision: "D2/R8",
    },
    DroppedKey {
        key: "library_scan_dirty_scopes",
        decision: "D15",
    },
    DroppedKey {
        key: "local_files_settings",
        decision: "D3",
    },
    DroppedKey {
        key: "home_settings",
        decision: "D4",
    },
    DroppedKey {
        key: "_legacy_lidarr",
        decision: "D5",
    },
    DroppedKey {
        key: "jellyfin_url",
        decision: "D8",
    },
];

/// Reject a dropped section key with 410 Gone. Kept keys pass through.
pub fn ensure_kept_section(key: &str) -> Result<(), SettingsError> {
    match DROPPED.iter().find(|dropped| dropped.key == key) {
        Some(dropped) => Err(SettingsError::Dropped {
            message: format!(
                "The {:?} section was removed from v3 ({}); it cannot be read or saved.",
                dropped.key, dropped.decision
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
