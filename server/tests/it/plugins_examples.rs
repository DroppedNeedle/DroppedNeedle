//! The shipped example plugins, run as real subprocesses through the
//! Python helper: one short test per capability, plus crash, hang and
//! environment checks on the process runtime. Needs `python3` on PATH.

use crate::common::ScratchDir;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use droppedneedle::plugins::capabilities::acquisition::PluginEnqueue;
use droppedneedle::plugins::capabilities::stream::PluginStream;
use droppedneedle::plugins::host::PluginHost;
use droppedneedle::plugins::manifest::load_manifest;
use droppedneedle::plugins::process::ProcessLauncher;
use droppedneedle::plugins::runtime::{
    DownloadTaskEvent, EventKind, EventPayload, PluginEvent, RuntimeState, ScrobbleEvent,
};
use droppedneedle::runtime_config::ConfigStore;
use droppedneedle::runtime_config::crypto::Crypto;
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

fn rig(tag: &str, examples: &[&str]) -> Rig {
    let dir = ScratchDir::new(&format!("plugin-ex-{tag}"));
    for example in examples {
        copy_dir(
            &repo().join("examples/plugins").join(example),
            &dir.join("plugins").join(example),
        );
    }
    let crypto = Crypto::from_key_bytes(&[7u8; 32]).unwrap();
    let config = Arc::new(ConfigStore::open(&dir.join("config.json"), crypto).unwrap());
    let launcher = ProcessLauncher::new(repo().join("sdk/python"));
    let host = PluginHost::new(dir.join("plugins"), config, Arc::new(launcher));
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

async fn wait_for(rig: &Rig, name: &str, state: RuntimeState) {
    for _ in 0..200 {
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
    let rig = rig("scrobbler", &["webhook-scrobbler"]);
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
    let rig = rig("metadata", &["metadata-joke-toy"]);
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
    let rig = rig("events", &["events-echo-toy"]);
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

    let body = droppedneedle::plugins::runtime::PluginRouteBody::Json(
        serde_json::json!({"note": "hello"}),
    );
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
    let rig = rig("acquire", &["local-folder-client", "local-folder-indexer"]);
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
    let rig = rig("scheduler", &["http-catalog"]);
    enable(&rig, "http-catalog", &[]).await;
    assert_eq!(rig.host.desired_ticks()[0].interval_minutes, 60);
    rig.host
        .fire_tick("http-catalog", Duration::from_secs(10))
        .await
        .unwrap();
    let plugin = rig.host.get("http-catalog").unwrap();
    let health = rig.host.plugin_health(&plugin).await;
    assert_eq!(health.status, "error");
    assert!(!health.configured);
}

#[tokio::test]
async fn streaming_source_resolves_inside_its_own_folder() {
    let rig = rig("stream", &["stream-toy"]);
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
     api_version = 1\n\
     entrypoint = \"plugin:Probe\"\n\
     capabilities = [\"scheduler\", \"purchase_links\"]\n\
     [schedule]\n\
     interval_minutes = 5\n";

const PROBE_CODE: &str = r#"
import asyncio, os

class Probe:
    def __init__(self, context):
        self.ctx = context

    async def purchase_links(self, artist, album, mbid):
        if artist == "crash":
            os._exit(3)
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
    let rig = rig("probe", &[]);
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
    wait_for(&rig, "probe", RuntimeState::Running).await;
    for _ in 0..100 {
        if rig
            .host
            .get("probe")
            .unwrap()
            .runtime_status()
            .unwrap()
            .restarts
            >= 1
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
    panic!("the plugin did not come back after crashing");
}
