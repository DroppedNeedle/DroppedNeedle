//! What tests need from the catalog and production never uses: the
//! statement registry, scripted cover art, releases and wrapped data,
//! enrichment with no provider, and a concerts sweep that does not pause.
//!
//! Compiled for this crate's tests and for the `test-support` feature only.

mod covers;
mod search;
mod version;
mod wrapped;

pub use covers::FakeCoverArt;
pub use search::UnconfiguredEnrichment;
pub use version::FakeReleases;
pub use wrapped::FakeWrappedData;

use std::time::Duration;

use droppedneedle_kernel::db::statement::StatementFile;

use crate::artwork::covers::{CoverBytes, CoverOrigin};
use crate::concerts::sweep::ConcertsSweep;

/// Every `sql.rs` of this crate, one line each, for the schema test.
pub fn statement_files() -> Vec<StatementFile> {
    vec![
        crate::artwork::sql::registered(),
        crate::collections::sql::registered(),
        crate::concerts::sql::registered(),
        crate::discover::sql::registered(),
        crate::local::sql::registered(),
        crate::search::sql::registered(),
        crate::youtube::sql::registered(),
    ]
}

impl CoverBytes {
    /// Build one remote result, hashing the bytes.
    pub fn new(bytes: Vec<u8>, content_type: &str, source: &str) -> Self {
        Self {
            hash: crate::artwork::cache::sha256_hex(&bytes),
            bytes,
            content_type: content_type.to_owned(),
            source: source.to_owned(),
            origin: CoverOrigin::Remote,
            version: None,
        }
    }
}

impl ConcertsSweep {
    /// Skip the pause between artists (tests).
    #[must_use]
    pub fn without_delay(mut self) -> Self {
        self.inter_artist_delay = Duration::ZERO;
        self
    }
}
