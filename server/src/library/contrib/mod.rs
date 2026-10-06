//! Library contributions: draft lifecycle, MusicBrainz submission, and the
//! verification worker.
//!
//! A contribution carries one local album toward MusicBrainz: the curator
//! shapes a [`models::ReleaseDraft`], checks duplicates, opens the seeded
//! release editor in their browser, and the verification worker confirms the
//! resulting release and links the album.
//!
//! Ported from v2's library contribution service, its verification worker,
//! and its contribution models. Intended quirks keep `Quirk (v2 ...)`
//! citations at the code that preserves them.
//!
//! Boundaries (see `seams` for the traits):
//! - Identity reads, the evidence engine, and catalog invalidation are
//!   owned elsewhere; contributions consume them through narrow traits.
//! - Provider contact is reads-only with explicit priorities; the submission
//!   itself is a browser POST to the MusicBrainz release editor, which sends
//!   the curator back to the callback route - no live provider writes, ever.
//! - State lives in SQLite ([`sqlite::SqliteContributions`]), which also
//!   reads the album identification context from the catalog.
//! - [`worker::spawn_verification_worker`] runs the background loop;
//!   `LibrarySetup::spawn_loops` starts it.

pub mod error;
#[cfg(any(test, feature = "test-support"))]
pub mod memory;
pub mod models;
pub mod reasons;
pub mod rules;
pub mod seams;
pub mod service;
pub mod sqlite;
pub mod worker;

use std::path::Path;
use std::sync::Arc;

/// The provider reads contributions run against: live clients in
/// production, scripted ones in test bundles.
#[derive(Clone)]
pub struct ContribProviders {
    pub discogs: Arc<dyn seams::DiscogsContrib>,
    pub musicbrainz: Arc<dyn seams::MusicBrainzContrib>,
}

/// Build the contribution service and its verification worker over the
/// application database.
pub fn assemble(
    db_path: &Path,
    providers: ContribProviders,
) -> Result<
    (
        Arc<service::ContributionService>,
        Arc<worker::VerificationWorker>,
    ),
    String,
> {
    let store = Arc::new(
        sqlite::SqliteContributions::open(db_path)
            .map_err(|error| format!("contribution store: {error}"))?,
    );
    let service = Arc::new(
        service::ContributionService::new(
            store.clone(),
            store.clone(),
            Arc::new(super::adapters::MinimalAttachmentEvidence),
            Arc::new(seams::SystemClock),
        )
        .with_discogs(providers.discogs)
        .with_musicbrainz(providers.musicbrainz.clone())
        .with_catalog(Arc::new(super::adapters::NoopContributionCatalog)),
    );
    let worker = Arc::new(worker::VerificationWorker::new(
        service.clone(),
        providers.musicbrainz,
        store,
        worker::VerificationWorkerConfig::default(),
    ));
    Ok((service, worker))
}
