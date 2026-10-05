//! Plugin host: dropped folders run nothing until enabled, installs refuse
//! bad URLs and unsafe archives, plugin secrets are sealed, the ext proxy
//! and panels hide undeclared routes from non-admins, publish is stamped
//! and paced, fan-out runs once per causation, and the admin lifecycle
//! over HTTP. Installs use scripted fetchers and unpackers.

use crate::common::ScratchDir;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
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
    MemoryTickStore as JobsTickStore, TickHost as JobsTickHost, TickPlugin as JobsTickPlugin,
    TickStore as JobsTickStoreTrait,
};
use droppedneedle::jobs::registry::{JobRegistry, MemoryRegistryStore};
use droppedneedle::plugins::fakes::{
    FakeFetcher, FakeLoader, FakeModule, FakeRoles, FakeTickBehavior, FakeUnpacker,
    scrobble_plugin_event,
};
use droppedneedle::plugins::handlers::{ExtRateLimiter, PluginsDeps, plugins_router};
use droppedneedle::plugins::host::{InstallError, PluginHost};
use droppedneedle::plugins::manifest::load_manifest;
use droppedneedle::plugins::runtime::{PublishPayload, ScrobbleEvent};
use droppedneedle::plugins::ticks::{
    HostTickAdapter, NoopTickSync, PluginTickLoops, TickLoopSync, desired_specs,
};
use droppedneedle::runtime_config::ConfigStore;
use droppedneedle::runtime_config::crypto::Crypto;
use tower::ServiceExt as _;

/// Fixed id generator so error ids assert stably.
#[derive(Debug, Clone)]
struct FixedIds;

impl IdGenerator for FixedIds {
    fn new_id(&self) -> String {
        "123e4567-e89b-12d3-a456-426614174000".to_owned()
    }
}

fn test_crypto() -> Crypto {
    Crypto::from_key_bytes(&[7u8; 32]).unwrap()
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
    loader: Arc<FakeLoader>,
}

fn rig(tag: &str) -> Rig {
    let dir = ScratchDir::new(&format!("plugins-{tag}"));
    let config = Arc::new(ConfigStore::open(&dir.join("config.json"), test_crypto()).unwrap());
    let loader = Arc::new(FakeLoader::new());
    let host = Arc::new(PluginHost::new(
        dir.join("plugins"),
        Arc::clone(&config),
        loader.clone(),
    ));
    Rig {
        dir,
        host,
        config,
        loader,
    }
}

