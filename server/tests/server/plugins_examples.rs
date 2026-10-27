//! The shipped example plugins, run as real subprocesses through the
//! Python helper: one short test per capability, plus crash, hang and
//! environment checks on the process runtime. Needs `python3` on PATH.

use crate::common::ScratchDir;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use droppedneedle_kernel::runtime_config::ConfigStore;
use droppedneedle_kernel::runtime_config::crypto::Crypto;
use droppedneedle_plugins::capabilities::acquisition::PluginEnqueue;
use droppedneedle_plugins::capabilities::stream::PluginStream;
use droppedneedle_plugins::host::PluginHost;
use droppedneedle_plugins::manifest::{load_manifest, parse_manifest};
use droppedneedle_plugins::models::{PluginHealthStatus, PluginSource, PluginSourceHealth};
use droppedneedle_plugins::process::ProcessLauncher;
use droppedneedle_plugins::runtime::{
    DownloadTaskEvent, EventKind, EventPayload, PluginEvent, RuntimeState, ScrobbleEvent,
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

struct Rig {
    dir: ScratchDir,
    host: Arc<PluginHost>,
}

impl Drop for Rig {
    fn drop(&mut self) {
        // Kill the children even when a test fails part-way.
        let host = Arc::clone(&self.host);
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move { host.stop_all().await });
        }
    }
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

async fn rig(tag: &str, examples: &[&str]) -> Rig {
    let dir = ScratchDir::new(&format!("plugin-ex-{tag}"));
    for example in examples {
        copy_dir(
            &repo().join("examples/plugins").join(example),
            &dir.join("plugins").join(example),
        );
    }
    let crypto = Crypto::from_key_bytes(&[7u8; 32]).unwrap();
    let config = Arc::new(ConfigStore::scratch(&dir, crypto).unwrap());
    crate::common::migrated_db(&dir.join("app.db"));
    let db = droppedneedle_kernel::db::Db::open_for_tests(&dir.join("app.db")).unwrap();
    let platform = crate::common::TestPlatform::on(&db, &dir).build().await;
    let library = crate::common::library::library_core(
        &platform,
        droppedneedle_kernel::jobs::queue::JobQueue::new(
            db,
            Arc::new(droppedneedle_kernel::clock::SystemClock),
        ),
    );
    let launcher = ProcessLauncher::new(repo().join("sdk/python"), platform.supervisor.clone());
    let host = PluginHost::new(
        dir.join("plugins"),
        config,
        Arc::new(launcher),
        droppedneedle_plugins::wiring::tick_store(&platform),
        library,
    );
    host.load_all();
    Rig { dir, host }
}

async fn enable(rig: &Rig, name: &str, settings: &[(&str, &str)]) {
    let settings: HashMap<String, String> = settings
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect();
    rig.host.update_settings(name, true, settings).unwrap();
    wait_for(rig, name, RuntimeState::Running).await;
}

/// The host gives a plugin 15 seconds to answer its handshake, so the wait
/// runs a little past that: a slow start on a busy machine is not a failure.
async fn wait_for(rig: &Rig, name: &str, state: RuntimeState) {
    for _ in 0..800 {
        let status = rig
            .host
            .get(name)
            .and_then(|plugin| plugin.runtime_status());
        if status.as_ref().is_some_and(|status| status.state == state) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let status = rig
        .host
        .get(name)
        .and_then(|plugin| plugin.runtime_status());
    panic!("{name} never reached {state:?}: {status:?}");
}

#[test]
fn every_example_manifest_validates() {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(repo().join("examples/plugins")).unwrap() {
        let dir = entry.unwrap().path();
        names.push(load_manifest(&dir).unwrap().name);
    }
    names.sort();
    assert_eq!(
        names,
        [
            "events-echo-toy",
            "http-catalog",
            "local-folder-client",
            "local-folder-indexer",
            "metadata-joke-toy",
            "stream-toy",
            "webhook-scrobbler",
        ]
    );
}

/// A manifest written for an older plugin API is refused before any of its
/// code runs, and the message says where the porting guide is.
#[test]
fn manifests_for_older_plugin_apis_are_refused() {
    for version in [0, 1] {
        let text = format!(
            "[plugin]\nname = \"old-toy\"\napi_version = {version}\n\
             entrypoint = \"plugin:Toy\"\ncapabilities = [\"scrobbler\"]\n"
        );
        let error = parse_manifest("old-toy", text.as_bytes()).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "old-toy: written for api_version {version}; this DroppedNeedle reads \
                 api_version 2; see Porting a v2 plugin in PLUGINS.md"
            )
        );
    }
}

