//! Plugin host with fake runtimes: dropped folders run nothing until
//! enabled, secrets are sealed and reach the plugin decrypted, settings
//! saves reach a running plugin without a restart, installs pin a commit
//! and show the trust warning, the ext proxy and panels hide undeclared
//! routes from non-admins, publish is stamped, paced and loop-guarded,
//! fan-out runs once per causation, and plugin state persists through the
//! tick store. The real subprocess runtime is tested with the shipped
//! examples in `plugins_examples`.

use crate::common::auth::{test_principal, with_test_principal};
use crate::common::{ScratchDir, TestApp};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crate::common::plugin_fakes::{
    FakeConnection, FakeFetcher, FakeLauncher, scrobble_plugin_event,
};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use droppedneedle_auth::Principal;
use droppedneedle_auth::session::store::SessionKind;
use droppedneedle_auth::users::roles::Role;
use droppedneedle_kernel::clock::SystemClock;
use droppedneedle_kernel::db::Db;
use droppedneedle_kernel::jobs::plugin_ticks::{
    SqliteTickStore, TickHost as JobsTickHost, TickPlugin as JobsTickPlugin,
};
use droppedneedle_kernel::jobs::queue::JobQueue;
use droppedneedle_kernel::platform::Platform;
use droppedneedle_kernel::runtime_config::ConfigStore;
use droppedneedle_kernel::runtime_config::crypto::Crypto;
use droppedneedle_library::wiring::LibraryCore;
use droppedneedle_plugins::handlers::{ExtRateLimiter, PluginsDeps, plugin_routes};
use droppedneedle_plugins::host::PluginHost;
use droppedneedle_plugins::install::{Fetched, InstallError, parse_source, resolve};
use droppedneedle_plugins::models::PluginRefKind;
use droppedneedle_plugins::protocol::methods;
use droppedneedle_plugins::runtime::{PublishPayload, ScrobbleEvent};
use droppedneedle_plugins::ticks::{HostTickAdapter, PluginTickLoops, TICK_CANCEL_GRACE};
use serde_json::json;
use tower::ServiceExt as _;

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
    /// The host's plugin state store.
    ticks: SqliteTickStore,
    /// The platform the library's write side is built over: its settings
    /// hold the library folders plugin files go to.
    platform: Platform,
    /// The library's write side the host adds plugin files through.
    library: LibraryCore,
}

async fn rig(tag: &str) -> Rig {
    let dir = ScratchDir::new(&format!("plugins-{tag}"));
    let crypto = Crypto::from_key_bytes(&[7u8; 32]).unwrap();
    let config = Arc::new(ConfigStore::scratch(&dir, crypto).unwrap());
    let launcher = Arc::new(FakeLauncher::new());
    crate::common::migrated_db(&dir.join("app.db"));
    let db = Db::open_for_tests(&dir.join("app.db")).unwrap();
    let platform = crate::common::TestPlatform::on(&db, &dir).build().await;
    let ticks = droppedneedle_plugins::wiring::tick_store(&platform);
    let library =
        crate::common::library::library_core(&platform, JobQueue::new(db, Arc::new(SystemClock)));
    let host = PluginHost::new(
        dir.join("plugins"),
        Arc::clone(&config),
        launcher.clone(),
        ticks.clone(),
        library.clone(),
    );
    Rig {
        dir,
        host,
        config,
        launcher,
        ticks,
        platform,
        library,
    }
}

fn session(user_id: &str) -> Principal {
    test_principal(
        user_id,
        if user_id == "admin-1" {
            Role::Admin
        } else {
            Role::User
        },
        "session-1",
        SessionKind::Standard,
    )
}

fn deps_for(rig: &Rig, fetcher: Arc<FakeFetcher>) -> PluginsDeps {
    PluginsDeps {
        host: Arc::clone(&rig.host),
        config: Arc::clone(&rig.config),
        github: droppedneedle::bootstrap::Outbound::live().plugin_hosts(),
        fetcher,
        tick_sync: Arc::new(PluginTickLoops::new(
            rig.platform.supervisor.clone(),
            rig.ticks.clone(),
            TICK_CANCEL_GRACE,
        )),
        ext_limiter: Arc::new(ExtRateLimiter::new()),
        clock: Arc::new(droppedneedle_kernel::clock::SystemClock),
    }
}