fn admin_session(user_id: &str) -> CurrentSession {
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

fn scripted_deps(rig: &Rig) -> PluginsDeps {
    deps_for(
        rig,
        Arc::new(FakeFetcher::new()),
        Arc::new(FakeUnpacker::new(Vec::new())),
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
     auth = \"user\"\n";

#[tokio::test]
async fn dropped_folder_runs_no_code_until_enabled() {
    let rig = rig("no-code");
    write_plugin(&rig.dir, "toy", TOY_MANIFEST);
    rig.host.load_all();
    let plugin = rig.host.get("toy").unwrap();
    assert!(!plugin.enabled);
    assert!(plugin.module.is_none());
    assert!(plugin.active_capabilities.is_empty());
    assert!(rig.host.desired_ticks().is_empty());
}

#[tokio::test]
async fn shipped_example_manifests_all_validate() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../examples/plugins");
    let mut names = Vec::new();
    let entries = std::fs::read_dir(&root).unwrap();
    for entry in entries {
        let dir = entry.unwrap().path();
        if !dir.join("plugin.toml").is_file() {
            continue;
        }
        let manifest = load_manifest(&dir).unwrap();
        names.push(manifest.name);
    }
    assert!(!names.is_empty(), "example plugins vanished from {root:?}");
    assert!(names.contains(&"events-echo-toy".to_owned()));
    assert!(names.contains(&"http-catalog".to_owned()));
}

#[tokio::test]
async fn install_tick_state_journey() {
    // The stage-10 journey: install -> enable -> tick fires through the
    // jobs adapter -> persisted state reads back from the jobs store, and
    // the loop registers under the jobs registry.
    let rig = rig("journey");
    let module = Arc::new(FakeModule::providing(&["scheduler"]));
    *module.tick_behavior.lock().unwrap() =
        FakeTickBehavior::WriteState("{\"cursor\": 41}".to_owned());
    rig.loader.insert("toy", Arc::clone(&module));
    let fetcher = Arc::new(FakeFetcher::new());
    fetcher.insert(
        "https://codeload.github.com/acme/toy/zip/refs/heads/main",
        b"archive-bytes",
    );
    let unpacker = Arc::new(FakeUnpacker::new(vec![
        FakeUnpacker::file("toy-main/plugin.toml", TOY_MANIFEST.as_bytes()),
        FakeUnpacker::file("toy-main/plugin.py", b"# code"),
    ]));

    let archive = PluginHost::fetch_plugin_archive("https://github.com/acme/toy", fetcher.as_ref())
        .await
        .unwrap();
    let name = rig
        .host
        .install_archive(&archive, unpacker.as_ref())
        .unwrap();
    assert_eq!(name, "toy");
    assert!(!rig.host.get("toy").unwrap().enabled);

    rig.host
        .update_settings("toy", true, HashMap::new())
        .unwrap();
    let specs = desired_specs(rig.host.as_ref());
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].name, "toy");
    assert_eq!(specs[0].interval, Duration::from_secs(60 * 60));
    assert!(specs[0].run_on_load);

    let store = JobsTickStore::with_plugins(&["toy"]);
    let adapter = HostTickAdapter::new(Arc::clone(&rig.host), store.clone());
    let plugin = adapter.get("toy").unwrap();
    assert!(plugin.tick_enabled());
    plugin.on_tick().await.unwrap();
    assert_eq!(module.tick_count(), 1);
    let bytes = store.read("toy", "state").await.unwrap().unwrap();
    assert_eq!(bytes, b"{\"cursor\": 41}");

    let registry = JobRegistry::new(MemoryRegistryStore::new());
    let loops = PluginTickLoops::new(registry.clone(), store, Duration::from_secs(5));
    loops.sync_host(&rig.host).await;
    assert!(registry.is_running("plugin-tick:toy"));
    registry.cancel_all(Duration::from_secs(5)).await;
}

#[tokio::test]
async fn secret_values_encrypt_mask_and_round_trip() {
    let rig = rig("secrets");
    let manifest = "[plugin]\n\
         name = \"toy\"\n\
         api_version = 1\n\
         entrypoint = \"plugin:Toy\"\n\
         capabilities = [\"subscriber\"]\n\
         [[settings]]\n\
         key = \"api_key\"\n\
         label = \"API key\"\n\
         secret = true\n\
         [[settings]]\n\
         key = \"nick\"\n\
         label = \"Nick\"\n";
    write_plugin(&rig.dir, "toy", manifest);
    rig.loader
        .insert("toy", Arc::new(FakeModule::providing(&["subscriber"])));
    rig.host.load_all();

    let mut settings = HashMap::new();
    settings.insert("api_key".to_owned(), "live-secret".to_owned());
    settings.insert("nick".to_owned(), "toybrick".to_owned());
    rig.host.update_settings("toy", true, settings).unwrap();

    // Ciphertext at rest, plaintext nowhere on disk.
    let raw = std::fs::read_to_string(rig.dir.join("config.json")).unwrap();
    assert!(!raw.contains("live-secret"));
    assert!(raw.contains("toybrick"));

    // Masked reads show the sentinel; raw reads decrypt.
    let secret_keys = rig.host.get("toy").unwrap().manifest.secret_keys();
    let masked = rig.config.get_plugin_masked("toy", &secret_keys).unwrap();
    assert_eq!(masked.settings["api_key"], "plugin****");
    assert_eq!(masked.settings["nick"], "toybrick");
    let plain = rig.config.get_plugin_raw("toy", &secret_keys).unwrap();
    assert_eq!(plain.settings["api_key"], "live-secret");

    // Sending the mask back keeps the stored value.
    let mut keep = HashMap::new();
    keep.insert("api_key".to_owned(), "plugin****".to_owned());
    keep.insert("nick".to_owned(), "toybrick".to_owned());
    rig.host.update_settings("toy", true, keep).unwrap();
    let again = rig.config.get_plugin_raw("toy", &secret_keys).unwrap();
    assert_eq!(again.settings["api_key"], "live-secret");
}