/// One-request HTTP server: answers 200 and hands back the request.
async fn capture_one_request() -> (String, tokio::task::JoinHandle<String>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/hook", listener.local_addr().unwrap());
    let served = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut raw = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let read = socket.read(&mut chunk).await.unwrap();
            raw.extend_from_slice(&chunk[..read]);
            let text = String::from_utf8_lossy(&raw).to_string();
            if let Some((head, body)) = text.split_once("\r\n\r\n") {
                let length = head
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                if body.len() >= length || read == 0 {
                    break;
                }
            }
        }
        socket
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
            .await
            .unwrap();
        String::from_utf8_lossy(&raw).to_string()
    });
    (url, served)
}

#[tokio::test]
async fn scrobbler_posts_each_play_to_its_webhook() {
    let rig = rig("scrobbler", &["webhook-scrobbler"]).await;
    let (url, served) = capture_one_request().await;
    enable(&rig, "webhook-scrobbler", &[("webhook_url", &url)]).await;
    rig.host.dispatch_scrobble(&ScrobbleEvent {
        artist: "Test Artist".to_owned(),
        track: "Tone".to_owned(),
        album: Some("Fixtures".to_owned()),
        timestamp: 1_704_067_200,
        duration_ms: Some(1000),
        recording_mbid: None,
    });
    let request = tokio::time::timeout(Duration::from_secs(10), served)
        .await
        .unwrap()
        .unwrap();
    assert!(request.starts_with("POST /hook"));
    assert!(
        request.contains(r#""artist": "Test Artist""#)
            || request.contains(r#""artist":"Test Artist""#)
    );
}

#[tokio::test]
async fn metadata_and_purchase_links_fill_only_their_own_artist() {
    let rig = rig("metadata", &["metadata-joke-toy"]).await;
    enable(&rig, "metadata-joke-toy", &[]).await;
    let found = rig.host.enrich_artist("Test Artist", None).await;
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].tags, ["example", "test-fixture"]);
    assert!(
        rig.host
            .enrich_artist("Someone Else", None)
            .await
            .is_empty()
    );

    let links = rig
        .host
        .gather_purchase_links("Test Artist", "Fixtures", "")
        .await;
    assert_eq!(links.len(), 1);
    assert_eq!(
        links[0].url,
        "https://shop.example-catalog.test/test-artist"
    );
}

