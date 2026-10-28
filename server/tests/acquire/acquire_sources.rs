//! Every download source through the one source trait: slskd and SABnzbd
//! against their loopback mocks, and the example plugin client as a real
//! subprocess (needs `python3` on PATH). Each is driven the way the worker
//! drives it: enqueue, poll to the end, the landed files inside the
//! client's own folder, discard, abort. Every Usenet indexer through the
//! one indexer trait: Newznab and Prowlarr against their mocks answering,
//! refusing the key and limiting with `Retry-After`, and a plugin indexer
//! as a real subprocess. Plus the gates that keep a source's stored text
//! in the one conversion and the Usenet backend in the wiring.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::http::{StatusCode, header};
use axum::routing::get;
use droppedneedle_acquire::db::AcquireDb;
use droppedneedle_acquire::downloads::journal::Journal;
use droppedneedle_acquire::downloads::store::NewTask;
use droppedneedle_acquire::downloads::{DownloadSource, SourceHandle, SourceTag};
use droppedneedle_acquire::plugin_source::{PluginDownloadSource, ReleasePolicy};
use droppedneedle_acquire::slskd::{
    DownloadPolicy, ReqwestSlskdHttp, SlskdClient, SlskdRepository,
};
use droppedneedle_acquire::sources::{SabnzbdSource, SlskdSource};
use droppedneedle_acquire::target::SearchTarget;
use droppedneedle_acquire::usenet::indexer::{Indexer, IndexerQuery, PluginIndexers};
use droppedneedle_acquire::usenet::newznab::{NewznabClient, NewznabIndexer, NewznabIndexerEntry};
use droppedneedle_acquire::usenet::policy::UsenetPolicy;
use droppedneedle_acquire::usenet::prowlarr::{ProwlarrClient, ProwlarrIndexer};
use droppedneedle_acquire::usenet::sabnzbd::{SabnzbdClient, SabnzbdQueue};
use droppedneedle_kernel::runtime_config::ConfigStore;
use droppedneedle_kernel::runtime_config::crypto::Crypto;
use droppedneedle_plugins::host::PluginHost;
use droppedneedle_plugins::process::ProcessLauncher;
use droppedneedle_plugins::runtime::RuntimeState;
use droppedneedle_testkit::slskd::{MOCK_API_KEY, MockSlskd};
use droppedneedle_testkit::usenet::{
    NewznabMock, NewznabScenario, ProwlarrMock, ProwlarrScenario, SabnzbdMock, serve_loopback,
};
use serde_json::json;

use crate::common::ScratchDir;
use crate::common::bundles::quiet_ticks;

const WAIT: Duration = Duration::from_secs(5);

/// A journal on a scratch database holding one album task.
async fn journal_with_task(task_id: &str, artist: &str, album: &str) -> Arc<Journal> {
    let db = AcquireDb::scratch().expect("scratch db");
    db.add_user("u1", "U", "user").await.expect("user seeds");
    let journal = Arc::new(Journal::new(db, quiet_ticks()));
    let task = NewTask {
        id: task_id.to_owned(),
        user_id: "u1".to_owned(),
        artist_name: artist.to_owned(),
        album_title: album.to_owned(),
        release_group_mbid: "rg-1".to_owned(),
        origin: "user".to_owned(),
        retry_count: 0,
    };
    journal
        .run("test.seed", move |store| {
            store.insert_task(&task, 1_700_000_000.0)
        })
        .await
        .expect("task inserts");
    journal
}