fn router_as(rig: &Rig, user: &str) -> axum::Router {
    let deps = deps_for(rig, Arc::new(FakeFetcher::new()));
    with_test_principal(
        plugin_routes().with_state(deps).into_router(),
        session(user),
    )
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
     api_version = 2\n\
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
     path = \"status/deep\"\n\
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

/// Give the tasks the host spawned their turns until `done` holds. The fake
/// runtime never leaves the scheduler, so no clock is involved.
async fn turns_until(done: impl Fn() -> bool) {
    for _ in 0..1_000 {
        if done() {
            return;
        }
        tokio::task::yield_now().await;
    }
}

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
    let rig = rig("no-code").await;
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
    let rig = rig("secrets").await;
    let manifest = "[plugin]\n\
         name = \"toy\"\n\
         api_version = 2\n\
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
    let mut raw = std::fs::read(rig.dir.join("settings.db")).unwrap();
    raw.extend(std::fs::read(rig.dir.join("settings.db-wal")).unwrap_or_default());
    assert!(!String::from_utf8_lossy(&raw).contains("live-secret"));

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
        "********"
    );
    let keep = HashMap::from([("api_key".to_owned(), "********".to_owned())]);
    rig.host.update_settings("toy", true, keep).unwrap();
    assert_eq!(
        rig.config
            .get_plugin_raw("toy", &keys)
            .unwrap()
            .into_settings()["api_key"],
        "live-secret"
    );
}

#[tokio::test]
async fn uninstall_stops_the_plugin_and_keeps_settings() {
    let rig = rig("uninstall").await;
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
    turns_until(|| runtime.was_stopped()).await;
    assert!(runtime.was_stopped());
    assert_eq!(
        rig.config.get_plugin("toy").unwrap().settings["level"],
        "11"
    );
}

const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

const REPOSITORY: &str = "https://api.github.com/repos/acme/toy";

fn github(fetcher: &FakeFetcher) {
    fetcher.insert(REPOSITORY, br#"{"id": 1001}"#);
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
        &toy_zip(None),
    );
}

/// The toy plugin as GitHub's codeload serves it: one top folder holding
/// the manifest and the code, or a symlink in place of the code.
fn toy_zip(code_link: Option<&str>) -> Vec<u8> {
    use std::io::Write as _;

    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    writer
        .start_file("toy-0123456/plugin.toml", options)
        .unwrap();
    writer.write_all(TOY_MANIFEST.as_bytes()).unwrap();
    match code_link {
        Some(target) => writer
            .add_symlink("toy-0123456/plugin.py", target, options)
            .unwrap(),
        None => {
            writer.start_file("toy-0123456/plugin.py", options).unwrap();
            writer.write_all(b"# code").unwrap();
        }
    }
    writer.finish().unwrap().into_inner()
}

