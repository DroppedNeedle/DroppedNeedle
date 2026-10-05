//! Plugin host with fake runtimes: dropped folders run nothing until
//! enabled, secrets are sealed and reach the plugin decrypted, settings
//! saves reach a running plugin without a restart, installs pin a commit
//! and show the trust warning, the ext proxy and panels hide undeclared
//! routes from non-admins, publish is stamped, paced and loop-guarded,
//! fan-out runs once per causation, and plugin state persists through the
//! tick store. The real subprocess runtime is tested with the shipped
//! examples in `plugins_examples`.

use crate::common::ScratchDir;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use droppedneedle::auth::session::extract::Transport;
use droppedneedle::auth::session::middleware::CurrentSession;
use droppedneedle::auth::session::store::SessionKind;
use droppedneedle::auth::users::memory::with_test_principal;
use droppedneedle::auth::users::roles::Role;
use droppedneedle::ids::IdGenerator;
use droppedneedle::jobs::plugin_ticks::{
    MemoryTickStore, TickHost as JobsTickHost, TickPlugin as JobsTickPlugin,
};
use droppedneedle::jobs::registry::{JobRegistry, MemoryRegistryStore};
use droppedneedle::plugins::fakes::{
    FakeConnection, FakeFetcher, FakeLauncher, FakeRoles, FakeUnpacker, scrobble_plugin_event,
};
use droppedneedle::plugins::handlers::{ExtRateLimiter, PluginsDeps, plugins_router};
use droppedneedle::plugins::host::PluginHost;
use droppedneedle::plugins::protocol::methods;
use droppedneedle::plugins::runtime::{PublishPayload, ScrobbleEvent};
use droppedneedle::plugins::ticks::{
    HostTickAdapter, NoopTickSync, PluginTickLoops, TickLoopSync, TickStoreKind,
};
use droppedneedle::runtime_config::ConfigStore;
use droppedneedle::runtime_config::crypto::Crypto;
use serde_json::json;
use tower::ServiceExt as _;

/// Fixed id generator so error ids assert stably.
#[derive(Debug, Clone)]
struct FixedIds;

impl IdGenerator for FixedIds {
    fn new_id(&self) -> String {
        "123e4567-e89b-12d3-a456-426614174000".to_owned()
    }
}

fn write_plugin(dir: &Path, folder: &str, manifest: &str) {
    let plugin_dir = dir.join("plugins").join(folder);
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(plugin_dir.join("plugin.toml"), manifest).unwrap();
}

struct Rig {
    dir: ScratchDir,
    host: Arc<PluginHost>,
    config: Arc<ConfigStore>,
    launcher: Arc<FakeLauncher>,
}

fn rig(tag: &str) -> Rig {
    let dir = ScratchDir::new(&format!("plugins-{tag}"));
    let crypto = Crypto::from_key_bytes(&[7u8; 32]).unwrap();
    let config = Arc::new(ConfigStore::open(&dir.join("config.json"), crypto).unwrap());
    let launcher = Arc::new(FakeLauncher::new());
    let host = PluginHost::new(dir.join("plugins"), Arc::clone(&config), launcher.clone());
    Rig {
        dir,
        host,
        config,
        launcher,
    }
}

fn session(user_id: &str) -> CurrentSession {
    CurrentSession {
        user_id: user_id.to_owned(),
        session_id: "session-1".to_owned(),
        kind: SessionKind::Standard,
        transport: Transport::Bearer,
    }
}

fn deps_for(rig: &Rig, fetcher: Arc<FakeFetcher>, unpacker: Arc<FakeUnpacker>) -> PluginsDeps {
    let roles = Arc::new(FakeRoles::new());
    roles.insert("admin-1", Role::Admin);
    roles.insert("user-1", Role::User);
    PluginsDeps {
        host: Arc::clone(&rig.host),
        config: Arc::clone(&rig.config),
        roles,
        ids: Arc::new(FixedIds),
        fetcher,
        unpacker,
        tick_sync: Arc::new(NoopTickSync),
        ext_limiter: Arc::new(ExtRateLimiter::new()),
    }
}