#[tokio::test]
async fn uninstall_removes_code_but_keeps_settings() {
    let rig = rig("uninstall");
    write_plugin(&rig.dir, "toy", TOY_MANIFEST);
    rig.loader
        .insert("toy", Arc::new(FakeModule::providing(&["scheduler"])));
    rig.host.load_all();
    let mut settings = HashMap::new();
    settings.insert("level".to_owned(), "11".to_owned());
    rig.host.update_settings("toy", true, settings).unwrap();
    assert!(rig.dir.join("plugins").join("toy").is_dir());

    rig.host.uninstall("toy").unwrap();
    assert!(!rig.dir.join("plugins").join("toy").exists());
    assert!(rig.host.get("toy").is_none());
    let stored = rig.config.get_plugin("toy").unwrap();
    assert_eq!(stored.settings["level"], "11");

    assert!(rig.host.uninstall("toy").is_err());
}

#[tokio::test]
async fn install_rejects_bad_urls() {
    let fetcher = FakeFetcher::new();
    for bad in [
        "",
        "https://example.com/acme/toy",
        "https://github.com/acme",
        "https://github.com/acme/toy/blob/main/x",
        "https://github.com/../etc/toy",
        "https://github.com/acme/toy/tree/feat/../x",
    ] {
        assert_eq!(
            PluginHost::fetch_plugin_archive(bad, &fetcher).await,
            Err(InstallError::InvalidUrl),
            "url: {bad}"
        );
    }
}

#[tokio::test]
async fn install_refuses_unsafe_archives() {
    let rig = rig("unsafe");
    let traversal = FakeUnpacker::new(vec![
        FakeUnpacker::file("toy-main/plugin.toml", TOY_MANIFEST.as_bytes()),
        FakeUnpacker::file("toy-main/../../evil.py", b"evil"),
    ]);
    assert_eq!(
        rig.host.install_archive(b"zip", &traversal),
        Err(InstallError::UnsafePaths)
    );
    let mut symlink = FakeUnpacker::file("toy-main/plugin.toml", TOY_MANIFEST.as_bytes());
    symlink.is_symlink = true;
    let links = FakeUnpacker::new(vec![symlink]);
    assert_eq!(
        rig.host.install_archive(b"zip", &links),
        Err(InstallError::Symlinks)
    );
    assert!(!rig.dir.join("plugins").join("toy").exists());
}

