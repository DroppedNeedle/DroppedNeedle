//! The support bundle holds how the server is set up and what is wrong,
//! never whose data it holds. Markers planted in a library folder's path, a
//! user name and email, a saved service address and secret, a log line's
//! field and message, a 5xx's cause and a request path never reach it, and
//! every string in it has its own plain shape: a token, or for a file a
//! path of tokens; nothing holds `://`. The ring is installed on
//! this thread (`set_default` is thread-local), so the test runs on a
//! current-thread runtime.

use axum::http::StatusCode;
use droppedneedle::handlers::test_hooks::LEAK_MARKERS;
use serde_json::{Value, json};
use tracing_subscriber::layer::SubscriberExt as _;

use crate::common::{ScratchDir, TestApp, closed_outbound};

const MARKERS: &[&str] = &[
    "marker-folder-5d1",
    "markeruser7",
    "marker-secret-31",
    "marker-field-55",
    "marker-message-66",
    "marker-path-77",
    "marker-host-88",
    "marker-mail-44",
];

/// Every string value in `value` with the name of the field holding it.
fn strings<'a>(field: &'a str, value: &'a Value, out: &mut Vec<(&'a str, &'a str)>) {
    match value {
        Value::String(text) => out.push((field, text)),
        Value::Array(items) => items.iter().for_each(|item| strings(field, item, out)),
        Value::Object(map) => map.iter().for_each(|(key, item)| strings(key, item, out)),
        _ => {}
    }
}

fn token(text: &str) -> bool {
    (1..=64).contains(&text.len())
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.:-+".contains(&byte))
}

/// A string's own shape: a file is a relative path of tokens, anything
/// else one token; nothing holds a scheme.
fn plain(field: &str, text: &str) -> bool {
    if text.contains("://") {
        return false;
    }
    match field {
        "file" => !text.starts_with('/') && text.len() <= 128 && text.split('/').all(token),
        _ => token(text),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn the_bundle_never_holds_a_planted_marker() {
    let app = TestApp::start_hooked_with("support-bundle", closed_outbound()).await;
    let _ring = tracing::subscriber::set_default(
        tracing_subscriber::registry().with(app.state().admin.recent.layer()),
    );
    let music = ScratchDir::new(MARKERS[0]);
    let folder = music.join(MARKERS[0]);
    std::fs::create_dir_all(&folder).unwrap();
    app.add_library_folder("marked", &folder).await;
    let _user = app.login_user(MARKERS[1]).await;
    let admin = app.login_admin().await;
    let headers = [("authorization", admin.as_str())];
    let (status, body) = app
        .call(
            "POST",
            "/api/v3/admin/users",
            &headers,
            Some(json!({
                "username": "mailed",
                "password": "mailed-password-1234",
                "email": format!("{}@example.invalid", MARKERS[7]),
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = app
        .call(
            "PUT",
            "/api/v3/settings/download-client/config",
            &headers,
            Some(json!({
                "enabled": true,
                "url": format!("http://{}.invalid:9", MARKERS[6]),
                "api_key": MARKERS[2],
                "downloads_subpath": "",
                "slskd_incomplete_mount": "",
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    tracing::warn!(detail = MARKERS[3], "{}", MARKERS[4]);
    let (status, fault) = app.call("GET", "/__test__/typed-error", &[], None).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    let error_id = fault["error"]["details"]["error_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let _ = app
        .call("GET", &format!("/api/v3/{}", MARKERS[5]), &headers, None)
        .await;

    let (status, _, bytes) = app
        .raw("GET", "/api/v3/admin/support-bundle", &headers, None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let text = String::from_utf8(bytes).unwrap();
    for marker in MARKERS.iter().chain(LEAK_MARKERS) {
        assert!(
            !text.contains(marker),
            "{marker} reached the bundle: {text}"
        );
    }
    let bundle: Value = serde_json::from_str(&text).unwrap();
    let mut found = Vec::new();
    strings("", &bundle, &mut found);
    for (field, value) in found {
        assert!(
            plain(field, value),
            "{field} holds a string of no allowed shape: {value:?}"
        );
    }

    assert_eq!(bundle["version"], env!("CARGO_PKG_VERSION"));
    let slskd = bundle["settings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|section| section["key"] == "download_client")
        .unwrap();
    assert_eq!(slskd["state"], "saved");
    let fault = bundle["problems"]
        .as_array()
        .unwrap()
        .iter()
        .find(|problem| {
            problem["refs"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r == error_id.as_str())
        })
        .unwrap_or_else(|| panic!("the fault's ref is not in the bundle: {text}"));
    assert_eq!(fault["file"], "src/middleware.rs");
    assert!(fault["line"].as_u64().is_some(), "{fault}");

    let user = app.login_user("plain").await.authorization;
    let (status, _) = app
        .call(
            "GET",
            "/api/v3/admin/support-bundle",
            &[("authorization", user.as_str())],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    app.stop().await;
}
