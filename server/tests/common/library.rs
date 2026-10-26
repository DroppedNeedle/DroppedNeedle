//! The library's outside world as tests script it: MusicBrainz and AcoustID
//! as identification sees them, Discogs and MusicBrainz as contributions see
//! them, and library bundles built by the production builder over them.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use droppedneedle::bootstrap::Port;
use droppedneedle_kernel::db::writer::Lane;
use droppedneedle_kernel::db::{Db, DbError};
use droppedneedle_kernel::platform::Platform;
use droppedneedle_library::contrib::ContribProviders;
use droppedneedle_library::contrib::scripted::{ScriptedDiscogs, ScriptedMusicBrainz};
use droppedneedle_library::identify::models::{LocalAlbumFacts, RecallResult};
use droppedneedle_library::identify::providers::{FakeProviders, IdentifyProviders, RecallOutcome};
use droppedneedle_library::identify::sources::{
    ArtistAnswer, EditionPage, EditionQuery, Prioritised, ReleaseAnswer, ReleaseSource,
    SourceError, Unanswered,
};
use droppedneedle_library::management::enrich::EnrichmentPort;
use droppedneedle_library::operations::service::Operations;
use droppedneedle_library::repairs::service::RepairWorker;
use droppedneedle_library::wiring::{LibraryCore, LibraryPorts, LibraryServices, LibrarySetup};
use droppedneedle_providers::Providers;
use droppedneedle_providers::musicbrainz::Criticality;

type Recall = Box<dyn Fn(&LocalAlbumFacts) -> RecallResult + Send>;

/// One scripted release source answering at every priority.
pub fn every_priority(source: Arc<dyn ReleaseSource>) -> Prioritised<Arc<dyn ReleaseSource>> {
    Prioritised::new(source.clone(), source)
}

/// The identification port of a [`TestApp`](super::TestApp), answering from
/// what the journey scripted. Nothing is found until it scripts something.
pub struct ScriptedIdentify {
    recall: Mutex<Recall>,
    editions: Mutex<EditionPage>,
    /// What each artist id answers, and every id asked about.
    artists: Mutex<HashMap<String, ArtistAnswer>>,
    artists_asked: Mutex<Vec<String>>,
    /// What each release document lookup answers, and every id asked about.
    documents: Mutex<HashMap<String, ReleaseAnswer>>,
    documents_asked: Mutex<Vec<String>>,
}

impl ScriptedIdentify {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            recall: Mutex::new(Box::new(|_| RecallResult::default())),
            editions: Mutex::new(EditionPage::default()),
            artists: Mutex::new(HashMap::new()),
            artists_asked: Mutex::new(Vec::new()),
            documents: Mutex::new(HashMap::new()),
            documents_asked: Mutex::new(Vec::new()),
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

    /// Answer a lookup of release `mbid`'s document with `document`. An id
    /// with no answer scripted gets none.
    pub fn set_document(&self, mbid: &str, document: ReleaseAnswer) {
        self.documents
            .lock()
            .expect("document script")
            .insert(mbid.to_owned(), document);
    }

    /// Every release document looked up so far, in order.
    pub fn documents_asked(&self) -> Vec<String> {
        self.documents_asked.lock().expect("document asks").clone()
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
        _named_group: Option<&str>,
        _priority: droppedneedle_providers::RequestPriority,
    ) -> Pin<Box<dyn Future<Output = RecallOutcome> + Send + '_>> {
        outcome(self.recall(facts))
    }

    fn recall_release(
        &self,
        facts: &LocalAlbumFacts,
        release_mbid: &str,
        _priority: droppedneedle_providers::RequestPriority,
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
        _priority: droppedneedle_providers::RequestPriority,
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

    fn release_document(
        &self,
        release_mbid: &str,
    ) -> Pin<Box<dyn Future<Output = Result<ReleaseAnswer, Unanswered>> + Send + '_>> {
        self.documents_asked
            .lock()
            .expect("document asks")
            .push(release_mbid.to_owned());
        let answer = self
            .documents
            .lock()
            .expect("document script")
            .get(release_mbid)
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

/// The library's services over `library`, built as the server builds them.
pub fn services(library: &LibrarySetup) -> LibraryServices {
    LibraryServices::build(library)
}

/// The operations service over `library`.
pub fn operations(library: &LibrarySetup) -> Operations {
    services(library).operations
}

/// A repairs worker that has followed nothing up yet, as the repairs loop
/// starts with.
pub fn repair_worker(library: &LibrarySetup) -> RepairWorker {
    services(library).repair_worker()
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
/// `enrichment`.
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
    let queue = droppedneedle_kernel::jobs::queue::JobQueue::new(
        platform.db.clone(),
        Arc::new(droppedneedle_kernel::clock::SystemClock),
    );
    LibrarySetup::build(
        platform,
        providers,
        mb_source,
        queue.clone(),
        outbound.library(),
        LibraryPorts {
            enrichment,
            scan_completed: Arc::new(|| {}),
            core: library_core(platform, queue),
        },
    )
    .expect("library bundle builds")
}

/// The library's write side over the platform, built the way the app
/// builds it, for a rig that holds a plugin host and no library.
pub fn library_core(
    platform: &Platform,
    queue: droppedneedle_kernel::jobs::queue::JobQueue,
) -> LibraryCore {
    LibraryCore::build(
        platform,
        queue,
        droppedneedle_acquire::wiring::recycle_guard(
            &platform.settings,
            platform.config.imports_dir(),
        ),
    )
    .expect("library write side builds")
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