async fn json_body(response: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn enable_toy(rig: &Rig, manifest: &str, caps: &[&str]) -> Arc<FakeModule> {
    write_plugin(&rig.dir, "toy", manifest);
    let module = Arc::new(FakeModule::providing(caps));
    rig.loader.insert("toy", Arc::clone(&module));
    rig.host.load_all();
    rig.host
        .update_settings("toy", true, HashMap::new())
        .unwrap();
    module
}

#[tokio::test]
async fn ext_proxy_serves_declared_routes_and_hides_the_rest() {
    let rig = rig("ext");
    enable_toy(&rig, TOY_MANIFEST, &["publisher"]);
    let router = with_test_principal(
        plugins_router(scripted_deps(&rig)),
        admin_session("admin-1"),
    );

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/plugins/ext/toy/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(json_body(response).await, serde_json::json!({"ok": true}));

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/plugins/ext/toy/missing")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let response = router
        .oneshot(
            Request::builder()
                .uri("/plugins/ext/ghost/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn ext_proxy_gates_admin_routes_and_paces_callers() {
    let rig = rig("ext-gate");
    let manifest = "[plugin]\n\
         name = \"toy\"\n\
         api_version = 1\n\
         entrypoint = \"plugin:Toy\"\n\
         capabilities = [\"publisher\"]\n\
         [[route]]\n\
         path = \"admin-only\"\n\
         method = \"GET\"\n\
         auth = \"admin\"\n\
         [[route]]\n\
         path = \"tight\"\n\
         method = \"GET\"\n\
         auth = \"user\"\n\
         rate_limit_per_minute = 1\n";
    enable_toy(&rig, manifest, &["publisher"]);

    let user_router =
        with_test_principal(plugins_router(scripted_deps(&rig)), admin_session("user-1"));
    let response = user_router
        .oneshot(
            Request::builder()
                .uri("/plugins/ext/toy/admin-only")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let paced = with_test_principal(plugins_router(scripted_deps(&rig)), admin_session("user-1"));
    let first = paced
        .clone()
        .oneshot(
            Request::builder()
                .uri("/plugins/ext/toy/tight")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let second = paced
        .oneshot(
            Request::builder()
                .uri("/plugins/ext/toy/tight")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(second.headers().contains_key("retry-after"));
}

#[tokio::test]
async fn panel_bundle_hides_missing_panels_and_non_admins() {
    let rig = rig("panel-gate");
    let manifest = "[plugin]\n\
         name = \"toy\"\n\
         api_version = 1\n\
         entrypoint = \"plugin:Toy\"\n\
         capabilities = [\"subscriber\"]\n";
    enable_toy(&rig, manifest, &["subscriber"]);

    // No panel declared: missing, even for admins.
    let admin = with_test_principal(
        plugins_router(scripted_deps(&rig)),
        admin_session("admin-1"),
    );
    let response = admin
        .oneshot(
            Request::builder()
                .uri("/plugins/toy/ui/panel.js")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    // Non-admins never see panels.
    let user = with_test_principal(plugins_router(scripted_deps(&rig)), admin_session("user-1"));
    let response = user
        .oneshot(
            Request::builder()
                .uri("/plugins/toy/ui/panel.js")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn publish_validates_fields_and_stamps_the_caller() {
    let rig = rig("publish");
    enable_toy(&rig, TOY_MANIFEST, &["publisher"]);

    assert!(
        PluginHost::publish_from_plugin(
            rig.host.as_ref(),
            "toy",
            "bogus",
            &PublishPayload::Empty,
            "",
            None,
        )
        .is_err()
    );

    let mut fields = HashMap::new();
    fields.insert("task_id".to_owned(), "t1".to_owned());
    let ok = rig
        .host
        .publish_from_plugin(
            "toy",
            "download_note",
            &PublishPayload::Fields(fields),
            "",
            None,
        )
        .unwrap();
    assert!(ok.ok);
    let drained = rig.host.drain_published();
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].source_plugin, "toy");
    assert_eq!(drained[0].payload["task_id"], "t1");
    assert_eq!(drained[0].payload["note"], "");
    assert!(!drained[0].causation_id.is_empty());

    let bad = rig
        .host
        .publish_from_plugin("toy", "download_note", &PublishPayload::Empty, "", None)
        .unwrap();
    assert_eq!(bad.status, 422);

    let ghost = rig
        .host
        .publish_from_plugin("ghost", "download_note", &PublishPayload::Empty, "", None)
        .unwrap();
    assert_eq!(ghost.status, 404);
}

#[tokio::test]
async fn publish_rate_limits_each_plugin() {
    let rig = rig("publish-rate");
    enable_toy(&rig, TOY_MANIFEST, &["publisher"]);
    let mut fields = HashMap::new();
    fields.insert("title".to_owned(), "hi".to_owned());
    let payload = PublishPayload::Fields(fields);
    for _ in 0..30 {
        let result = rig
            .host
            .publish_from_plugin("toy", "plugin_notice", &payload, "", None)
            .unwrap();
        assert!(result.ok);
    }
    let limited = rig
        .host
        .publish_from_plugin("toy", "plugin_notice", &payload, "", None)
        .unwrap();
    assert_eq!(limited.status, 429);
    assert!(limited.retry_after >= 1);
}

#[tokio::test]
async fn fanout_reaches_subscribers_once_per_causation() {
    let rig = rig("fanout");
    let manifest = "[plugin]\n\
         name = \"toy\"\n\
         api_version = 1\n\
         entrypoint = \"plugin:Toy\"\n\
         capabilities = [\"subscriber\", \"scrobbler\"]\n";
    let module = enable_toy(&rig, manifest, &["subscriber", "scrobbler"]);
    let event = scrobble_plugin_event(ScrobbleEvent {
        artist: "A".to_owned(),
        track: "T".to_owned(),
        album: None,
        timestamp: 1_704_067_200,
        duration_ms: None,
        recording_mbid: None,
    });
    rig.host.dispatch_event(event.clone()).await;
    // Same causation twice: the second dispatch dedups.
    rig.host.dispatch_event(event).await;
    // Let the spawned notifications land.
    for _ in 0..100 {
        if module.event_count() >= 1 && module.scrobbles.lock().unwrap().len() >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(module.event_count(), 1);
    // The scrobble kind also reaches scrobbler plugins, but that path has
    // no causation dedup (same as v2): two dispatches fire it twice.
    assert_eq!(module.scrobbles.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn plugin_routes_need_admins_and_sessions() {
    let rig = rig("auth");
    // No session at all: 401 with the Bearer challenge.
    let bare = plugins_router(scripted_deps(&rig));
    let response = bare
        .oneshot(
            Request::builder()
                .uri("/plugins")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response.headers().get("www-authenticate").unwrap(),
        "Bearer"
    );

    // Plain users cannot list plugins.
    let user = with_test_principal(plugins_router(scripted_deps(&rig)), admin_session("user-1"));
    let response = user
        .oneshot(
            Request::builder()
                .uri("/plugins")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn admin_plugin_lifecycle_over_http() {
    let rig = rig("lifecycle");
    rig.loader
        .insert("toy", Arc::new(FakeModule::providing(&["subscriber"])));
    let fetcher = Arc::new(FakeFetcher::new());
    fetcher.insert(
        "https://codeload.github.com/acme/toy/zip/refs/heads/main",
        b"archive",
    );
    let unpacker = Arc::new(FakeUnpacker::new(vec![FakeUnpacker::file(
        "toy-main/plugin.toml",
        TOY_MANIFEST.as_bytes(),
    )]));
    let router = with_test_principal(
        plugins_router(deps_for(&rig, fetcher, unpacker)),
        admin_session("admin-1"),
    );
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/plugins/install")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"repository_url": "https://github.com/acme/toy"})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let installed = json_body(response).await;
    assert_eq!(installed["name"], "toy");
    assert_eq!(installed["enabled"], false);

    let router = with_test_principal(
        plugins_router(scripted_deps(&rig)),
        admin_session("admin-1"),
    );
    let response = router
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/plugins/toy")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"enabled": true, "settings": {}}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(json_body(response).await["enabled"], true);

    let router = with_test_principal(
        plugins_router(scripted_deps(&rig)),
        admin_session("admin-1"),
    );
    let response = router
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/plugins/toy")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

/// Plugin route statuses clamp to the fixed table: upstream 5xx becomes
/// 502, a redirect becomes 200, and a 404 passes through.
#[tokio::test]
async fn ext_route_statuses_clamp_to_the_fixed_table() {
    use droppedneedle::plugins::fakes::FakeRouteScript;
    use droppedneedle::plugins::runtime::PluginRouteBody;

    let rig = rig("ext-clamp");
    let module = enable_toy(&rig, TOY_MANIFEST, &["publisher"]);
    let query = HashMap::new();
    for (plugin_status, served) in [(500, 502), (302, 200), (404, 404)] {
        *module.route_scripts.lock().unwrap() = vec![FakeRouteScript::Status(
            plugin_status,
            serde_json::json!({}),
        )];
        let result = rig
            .host
            .handle_plugin_route("toy", "GET", "status", &query, &PluginRouteBody::Empty)
            .await;
        assert_eq!(result.status, served, "plugin answered {plugin_status}");
    }
}
