//! Every download source through the one source trait: slskd and SABnzbd
//! against their loopback mocks, and the example plugin client as a real
//! subprocess (needs `python3` on PATH). Each is driven the way the worker
//! drives it: enqueue, poll to the end, the landed files inside the
//! client's own folder, discard, abort. Plus the gate that keeps a
//! source's stored text in the one conversion.

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
use droppedneedle_acquire::usenet::newznab::{NewznabClient, NewznabIndexer, NewznabIndexerEntry};
use droppedneedle_acquire::usenet::policy::UsenetPolicy;
use droppedneedle_acquire::usenet::prowlarr::ProwlarrIndexer;
use droppedneedle_acquire::usenet::sabnzbd::{SabnzbdClient, SabnzbdQueue};
use droppedneedle_kernel::runtime_config::ConfigStore;
use droppedneedle_kernel::runtime_config::crypto::Crypto;
use droppedneedle_kernel::runtime_config::sections::UsenetBackendSetting;
use droppedneedle_plugins::host::PluginHost;
use droppedneedle_plugins::process::ProcessLauncher;
use droppedneedle_plugins::runtime::RuntimeState;
use droppedneedle_testkit::slskd::{MOCK_API_KEY, MockSlskd};
use droppedneedle_testkit::usenet::{SabnzbdMock, serve_loopback};
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
                .any(|mount| file.path.starts_with(mount)),
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
        Arc::new(newznab),
        Arc::new(ProwlarrIndexer::new(
            None,
            Vec::new(),
            false,
            WAIT,
            WAIT,
            WAIT,
        )),
        UsenetBackendSetting::default(),
        policy,
        journal,
        None,
        WAIT,
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

/// The example plugin client and its paired indexer, running, with one
/// file for "Test Artist - Fixtures" in their source folder.
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
        Arc::new(droppedneedle_plugins::ticks::TickStoreKind::Memory(
            droppedneedle_kernel::jobs::plugin_ticks::MemoryTickStore::new(),
        )),
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
    PluginDownloadSource::new(
        rig.host.clone(),
        "plugin:local-folder-client".to_owned(),
        journal,
        ReleasePolicy {
            usenet: UsenetPolicy::v2_defaults(),
            ignored_terms: Vec::new(),
            required_terms: Vec::new(),
        },
    )
}

#[tokio::test]
async fn example_plugin_conforms() {
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

/// A source's stored text is written and read only by `SourceTag`: no
/// other acquire code spells `"soulseek"` or `"usenet"`.
#[test]
fn source_text_lives_in_the_tag() {
    let root = repo().join("server/acquire/src");
    // Allowed, with the reason:
    let allowed = [
        // The one conversion.
        "downloads/source_tag.rs",
        // The Usenet indexer modules: Prowlarr's `protocol` is Prowlarr's
        // own wire value, and their results are remodelled with the
        // indexer seam that replaces them.
        "usenet/newznab.rs",
        "usenet/prowlarr.rs",
    ];
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    let mut found = Vec::new();
    for file in files {
        let relative = file
            .strip_prefix(&root)
            .expect("under the root")
            .to_string_lossy()
            .replace('\\', "/");
        if allowed.contains(&relative.as_str()) {
            continue;
        }
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
            if code.contains("\"soulseek\"") || code.contains("\"usenet\"") {
                found.push(format!("{relative}:{}: {code}", number + 1));
            }
        }
    }
    assert!(
        found.is_empty(),
        "source text outside SourceTag:\n{}",
        found.join("\n")
    );
}