/// Drive one source the way the worker does and check what every source
/// owes it. `finish` stands in for the client finishing the job.
async fn conforms(
    source: Arc<dyn DownloadSource>,
    tag: SourceTag,
    task_id: &str,
    finish: impl FnOnce(&SourceHandle),
) {
    assert_eq!(source.tag(), tag);
    let handle = source.enqueue(task_id, 0).await.expect("enqueues");
    assert_eq!(handle.source, tag, "the handle names its source");
    finish(&handle);

    let progress = source.poll(&handle).await.expect("polls");
    assert!(
        progress.all_terminal && progress.all_succeeded,
        "{tag}: the job ends: {progress:?}"
    );

    let landed = source.landed(&handle).await.expect("reports what landed");
    assert!(!landed.files.is_empty(), "{tag}: something landed");
    for file in &landed.files {
        assert!(
            landed
                .mounts
                .iter()
                .any(|mount| file.path.starts_with(mount.path())),
            "{tag}: {} lies inside the client's folders {:?}",
            file.path.display(),
            landed.mounts
        );
    }

    source.discard(&handle).await.expect("discards");
    source.abort(&handle).await.expect("aborts");
}

#[tokio::test]
async fn slskd_conforms() {
    let mock = MockSlskd::start().await.expect("mock starts");
    let folder = "@@music\\Portishead\\Dummy (1994)";
    mock.script_responses(vec![json!({
        "username": "peer", "hasFreeUploadSlot": true, "uploadSpeed": 1_000_000,
        "queueLength": 0, "fileCount": 2, "lockedFileCount": 0, "lockedFiles": [], "token": 1,
        "files": [
            {"filename": format!("{folder}\\01 Mysterons.flac"), "size": 4,
             "extension": "flac", "bitDepth": 16, "sampleRate": 44100, "length": 300},
            {"filename": format!("{folder}\\02 Sour Times.flac"), "size": 4,
             "extension": "flac", "bitDepth": 16, "sampleRate": 44100, "length": 250},
        ],
    })]);
    let dir = ScratchDir::new("source-slskd");
    let mount = dir.join("downloads");
    let landed = mount.join("Dummy (1994)");
    std::fs::create_dir_all(&landed).expect("mount");
    for name in ["01 Mysterons.flac", "02 Sour Times.flac"] {
        std::fs::write(landed.join(name), b"fLaC").expect("file lands");
    }
    let repo = SlskdRepository::new(
        SlskdClient::new(ReqwestSlskdHttp::new(
            reqwest::Client::new(),
            &mock.base_url(),
            MOCK_API_KEY,
        )),
        &mock.base_url(),
        MOCK_API_KEY,
        mount,
        DownloadPolicy {
            search_timeout: Duration::from_secs(2),
            completion_grace: Duration::from_secs(2),
            poll_interval: Duration::from_millis(10),
            ..DownloadPolicy::default()
        },
    );
    let journal = journal_with_task("t-slskd", "Portishead", "Dummy").await;
    let source = Arc::new(SlskdSource::new(Arc::new(repo), journal));
    conforms(source, SourceTag::Soulseek, "t-slskd", |_| {}).await;
}

/// A Newznab feed with one release whose NZB the SABnzbd mock serves.
fn feed(nzb_url: &str) -> Router {
    let body = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:newznab="http://www.newznab.com/DTD/2010/feeds/attributes/">
 <channel>
  <newznab:response offset="0" total="1"/>
  <item>
   <title>Portishead - Dummy (1994) [FLAC]</title>
   <guid>dummy-flac</guid>
   <enclosure url="{nzb_url}" length="314572800" type="application/x-nzb"/>
   <newznab:attr name="category" value="3040"/>
   <newznab:attr name="size" value="314572800"/>
   <newznab:attr name="files" value="12"/>
  </item>
 </channel>
</rss>"#
    );
    Router::new().route(
        "/api",
        get(move || {
            let body = body.clone();
            async move {
                (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, "application/rss+xml")],
                    body,
                )
            }
        }),
    )
}

