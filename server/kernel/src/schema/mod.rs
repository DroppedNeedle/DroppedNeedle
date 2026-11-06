//! The schema this binary carries: every file in `server/migrations`,
//! embedded at compile time and applied at boot by the runner in
//! [`crate::db::migrate`] before anything serves.
//!
//! The list starts with the v3 baseline. A schema change is a new file with
//! the next number, added here in order; a file on `v3` is never edited,
//! renumbered or removed.

use crate::db::migrate::{Chain, Migration};

macro_rules! migration {
    ($version:literal, $file:literal) => {
        Migration {
            version: $version,
            name: $file,
            sql: include_str!(concat!("../../../migrations/", $file)),
        }
    };
}

/// Every migration, the baseline first.
pub static MIGRATIONS: &[Migration<'static>] = &[
    migration!(135, "0135_baseline.sql"),
    migration!(136, "0136_acquire_held_reasons.sql"),
    migration!(137, "0137_library_scan_indexes.sql"),
    migration!(138, "0138_lookup_indexes.sql"),
    migration!(139, "0139_library_album_listing_revision.sql"),
    migration!(140, "0140_artist_join_undo.sql"),
    migration!(141, "0141_import_folder.sql"),
];

/// The stamp of a database made by the migration chain the baseline
/// replaced, at that chain's last number. The runner's bridge moves such a
/// database onto the baseline.
pub const BRIDGE_FROM: i64 = 134;

/// The chain the runner applies at boot, to the test template and in the
/// offline tool.
pub static CHAIN: Chain<'static> = Chain::new(MIGRATIONS, Some(BRIDGE_FROM));

/// The newest migration's number: the stamp of a migrated database.
#[must_use]
pub fn latest_version() -> i64 {
    CHAIN.latest()
}