#[tokio::test]
async fn subscriber_publisher_and_routes_round_trip() {
    let rig = rig("events", &["events-echo-toy"]).await;
    enable(&rig, "events-echo-toy", &[]).await;
    rig.host.dispatch_event(PluginEvent {
        kind: EventKind::DownloadStarted,
        payload: EventPayload::Download(DownloadTaskEvent {
            task_id: "t1".to_owned(),
            user_id: "u1".to_owned(),
            release_group_mbid: String::new(),
            source: "soulseek".to_owned(),
            outcome: "started".to_owned(),
        }),
        causation_id: String::new(),
    });
    let query = HashMap::new();
    let mut seen = serde_json::Value::Null;
    for _ in 0..100 {
        let answer = rig
            .host
            .handle_plugin_route(
                "events-echo-toy",
                "GET",
                "status",
                &query,
                &Default::default(),
            )
            .await;
        seen = answer.body["seen"].clone();
        if seen == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(seen, 1);

    let body =
        droppedneedle_plugins::runtime::PluginRouteBody::Json(serde_json::json!({"note": "hello"}));
    let answer = rig
        .host
        .handle_plugin_route("events-echo-toy", "POST", "note", &query, &body)
        .await;
    assert_eq!(answer.status, 200);
    let record = rig.host.recent_published().pop().unwrap();
    assert_eq!(record.source_plugin, "events-echo-toy");
    assert_eq!(record.payload["note"], "hello");
}

#[tokio::test]
async fn indexer_and_download_client_pair_across_plugins() {
    let rig = rig("acquire", &["local-folder-client", "local-folder-indexer"]).await;
    let source = rig.dir.join("music-source");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("Test Artist - Fixtures.flac"), b"fLaC").unwrap();
    let downloads = rig.dir.join("downloads");
    let source_text = source.to_string_lossy().into_owned();
    let downloads_text = downloads.to_string_lossy().into_owned();
    enable(
        &rig,
        "local-folder-indexer",
        &[("source_dir", &source_text)],
    )
    .await;
    enable(
        &rig,
        "local-folder-client",
        &[
            ("source_dir", &source_text),
            ("downloads_dir", &downloads_text),
        ],
    )
    .await;

    let results = rig
        .host
        .search_album(
            "plugin:local-folder-client",
            "Test Artist",
            "Fixtures",
            None,
            None,
        )
        .await;
    assert_eq!(results.len(), 1);
    assert!((results[0].score - 0.9).abs() < 1e-9);

    let handle = rig
        .host
        .enqueue_download(&PluginEnqueue {
            task_id: "task-1".to_owned(),
            source: "plugin:local-folder-client".to_owned(),
            files: Vec::new(),
            payload: results[0].payload.clone(),
            job_name: "droppedneedle-task-1-0".to_owned(),
            download_type: "album".to_owned(),
        })
        .await
        .unwrap();
    assert_eq!(handle.source, "plugin:local-folder-client");
    let status = rig.host.download_status(&handle).await.unwrap();
    assert_eq!(status.status, "completed");
    let files = rig.host.download_inspect(&handle).await.unwrap();
    assert_eq!(files.state, "completed");
    assert!(files.file_paths[0].ends_with("Test Artist - Fixtures.flac"));
    assert!(
        downloads
            .join("task-1")
            .join("Test Artist - Fixtures.flac")
            .is_file()
    );
}

#[tokio::test]
async fn scheduler_ticks_and_reports_health() {
    let rig = rig("scheduler", &["http-catalog"]).await;
    enable(&rig, "http-catalog", &[]).await;
    assert_eq!(rig.host.desired_ticks()[0].interval_minutes, 60);
    rig.host
        .fire_tick("http-catalog", Duration::from_secs(10))
        .await
        .unwrap();

    // The plugin is asked once it runs, and the source list shows its answer.
    let source = wait_for_source(&rig, |source| source.health != PluginSourceHealth::Unknown).await;
    assert_eq!(source.health, PluginSourceHealth::Error);
    assert!(!source.configured);
    assert_eq!(source.health_message, "catalog_url is not configured");

    // A settings save asks again.
    let (url, served) = capture_one_request().await;
    rig.host
        .update_settings(
            "http-catalog",
            true,
            HashMap::from([("catalog_url".to_owned(), url)]),
        )
        .unwrap();
    let source = wait_for_source(&rig, |source| source.configured).await;
    assert_eq!(source.health, PluginSourceHealth::Ok);
    assert!(served.await.unwrap().starts_with("HEAD /hook"));
    let health = rig.host.get("http-catalog").unwrap().health().unwrap();
    assert_eq!(health.status, PluginHealthStatus::Ok);
}