#[tokio::test]
async fn sabnzbd_conforms() {
    let mock = SabnzbdMock::new();
    let (sab, _sab_server) = serve_loopback(mock.router()).await.expect("mock serves");
    let (indexer, _indexer_server) = serve_loopback(feed(&format!("{sab}/nzb/good")))
        .await
        .expect("feed serves");
    let dir = ScratchDir::new("source-sabnzbd");
    let mount = dir.join("complete");
    let policy = UsenetPolicy::v2_defaults();
    let queue = SabnzbdQueue::new(
        SabnzbdClient::new(
            reqwest::Client::new(),
            &sab,
            "SABKEY",
            3,
            Duration::from_millis(1),
        ),
        &sab,
        "SABKEY",
        mount.clone(),
        policy.clone(),
    );
    let newznab = NewznabIndexer::new(
        vec![NewznabIndexerEntry {
            client: NewznabClient::new(
                reqwest::Client::new(),
                &format!("{indexer}/api"),
                "NZKEY",
                "dummy",
                "Dummy",
            ),
            id: "dummy".to_owned(),
            name: "Dummy".to_owned(),
            categories: vec![3040],
            enabled: true,
            priority: 1,
            limit: 100,
        }],
        WAIT,
        WAIT,
        WAIT,
        WAIT,
    );
    let journal = journal_with_task("t-sab", "Portishead", "Dummy").await;
    let source = Arc::new(SabnzbdSource::new(
        Arc::new(queue),
        vec![Arc::new(newznab)],
        policy,
        journal,
        None,
    ));
    conforms(source, SourceTag::Usenet, "t-sab", |handle| {
        // SABnzbd finishes the job into its complete folder.
        std::fs::create_dir_all(mount.join(&handle.job_name)).expect("job folder");
        mock.state().queue_slots.clear();
        mock.history_job(
            &handle.nzo_id,
            &handle.job_name,
            "Completed",
            &format!("/data/Downloads/complete/{}", handle.job_name),
            314_572_800,
            "",
        );
    })
    .await;
}

fn album(artist: &str, title: &str) -> SearchTarget {
    SearchTarget {
        artist: artist.to_owned(),
        album_title: title.to_owned(),
        year: None,
        tracklist: Vec::new(),
        wanted: Vec::new(),
        track: None,
    }
}

/// An indexer that can answer finds releases, every one tagged Usenet and
/// carrying an NZB link.
async fn answers(indexer: &dyn Indexer) {
    let target = album("Radiohead", "In Rainbows");
    let hits = indexer.search(&IndexerQuery::Album(&target)).await;
    assert!(!hits.is_empty(), "the indexer answers");
    for hit in &hits {
        assert_eq!(hit.source, SourceTag::Usenet);
        assert!(!hit.usenet.nzb_url.is_empty(), "{:?}", hit.usenet);
    }
}

/// A refused key is no releases, never a failure.
async fn refused(indexer: &dyn Indexer) {
    let target = album("Radiohead", "In Rainbows");
    assert!(
        indexer
            .search(&IndexerQuery::Album(&target))
            .await
            .is_empty()
    );
}

/// A 429 is no releases, and the indexer sits out for the `Retry-After`
/// it was given (each indexer's own backoff is zero here, so only the
/// header can keep it out).
async fn limited(indexer: &dyn Indexer, searches: impl Fn() -> usize) {
    let target = album("Radiohead", "In Rainbows");
    assert!(
        indexer
            .search(&IndexerQuery::Album(&target))
            .await
            .is_empty()
    );
    let asked = searches();
    assert!(asked > 0, "the indexer was asked");
    assert!(
        indexer
            .search(&IndexerQuery::Album(&target))
            .await
            .is_empty()
    );
    assert_eq!(searches(), asked, "Retry-After keeps the indexer out");
}

/// An indexer, its mock's call log, and the mock's server task.
type Rig<I, M> = (I, M, tokio::task::JoinHandle<()>);

async fn newznab_on(scenario: NewznabScenario) -> Rig<NewznabIndexer, NewznabMock> {
    let mock = NewznabMock::new();
    let (base, server) = serve_loopback(mock.router(scenario))
        .await
        .expect("mock serves");
    let indexer = NewznabIndexer::new(
        vec![NewznabIndexerEntry {
            client: NewznabClient::new(
                reqwest::Client::new(),
                &format!("{base}/api"),
                "NZKEY",
                "nz",
                "NZ",
            ),
            id: "nz".to_owned(),
            name: "NZ".to_owned(),
            categories: vec![3040],
            enabled: true,
            priority: 1,
            limit: 100,
        }],
        WAIT,
        WAIT,
        Duration::ZERO,
        WAIT,
    );
    (indexer, mock, server)
}

