//! The web UI fallback and `BASE_PATH` mounting.

use crate::common::{ScratchDir, TestApp};

use axum::http::{HeaderMap, StatusCode};
use base64::Engine as _;
use droppedneedle::web::BASE_PATH_PLACEHOLDER;
use sha2::Digest as _;

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

/// The app serving a small adapter-static style build under `base_path`,
/// stamped by the production builder. Keep the guard alive while the UI
/// serves.
async fn web_app(name: &str, base_path: &str) -> (TestApp, ScratchDir) {
    let dir = ScratchDir::new(&format!("web-{name}"));
    let template = dir.join("template");
    std::fs::create_dir_all(template.join("_app/immutable")).unwrap();
    std::fs::write(template.join("index.html"), page(BASE_PATH_PLACEHOLDER)).unwrap();
    std::fs::write(
        template.join("_app/immutable/app.js"),
        "console.log('a fairly long script body');",
    )
    .unwrap();
    std::fs::write(template.join("_app/immutable/app.js.br"), "br").unwrap();
    let app = TestApp::start_configured(&format!("web-{name}"), |mut config| {
        config.static_dir = template;
        config.base_path = base_path.to_owned();
        config
    })
    .await;
    (app, dir)
}

async fn get(
    app: &TestApp,
    uri: &str,
    accept_encoding: Option<&str>,
) -> (StatusCode, axum::http::HeaderMap, String) {
    let headers: Vec<(&str, &str)> = accept_encoding
        .map(|value| ("accept-encoding", value))
        .into_iter()
        .collect();
    let (status, headers, body) = app.raw("GET", uri, &headers, None).await;
    (status, headers, String::from_utf8_lossy(&body).into_owned())
}

#[tokio::test]
async fn serves_the_spa_and_its_assets_without_shadowing_the_api() {
    let (app, _scratch) = web_app("root", "").await;

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
    let (app, _scratch) = web_app("base", "/music").await;

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

/// The page is never served as a plain file: a percent-encoded spelling of
/// `index.html` still gets the page's own policy and cache rule.
#[tokio::test]
async fn an_encoded_page_path_still_gets_the_page() {
    let (app, _scratch) = web_app("encoded", "").await;
    let (status, headers, body) = get(&app, "/index%2ehtml", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, page(""));
    assert_eq!(headers["cache-control"], "no-cache");
    let policy = text(&headers, "content-security-policy").unwrap();
    assert!(policy.contains("'sha256-"), "{policy}");
}

/// A browser preflight to an existing route reaches the debug CORS layer:
/// preflights are handled before routing, where the router's 405 fallback
/// cannot take them first.
#[tokio::test]
async fn debug_cors_answers_native_preflights() {
    let app = TestApp::start_configured("web-debug-cors", |mut config| {
        config.debug_cors = true;
        config
    })
    .await;
    let origin = "http://localhost:5173";
    let (status, headers, _) = app
        .raw(
            "OPTIONS",
            "/api/v3/auth/sessions",
            &[("origin", origin), ("access-control-request-method", "GET")],
            None,
        )
        .await;
    assert!(status.is_success(), "{status}");
    assert_eq!(text(&headers, "access-control-allow-origin"), Some(origin));
}

/// Per-user JSON is kept by neither a proxy nor the browser's disk cache,
/// and an answer whose handler sets its own `Cache-Control` keeps it.
#[tokio::test]
async fn api_json_is_no_store_and_a_handlers_own_cache_rule_is_kept() {
    let app = TestApp::start("web-no-store").await;
    let user = app.login_user("listener").await;
    let auth = [("authorization", user.authorization.as_str())];

    let (status, headers, _) = app.raw("GET", "/api/v3/library/artists", &auth, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(text(&headers, "content-type"), Some("application/json"));
    assert_eq!(text(&headers, "cache-control"), Some("no-store"));

    // An album with no art: the cover route answers its own `no-store`.
    let (status, headers, _) = app
        .raw(
            "GET",
            "/api/v3/library/albums/no-such-album/artwork",
            &auth,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(text(&headers, "x-cover-state"), Some("missing"));
    assert_eq!(text(&headers, "cache-control"), Some("no-store"));
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
/// browser gets. As installed the browser enforces it, and no directive in
/// it allows `unsafe-inline` or `unsafe-eval`. Every other native answer
/// runs nothing. Compat answers are
/// compared with real servers, so they gain no header at all.
#[tokio::test]
async fn the_page_runs_only_its_own_scripts_and_nothing_else_runs_anything() {
    const BARE: &str = "default-src 'none'; frame-ancestors 'none'; sandbox";
    const REPORTING: &str = "content-security-policy-report-only";
    const ENFORCED: &str = "content-security-policy";
    let (app, _scratch) = web_app("policy", "/music").await;

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
        let policy = text(headers, ENFORCED).unwrap_or_else(|| panic!("{what}: no policy"));
        assert_eq!(text(headers, REPORTING), None, "{what}");
        let scripts = directive(policy, "script-src");
        assert!(scripts.contains(&served.as_str()), "{what}: {policy}");
        assert!(scripts.contains(&"'self'"), "{what}: {policy}");
        assert!(!scripts.contains(&"'strict-dynamic'"), "{what}: {policy}");
        for loose in ["'unsafe-inline'", "'unsafe-eval'"] {
            assert!(!policy.contains(loose), "{what}: {policy}");
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
    let (status, headers, _) = app
        .raw("GET", "/music/", &[("if-none-match", etag.as_str())], None)
        .await;
    assert_eq!(status, StatusCode::NOT_MODIFIED);
    is_the_page(&headers, "304");

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
