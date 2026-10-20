//! The library's outside world as tests script it: MusicBrainz and AcoustID
//! as identification sees them, Discogs and MusicBrainz as contributions see
//! them, and library bundles built by the production builder over them.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use droppedneedle::bootstrap::Port;
use droppedneedle_acquire::library_removal::AcquireAlbumCleanup;
use droppedneedle_acquire::wiring::AcquireStores;
use droppedneedle_kernel::db::writer::Lane;
use droppedneedle_kernel::db::{Db, DbError};
use droppedneedle_kernel::platform::Platform;
use droppedneedle_library::contrib::ContribProviders;
use droppedneedle_library::contrib::scripted::{ScriptedDiscogs, ScriptedMusicBrainz};
use droppedneedle_library::identify::models::{LocalAlbumFacts, RecallResult};
use droppedneedle_library::identify::providers::{FakeProviders, IdentifyProviders, RecallOutcome};
use droppedneedle_library::identify::sources::{
    ArtistAnswer, EditionPage, EditionQuery, SourceError, Unanswered,
};
use droppedneedle_library::management::enrich::EnrichmentPort;
use droppedneedle_library::scan::fs::FsCoordinator;
use droppedneedle_library::wiring::{LibraryPorts, LibrarySetup};
use droppedneedle_providers::Providers;
use droppedneedle_providers::musicbrainz::Criticality;

type Recall = Box<dyn Fn(&LocalAlbumFacts) -> RecallResult + Send>;

/// The identification port of a [`TestApp`](super::TestApp), answering from
/// what the journey scripted. Nothing is found until it scripts something.
pub struct ScriptedIdentify {
    recall: Mutex<Recall>,
    editions: Mutex<EditionPage>,
    /// What each artist id answers, and every id asked about.
    artists: Mutex<HashMap<String, ArtistAnswer>>,
    artists_asked: Mutex<Vec<String>>,
}

impl ScriptedIdentify {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            recall: Mutex::new(Box::new(|_| RecallResult::default())),
            editions: Mutex::new(EditionPage::default()),
            artists: Mutex::new(HashMap::new()),
            artists_asked: Mutex::new(Vec::new()),
        })
    }

    /// Answer a lookup of artist id `mbid` with `answer`. An id with no
    /// answer scripted gets none.
    pub fn set_artist(&self, mbid: &str, answer: ArtistAnswer) {
        self.artists
            .lock()
            .expect("artist script")
            .insert(mbid.to_owned(), answer);
    }

    /// Every artist id looked up so far, in order.
    pub fn artists_asked(&self) -> Vec<String> {
        self.artists_asked.lock().expect("artist asks").clone()
    }

    /// Answer every recall with `result`.
    pub fn set_recall(&self, result: RecallResult) {
        self.recall_from(move |_| result.clone());
    }

    /// Answer each recall from the album it is asked about. A journey sets
    /// this before the scan, so the identification the scan queues finds it
    /// whenever the loop gets to it.
    pub fn recall_from(&self, recall: impl Fn(&LocalAlbumFacts) -> RecallResult + Send + 'static) {
        *self.recall.lock().expect("recall script") = Box::new(recall);
    }

    /// Answer every edition search with `page`.
    pub fn set_editions(&self, page: EditionPage) {
        *self.editions.lock().expect("edition script") = page;
    }

    fn recall(&self, facts: &LocalAlbumFacts) -> RecallResult {
        (self.recall.lock().expect("recall script"))(facts)
    }
}

fn outcome(result: RecallResult) -> Pin<Box<dyn Future<Output = RecallOutcome> + Send + 'static>> {
    Box::pin(async move {
        RecallOutcome {
            result,
            recall_criticality: Some(Criticality::IdentityCritical),
        }
    })
}