#[tokio::test]
async fn newznab_indexer_conforms() {
    let (indexer, _, _server) = newznab_on(NewznabScenario::DrunkenSlug).await;
    answers(&indexer).await;
    let (indexer, _, _server) = newznab_on(NewznabScenario::AuthError).await;
    refused(&indexer).await;
    let (indexer, mock, _server) = newznab_on(NewznabScenario::Http429).await;
    limited(&indexer, || mock.calls().len()).await;
}

async fn prowlarr_on(scenario: ProwlarrScenario) -> Rig<ProwlarrIndexer, ProwlarrMock> {
    let mock = ProwlarrMock::new();
    let (base, server) = serve_loopback(mock.router(scenario))
        .await
        .expect("mock serves");
    let indexer = ProwlarrIndexer::new(
        Some(ProwlarrClient::new(
            reqwest::Client::new(),
            &base,
            "PRKEY",
            "prowlarr",
        )),
        vec![3040],
        true,
        WAIT,
        Duration::ZERO,
        WAIT,
    );
    (indexer, mock, server)
}

#[tokio::test]
async fn prowlarr_indexer_conforms() {
    let (indexer, _, _server) = prowlarr_on(ProwlarrScenario::default()).await;
    answers(&indexer).await;
    let (indexer, _, _server) = prowlarr_on(ProwlarrScenario {
        auth_fail: true,
        ..ProwlarrScenario::default()
    })
    .await;
    refused(&indexer).await;
    let (indexer, mock, _server) = prowlarr_on(ProwlarrScenario {
        search_429: true,
        ..ProwlarrScenario::default()
    })
    .await;
    limited(&indexer, || mock.calls().len()).await;
}

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("plugin folder");
    for entry in std::fs::read_dir(from).expect("example reads") {
        let entry = entry.expect("entry reads");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).expect("example copies");
        }
    }
}

/// A plugin indexer that feeds Usenet: one release with an NZB link and
/// one without.
const USENET_FEED_MANIFEST: &str = r#"[plugin]
name = "usenet-feed"
display_name = "Usenet feed"
version = "1.0.0"
api_version = 2
entrypoint = "plugin:UsenetFeed"
capabilities = ["indexer"]
description = "Answers one NZB release."
author = "DroppedNeedle tests"

[[capability]]
id = "indexer"
target_source = "usenet"
"#;

const USENET_FEED_PLUGIN: &str = r#"from droppedneedle_plugin import IndexerResult, PluginSearchResult, ServiceStatus


class UsenetFeed:
    def __init__(self, context):
        self.ctx = context

    @property
    def indexer_name(self) -> str:
        return "plugin:usenet-feed"

    def is_configured(self) -> bool:
        return True

    async def health_check(self) -> ServiceStatus:
        return ServiceStatus(status="ok")

    async def search_album(self, artist_name, album_title, year=None, track_count=None, *, timeout=30.0):
        return [
            IndexerResult(
                source="usenet",
                plugin=PluginSearchResult(
                    title=f"{artist_name} - {album_title} [FLAC]",
                    size_bytes=314572800,
                    payload="feed-1",
                    nzb_url="http://127.0.0.1:9/nzb/feed-1",
                ),
            ),
            IndexerResult(
                source="usenet",
                plugin=PluginSearchResult(title="No link", size_bytes=1, payload="feed-2"),
            ),
        ]

    async def search_track(self, artist_name, track_title, album_title=None, duration_seconds=None, *, timeout=30.0):
        return []
"#;

/// A plugin client whose every poll fails with the same text, saying the
/// fault its handle's token names (`local`, `source`), or saying nothing.
const FAULT_CLIENT_MANIFEST: &str = r#"[plugin]
name = "fault-client"
display_name = "Fault client"
version = "1.0.0"
api_version = 2
entrypoint = "plugin:FaultClient"
capabilities = ["download_client"]
description = "Fails every poll."
author = "DroppedNeedle tests"

