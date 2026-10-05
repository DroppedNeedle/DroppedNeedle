//! The web UI fallback and `BASE_PATH` mounting.

use crate::common;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use droppedneedle::{
    create_app_with_web,
    web::{BASE_PATH_PLACEHOLDER, WebUi},
};
use tower::ServiceExt as _;

/// A scratch directory removed when the test ends, pass or fail.
struct Scratch(std::path::PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A small adapter-static style build, stamped for `base_path`. Keep the
/// guard alive while the UI serves.
fn web_ui(name: &str, base_path: &str) -> (WebUi, Scratch) {
    let dir = std::env::temp_dir().join(format!("dn-web-it-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let guard = Scratch(dir.clone());
    let template = dir.join("template");
    std::fs::create_dir_all(template.join("_app/immutable")).unwrap();
    std::fs::write(
        template.join("index.html"),
        format!("<script src=\"{BASE_PATH_PLACEHOLDER}/_app/immutable/app.js\"></script>"),
    )
    .unwrap();
    std::fs::write(
        template.join("_app/immutable/app.js"),
        "console.log('a fairly long script body');",
    )
    .unwrap();
    std::fs::write(template.join("_app/immutable/app.js.br"), "br").unwrap();
    let web = WebUi::prepare(&template, &dir.join("static"), base_path)
        .unwrap()
        .unwrap();
    (web, guard)
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
        assert_eq!(body, "<script src=\"/_app/immutable/app.js\"></script>");
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
        assert_eq!(
            body,
            "<script src=\"/music/_app/immutable/app.js\"></script>"
        );
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
