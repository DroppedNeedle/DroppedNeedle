//! The web UI fallback and `BASE_PATH` mounting.

use crate::common;

use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Request, StatusCode},
};
use base64::Engine as _;
use droppedneedle::{
    create_app_with_web,
    web::{BASE_PATH_PLACEHOLDER, WebUi},
};
use sha2::Digest as _;
use tower::ServiceExt as _;

/// The boot script of the fixture page, as SvelteKit writes one: inline,
/// with the base path inside it.
fn boot_script(base_path: &str) -> String {
    format!(
        "\n\t__sveltekit = {{ base: \"{base_path}\" }};\n\t\
         import(\"{base_path}/_app/immutable/app.js\");\n"
    )
}

/// The fixture page for `base_path`.
fn page(base_path: &str) -> String {
    format!(
        "<link href=\"{base_path}/_app/immutable/app.js\" rel=\"modulepreload\">\
         <div><script>{}</script></div>",
        boot_script(base_path)
    )
}

/// A small adapter-static style build, stamped for `base_path`. Keep the
/// guard alive while the UI serves.
fn web_ui(name: &str, base_path: &str) -> (WebUi, common::ScratchDir) {
    let dir = common::ScratchDir::new(&format!("web-{name}"));
    let template = dir.join("template");
    std::fs::create_dir_all(template.join("_app/immutable")).unwrap();
    std::fs::write(template.join("index.html"), page(BASE_PATH_PLACEHOLDER)).unwrap();
    std::fs::write(
        template.join("_app/immutable/app.js"),
        "console.log('a fairly long script body');",
    )
    .unwrap();
    std::fs::write(template.join("_app/immutable/app.js.br"), "br").unwrap();
    let web = WebUi::prepare(&template, &dir.join("static"), base_path)
        .unwrap()
        .unwrap();
    (web, dir)
}

async fn get(
    app: &Router,
    uri: &str,
    accept_encoding: Option<&str>,
) -> (StatusCode, axum::http::HeaderMap, String) {
    let mut request = Request::builder().uri(uri);
    if let Some(value) = accept_encoding {
        request = request.header("accept-encoding", value);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, headers, String::from_utf8_lossy(&body).into_owned())
}

#[tokio::test]
async fn serves_the_spa_and_its_assets_without_shadowing_the_api() {
    let (web, _scratch) = web_ui("root", "");
    let app = create_app_with_web(common::hooked_state(), Some(web));

    for uri in ["/", "/library/artists/some-artist"] {
        let (status, headers, body) = get(&app, uri, None).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        assert_eq!(headers["cache-control"], "no-cache");
        assert_eq!(body, page(""));
    }

    let (status, headers, body) = get(&app, "/_app/immutable/app.js", Some("gzip, br")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers["cache-control"],
        "public, max-age=31536000, immutable"
    );
    assert_eq!(headers["content-encoding"], "br");
    assert_eq!(headers["vary"], "Accept-Encoding");
    assert_eq!(body, "br");

    for uri in ["/api/v3/nope", "/_app/immutable/missing.js"] {
        let (status, headers, body) = get(&app, uri, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
        assert_eq!(headers["content-type"], "application/json", "{uri}");
        assert!(body.contains("\"error\""), "{uri}: {body}");
    }
}

#[tokio::test]
async fn base_path_moves_everything_and_the_gate_sees_routed_paths() {
    let mut state = common::hooked_state();
    state.config.base_path = "/music".to_owned();
    let (web, _scratch) = web_ui("base", "/music");
    let app = create_app_with_web(state, Some(web));

    for uri in ["/music", "/music/", "/music/library?tab=albums"] {
        let (status, _, body) = get(&app, uri, None).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        assert_eq!(body, page("/music"));
    }
    assert_eq!(get(&app, "/music/health", None).await.0, StatusCode::OK);
    assert_eq!(
        get(&app, "/music/api/v3/auth/sessions", None).await.0,
        StatusCode::UNAUTHORIZED
    );
    for outside in ["/health", "/", "/api/v3/auth/sessions", "/musicx/health"] {
        let (status, headers, _) = get(&app, outside, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{outside}");
        assert_eq!(headers["content-type"], "application/json", "{outside}");
    }
}

fn text<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).map(|value| value.to_str().unwrap())
}

