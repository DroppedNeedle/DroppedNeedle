//! Upgrade mode, the page a server started on v2 data serves until the
//! upgrade is done: its own sign-in, its routes, and how it reads the
//! upgrade tool. The tool is a scripted child here; the journeys with the
//! real tool are in the tool's tests (`upgrade_mode`).

use std::{
    net::{Ipv4Addr, SocketAddr},
    path::Path,
    sync::atomic::{AtomicU32, Ordering},
    time::{Duration, Instant},
};

use axum::{
    Router,
    body::Body,
    extract::ConnectInfo,
    http::{HeaderMap, Request, StatusCode},
};
use droppedneedle::{
    bootstrap::{self, Claimed},
    docs::ApiDoc,
    upgrade_mode::{ToolCommand, UpgradeMode},
    web::WebUi,
};
use droppedneedle_kernel::{
    config::AppConfig,
    db::datalock::{DataLock, DataLockError},
};
use serde_json::{Value, json};
use tower::ServiceExt as _;
use tracing::instrument::WithSubscriber as _;
use utoipa::OpenApi as _;

use crate::common::{PATIENCE, ScratchDir, TestApp, logs};

/// A v2 data directory: Alice is an admin with a password, Bob a trusted
/// user with one.
fn v2_dir(tag: &str) -> (ScratchDir, AppConfig) {
    let dir = ScratchDir::new(&format!("upgrade-mode-{tag}"));
    droppedneedle_tool::tooling::fixture::build_v2_fixture(&dir).expect("fixture builds");
    let config = AppConfig::with_root(0, &dir);
    (dir, config)
}

/// The tool replaced by `script`, a shell script that answers the way
/// `droppedneedle-tool upgrade --progress json` does.
fn scripted(dir: &Path, script: &str) -> ToolCommand {
    let path = dir.join("tool.sh");
    std::fs::write(&path, script).expect("script writes");
    ToolCommand::new("/bin/sh", vec![path.into_os_string()])
}

async fn open(config: &AppConfig, interrupted: bool, tool: ToolCommand) -> (UpgradeMode, Router) {
    let mode = UpgradeMode::open(config, interrupted, tool)
        .await
        .expect("upgrade mode opens");
    let router = mode.router(None);
    (mode, router)
}

/// One request to `router` from the page itself, as the browser sends it,
/// each from an address of its own so no two share a rate budget.
async fn call(
    router: &Router,
    method: &str,
    uri: &str,
    cookie: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, HeaderMap, Value) {
    static NEXT: AtomicU32 = AtomicU32::new(1);
    let peer = SocketAddr::from((
        Ipv4Addr::from(0x0a00_0000 + NEXT.fetch_add(1, Ordering::Relaxed)),
        40_000,
    ));
    call_from(router, peer, method, uri, cookie, body).await
}

/// [`call`] from `peer`.
async fn call_from(
    router: &Router,
    peer: SocketAddr,
    method: &str,
    uri: &str,
    cookie: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, HeaderMap, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "test.local")
        .header("sec-fetch-site", "same-origin")
        .extension(ConnectInfo(peer));
    if let Some(cookie) = cookie {
        request = request.header("cookie", cookie);
    }
    let body = match body {
        Some(body) => {
            request = request.header("content-type", "application/json");
            Body::from(body.to_string())
        }
        None => Body::empty(),
    };
    let response = router
        .clone()
        .oneshot(request.body(body).expect("request builds"))
        .await
        .expect("upgrade mode answers");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body reads");
    let value = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
    (status, headers, value)
}

/// The `name=value` of the upgrade cookie a sign-in set.
fn upgrade_cookie(headers: &HeaderMap) -> String {
    let line = headers
        .get_all("set-cookie")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find(|line| line.starts_with("droppedneedle_upgrade="))
        .expect("the upgrade cookie is set");
    line.split(';').next().expect("a name and value").to_owned()
}

async fn sign_in(router: &Router, body: Value) -> (StatusCode, HeaderMap, Value) {
    call(router, "POST", "/api/v3/upgrade/sign-in", None, Some(body)).await
}

