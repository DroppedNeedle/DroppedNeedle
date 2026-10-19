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

use axum::body::Body;
use axum::http::{Request, StatusCode};
use droppedneedle_auth::Principal;
use droppedneedle_auth::session::store::SessionKind;
use droppedneedle_auth::users::roles::Role;
use droppedneedle_kernel::jobs::plugin_ticks::{
    MemoryTickStore, TickHost as JobsTickHost, TickPlugin as JobsTickPlugin,
};
use droppedneedle_kernel::runtime_config::ConfigStore;
use droppedneedle_kernel::runtime_config::crypto::Crypto;
use droppedneedle_plugins::fakes::{
    FakeConnection, FakeFetcher, FakeLauncher, FakeUnpacker, scrobble_plugin_event,
};
use droppedneedle_plugins::handlers::{ExtRateLimiter, PluginsDeps, plugin_routes};
use droppedneedle_plugins::host::PluginHost;
use droppedneedle_plugins::install::{Fetched, InstallError, parse_source, resolve};
use droppedneedle_plugins::models::PluginRefKind;
use droppedneedle_plugins::protocol::methods;
use droppedneedle_plugins::runtime::{PublishPayload, ScrobbleEvent};
use droppedneedle_plugins::ticks::{
    HostTickAdapter, NoopTickSync, PluginTickLoops, TickLoopSync, TickStoreKind,
};
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
}

fn rig(tag: &str) -> Rig {
    let dir = ScratchDir::new(&format!("plugins-{tag}"));
    let crypto = Crypto::from_key_bytes(&[7u8; 32]).unwrap();
    let config = Arc::new(ConfigStore::scratch(&dir, crypto).unwrap());
    let launcher = Arc::new(FakeLauncher::new());
    let host = PluginHost::new(dir.join("plugins"), Arc::clone(&config), launcher.clone());
    Rig {
        dir,
        host,
        config,
        launcher,
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

fn deps_for(rig: &Rig, fetcher: Arc<FakeFetcher>, unpacker: Arc<FakeUnpacker>) -> PluginsDeps {
    PluginsDeps {
        host: Arc::clone(&rig.host),
        config: Arc::clone(&rig.config),
        github: droppedneedle::bootstrap::Outbound::live().plugin_hosts(),
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
    turns_until(|| runtime.was_stopped()).await;
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
    fetcher.insert(
        DEFAULT_BRANCH,
        format!(r#"{{"sha": "{BRANCH_SHA}"}}"#).as_bytes(),
    );
    fetcher.insert(
        &format!("https://codeload.github.com/acme/toy/zip/{BRANCH_SHA}"),
        b"zip",
    );
}

fn toy_archive() -> Arc<FakeUnpacker> {
    Arc::new(FakeUnpacker::new(vec![
        FakeUnpacker::file("toy-0123456/plugin.toml", TOY_MANIFEST.as_bytes()),
        FakeUnpacker::file("toy-0123456/plugin.py", b"# code"),
    ]))
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
        let rig = rig(&format!("refused-{index}"));
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

        let admin = admin_app(&scope, deps_for(&rig, Arc::clone(&fetcher), toy_archive()));
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
    let rig = rig("no-releases");
    let fetcher = Arc::new(FakeFetcher::new());
    fetcher.answer(LATEST_RELEASE, Fetched::NotFound);
    default_branch(&fetcher);
    let scope = TestApp::start("plugins-no-releases").await;
    let admin = admin_app(&scope, deps_for(&rig, fetcher, toy_archive()));
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

#[tokio::test]
async fn a_ref_github_does_not_know_is_the_admins_mistake_not_an_outage() {
    let rig = rig("unknown-ref");
    let fetcher = Arc::new(FakeFetcher::new());
    fetcher.answer(
        "https://api.github.com/repos/acme/toy/commits/mian",
        Fetched::Failed(422),
    );
    let scope = TestApp::start("plugins-unknown-ref").await;
    let admin = admin_app(&scope, deps_for(&rig, fetcher, toy_archive()));
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
        plugin_routes()
            .with_state(deps_for(&rig, fetcher, unpacker))
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
    let rig = rig("auth");
    let bare = plugin_routes()
        .with_state(deps_for(
            &rig,
            Arc::new(FakeFetcher::new()),
            Arc::new(FakeUnpacker::new(Vec::new())),
        ))
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
    let rig = rig("panel");
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
    let rig = rig("ext");
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
        Err(droppedneedle_plugins::runtime::CallError::Failed(
            "Traceback: secret".to_owned(),
        )),
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

    let (supervisor, db, _dir) = crate::common::scratch_supervisor("plugin-ticks").await;
    let loops = PluginTickLoops::new(supervisor.clone(), store, Duration::from_secs(5));
    loops.sync_host(&rig.host).await;
    assert!(supervisor.is_running("plugin-tick:toy"));
    supervisor.shutdown(Duration::from_secs(5)).await;
    db.shutdown().await;
}