[[capability]]
id = "download_client"
source = "fault-client"
display_name = "Fault client"
"#;

const FAULT_CLIENT_PLUGIN: &str = r#"from droppedneedle_plugin import DownloadFailure, ServiceStatus, TaskHandle


class FaultClient:
    def __init__(self, context):
        self.ctx = context

    def is_configured(self) -> bool:
        return True

    async def health_check(self) -> ServiceStatus:
        return ServiceStatus(status="ok")

    async def enqueue(self, request) -> TaskHandle:
        return TaskHandle(plugin_token=request.payload)

    async def get_status(self, handle):
        if handle.plugin_token in ("local", "source"):
            raise DownloadFailure("disk full", fault=handle.plugin_token)
        raise RuntimeError("disk full")

    async def discard_client_artifacts(self, handle) -> bool:
        return True
"#;

/// The example plugin client and its paired indexer, a plugin indexer
/// feeding Usenet and a client that fails every poll, running, with one
/// file for "Test Artist - Fixtures" in the example's source folder.
struct PluginRig {
    _dir: ScratchDir,
    host: Arc<PluginHost>,
    downloads: PathBuf,
}

impl Drop for PluginRig {
    fn drop(&mut self) {
        let host = Arc::clone(&self.host);
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move { host.stop_all().await });
        }
    }
}

async fn plugin_rig(tag: &str) -> PluginRig {
    let dir = ScratchDir::new(&format!("source-plugin-{tag}"));
    for example in ["local-folder-client", "local-folder-indexer"] {
        copy_dir(
            &repo().join("examples/plugins").join(example),
            &dir.join("plugins").join(example),
        );
    }
    let feed = dir.join("plugins").join("usenet-feed");
    std::fs::create_dir_all(&feed).expect("plugin folder");
    std::fs::write(feed.join("plugin.toml"), USENET_FEED_MANIFEST).expect("manifest");
    std::fs::write(feed.join("plugin.py"), USENET_FEED_PLUGIN).expect("plugin");
    let faulty = dir.join("plugins").join("fault-client");
    std::fs::create_dir_all(&faulty).expect("plugin folder");
    std::fs::write(faulty.join("plugin.toml"), FAULT_CLIENT_MANIFEST).expect("manifest");
    std::fs::write(faulty.join("plugin.py"), FAULT_CLIENT_PLUGIN).expect("plugin");
    let config = Arc::new(
        ConfigStore::scratch(
            &dir.join("plugin-config"),
            Crypto::from_key_bytes(&[7u8; 32]).expect("key"),
        )
        .expect("config"),
    );
    crate::common::migrated_db(&dir.join("app.db"));
    let db = droppedneedle_kernel::db::Db::open_for_tests(&dir.join("app.db")).expect("db");
    let platform = crate::common::TestPlatform::on(&db, &dir).build().await;
    let library = crate::common::library::library_core(
        &platform,
        droppedneedle_kernel::jobs::queue::JobQueue::new(
            db,
            Arc::new(droppedneedle_kernel::clock::SystemClock),
        ),
    );
    let host = PluginHost::new(
        dir.join("plugins"),
        config,
        Arc::new(ProcessLauncher::new(
            repo().join("sdk/python"),
            platform.supervisor.clone(),
        )),
        droppedneedle_plugins::wiring::tick_store(&platform),
        library,
    );
    host.load_all();
    let source_dir = dir.join("music-source");
    std::fs::create_dir_all(&source_dir).expect("source folder");
    std::fs::write(source_dir.join("Test Artist - Fixtures.flac"), b"fLaC").expect("file");
    let downloads = dir.join("downloads");
    let source_text = source_dir.to_string_lossy().into_owned();
    let downloads_text = downloads.to_string_lossy().into_owned();
    for (name, settings) in [
        (
            "local-folder-indexer",
            vec![("source_dir", source_text.clone())],
        ),
        (
            "local-folder-client",
            vec![
                ("source_dir", source_text),
                ("downloads_dir", downloads_text),
            ],
        ),
        ("usenet-feed", Vec::new()),
        ("fault-client", Vec::new()),
    ] {
        let settings: HashMap<String, String> = settings
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect();
        host.update_settings(name, true, settings)
            .expect("plugin enabled");
        running(&host, name).await;
    }
    PluginRig {
        _dir: dir,
        host,
        downloads,
    }
}