/// The latest code the log names.
fn logged_code(text: &[u8]) -> String {
    let text = String::from_utf8_lossy(text);
    let code = text
        .lines()
        .rev()
        .find_map(|line| line.split("code=").nth(1))
        .expect("a code is logged");
    code.split_whitespace().next().expect("the code").to_owned()
}

/// Read the progress until the latest run has ended.
async fn ended_run(router: &Router, cookie: &str) -> Value {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let (status, _, body) = call(
            router,
            "GET",
            "/api/v3/upgrade/progress",
            Some(cookie),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        if body["run"]["running"] == json!(false) {
            return body;
        }
        assert!(Instant::now() < deadline, "the run never ended: {body}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The state read and sign-in are the only routes without the session:
/// every other upgrade operation answers 401 with no cookie, a v3 session
/// cookie or a made-up upgrade cookie. The app's API answers 503
/// `UPGRADE_REQUIRED`, Subsonic its failed envelope, `/health` the upgrade
/// status, and an undocumented method 405.
///
/// Only an admin's v2 password signs in, with one uniform 401 for a wrong
/// password, an unknown name and a non-admin. The logged code signs in
/// once, and a new one is logged when it is used. The cookie is the
/// upgrade page's own: HttpOnly, SameSite=Strict, scoped to the upgrade
/// routes, 12 hours, and not Secure over plain HTTP. The app itself
/// refuses an upgrade cookie.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn upgrade_mode_signs_in_only_admins_and_reaches_only_its_routes() {
    let (dir, config) = v2_dir("sign-in");
    let (dispatch, logged) = logs::warnings();
    // Only the sign-in runs under the log capture: the code is read back
    // from the latest `code=` line, and other answers log lines of their own.
    let (mode, router, cookie) = async {
        let (mode, router) = open(&config, false, scripted(&dir, "exit 1\n")).await;
        mode.sign_in().log_code();

        for (username, password) in [
            ("alice", "not-her-password"),
            ("nobody", "alice-v2-pass"),
            ("bob", "bob-v2-pass"),
        ] {
            let (status, headers, body) = sign_in(
                &router,
                json!({"method": "password", "username": username, "password": password}),
            )
            .await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{username}: {body}");
            assert_eq!(body["error"]["message"], "That sign-in did not work");
            assert!(headers.get("set-cookie").is_none());
        }

        let (status, headers, body) = sign_in(
            &router,
            json!({"method": "password", "username": " Alice ", "password": "alice-v2-pass"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, json!({"layout": "needs_upgrade", "signed_in": true}));
        let line = headers
            .get_all("set-cookie")
            .iter()
            .filter_map(|value| value.to_str().ok())
            .find(|line| line.starts_with("droppedneedle_upgrade="))
            .expect("the upgrade cookie is set")
            .to_owned();
        for part in [
            "HttpOnly",
            "SameSite=Strict",
            "Path=/api/v3/upgrade",
            "Max-Age=43200",
        ] {
            assert!(line.contains(part), "{line}");
        }
        assert!(!line.contains("Secure"), "{line}");
        let cookie = upgrade_cookie(&headers);
        let (_, _, state) =
            call(&router, "GET", "/api/v3/upgrade/state", Some(&cookie), None).await;
        assert_eq!(state["signed_in"], true);

        let code = logged_code(&logged.lock().unwrap());
        // Change the last character; a code that already ends in X gets Y.
        let last = if code.ends_with('X') { 'Y' } else { 'X' };
        let wrong = format!("{}{last}", &code[..code.len() - 1]);
        let (status, _, _) = sign_in(&router, json!({"method": "code", "code": wrong})).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, _, body) = sign_in(
            &router,
            json!({"method": "code", "code": code.to_lowercase()}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (status, _, _) = sign_in(&router, json!({"method": "code", "code": code})).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "a used code is refused");
        let next = logged_code(&logged.lock().unwrap());
        assert_ne!(next, code, "a new code is logged once one is used");
        let (status, _, _) = sign_in(&router, json!({"method": "code", "code": next})).await;
        assert_eq!(status, StatusCode::OK);

        // Attempts from one address are limited as the app's login is.
        let guesser = SocketAddr::from(([192, 0, 2, 7], 50_000));
        let mut statuses = Vec::new();
        for _ in 0..6 {
            let (status, _, _) = call_from(
                &router,
                guesser,
                "POST",
                "/api/v3/upgrade/sign-in",
                None,
                Some(json!({"method": "code", "code": "WRONG"})),
            )
            .await;
            statuses.push(status);
        }
        assert_eq!(statuses[..5], [StatusCode::UNAUTHORIZED; 5]);
        assert_eq!(statuses[5], StatusCode::TOO_MANY_REQUESTS);
        (mode, router, cookie)
    }
    .with_subscriber(dispatch)
    .await;

    let doc = serde_json::to_value(ApiDoc::openapi()).expect("document serializes");
    let mut gated = 0;
    for (path, item) in doc["paths"].as_object().expect("paths") {
        if !path.starts_with("/api/v3/upgrade/") {
            continue;
        }
        for (method, operation) in item.as_object().expect("path item") {
            let method = method.to_uppercase();
            let body = (method == "POST").then(|| json!({}));
            if operation["security"] == json!([{}]) {
                assert!(
                    path.ends_with("/state") || path.ends_with("/sign-in"),
                    "{method} {path} is public"
                );
                continue;
            }
            assert_eq!(
                operation["security"],
                json!([{"upgrade_session": []}]),
                "{path}"
            );
            gated += 1;
            for cookie in [
                None,
                Some("droppedneedle_sid=a-v3-session"),
                Some("droppedneedle_upgrade=made-up"),
            ] {
                let (status, _, answer) = call(&router, &method, path, cookie, body.clone()).await;
                assert_eq!(
                    status,
                    StatusCode::UNAUTHORIZED,
                    "{method} {path} {cookie:?}: {answer}"
                );
            }
            let wrong = if method == "GET" { "DELETE" } else { "GET" };
            let (status, _, _) = call(&router, wrong, path, None, None).await;
            assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "{wrong} {path}");
        }
    }
    assert_eq!(gated, 6, "the six gated operations are documented");

    let (status, headers, body) = call(&router, "GET", "/api/v3/library/albums", None, None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"]["code"], "UPGRADE_REQUIRED", "{body}");
    assert!(headers.get("x-request-id").is_some());
    let (status, _, body) = call(
        &router,
        "GET",
        "/subsonic/rest/ping.view?f=json",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["subsonic-response"]["status"], "failed", "{body}");
    let (status, _, body) = call(&router, "GET", "/jellyfin/System/Info", None, None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    let (status, _, body) = call(&router, "GET", "/health", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "upgrade");
    drop(router);
    drop(mode);

    let app = TestApp::start("upgrade-cookie-in-the-app").await;
    let (status, _, _) = app
        .raw(
            "GET",
            "/api/v3/auth/sessions",
            &[("cookie", cookie.as_str())],
            None,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the app refuses an upgrade cookie"
    );
    app.stop().await;
}

/// A request to `router` with exactly `headers`, as an outside page or a
/// browser would send it.
async fn raw(
    router: &Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: Option<Value>,
) -> (StatusCode, HeaderMap) {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "test.local");
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let body = match body {
        Some(body) => {
            request = request.header("content-type", "application/json");
            Body::from(body.to_string())
        }
        None => Body::empty(),
    };
    let response = router
        .clone()
        .oneshot(request.body(body).expect("request builds"))
        .await
        .expect("upgrade mode answers");
    (response.status(), response.headers().clone())
}

/// Upgrade mode keeps the app's response rules with no settings to read:
/// the page under an enforced content-security policy, every other answer
/// under the policy where nothing runs, `nosniff` everywhere, `no-store`
/// on its JSON, and no HSTS. An unsafe request from another page is
/// refused before it signs in.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn upgrade_mode_keeps_its_security_headers_and_origin_check() {
    let (dir, config) = v2_dir("headers");
    let template = dir.join("build");
    std::fs::create_dir_all(&template).unwrap();
    std::fs::write(
        template.join("index.html"),
        "<!doctype html><html><head><script>start()</script></head></html>",
    )
    .unwrap();
    let web = WebUi::prepare(&template, &dir.join("served"), "")
        .unwrap()
        .expect("a web UI build");
    let mode = UpgradeMode::open(&config, false, scripted(&dir, "exit 1\n"))
        .await
        .expect("upgrade mode opens");
    let router = mode.router(Some(web));
    let forwarded = [("x-forwarded-proto", "https")];

    let (status, headers) = raw(&router, "GET", "/", &forwarded, None).await;
    assert_eq!(status, StatusCode::OK);
    let policy = headers["content-security-policy"].to_str().unwrap();
    assert!(policy.contains("script-src 'self'"), "{policy}");
    assert!(policy.contains("frame-ancestors 'none'"), "{policy}");
    assert_eq!(headers["x-content-type-options"], "nosniff");
    assert!(headers.get("strict-transport-security").is_none());

    let (status, headers) = raw(&router, "GET", "/api/v3/upgrade/state", &forwarded, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers["content-security-policy"],
        "default-src 'none'; frame-ancestors 'none'; sandbox"
    );
    assert_eq!(headers["x-content-type-options"], "nosniff");
    assert_eq!(headers["cache-control"], "no-store");
    assert!(headers.get("strict-transport-security").is_none());

    let attempt = json!({"method": "password", "username": "alice", "password": "alice-v2-pass"});
    for headers in [
        &[("origin", "https://evil.example")][..],
        &[
            ("sec-fetch-site", "cross-site"),
            ("origin", "https://evil.example"),
        ][..],
        &[][..],
    ] {
        let (status, answer) = raw(
            &router,
            "POST",
            "/api/v3/upgrade/sign-in",
            headers,
            Some(attempt.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{headers:?}");
        assert!(answer.get("set-cookie").is_none());
    }
    let (status, _) = raw(
        &router,
        "POST",
        "/api/v3/upgrade/sign-in",
        &[("origin", "http://test.local")],
        Some(attempt),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the page's own origin signs in");
}

/// Start refuses until a pre-flight found nothing to fix and every address
/// change it listed is acknowledged, and only then passes
/// `--callbacks-changed`. A second run while one is under way is refused.
/// A pre-flight that found something to fix, or no v2 data, is refused
/// whatever it acknowledges, and nothing but the check runs on it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_needs_a_clean_acknowledged_preflight() {
    start_passes_a_clean_acknowledged_preflight().await;
    start_refuses_a_preflight_with_blockers_or_without_v2_data().await;
}

/// The clean case of [`start_needs_a_clean_acknowledged_preflight`].
async fn start_passes_a_clean_acknowledged_preflight() {
    let (dir, config) = v2_dir("start");
    let args = dir.join("args.txt");
    let warning = json!({
        "feature": "spotify",
        "message": "Change the Spotify redirect address.",
        "old_address": {"kind": "path", "value": "/api/v1/me/connections/spotify/auth/callback"},
        "new_address": {"kind": "path", "value": "/api/v3/acquire/spotify/auth/callback"},
    });
    let check = json!({
        "v": 1, "type": "check", "layout": "v2", "counts": {"users": 2}, "orphans": 0,
        "blockers": [], "warnings": [warning],
    });
    let script = format!(
        "echo \"$*\" >> '{args}'\n\
         case \"$*\" in\n\
         *--check*) echo '{check}'; exit 3;;\n\
         *--callbacks-changed*) \
           echo '{{\"v\":1,\"type\":\"step\",\"step\":\"move_aside\",\"label\":\"Moving\",\"total\":2}}'; \
           echo '{{\"v\":1,\"type\":\"progress\",\"step\":\"move_aside\",\"done\":2}}'; \
           sleep 1; \
           echo '{{\"v\":1,\"type\":\"result\",\"outcome\":\"upgraded\",\"backup\":null,\"summary\":null}}'; \
           exit 0;;\n\
         esac\n\
         exit 2\n",
        args = args.display(),
    );
    let (_mode, router) = open(&config, false, scripted(&dir, &script)).await;
    let (_, headers, _) = sign_in(
        &router,
        json!({"method": "password", "username": "alice", "password": "alice-v2-pass"}),
    )
    .await;
    let cookie = upgrade_cookie(&headers);
    let start = |acknowledged: Value| {
        let (router, cookie) = (router.clone(), cookie.clone());
        async move {
            call(
                &router,
                "POST",
                "/api/v3/upgrade/start",
                Some(&cookie),
                Some(json!({"acknowledged": acknowledged})),
            )
            .await
        }
    };

    let (status, _, body) = start(json!([])).await;
    assert_eq!(status, StatusCode::CONFLICT, "no pre-flight yet: {body}");
    let (status, _, body) = call(
        &router,
        "POST",
        "/api/v3/upgrade/preflight",
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["check"]["warnings"], json!([warning]), "{body}");
    let (status, _, body) = start(json!([])).await;
    assert_eq!(status, StatusCode::CONFLICT, "unacknowledged: {body}");
    let mut other = warning.clone();
    other["new_address"]["value"] = json!("/somewhere/else");
    let (status, _, body) = start(json!([other])).await;
    assert_eq!(status, StatusCode::CONFLICT, "another address: {body}");
    assert_eq!(
        std::fs::read_to_string(&args).unwrap().lines().count(),
        1,
        "nothing ran but the check"
    );

    let (status, _, body) = start(json!([warning])).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["run"]["task"], "upgrade");
    let (status, _, _) = call(
        &router,
        "POST",
        "/api/v3/upgrade/fresh",
        Some(&cookie),
        Some(json!({"confirm": true})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "one run at a time");
    let ended = ended_run(&router, &cookie).await;
    assert_eq!(ended["run"]["result"]["outcome"], "upgraded", "{ended}");
    assert_eq!(ended["run"]["step"], "move_aside");
    assert_eq!(ended["run"]["done"], 2);
    let ran = std::fs::read_to_string(&args).unwrap();
    assert_eq!(
        ran.lines().last(),
        Some("upgrade --callbacks-changed --progress json"),
        "{ran}"
    );
}

/// The refusal cases of [`start_needs_a_clean_acknowledged_preflight`]:
/// `--callbacks-changed` is never passed on a blocked or v3 pre-flight.
async fn start_refuses_a_preflight_with_blockers_or_without_v2_data() {
    let (dir, config) = v2_dir("blocked");
    let (args, check, exit) = (
        dir.join("args.txt"),
        dir.join("check.json"),
        dir.join("check.exit"),
    );
    let script = format!(
        "case \"$*\" in\n\
         *--check*) cat '{check}'; exit \"$(cat '{exit}')\";;\n\
         esac\n\
         echo \"$*\" >> '{args}'\n\
         exit 2\n",
        check = check.display(),
        exit = exit.display(),
        args = args.display(),
    );
    let (_mode, router) = open(&config, false, scripted(&dir, &script)).await;
    let (_, headers, _) = sign_in(
        &router,
        json!({"method": "password", "username": "alice", "password": "alice-v2-pass"}),
    )
    .await;
    let cookie = upgrade_cookie(&headers);
    let warning = json!({
        "feature": "single_sign_on",
        "message": "Change the sign-in redirect address.",
        "old_address": {"kind": "path", "value": "/api/v1/auth/oidc/callback"},
        "new_address": {"kind": "path", "value": "/api/v3/auth/oidc/callback"},
    });
    let blocker =
        json!({"code": "V2_WAL_PRESENT", "message": "Unmerged writes.", "fix": "Stop v2."});
    for (layout, blockers, code) in [("v2", json!([blocker]), "1"), ("v3", json!([]), "0")] {
        let line = json!({
            "v": 1, "type": "check", "layout": layout, "counts": null, "orphans": 0,
            "blockers": blockers, "warnings": [warning],
        });
        std::fs::write(&check, format!("{line}\n")).unwrap();
        std::fs::write(&exit, code).unwrap();
        let (status, _, body) = call(
            &router,
            "POST",
            "/api/v3/upgrade/preflight",
            Some(&cookie),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["check"]["layout"], layout, "{body}");
        let (status, _, body) = call(
            &router,
            "POST",
            "/api/v3/upgrade/start",
            Some(&cookie),
            Some(json!({"acknowledged": [warning]})),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{layout}: {body}");
    }
    assert!(!args.exists(), "nothing but the check ran");
}

/// A run that ends without a line it can stand on (a result of another
/// line version, or a kill midway) is a failure with a fixed message, and
/// the folders a kill left halfway read as interrupted, where the
/// pre-flight is refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_run_killed_midway_leaves_an_interrupted_upgrade() {
    let (dir, config) = v2_dir("killed");
    let marker = config.cache_dir.join(".upgrade-in-progress");
    let once = dir.join("ran-once");
    let script = format!(
        "case \"$*\" in\n\
         *--fresh*) if [ ! -e '{once}' ]; then : > '{once}'; \
           echo '{{\"v\":2,\"type\":\"result\",\"outcome\":\"started_fresh\",\"backup\":null,\"summary\":null}}'; \
           exit 0; fi; \
           : > '{marker}'; \
           echo '{{\"v\":1,\"type\":\"step\",\"step\":\"move_aside\",\"label\":\"Moving\",\"total\":2}}'; \
           kill -9 $$;;\n\
         esac\n\
         exit 2\n",
        once = once.display(),
        marker = marker.display(),
    );
    let (mode, router) = open(&config, false, scripted(&dir, &script)).await;
    let (_, headers, _) = sign_in(
        &router,
        json!({"method": "password", "username": "alice", "password": "alice-v2-pass"}),
    )
    .await;
    let cookie = upgrade_cookie(&headers);
    let (status, _, body) = call(
        &router,
        "POST",
        "/api/v3/upgrade/fresh",
        Some(&cookie),
        Some(json!({"confirm": false})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "unconfirmed: {body}");
    let fresh = || async {
        let (status, _, body) = call(
            &router,
            "POST",
            "/api/v3/upgrade/fresh",
            Some(&cookie),
            Some(json!({"confirm": true})),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        ended_run(&router, &cookie).await
    };

    let ended = fresh().await;
    assert!(
        ended["run"]["result"].is_null(),
        "a version 2 line: {ended}"
    );
    assert_eq!(
        ended["run"]["failure"]["message"],
        "The upgrade tool stopped without saying how it ended. The server log says why."
    );
    assert_eq!(ended["layout"], "needs_upgrade", "{ended}");

    let ended = fresh().await;
    assert_eq!(ended["layout"], "interrupted", "{ended}");
    assert_eq!(ended["run"]["failure"]["code"], "INTERNAL", "{ended}");
    assert!(ended["run"]["result"].is_null());
    let (_, _, state) = call(&router, "GET", "/api/v3/upgrade/state", None, None).await;
    assert_eq!(state, json!({"layout": "interrupted", "signed_in": false}));
    let (status, _, body) = call(
        &router,
        "POST",
        "/api/v3/upgrade/preflight",
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    drop(mode);
}

/// Upgrade mode holds the server lock alone: a second server on the folder
/// is refused, and the upgrade tool can still take the folder's exclusive
/// lock.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn upgrade_mode_keeps_a_second_server_off_and_lets_the_tool_in() {
    let (_dir, config) = v2_dir("lock");
    let Claimed::Upgrade { lock, interrupted } = bootstrap::claim(&config).await.expect("claims")
    else {
        panic!("a v2 folder opens upgrade mode");
    };
    assert!(!interrupted);
    let second = bootstrap::claim(&config).await;
    assert!(
        matches!(
            second,
            Err(bootstrap::BootError::DataLock(DataLockError::Held { .. }))
        ),
        "a second server is refused"
    );
    let tool = DataLock::exclusive(&config.library_db_path).expect("the tool takes the folder");
    drop(tool);
    drop(lock);
}