#[tokio::test]
async fn install_previews_permissions_then_pins_the_previewed_commit() {
    let rig = rig("install").await;
    let fetcher = Arc::new(FakeFetcher::new());
    github(&fetcher);
    let deps = deps_for(&rig, Arc::clone(&fetcher));
    let admin = with_test_principal(
        plugin_routes().with_state(deps).into_router(),
        session("admin-1"),
    );
    let body = json!({"repository_url": "https://github.com/acme/toy"});

    let (status, preview) = send(
        admin.clone(),
        "POST",
        "/api/v3/plugins/install/preview",
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
    let (status, _) = send(
        admin.clone(),
        "POST",
        "/api/v3/plugins/install",
        Some(stale),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    let pinned = json!({"repository_url": "https://github.com/acme/toy", "commit": SHA});
    let (status, installed) = send(
        admin.clone(),
        "POST",
        "/api/v3/plugins/install",
        Some(pinned),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(installed["enabled"], false, "installs arrive disabled");
    assert_eq!(installed["install"]["commit"], SHA);
    assert_eq!(installed["install"]["ref_kind"], "release");
    assert!(rig.launcher.launched.lock().unwrap().is_empty());

    // Same release again: nothing to update.
    let (status, update) = send(admin, "POST", "/api/v3/plugins/toy/update", Some(json!({}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(update["updated"], false);
}

const LATEST_RELEASE: &str = "https://api.github.com/repos/acme/toy/releases/latest";
const DEFAULT_BRANCH: &str = "https://api.github.com/repos/acme/toy/commits/HEAD";
const BRANCH_SHA: &str = "89abcdef0123456789abcdef0123456789abcdef";

/// The default branch holds unreleased code that would install fine.
fn default_branch(fetcher: &FakeFetcher) {
    fetcher.insert(REPOSITORY, br#"{"id": 1001}"#);
    fetcher.insert(
        DEFAULT_BRANCH,
        format!(r#"{{"sha": "{BRANCH_SHA}"}}"#).as_bytes(),
    );
    fetcher.insert(
        &format!("https://codeload.github.com/acme/toy/zip/{BRANCH_SHA}"),
        &toy_zip(None),
    );
}

/// The admin's router behind `scope`'s request wrapper, which replaces
/// every 5xx body with the fixed one.
fn admin_app(scope: &TestApp, deps: PluginsDeps) -> axum::Router {
    scope.request_scope(with_test_principal(
        plugin_routes().with_state(deps).into_router(),
        session("admin-1"),
    ))
}

/// What an admin reads when a request fails: status, code, sentence, and
/// the wait GitHub asked for in the header and in `details`.
#[derive(Debug, PartialEq)]
struct Refusal {
    status: StatusCode,
    code: String,
    message: String,
    retry_after: Option<String>,
    details_retry_after: Option<u64>,
}

async fn post_for_refusal(router: axum::Router, uri: &str, body: serde_json::Value) -> Refusal {
    let request = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = router.oneshot(request).await.unwrap();
    let status = response.status();
    let retry_after = response
        .headers()
        .get("retry-after")
        .map(|value| value.to_str().unwrap().to_owned());
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    Refusal {
        status,
        code: json["error"]["code"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        message: json["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        retry_after,
        details_retry_after: json["error"]["details"]["retry_after"].as_u64(),
    }
}

/// An install or update GitHub refuses, limits or cannot answer tells the
/// admin which, in the installer's own sentence, through the request
/// boundary that replaces every other 5xx body.
#[tokio::test]
async fn a_github_refusal_never_falls_back_to_the_default_branch() {
    let refusal = |status, code: &str, message: &str, retry_after: Option<u64>| Refusal {
        status,
        code: code.to_owned(),
        message: message.to_owned(),
        retry_after: retry_after.map(|seconds| seconds.to_string()),
        details_retry_after: retry_after,
    };
    let cases = [
        (
            Fetched::Denied,
            InstallError::Denied,
            refusal(
                StatusCode::BAD_GATEWAY,
                "UPSTREAM_REFUSED",
                "GitHub refused the request",
                None,
            ),
        ),
        (
            Fetched::RateLimited {
                retry_after: Some(90),
            },
            InstallError::RateLimited {
                retry_after: Some(90),
            },
            refusal(
                StatusCode::SERVICE_UNAVAILABLE,
                "UPSTREAM_RATE_LIMITED",
                "GitHub is rate limiting this server, try again later",
                Some(90),
            ),
        ),
        (
            Fetched::Failed(502),
            InstallError::Download,
            refusal(
                StatusCode::BAD_GATEWAY,
                "UPSTREAM_ERROR",
                "GitHub could not be reached",
                None,
            ),
        ),
    ];
    let scope = TestApp::start("plugins-refused").await;
    for (index, (answer, kind, expected)) in cases.into_iter().enumerate() {
        let rig = rig(&format!("refused-{index}")).await;
        let fetcher = Arc::new(FakeFetcher::new());
        github(&fetcher);
        default_branch(&fetcher);
        fetcher.answer(LATEST_RELEASE, answer.clone());
        let source = parse_source("https://github.com/acme/toy", None).unwrap();
        assert_eq!(
            resolve(
                fetcher.as_ref(),
                &droppedneedle::bootstrap::Outbound::live().plugin_hosts(),
                source
            )
            .await,
            Err(kind)
        );

        let admin = admin_app(&scope, deps_for(&rig, Arc::clone(&fetcher)));
        let body = json!({"repository_url": "https://github.com/acme/toy"});
        for uri in ["/api/v3/plugins/install/preview", "/api/v3/plugins/install"] {
            let got = post_for_refusal(admin.clone(), uri, body.clone()).await;
            assert_eq!(got, expected, "{uri}");
        }
        assert!(!rig.dir.join("plugins").join("toy").exists());

        // An installed release must not start following the branch either.
        github(&fetcher);
        let (got, _) = send(admin.clone(), "POST", "/api/v3/plugins/install", Some(body)).await;
        assert_eq!(got, StatusCode::CREATED);
        fetcher.answer(LATEST_RELEASE, answer);
        let got = post_for_refusal(admin, "/api/v3/plugins/toy/update", json!({})).await;
        assert_eq!(got, expected, "update");
        let record = rig.host.install_record("toy").unwrap();
        assert_eq!(
            (record.ref_kind, record.commit.as_str()),
            (PluginRefKind::Release, SHA)
        );

        let requested = fetcher.requested.lock().unwrap();
        assert!(!requested.iter().any(|url| url == DEFAULT_BRANCH));
    }
}

#[tokio::test]
async fn a_repository_without_releases_installs_its_default_branch() {
    let rig = rig("no-releases").await;
    let fetcher = Arc::new(FakeFetcher::new());
    fetcher.answer(LATEST_RELEASE, Fetched::NotFound);
    default_branch(&fetcher);
    let scope = TestApp::start("plugins-no-releases").await;
    let admin = admin_app(&scope, deps_for(&rig, fetcher));
    let (status, installed) = send(
        admin,
        "POST",
        "/api/v3/plugins/install",
        Some(json!({"repository_url": "https://github.com/acme/toy"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(installed["install"]["ref_kind"], "branch");
    assert_eq!(installed["install"]["reference"], "HEAD");
    assert_eq!(installed["install"]["commit"], BRANCH_SHA);
}

/// A repository deleted and created again under the same address holds
/// someone else's code: an update refuses it and installs nothing.
#[tokio::test]
async fn an_update_never_installs_from_a_replaced_repository() {
    let rig = rig("replaced").await;
    let fetcher = Arc::new(FakeFetcher::new());
    github(&fetcher);
    let admin = with_test_principal(
        plugin_routes()
            .with_state(deps_for(&rig, Arc::clone(&fetcher)))
            .into_router(),
        session("admin-1"),
    );
    let body = json!({"repository_url": "https://github.com/acme/toy"});
    let (status, _) = send(admin.clone(), "POST", "/api/v3/plugins/install", Some(body)).await;
    assert_eq!(status, StatusCode::CREATED);

    fetcher.insert(REPOSITORY, br#"{"id": 2002}"#);
    fetcher.insert(LATEST_RELEASE, br#"{"tag_name": "v2.0.0"}"#);
    fetcher.insert(
        "https://api.github.com/repos/acme/toy/commits/v2.0.0",
        format!(r#"{{"sha": "{BRANCH_SHA}"}}"#).as_bytes(),
    );
    let new_code = format!("https://codeload.github.com/acme/toy/zip/{BRANCH_SHA}");
    fetcher.insert(&new_code, &toy_zip(None));
    let (status, error) = send(admin, "POST", "/api/v3/plugins/toy/update", Some(json!({}))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        error["error"]["message"],
        "The repository at this address was replaced since the plugin was installed. Install it again if you trust the new one"
    );
    assert_eq!(rig.host.install_record("toy").unwrap().commit, SHA);
    assert!(!fetcher.requested.lock().unwrap().contains(&new_code));
}

#[tokio::test]
async fn a_ref_github_does_not_know_is_the_admins_mistake_not_an_outage() {
    let rig = rig("unknown-ref").await;
    let fetcher = Arc::new(FakeFetcher::new());
    fetcher.insert(REPOSITORY, br#"{"id": 1001}"#);
    fetcher.answer(
        "https://api.github.com/repos/acme/toy/commits/mian",
        Fetched::Failed(422),
    );
    let scope = TestApp::start("plugins-unknown-ref").await;
    let admin = admin_app(&scope, deps_for(&rig, fetcher));
    let (status, error) = send(
        admin,
        "POST",
        "/api/v3/plugins/install",
        Some(json!({"repository_url": "https://github.com/acme/toy/tree/mian"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        error["error"]["message"],
        "GitHub has no release, tag or branch named mian in that repository"
    );
    assert!(!rig.dir.join("plugins").join("toy").exists());
}

#[tokio::test]
async fn unsafe_archives_never_reach_the_plugins_folder() {
    let rig = rig("unsafe").await;
    let fetcher = Arc::new(FakeFetcher::new());
    github(&fetcher);
    fetcher.insert(
        &format!("https://codeload.github.com/acme/toy/zip/{SHA}"),
        &toy_zip(Some("/etc/passwd")),
    );
    let admin = with_test_principal(
        plugin_routes()
            .with_state(deps_for(&rig, fetcher))
            .into_router(),
        session("admin-1"),
    );
    let (status, body) = send(
        admin,
        "POST",
        "/api/v3/plugins/install",
        Some(json!({"repository_url": "https://github.com/acme/toy"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["message"], "The archive contains symlinks");
    assert!(!rig.dir.join("plugins").join("toy").exists());
}

#[tokio::test]
async fn plugin_routes_need_sessions_and_admins() {
    let rig = rig("auth").await;
    let bare = plugin_routes()
        .with_state(deps_for(&rig, Arc::new(FakeFetcher::new())))
        .into_router();
    let (status, _) = send(bare, "GET", "/api/v3/plugins", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    for (method, uri) in [
        ("GET", "/api/v3/plugins"),
        ("POST", "/api/v3/plugins/install/preview"),
        ("POST", "/api/v3/plugins/install"),
        ("POST", "/api/v3/plugins/toy/update"),
        ("PUT", "/api/v3/plugins/toy"),
        ("DELETE", "/api/v3/plugins/toy"),
        ("GET", "/api/v3/plugins/toy/ui/panel"),
    ] {
        let (status, _) = send(router_as(&rig, "user-1"), method, uri, Some(json!({}))).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}");
    }
}

/// The panel page is plugin script shown inside the admin UI. Its policy is
/// what keeps that script out of the app's origin, so it is on every answer
/// a browser can cache, and the script text cannot close its own element.
/// Only the app may frame it, plus the addresses an admin allowed to frame
/// the app: a browser checks every ancestor, so a panel inside a framed app
/// would otherwise stay empty. The layer that gives every other answer its
/// policy leaves this one as the route set it.
#[tokio::test]
async fn the_panel_page_is_sealed_on_every_answer() {
    use droppedneedle::security_headers::{ContentSecurity, content_security};
    use droppedneedle_kernel::runtime_config::sections::SecuritySettings;

    const DASHBOARD: &str = "https://home.example.com";
    let policy = |ancestors: &str| {
        format!(
            "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; \
             img-src data: blob:; connect-src 'none'; frame-src 'none'; object-src 'none'; \
             base-uri 'none'; form-action 'none'; frame-ancestors {ancestors}; \
             sandbox allow-scripts"
        )
    };
    let rig = rig("panel").await;
    let manifest = format!("{TOY_MANIFEST}[plugin_ui]\nentry = \"ui/panel.js\"\n");
    write_plugin(&rig.dir, "toy", &manifest);
    let ui = rig.dir.join("plugins").join("toy").join("ui");
    std::fs::create_dir_all(&ui).unwrap();
    std::fs::write(
        ui.join("panel.js"),
        "var a = 1;</script><script>var b = 2;</SCRIPT >",
    )
    .unwrap();
    rig.host.load_all();

    let get = |etag: Option<String>| {
        let mut request = Request::builder().uri("/api/v3/plugins/toy/ui/panel");
        if let Some(etag) = etag {
            request = request.header("if-none-match", etag);
        }
        router_as(&rig, "admin-1")
            .layer(axum::middleware::from_fn_with_state(
                ContentSecurity::new(rig.config.live()),
                content_security,
            ))
            .oneshot(request.body(Body::empty()).unwrap())
    };
    let sealed = |response: &axum::http::Response<Body>, ancestors: &str| {
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned)
        };
        assert_eq!(
            header("content-security-policy").as_deref(),
            Some(policy(ancestors).as_str())
        );
        assert_eq!(header("content-security-policy-report-only"), None);
        assert_eq!(header("x-content-type-options").as_deref(), Some("nosniff"));
        assert_eq!(
            header("content-type").as_deref(),
            Some("text/html; charset=utf-8")
        );
    };

    // Dropped in but not enabled: no page, and an answer nothing runs in.
    let missing = get(None).await.unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        missing.headers()["content-security-policy"],
        "default-src 'none'; frame-ancestors 'none'; sandbox"
    );
    rig.host
        .update_settings("toy", true, HashMap::new())
        .unwrap();

    let response = get(None).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    sealed(&response, "'self'");
    let etag = response.headers()["etag"].to_str().unwrap().to_owned();
    assert!(etag.contains(env!("CARGO_PKG_VERSION")), "{etag}");
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let page = String::from_utf8(bytes.to_vec()).unwrap().to_lowercase();
    assert_eq!(page.matches("</script").count(), 1, "{page}");
    assert!(page.ends_with("</script></body></html>"), "{page}");
    assert!(page.contains("var b = 2;"), "{page}");

    rig.config
        .save_blocking(SecuritySettings {
            frame_ancestors: vec![format!("{DASHBOARD}/")],
            ..SecuritySettings::default()
        })
        .unwrap();
    let framed = format!("'self' {DASHBOARD}");
    sealed(&get(None).await.unwrap(), &framed);
    let response = get(Some(etag)).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    sealed(&response, &framed);

    let (status, _) = send(
        router_as(&rig, "admin-1"),
        "GET",
        "/api/v3/plugins/ghost/ui/panel",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn ext_proxy_serves_declared_routes_and_gates_the_rest() {
    let rig = rig("ext").await;
    let runtime = enable_toy(&rig, TOY_MANIFEST);
    runtime.answer(
        methods::ROUTE,
        Ok(json!({"status": 200, "body": {"ok": true}})),
    );

    let (status, body) = send(
        router_as(&rig, "user-1"),
        "GET",
        "/api/v3/plugins/ext/toy/status",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"ok": true}));
    let call = runtime.calls_to(methods::ROUTE).pop().unwrap();
    assert_eq!(call["subpath"], "status");

    // A declared route's path may hold slashes.
    let (status, _) = send(
        router_as(&rig, "user-1"),
        "GET",
        "/api/v3/plugins/ext/toy/status/deep",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let call = runtime.calls_to(methods::ROUTE).pop().unwrap();
    assert_eq!(call["subpath"], "status/deep");

    let (status, _) = send(
        router_as(&rig, "user-1"),
        "GET",
        "/api/v3/plugins/ext/toy/missing",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = send(
        router_as(&rig, "user-1"),
        "GET",
        "/api/v3/plugins/ext/ghost/status",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = send(
        router_as(&rig, "user-1"),
        "GET",
        "/api/v3/plugins/ext/toy/admin-only",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let paced = router_as(&rig, "user-1");
    let (first, _) = send(paced.clone(), "GET", "/api/v3/plugins/ext/toy/tight", None).await;
    let (second, _) = send(paced, "GET", "/api/v3/plugins/ext/toy/tight", None).await;
    assert_eq!(
        (first, second),
        (StatusCode::OK, StatusCode::TOO_MANY_REQUESTS)
    );

    // A plugin failure is a fixed 502, never the plugin's text.
    runtime.answer(
        methods::ROUTE,
        Err(droppedneedle_plugins::runtime::CallError::Failed {
            message: "Traceback: secret".to_owned(),
            fault: droppedneedle_plugins::protocol::Fault::Unknown,
        }),
    );
    let (status, body) = send(
        router_as(&rig, "user-1"),
        "GET",
        "/api/v3/plugins/ext/toy/status",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(!body.to_string().contains("secret"));
}

#[tokio::test]
async fn publish_is_stamped_validated_paced_and_loop_guarded() {
    let rig = rig("publish").await;
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
    let rig = rig("depth").await;
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
    turns_until(|| !runtime.calls_to(methods::EVENT).is_empty()).await;
    assert_eq!(runtime.calls_to(methods::EVENT)[0]["kind"], "plugin_notice");
    let echo = rig
        .host
        .publish_from_plugin("toy", "plugin_notice", &notice, "p", None)
        .unwrap();
    assert_eq!(echo.status, 409);
}

#[tokio::test]
async fn fanout_reaches_subscribers_once_per_causation() {
    let rig = rig("fanout").await;
    let manifest = "[plugin]\n\
         name = \"toy\"\n\
         api_version = 2\n\
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
    turns_until(|| {
        runtime.calls_to(methods::EVENT).len() == 1
            && runtime.calls_to(methods::SCROBBLE).len() == 1
    })
    .await;
    rig.host.dispatch_event(event);
    // The host spawns the scrobble before it would spawn an event delivery,
    // so a repeated event would have run by the time the scrobble shows.
    turns_until(|| runtime.calls_to(methods::SCROBBLE).len() == 2).await;
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
    let rig = rig("ticks").await;
    let store = rig.ticks.clone();
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

    let (supervisor, db, _dir) = crate::common::scratch_supervisor("plugin-ticks").await;
    let loops = PluginTickLoops::new(supervisor.clone(), store, Duration::from_secs(5));
    loops.sync_host(&rig.host).await;
    assert!(supervisor.is_running("plugin-tick:toy"));
    supervisor.shutdown(Duration::from_secs(5)).await;
    db.shutdown().await;
}

const WRITER_MANIFEST: &str = "[plugin]\n\
     name = \"toy\"\n\
     version = \"1.0.0\"\n\
     api_version = 2\n\
     entrypoint = \"plugin:Toy\"\n\
     capabilities = [\"library_writer\"]\n";

/// Make `<rig>/music` the one library folder.
async fn music_root(rig: &Rig) -> std::path::PathBuf {
    use droppedneedle_kernel::runtime_config::secret_sections::{LibraryRoot, TypedLibrary};

    let music = rig.dir.join("music");
    std::fs::create_dir_all(&music).unwrap();
    let settings = Arc::clone(&rig.platform.settings);
    let path = music.to_string_lossy().into_owned();
    let saved = tokio::task::spawn_blocking(move || {
        settings.update_secret_blocking::<TypedLibrary, String, _>(move |stored| {
            let mut library = stored.into_inner();
            library.library_roots = vec![LibraryRoot {
                id: "music".to_owned(),
                path,
                ..LibraryRoot::default()
            }];
            Ok(library)
        })
    })
    .await
    .unwrap();
    assert!(saved.is_ok());
    music
}

/// A plugin's library write goes through the publisher: the file lands
/// under the first library folder as a change recorded under the plugin's
/// name, the answer names that change, a taken name is refused with the
/// file left as it was, and no ledger folder is made.
#[tokio::test]
async fn a_plugin_write_is_journaled_and_keeps_no_ledger() {
    use base64::Engine as _;

    let rig = rig("library-write").await;
    let music = music_root(&rig).await;
    let runtime = enable_toy(&rig, WRITER_MANIFEST);
    let write = |data: &[u8]| {
        runtime.host_request(
            methods::HOST_LIBRARY_WRITE,
            json!({
                "rel_path": "notes/one.txt",
                "data_base64": base64::engine::general_purpose::STANDARD.encode(data),
            }),
        )
    };

    let answer = write(b"first").await.unwrap();
    let placed = music.join("notes/one.txt");
    assert_eq!(answer["path"], placed.to_string_lossy().as_ref());
    assert_eq!(std::fs::read(&placed).unwrap(), b"first");
    let bundle_id = answer["bundle_id"].as_str().unwrap().to_owned();
    let conn = droppedneedle_kernel::db::open_connection(&rig.dir.join("app.db")).unwrap();
    let (recorded, state): (String, String) = conn
        .query_row(
            "SELECT o.bundle_json, j.state FROM library_publish_operations o \
             JOIN library_publish_journal j ON j.bundle_id = o.bundle_id \
             WHERE o.bundle_id = ?1",
            [&bundle_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert!(
        recorded.contains(r#""origin":{"plugin":"toy"}"#),
        "{recorded}"
    );
    assert_eq!(state, "cleaned");

    let refused = write(b"second").await.unwrap_err();
    assert_eq!(refused.code, -32001);
    assert!(refused.message.contains("already exists"), "{refused:?}");
    assert_eq!(std::fs::read(&placed).unwrap(), b"first");
    assert!(
        !rig.dir.join("plugins/.data/.library-writes").exists(),
        "no ledger folder"
    );
}

/// Record that the toy plugin wrote `bytes` just now, in a change that
/// landed.
fn seed_written(rig: &Rig, bundle_id: &str, bytes: usize) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let conn = droppedneedle_kernel::db::open_connection(&rig.dir.join("app.db")).unwrap();
    conn.execute_batch(&format!(
        "INSERT INTO library_publish_journal (id, bundle_id, kind, dest_root, dest_rel, \
             staged, staged_sha256, state) \
         VALUES ('{bundle_id}-add-000', '{bundle_id}', 'add', 'music', 'seed/{bundle_id}', \
             '.dn-seed', 'abc', 'cleaned'); \
         INSERT INTO plugin_library_writes (bundle_id, plugin, bytes, written_at) \
         VALUES ('{bundle_id}', 'toy', {bytes}, {now});"
    ))
    .unwrap();
}

/// A write the plugin stops waiting for before its file is placed (the
/// host's cut on a slow request) is unwound: the file never appears, and
/// none of the plugin's daily allowance is spent on it.
#[tokio::test]
async fn a_write_the_plugin_gave_up_on_never_lands_and_spends_nothing() {
    use droppedneedle_plugins::capabilities::library_files::QUOTA_BYTES_PER_DAY;

    let rig = rig("write-cut").await;
    let music = music_root(&rig).await;
    // A host over a whole library, so the test can hold its publisher.
    let (library, _) = crate::common::library::library_at(&rig.platform);
    let host = PluginHost::new(
        rig.dir.join("plugins"),
        Arc::clone(&rig.config),
        Arc::new(FakeLauncher::new()),
        rig.ticks.clone(),
        library.core.clone(),
    );
    write_plugin(&rig.dir, "toy", WRITER_MANIFEST);
    host.load_all();
    host.update_settings("toy", true, HashMap::new()).unwrap();
    let (reached, at_place) = std::sync::mpsc::channel::<()>();
    let (release, released) = std::sync::mpsc::channel::<()>();
    library.publishing.set_publish_hooks(
        Some(Box::new(move |_: &Path| {
            let _ = reached.send(());
            let _ = released.recv();
        })),
        None,
    );
    let writer = Arc::clone(&host);
    let write = tokio::spawn(async move {
        writer
            .write_library_file("toy", "notes/late.txt", b"late".to_vec())
            .await
    });
    tokio::task::spawn_blocking(move || at_place.recv_timeout(Duration::from_secs(10)))
        .await
        .unwrap()
        .expect("the write reaches placement");
    write.abort();
    assert!(write.await.unwrap_err().is_cancelled());
    release.send(()).unwrap();

    let conn = droppedneedle_kernel::db::open_connection(&rig.dir.join("app.db")).unwrap();
    let state = || -> String {
        conn.query_row(
            "SELECT state FROM library_publish_journal WHERE dest_rel = 'notes/late.txt'",
            [],
            |row| row.get(0),
        )
        .unwrap_or_default()
    };
    for _ in 0..400 {
        if state() == "compensated" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(state(), "compensated");
    assert!(!music.join("notes/late.txt").exists());

    // Only the unwound write's bytes stand between the plugin and its
    // allowance: they are back, so the same write now lands.
    library.publishing.set_publish_hooks(None, None);
    seed_written(&rig, "seeded", QUOTA_BYTES_PER_DAY - 4);
    let (path, _) = host
        .write_library_file("toy", "notes/late.txt", b"late".to_vec())
        .await
        .unwrap();
    assert_eq!(std::fs::read(path).unwrap(), b"late");
}

/// What a plugin wrote before a restart still counts against its daily
/// allowance after it.
#[tokio::test]
async fn the_daily_allowance_survives_a_restart() {
    use droppedneedle_plugins::capabilities::library_files::QUOTA_BYTES_PER_DAY;

    let rig = rig("write-restart").await;
    let music = music_root(&rig).await;
    enable_toy(&rig, WRITER_MANIFEST);
    rig.host
        .write_library_file("toy", "notes/before.txt", b"12345".to_vec())
        .await
        .unwrap();
    seed_written(&rig, "seeded", QUOTA_BYTES_PER_DAY - 5);

    let library = crate::common::library::library_core(
        &rig.platform,
        JobQueue::new(rig.platform.db.clone(), Arc::new(SystemClock)),
    );
    let host = PluginHost::new(
        rig.dir.join("plugins"),
        Arc::clone(&rig.config),
        Arc::new(FakeLauncher::new()),
        rig.ticks.clone(),
        library,
    );
    host.load_all();
    host.update_settings("toy", true, HashMap::new()).unwrap();
    let refused = host
        .write_library_file("toy", "notes/after.txt", b"1".to_vec())
        .await
        .unwrap_err();
    assert!(refused.contains("allowance"), "{refused}");
    assert!(!music.join("notes/after.txt").exists());
}

const BIG_MANIFEST: &str = "[plugin]\n\
     name = \"big\"\n\
     api_version = 2\n\
     entrypoint = \"plugin:Big\"\n\
     capabilities = [\"library_writer\", \"purchase_links\"]\n";

const BIG_CODE: &str = r#"
import base64
import droppedneedle_plugin as sdk

class Big:
    def __init__(self, context):
        self.ctx = context

    async def purchase_links(self, artist, album, mbid):
        size = sdk.MAX_LIBRARY_FILE_BYTES
        path = await self.ctx.write_library_file("big/max.bin", b"x" * size)
        try:
            await self.ctx.write_library_file("big/over.bin", b"x" * (size + 1))
            from_sdk = "sent"
        except ValueError:
            from_sdk = "refused"
        try:
            text = base64.b64encode(b"x" * (size + 1)).decode("ascii")
            await self.ctx._server.request(
                "host.library.write", {"rel_path": "big/over.bin", "data_base64": text})
            from_host = "written"
        except RuntimeError as error:
            from_host = str(error)
        return [{"label": f"{size}|{from_sdk}|{from_host}", "url": "https://x.test" + path}]
"#;

/// The largest file the cap allows travels in one protocol message from
/// the Python helper and lands; one byte more is refused by the helper
/// before sending, and by the host when sent anyway.
#[tokio::test]
async fn the_largest_library_file_fits_one_message() {
    use droppedneedle_plugins::capabilities::library_files::MAX_FILE_BYTES;
    use droppedneedle_plugins::process::ProcessLauncher;

    let rig = rig("write-max").await;
    let music = music_root(&rig).await;
    write_plugin(&rig.dir, "big", BIG_MANIFEST);
    std::fs::write(rig.dir.join("plugins/big/plugin.py"), BIG_CODE).unwrap();
    let sdk = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../sdk/python");
    let host = PluginHost::new(
        rig.dir.join("plugins"),
        Arc::clone(&rig.config),
        Arc::new(ProcessLauncher::new(sdk, rig.platform.supervisor.clone())),
        rig.ticks.clone(),
        rig.library.clone(),
    );
    host.load_all();
    host.update_settings("big", true, HashMap::new()).unwrap();
    // The host gives a plugin 15 seconds to answer its handshake.
    for _ in 0..800 {
        let running = host
            .get("big")
            .and_then(|plugin| plugin.runtime_status())
            .is_some_and(|status| {
                status.state == droppedneedle_plugins::runtime::RuntimeState::Running
            });
        if running {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    let links = host.gather_purchase_links("", "", "").await;
    host.stop_all().await;
    let link = links.first().expect("the plugin answers");
    let placed = music.join("big/max.bin");
    assert_eq!(link.url, format!("https://x.test{}", placed.display()));
    assert_eq!(
        std::fs::metadata(&placed).unwrap().len(),
        MAX_FILE_BYTES as u64
    );
    let parts: Vec<&str> = link.label.splitn(3, '|').collect();
    assert_eq!(parts[0], MAX_FILE_BYTES.to_string());
    assert_eq!(parts[1], "refused");
    assert!(parts[2].contains("at most"), "{}", parts[2]);
    assert!(!music.join("big/over.bin").exists());
}