/// The sources of one directive of a policy.
fn directive<'a>(policy: &'a str, name: &str) -> Vec<&'a str> {
    policy
        .split(';')
        .map(|directive| directive.split_whitespace().collect::<Vec<_>>())
        .find(|words| words.first() == Some(&name))
        .map(|words| words[1..].to_vec())
        .unwrap_or_else(|| panic!("no {name} in {policy}"))
}

/// The page is the only answer that may run script, and only its own: the
/// files it ships and the one inline script, by the hash of the text the
/// browser gets. Every other native answer runs nothing. Compat answers are
/// compared with real servers, so they gain no header at all.
#[tokio::test]
async fn the_page_runs_only_its_own_scripts_and_nothing_else_runs_anything() {
    const BARE: &str = "default-src 'none'; frame-ancestors 'none'; sandbox";
    const REPORTING: &str = "content-security-policy-report-only";
    const ENFORCED: &str = "content-security-policy";
    let mut state = common::hooked_state();
    state.config.base_path = "/music".to_owned();
    let (web, _scratch) = web_ui("policy", "/music");
    let app = create_app_with_web(state, Some(web));

    // Of the script as stamped: the template's text hashes differently.
    let hash = |script: String| {
        let digest = sha2::Sha256::digest(script.as_bytes());
        format!(
            "'sha256-{}'",
            base64::engine::general_purpose::STANDARD.encode(digest)
        )
    };
    let served = hash(boot_script("/music"));
    assert_ne!(served, hash(boot_script(BASE_PATH_PLACEHOLDER)));

    let is_the_page = |headers: &HeaderMap, what: &str| {
        let policy = text(headers, REPORTING).unwrap_or_else(|| panic!("{what}: no policy"));
        assert_eq!(text(headers, ENFORCED), None, "{what}");
        let scripts = directive(policy, "script-src");
        assert!(scripts.contains(&served.as_str()), "{what}: {policy}");
        assert!(scripts.contains(&"'self'"), "{what}: {policy}");
        for loose in ["'unsafe-inline'", "'unsafe-eval'", "'strict-dynamic'"] {
            assert!(!scripts.contains(&loose), "{what}: {policy}");
        }
        assert_eq!(directive(policy, "default-src"), ["'none'"], "{what}");
        assert_eq!(directive(policy, "frame-ancestors"), ["'none'"], "{what}");
        assert_eq!(text(headers, "x-content-type-options"), Some("nosniff"));
        assert_eq!(
            text(headers, "referrer-policy"),
            Some("strict-origin-when-cross-origin")
        );
    };

    let mut etag = String::new();
    for uri in ["/music/", "/music/library/artists/some-artist"] {
        let (status, headers, _) = get(&app, uri, Some("gzip, br")).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        is_the_page(&headers, uri);
        etag = text(&headers, "etag").unwrap().to_owned();
    }
    let revalidated = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/music/")
                .header("if-none-match", &etag)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(revalidated.status(), StatusCode::NOT_MODIFIED);
    is_the_page(revalidated.headers(), "304");

    // A static file, an API 404, a route, and the 404 outside the base.
    for uri in [
        "/music/_app/immutable/app.js",
        "/music/api/v3/nope",
        "/music/health",
        "/health",
    ] {
        let (_, headers, _) = get(&app, uri, None).await;
        assert_eq!(text(&headers, ENFORCED), Some(BARE), "{uri}");
        assert_eq!(text(&headers, REPORTING), None, "{uri}");
        assert_eq!(
            text(&headers, "x-content-type-options"),
            Some("nosniff"),
            "{uri}"
        );
        assert_eq!(text(&headers, "referrer-policy"), None, "{uri}");
    }

    for uri in ["/music/subsonic/rest/ping", "/music/Jellyfin/System/Nope"] {
        let (_, headers, _) = get(&app, uri, None).await;
        for added in [ENFORCED, REPORTING, "x-content-type-options"] {
            assert_eq!(text(&headers, added), None, "{uri}: {added}");
        }
    }
}
