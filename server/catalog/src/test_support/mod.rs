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

use std::sync::Arc;
use std::time::Duration;

use droppedneedle_kernel::db::statement::StatementFile;
use droppedneedle_kernel::platform::Platform;
use droppedneedle_providers::{Providers, RequestPriority};
use futures_util::future::BoxFuture;

use crate::artwork::cache::ArtworkCache;
use crate::artwork::covers::{CoverBytes, CoverOrigin};
use crate::artwork::local::LocalArtwork;
use crate::artwork::{ArtistImageCandidates, ArtistImageSource, ArtworkService};
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

/// An artwork service over `platform` and `providers` that knows no
/// artist images.
pub fn artwork(platform: &Platform, providers: &Providers) -> ArtworkService {
    ArtworkService::new(
        ArtworkCache::new(
            platform.config.cache_dir.join("covers"),
            1024 * 1024,
            Arc::clone(&platform.clock),
        ),
        LocalArtwork::new(platform.db.clone()),
        Arc::new(providers.coverart.clone()),
        platform.settings.live(),
        Arc::new(providers.images.clone()),
        Arc::new(NoArtistImages),
    )
}

struct NoArtistImages;

impl ArtistImageSource for NoArtistImages {
    fn candidates<'a>(
        &'a self,
        _artist_mbid: &'a str,
        _priority: RequestPriority,
    ) -> BoxFuture<'a, ArtistImageCandidates> {
        Box::pin(async { ArtistImageCandidates::default() })
    }

    fn fill<'a>(&'a self, _artist_mbid: &'a str) -> BoxFuture<'a, bool> {
        Box::pin(async { false })
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