/// Wait until the one plugin source matches `done`.
async fn wait_for_source(rig: &Rig, done: impl Fn(&PluginSource) -> bool) -> PluginSource {
    for _ in 0..400 {
        if let Some(source) = rig.host.plugin_sources().into_iter().find(|s| done(s)) {
            return source;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the source never matched: {:?}", rig.host.plugin_sources());
}

#[tokio::test]
async fn streaming_source_resolves_inside_its_own_folder() {
    let rig = rig("stream", &["stream-toy"]).await;
    enable(&rig, "stream-toy", &[]).await;
    let stream = rig
        .host
        .resolve_stream("00000000-0000-4000-8000-000000000000", "user-1", &[])
        .await;
    match stream {
        Some(PluginStream::File { path, .. }) => assert!(path.ends_with("fixtures/test-tone.flac")),
        other => panic!("unexpected {other:?}"),
    }
    assert!(
        rig.host
            .resolve_stream("11111111-1111-4111-8111-111111111111", "user-1", &[])
            .await
            .is_none()
    );
}

const PROBE_MANIFEST: &str = "[plugin]\n\
     name = \"probe\"\n\
     api_version = 2\n\
     entrypoint = \"plugin:Probe\"\n\
     capabilities = [\"scheduler\", \"purchase_links\"]\n\
     [schedule]\n\
     interval_minutes = 5\n";

const PROBE_CODE: &str = r#"
import asyncio, os, time

class Probe:
    def __init__(self, context):
        self.ctx = context

    async def purchase_links(self, artist, album, mbid):
        if artist == "crash":
            os._exit(3)
        if artist == "close":
            # Close the protocol stream (and everything else past stderr),
            # then keep the process alive and stuck.
            os.closerange(3, 4096)
            time.sleep(3600)
        if artist == "env":
            return [{"label": key, "url": "https://x.test/" + value[:50].replace(" ", "")}
                    for key, value in [("cwd", os.getcwd()), ("home", os.environ.get("HOME", "")),
                                       ("cargo", os.environ.get("CARGO_MANIFEST_DIR", "none"))]]
        return [{"label": "alive", "url": "https://x.test/alive"}]

    async def on_tick(self):
        await asyncio.sleep(30)
"#;

#[tokio::test]
async fn crashes_restart_hangs_time_out_and_the_environment_is_scrubbed() {
    let rig = rig("probe", &[]).await;
    let dir = rig.dir.join("plugins").join("probe");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("plugin.toml"), PROBE_MANIFEST).unwrap();
    std::fs::write(dir.join("plugin.py"), PROBE_CODE).unwrap();
    rig.host.load_all();
    enable(&rig, "probe", &[]).await;

    // Scrubbed environment, its own data folder as home and working dir.
    let env = rig.host.gather_purchase_links("env", "", "").await;
    let data_dir = rig.host.data_dir("probe").canonicalize().unwrap();
    let data = data_dir.to_string_lossy().replace(' ', "");
    let label = |key: &str| {
        env.iter()
            .find(|link| link.label == key)
            .map(|link| link.url.clone())
    };
    assert_eq!(
        label("cwd").unwrap(),
        format!("https://x.test/{}", &data[..data.len().min(50)])
    );
    assert_eq!(label("cargo").unwrap(), "https://x.test/none");

    // A hang times out without blocking anything else.
    let hung = rig
        .host
        .fire_tick("probe", Duration::from_millis(300))
        .await;
    assert!(hung.is_err());
    assert_eq!(rig.host.gather_purchase_links("ok", "", "").await.len(), 1);

    // A crash fails that call; the plugin comes back on its own.
    assert!(
        rig.host
            .gather_purchase_links("crash", "", "")
            .await
            .is_empty()
    );
    wait_until_back(&rig, 1).await;

    // A plugin that closes its output but keeps running is killed and
    // started again, not waited on forever.
    assert!(
        rig.host
            .gather_purchase_links("close", "", "")
            .await
            .is_empty()
    );
    wait_until_back(&rig, 2).await;
}

/// Wait until the probe has restarted `restarts` times and answers again.
async fn wait_until_back(rig: &Rig, restarts: u64) {
    for _ in 0..200 {
        let status = rig.host.get("probe").unwrap().runtime_status().unwrap();
        if status.restarts >= restarts
            && status.state == RuntimeState::Running
            && !rig
                .host
                .gather_purchase_links("ok", "", "")
                .await
                .is_empty()
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the plugin did not come back after restart {restarts}");
}