/// The host gives a plugin 15 seconds to answer its handshake, so the wait
/// runs a little past that.
async fn running(host: &PluginHost, name: &str) {
    for _ in 0..800 {
        let state = host
            .get(name)
            .and_then(|plugin| plugin.runtime_status())
            .map(|status| status.state);
        if state == Some(RuntimeState::Running) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("{name} never started");
}

/// The example client as a download source over `journal`.
fn plugin_source(rig: &PluginRig, journal: Arc<Journal>) -> PluginDownloadSource {
    plugin_client(rig, "local-folder-client", journal)
}

/// One running plugin client as a download source over `journal`.
fn plugin_client(rig: &PluginRig, name: &str, journal: Arc<Journal>) -> PluginDownloadSource {
    PluginDownloadSource::new(
        rig.host.clone(),
        format!("plugin:{name}"),
        journal,
        ReleasePolicy {
            usenet: UsenetPolicy::v2_defaults(),
            ignored_terms: Vec::new(),
            required_terms: Vec::new(),
            auto_accept: 0.70,
            manual_min: 0.50,
        },
    )
}

#[tokio::test]
async fn plugins_conform() {
    let rig = plugin_rig("conform").await;
    let journal = journal_with_task("t-plugin", "Test Artist", "Fixtures").await;
    let source = Arc::new(plugin_source(&rig, journal));
    conforms(
        source,
        SourceTag::plugin("local-folder-client"),
        "t-plugin",
        |_| {},
    )
    .await;
    assert!(
        rig.downloads
            .join("t-plugin/Test Artist - Fixtures.flac")
            .is_file()
    );

    // The plugin indexer feeding Usenet, on the same running host: only
    // the release SABnzbd can fetch comes back.
    let indexers = PluginIndexers::new(rig.host.clone());
    answers(&indexers).await;
    let target = album("Radiohead", "In Rainbows");
    let hits = indexers.search(&IndexerQuery::Album(&target)).await;
    assert_eq!(hits.len(), 1, "a release without an NZB link is dropped");
}

/// A plugin client says whose fault a failed download was, and the
/// worker believes it, never the text (all three say "disk full"): a
/// local failure moves to the next result with its release unblamed;
/// the source's, or one that says nothing, blocklists the release.
#[tokio::test]
async fn a_plugin_says_whose_fault_a_failed_download_was() {
    use droppedneedle_acquire::downloads::worker::{DownloadWorker, WorkerConfig};
    use droppedneedle_acquire::downloads::{AttemptState, TaskStatus};
    use droppedneedle_acquire::reason::Reason;
    use droppedneedle_providers::health::ServiceHealth;

    let rig = plugin_rig("fault").await;
    let db = AcquireDb::scratch().expect("scratch db");
    db.add_user("u1", "U", "user").await.expect("user seeds");
    let journal = Arc::new(Journal::new(db, quiet_ticks()));
    for fault in ["local", "source", "unknown"] {
        let task = NewTask {
            id: format!("t-{fault}"),
            user_id: "u1".to_owned(),
            artist_name: "Test Artist".to_owned(),
            album_title: "Fixtures".to_owned(),
            release_group_mbid: "rg-1".to_owned(),
            origin: "user".to_owned(),
            retry_count: 0,
        };
        let mut handle = SourceHandle::new(SourceTag::plugin("fault-client"));
        handle.plugin_token = fault.to_owned();
        handle.release_identity = format!("release-{fault}");
        let handle = serde_json::to_string(&handle).expect("handle encodes");
        journal
            .run("test.seed", move |store| {
                store.insert_task(&task, 1_700_000_000.0)?;
                store.transition_task(&task.id, TaskStatus::Downloading, 1_700_000_000.0, None)?;
                store.insert_attempt(
                    &format!("{}-a0", task.id),
                    &task.id,
                    "plugin:fault-client",
                    0,
                    "",
                    &handle,
                    AttemptState::InUse,
                    1_700_000_000.0,
                )
            })
            .await
            .expect("download seeds");
    }
    let source: Arc<dyn DownloadSource> =
        Arc::new(plugin_client(&rig, "fault-client", journal.clone()));
    let worker = Arc::new(DownloadWorker::fixed(
        journal.clone(),
        vec![source],
        WorkerConfig::default(),
        droppedneedle_kernel::events::EventHub::new(),
        Arc::new(ServiceHealth::new()),
    ));

    worker.run_once(1).await;

    let mut blocked: Vec<String> = journal
        .read("test.quarantine", |store| store.list_quarantine())
        .await
        .expect("blocklist reads")
        .into_iter()
        .map(|row| row.identity)
        .collect();
    blocked.sort();
    assert_eq!(blocked, ["release-source", "release-unknown"]);
    let local = journal
        .read_task("t-local")
        .await
        .expect("task reads")
        .expect("task exists");
    assert_eq!(
        (local.status, local.outcome_code),
        (TaskStatus::Queued, Some(Reason::LocalFault)),
        "a local failure moves on, saying why"
    );
}

/// Files under `dir`, recursively.
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("source folder reads") {
        let path = entry.expect("entry reads").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// Each line of code in the acquire crate, as (file relative to the crate's
/// source folder, line number, trimmed text). Comments and the unit tests
/// at the end of a file are left out.
fn acquire_code() -> Vec<(String, usize, String)> {
    let root = repo().join("server/acquire/src");
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    let mut lines = Vec::new();
    for file in files {
        let relative = file
            .strip_prefix(&root)
            .expect("under the root")
            .to_string_lossy()
            .replace('\\', "/");
        let text = std::fs::read_to_string(&file).expect("file reads");
        for (number, line) in text.lines().enumerate() {
            let code = line.trim_start();
            if code.starts_with("#[cfg(test)]") {
                // Unit tests sit at the end of a file.
                break;
            }
            if code.starts_with("//") {
                continue;
            }
            lines.push((relative.clone(), number + 1, code.to_owned()));
        }
    }
    lines
}

/// A source's stored text is written and read only by `SourceTag`: no
/// other acquire code spells `"soulseek"` or `"usenet"`.
#[test]
fn source_text_lives_in_the_tag() {
    // The one conversion.
    let tag_file = "downloads/source_tag.rs";
    // Allowed lines, with the reason: Prowlarr's `protocol` field is
    // Prowlarr's own wire value, not our stored text.
    let allowed = [(
        "usenet/prowlarr.rs",
        "if !release.protocol.eq_ignore_ascii_case(\"usenet\") {",
    )];
    let found: Vec<String> = acquire_code()
        .into_iter()
        .filter(|(file, _, code)| {
            file != tag_file
                && (code.contains("\"soulseek\"") || code.contains("\"usenet\""))
                && !allowed.contains(&(file.as_str(), code.as_str()))
        })
        .map(|(file, number, code)| format!("{file}:{number}: {code}"))
        .collect();
    assert!(
        found.is_empty(),
        "source text outside SourceTag:\n{}",
        found.join("\n")
    );
}

/// Only the wiring reads which Usenet backend is chosen, to build the
/// indexer list; every search goes through the indexer seam.
#[test]
fn usenet_backend_is_matched_only_in_the_wiring() {
    let found: Vec<String> = acquire_code()
        .into_iter()
        .filter(|(file, _, code)| file != "wiring.rs" && code.contains("UsenetBackend::"))
        .map(|(file, number, code)| format!("{file}:{number}: {code}"))
        .collect();
    assert!(
        found.is_empty(),
        "Usenet backend matched outside the wiring:\n{}",
        found.join("\n")
    );
}