impl IdentifyProviders for ScriptedIdentify {
    fn recall_candidates(
        &self,
        facts: &LocalAlbumFacts,
    ) -> Pin<Box<dyn Future<Output = RecallOutcome> + Send + '_>> {
        outcome(self.recall(facts))
    }

    fn recall_release(
        &self,
        facts: &LocalAlbumFacts,
        release_mbid: &str,
    ) -> Pin<Box<dyn Future<Output = RecallOutcome> + Send + '_>> {
        let mut result = self.recall(facts);
        result
            .releases
            .retain(|release| release.answers_to(release_mbid));
        outcome(result)
    }

    fn search_editions(
        &self,
        _query: EditionQuery,
    ) -> Pin<Box<dyn Future<Output = Result<EditionPage, SourceError>> + Send + '_>> {
        let page = self.editions.lock().expect("edition script").clone();
        Box::pin(async move { Ok(page) })
    }

    fn canonical_artist(
        &self,
        mbid: &str,
    ) -> Pin<Box<dyn Future<Output = Result<ArtistAnswer, Unanswered>> + Send + '_>> {
        self.artists_asked
            .lock()
            .expect("artist asks")
            .push(mbid.to_owned());
        let answer = self
            .artists
            .lock()
            .expect("artist script")
            .get(mbid)
            .cloned();
        Box::pin(async move { answer.ok_or_else(Unanswered::cannot_tell) })
    }
}

/// Discogs and MusicBrainz as contributions see them, scripted by a test.
/// Nothing is found until it scripts something.
#[derive(Clone, Default)]
pub struct ScriptedContrib {
    pub discogs: Arc<ScriptedDiscogs>,
    pub musicbrainz: Arc<ScriptedMusicBrainz>,
}

impl ScriptedContrib {
    /// The contribution port these scripts answer through.
    pub fn providers(&self) -> ContribProviders {
        ContribProviders {
            discogs: self.discogs.clone(),
            musicbrainz: self.musicbrainz.clone(),
        }
    }
}

/// A library bundle built the way the app builds it, over the platform's
/// database and settings, with identification answering from scripted
/// providers. Returns the bundle and those providers.
pub fn library_at(platform: &Platform) -> (LibrarySetup, Arc<FakeProviders>) {
    let scripted = Arc::new(FakeProviders::default());
    let library = library_asking(platform, scripted.clone());
    (library, scripted)
}

/// As [`library_at`], with identification asking `identify`. Contributions
/// answer from scripts with nothing scripted, Library Management has no
/// outside sources, and every other outside address is closed.
pub fn library_asking(platform: &Platform, identify: Arc<dyn IdentifyProviders>) -> LibrarySetup {
    library_with(platform, identify, None)
}

/// As [`library_asking`], with Library Management planning through
/// `enrichment`. Album removals clean up acquisition's rows as in the app.
pub fn library_with(
    platform: &Platform,
    identify: Arc<dyn IdentifyProviders>,
    enrichment: Option<Arc<dyn EnrichmentPort>>,
) -> LibrarySetup {
    let mut outbound = super::closed_outbound();
    outbound.identify_providers = Port::given(identify);
    outbound.contribution_providers = Port::given(ScriptedContrib::default().providers());
    let providers = Arc::new(Providers::unpaced(Arc::new(
        droppedneedle_providers::executor::ScriptedWire::new(),
    )));
    let mb_source = providers.musicbrainz.live_source(platform.settings.live());
    LibrarySetup::build(
        platform,
        providers,
        mb_source,
        droppedneedle_kernel::jobs::queue::JobQueue::new(
            platform.db.clone(),
            Arc::new(droppedneedle_kernel::clock::SystemClock),
        ),
        outbound.library(),
        LibraryPorts {
            removal: Arc::new(AcquireAlbumCleanup::new(&AcquireStores::open(&platform.db))),
            enrichment,
            scan_completed: Arc::new(|| {}),
            fs: FsCoordinator::new(),
            download_trees: droppedneedle_acquire::wiring::recycle_guard(
                &platform.settings,
                platform.config.imports_dir(),
            ),
        },
    )
    .expect("library bundle builds")
}

/// Raw SQL on a test library's database: seeds no store method writes
/// (legacy revisions, bare catalog albums) and assertions on stored rows.
pub trait TestSql {
    /// Run `sql` (any number of statements) in one write.
    fn run_sql(&self, sql: &str) -> Result<(), DbError>;
    /// One integer from `sql`.
    fn query_i64(&self, sql: &str) -> Result<i64, DbError>;
}

impl TestSql for Db {
    fn run_sql(&self, sql: &str) -> Result<(), DbError> {
        self.write_blocking(Lane::Foreground, "test_sql", |tx| {
            tx.execute_batch(sql).map_err(DbError::from)
        })
    }

    fn query_i64(&self, sql: &str) -> Result<i64, DbError> {
        self.read_blocking("test_sql", |conn| {
            conn.query_row(sql, [], |row| row.get(0))
                .map_err(DbError::from)
        })
    }
}