fn router_as(rig: &Rig, user: &str) -> axum::Router {
    let deps = deps_for(
        rig,
        Arc::new(FakeFetcher::new()),
        Arc::new(FakeUnpacker::new(Vec::new())),
    );
    with_test_principal(plugins_router(deps), session(user))
}

async fn send(
    router: axum::Router,
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let mut request = Request::builder().method(method).uri(uri);
    let body = match body {
        Some(json) => {
            request = request.header("content-type", "application/json");
            Body::from(json.to_string())
        }
        None => Body::empty(),
    };
    let response = router.oneshot(request.body(body).unwrap()).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

const TOY_MANIFEST: &str = "[plugin]\n\
     name = \"toy\"\n\
     version = \"1.0.0\"\n\
     api_version = 1\n\
     entrypoint = \"plugin:Toy\"\n\
     capabilities = [\"scheduler\", \"subscriber\", \"publisher\"]\n\
     [schedule]\n\
     interval_minutes = 60\n\
     run_on_load = true\n\
     [[route]]\n\
     path = \"status\"\n\
     method = \"GET\"\n\
     auth = \"user\"\n\
     [[route]]\n\
     path = \"admin-only\"\n\
     method = \"GET\"\n\
     auth = \"admin\"\n\
     [[route]]\n\
     path = \"tight\"\n\
     method = \"GET\"\n\
     auth = \"user\"\n\
     rate_limit_per_minute = 1\n";

fn enable_toy(rig: &Rig, manifest: &str) -> Arc<FakeConnection> {
    write_plugin(&rig.dir, "toy", manifest);
    rig.host.load_all();
    rig.host
        .update_settings("toy", true, HashMap::new())
        .unwrap();
    rig.launcher.runtime("toy")
}

#[tokio::test]
async fn dropped_folder_runs_no_code_until_enabled() {
    let rig = rig("no-code");
    write_plugin(&rig.dir, "toy", TOY_MANIFEST);
    rig.host.load_all();
    let plugin = rig.host.get("toy").unwrap();
    assert!(!plugin.enabled);
    assert!(plugin.runtime.is_none());
    assert!(rig.launcher.launched.lock().unwrap().is_empty());

    enable_toy(&rig, TOY_MANIFEST);
    assert_eq!(*rig.launcher.launched.lock().unwrap(), ["toy"]);
    rig.host
        .update_settings("toy", false, HashMap::new())
        .unwrap();
    assert!(rig.host.get("toy").unwrap().runtime.is_none());
}

#[tokio::test]
async fn secrets_seal_at_rest_and_saves_reach_the_running_plugin() {
    let rig = rig("secrets");
    let manifest = "[plugin]\n\
         name = \"toy\"\n\
         api_version = 1\n\
         entrypoint = \"plugin:Toy\"\n\
         capabilities = [\"subscriber\"]\n\
         [[settings]]\n\
         key = \"api_key\"\n\
         secret = true\n\
         [[settings]]\n\
         key = \"nick\"\n";
    let runtime = enable_toy(&rig, manifest);

    let settings = HashMap::from([
        ("api_key".to_owned(), "live-secret".to_owned()),
        ("nick".to_owned(), "toybrick".to_owned()),
    ]);
    rig.host.update_settings("toy", true, settings).unwrap();
    let raw = std::fs::read_to_string(rig.dir.join("config.json")).unwrap();
    assert!(!raw.contains("live-secret"));

    // Same code, new settings: no relaunch, the plugin hears the decrypted
    // values.
    assert_eq!(rig.launcher.launched.lock().unwrap().len(), 1);
    let notes = runtime.notifications.lock().unwrap().clone();
    let (method, params) = notes.last().unwrap();
    assert_eq!(method, methods::SETTINGS_UPDATE);
    assert_eq!(params["settings"]["api_key"], "live-secret");

    // Reads mask; sending the mask back keeps the stored value.
    let keys = rig.host.get("toy").unwrap().manifest.secret_keys();
    assert_eq!(
        rig.config.get_plugin_masked("toy", &keys).unwrap().settings["api_key"],
        "plugin****"
    );
    let keep = HashMap::from([("api_key".to_owned(), "plugin****".to_owned())]);
    rig.host.update_settings("toy", true, keep).unwrap();
    assert_eq!(
        rig.config.get_plugin_raw("toy", &keys).unwrap().settings["api_key"],
        "live-secret"
    );
}

#[tokio::test]
async fn uninstall_stops_the_plugin_and_keeps_settings() {
    let rig = rig("uninstall");
    let runtime = enable_toy(&rig, TOY_MANIFEST);
    rig.host
        .update_settings(
            "toy",
            true,
            HashMap::from([("level".to_owned(), "11".to_owned())]),
        )
        .unwrap();
    rig.host.uninstall("toy").unwrap();
    assert!(!rig.dir.join("plugins").join("toy").exists());
    assert!(rig.host.get("toy").is_none());
    for _ in 0..50 {
        if runtime.was_stopped() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(runtime.was_stopped());
    assert_eq!(
        rig.config.get_plugin("toy").unwrap().settings["level"],
        "11"
    );
}

const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

fn github(fetcher: &FakeFetcher) {
    fetcher.insert(
        "https://api.github.com/repos/acme/toy/releases/latest",
        br#"{"tag_name": "v1.0.0"}"#,
    );
    fetcher.insert(
        "https://api.github.com/repos/acme/toy/commits/v1.0.0",
        format!(r#"{{"sha": "{SHA}"}}"#).as_bytes(),
    );
    fetcher.insert(
        &format!("https://codeload.github.com/acme/toy/zip/{SHA}"),
        b"zip",
    );
}

#[tokio::test]
async fn install_previews_permissions_then_pins_the_previewed_commit() {
    let rig = rig("install");
    let fetcher = Arc::new(FakeFetcher::new());
    github(&fetcher);
    let unpacker = Arc::new(FakeUnpacker::new(vec![
        FakeUnpacker::file("toy-0123456/plugin.toml", TOY_MANIFEST.as_bytes()),
        FakeUnpacker::file("toy-0123456/plugin.py", b"# code"),
    ]));
    let deps = deps_for(&rig, Arc::clone(&fetcher), unpacker);
    let admin = with_test_principal(plugins_router(deps), session("admin-1"));
    let body = json!({"repository_url": "https://github.com/acme/toy"});

    let (status, preview) = send(
        admin.clone(),
        "POST",
        "/plugins/install/preview",
        Some(body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(preview["commit"], SHA);
    assert_eq!(preview["reference"], "v1.0.0");
    assert!(
        preview["warning"]
            .as_str()
            .unwrap()
            .starts_with("Only install plugins you trust")
    );
    assert!(
        preview["permissions"]
            .to_string()
            .contains("every 60 minutes")
    );
    assert!(
        !rig.dir.join("plugins").join("toy").exists(),
        "preview writes nothing"
    );

    let stale = json!({"repository_url": "https://github.com/acme/toy", "commit": "f".repeat(40)});
    let (status, _) = send(admin.clone(), "POST", "/plugins/install", Some(stale)).await;
    assert_eq!(status, StatusCode::CONFLICT);

    let pinned = json!({"repository_url": "https://github.com/acme/toy", "commit": SHA});
    let (status, installed) = send(admin.clone(), "POST", "/plugins/install", Some(pinned)).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(installed["enabled"], false, "installs arrive disabled");
    assert_eq!(installed["install"]["commit"], SHA);
    assert_eq!(installed["install"]["ref_kind"], "release");
    assert!(rig.launcher.launched.lock().unwrap().is_empty());

    // Same release again: nothing to update.
    let (status, update) = send(admin, "POST", "/plugins/toy/update", Some(json!({}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(update["updated"], false);
}

#[tokio::test]
async fn unsafe_archives_never_reach_the_plugins_folder() {
    let rig = rig("unsafe");
    let fetcher = Arc::new(FakeFetcher::new());
    github(&fetcher);
    let mut link = FakeUnpacker::file("toy-0123456/plugin.py", b"x");
    link.is_symlink = true;
    let unpacker = Arc::new(FakeUnpacker::new(vec![
        FakeUnpacker::file("toy-0123456/plugin.toml", TOY_MANIFEST.as_bytes()),
        link,
    ]));
    let admin = with_test_principal(
        plugins_router(deps_for(&rig, fetcher, unpacker)),
        session("admin-1"),
    );
    let (status, body) = send(
        admin,
        "POST",
        "/plugins/install",
        Some(json!({"repository_url": "https://github.com/acme/toy"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["message"], "The archive contains symlinks");
    assert!(!rig.dir.join("plugins").join("toy").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn plugin_routes_need_sessions_and_admins() {
    let rig = rig("auth");
    let bare = plugins_router(deps_for(
        &rig,
        Arc::new(FakeFetcher::new()),
        Arc::new(FakeUnpacker::new(Vec::new())),
    ));
    let (status, _) = send(bare, "GET", "/plugins", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    for (method, uri) in [
        ("GET", "/plugins"),
        ("POST", "/plugins/install/preview"),
        ("POST", "/plugins/install"),
        ("POST", "/plugins/toy/update"),
        ("PUT", "/plugins/toy"),
        ("DELETE", "/plugins/toy"),
        ("GET", "/plugins/toy/ui/panel.js"),
    ] {
        let (status, _) = send(router_as(&rig, "user-1"), method, uri, Some(json!({}))).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}");
    }
}

#[tokio::test]
async fn ext_proxy_serves_declared_routes_and_gates_the_rest() {
    let rig = rig("ext");
    let runtime = enable_toy(&rig, TOY_MANIFEST);
    runtime.answer(
        methods::ROUTE,
        Ok(json!({"status": 200, "body": {"ok": true}})),
    );

    let (status, body) = send(
        router_as(&rig, "user-1"),
        "GET",
        "/plugins/ext/toy/status",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"ok": true}));
    let call = runtime.calls_to(methods::ROUTE).pop().unwrap();
    assert_eq!(call["subpath"], "status");

    let (status, _) = send(
        router_as(&rig, "user-1"),
        "GET",
        "/plugins/ext/toy/missing",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = send(
        router_as(&rig, "user-1"),
        "GET",
        "/plugins/ext/ghost/status",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = send(
        router_as(&rig, "user-1"),
        "GET",
        "/plugins/ext/toy/admin-only",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let paced = router_as(&rig, "user-1");
    let (first, _) = send(paced.clone(), "GET", "/plugins/ext/toy/tight", None).await;
    let (second, _) = send(paced, "GET", "/plugins/ext/toy/tight", None).await;
    assert_eq!(
        (first, second),
        (StatusCode::OK, StatusCode::TOO_MANY_REQUESTS)
    );

    // A plugin failure is a fixed 502, never the plugin's text.
    runtime.answer(
        methods::ROUTE,
        Err(droppedneedle::plugins::runtime::CallError::Failed(
            "Traceback: secret".to_owned(),
        )),
    );
    let (status, body) = send(
        router_as(&rig, "user-1"),
        "GET",
        "/plugins/ext/toy/status",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(!body.to_string().contains("secret"));
}

#[tokio::test]
async fn publish_is_stamped_validated_paced_and_loop_guarded() {
    let rig = rig("publish");
    let runtime = enable_toy(&rig, TOY_MANIFEST);

    // Through the plugin's own host request: the host stamps the source.
    let answer = runtime
        .host_request(
            methods::HOST_PUBLISH,
            json!({"kind": "download_note", "payload": {"task_id": "t1"}}),
        )
        .await
        .unwrap();
    assert_eq!(answer["ok"], true);
    let record = rig.host.recent_published().pop().unwrap();
    assert_eq!(record.source_plugin, "toy");
    assert_eq!(record.payload["note"], "");
    assert!(
        runtime
            .host_request(methods::HOST_PUBLISH, json!({"kind": "bogus"}))
            .await
            .is_err()
    );

    let missing = rig
        .host
        .publish_from_plugin("toy", "download_note", &PublishPayload::Empty, "p", None)
        .unwrap();
    assert_eq!(missing.status, 422);
    let ghost = rig
        .host
        .publish_from_plugin("ghost", "download_note", &PublishPayload::Empty, "p", None)
        .unwrap();
    assert_eq!(ghost.status, 404);

    // Notes do not fan out, so the plugin is never busy with an event here.
    let note = PublishPayload::Fields(HashMap::from([("task_id".to_owned(), "t1".to_owned())]));
    let mut statuses = Vec::new();
    for _ in 0..31 {
        let result = rig
            .host
            .publish_from_plugin("toy", "download_note", &note, "pacer", None)
            .unwrap();
        statuses.push(result.status);
    }
    assert!(statuses[..30].iter().all(|status| *status == 200));
    assert_eq!(statuses[30], 429);
}

#[tokio::test]
async fn a_plugin_handling_a_notice_cannot_publish_again() {
    let rig = rig("depth");
    let runtime = enable_toy(&rig, TOY_MANIFEST);
    // The plugin takes a while with every event, so it is still handling
    // the notice when it tries to publish.
    *runtime.delay.lock().unwrap() = Duration::from_millis(300);
    let notice = PublishPayload::Fields(HashMap::from([("title".to_owned(), "hi".to_owned())]));
    rig.host
        .publish_from_plugin("toy", "plugin_notice", &notice, "p", Some("cause-1"))
        .unwrap();
    // The notice fans out to subscribers, including this plugin (same
    // causation is new to it), at depth 1.
    for _ in 0..50 {
        if !runtime.calls_to(methods::EVENT).is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(runtime.calls_to(methods::EVENT)[0]["kind"], "plugin_notice");
    let echo = rig
        .host
        .publish_from_plugin("toy", "plugin_notice", &notice, "p", None)
        .unwrap();
    assert_eq!(echo.status, 409);
}

#[tokio::test]
async fn fanout_reaches_subscribers_once_per_causation() {
    let rig = rig("fanout");
    let manifest = "[plugin]\n\
         name = \"toy\"\n\
         api_version = 1\n\
         entrypoint = \"plugin:Toy\"\n\
         capabilities = [\"subscriber\", \"scrobbler\"]\n";
    let runtime = enable_toy(&rig, manifest);
    let event = scrobble_plugin_event(ScrobbleEvent {
        artist: "A".to_owned(),
        track: "T".to_owned(),
        album: None,
        timestamp: 1_704_067_200,
        duration_ms: None,
        recording_mbid: None,
    });
    rig.host.dispatch_event(event.clone());
    for _ in 0..100 {
        if runtime.calls_to(methods::EVENT).len() == 1
            && runtime.calls_to(methods::SCROBBLE).len() == 1
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    rig.host.dispatch_event(event);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        runtime.calls_to(methods::EVENT).len(),
        1,
        "same causation dedups"
    );
    // The scrobble alias path has no causation dedup (same as v2).
    assert_eq!(runtime.calls_to(methods::SCROBBLE).len(), 2);
}

#[tokio::test]
async fn ticks_run_through_the_jobs_loop_and_state_persists() {
    let rig = rig("ticks");
    let store = TickStoreKind::Memory(MemoryTickStore::new());
    rig.host.set_state_store(Arc::new(store.clone()));
    let runtime = enable_toy(&rig, TOY_MANIFEST);

    let adapter = HostTickAdapter::new(Arc::clone(&rig.host));
    let tick = adapter.get("toy").unwrap();
    assert!(tick.tick_enabled());
    tick.on_tick().await.unwrap();
    assert_eq!(runtime.calls_to(methods::TICK).len(), 1);

    runtime
        .host_request(
            methods::HOST_STATE_SET,
            json!({"key": "cursor", "value": "41"}),
        )
        .await
        .unwrap();
    let read = runtime
        .host_request(methods::HOST_STATE_GET, json!({"key": "cursor"}))
        .await
        .unwrap();
    assert_eq!(read["value"], "41");
    assert!(
        runtime
            .host_request(
                methods::HOST_STATE_SET,
                json!({"key": "../x", "value": "1"})
            )
            .await
            .is_err()
    );

    let registry = JobRegistry::new(MemoryRegistryStore::new());
    let loops = PluginTickLoops::new(registry.clone(), store, Duration::from_secs(5));
    loops.sync_host(&rig.host).await;
    assert!(registry.is_running("plugin-tick:toy"));
    registry.cancel_all(Duration::from_secs(5)).await;
}
