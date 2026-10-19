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
//! - [`worker::start_verification`] runs the background loop;
//!   `LibrarySetup::spawn_loops` starts it.

pub mod error;
pub mod models;
pub mod reasons;
pub mod rules;
#[cfg(any(test, feature = "test-support"))]
pub mod scripted;
pub mod seams;
pub mod service;
pub(crate) mod sql;
pub mod sqlite;
pub mod worker;

use std::sync::Arc;

use droppedneedle_kernel::db::Db;

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
    db: &Db,
    providers: ContribProviders,
    catalog: Arc<dyn seams::ContributionCatalog>,
) -> (
    Arc<service::ContributionService>,
    Arc<worker::VerificationWorker>,
) {
    let store = Arc::new(sqlite::SqliteContributions::new(db.clone()));
    let service = Arc::new(
        service::ContributionService::new(
            store,
            Arc::new(super::adapters::MatchingAttachmentEvidence),
            Arc::new(seams::SystemClock),
        )
        .with_discogs(providers.discogs)
        .with_musicbrainz(providers.musicbrainz.clone())
        .with_catalog(catalog),
    );
    let worker = Arc::new(worker::VerificationWorker::new(
        service.clone(),
        providers.musicbrainz,
        worker::VerificationWorkerConfig::default(),
    ));
    (service, worker)
}
