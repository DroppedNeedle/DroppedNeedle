//! A person's theme and custom CSS on their account: a change is checked
//! whole and saves all or nothing, the site's switch stops new CSS but not
//! a clear or a switch-off, the browser's copy moves up once, the newest
//! ten saves are kept, and an admin's switch-off reaches only its user.

use axum::http::StatusCode;
use serde_json::{Value, json};

use crate::common::TestApp;

const MINE: &str = "/api/v3/me/appearance";
const IMPORT: &str = "/api/v3/me/appearance/import";
const SAVES: &str = "/api/v3/me/appearance/css-saves";
const SITE: &str = "/api/v3/settings/appearance";

async fn call(
    app: &TestApp,
    auth: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    app.state().auth.limits.reset();
    app.call(method, uri, &[("authorization", auth)], body)
        .await
}

fn css(text: &str) -> Value {
    json!({ "custom_css": { "text": text } })
}

/// A refused, oversized or turned-off change saves none of its parts,
/// while clearing the CSS, switching it off and the theme still save with
/// the site's switch off. The site CSS passes the same check.
#[tokio::test]
async fn a_refused_change_saves_nothing() {
    let app = TestApp::start("appearance-refused").await;
    let admin = app.login_admin().await;
    let ada = app.login_user("ada").await.authorization;

    let (status, body) = call(&app, &ada, "GET", MINE, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["stored"], json!(false));
    assert_eq!(body["theme"], Value::Null);
    assert_eq!(
        body["custom_css"],
        json!({"text": "", "enabled": true, "saved_at": null})
    );
    assert_eq!(body["site"], json!({"css": "", "allow_user_css": true}));

    let refused = json!({"theme": "dark", "custom_css": {"text": "a {}\nb { c: url(x); }"}});
    let (status, body) = call(&app, &ada, "PUT", MINE, Some(refused)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], json!("CUSTOM_CSS_REFUSED"));
    assert_eq!(
        body["error"]["details"]["problems"],
        json!([{"what": "function:url", "line": 2}])
    );
    let long = json!({"theme": "dark", "custom_css": {"text": "a".repeat(20_001)}});
    let (status, body) = call(&app, &ada, "PUT", MINE, Some(long)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], json!("CUSTOM_CSS_TOO_LONG"));
    let (status, body) = call(&app, &ada, "PUT", MINE, Some(json!({"theme": null}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "a null theme: {body}");
    let (_, body) = call(&app, &ada, "GET", MINE, None).await;
    assert_eq!(
        body["stored"],
        json!(false),
        "a refused change saved: {body}"
    );

    let (status, body) = call(&app, &ada, "PUT", MINE, Some(css(":root { --x: 1px; }"))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["custom_css"]["text"], json!(":root { --x: 1px; }"));
    assert!(body["custom_css"]["saved_at"].is_i64(), "{body}");

    let site = json!({"site_css": "a { b: src(x); }", "allow_user_css": false});
    let (status, body) = call(&app, &admin, "PUT", SITE, Some(site)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], json!("CUSTOM_CSS_REFUSED"));
    let site = json!({"site_css": "a".repeat(50_000), "allow_user_css": false});
    let (status, body) = call(&app, &admin, "PUT", SITE, Some(site)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the site's limit is its own: {body}"
    );
    let site = json!({"site_css": ":root { --site: 1px; }", "allow_user_css": false});
    let (status, body) = call(&app, &admin, "PUT", SITE, Some(site)).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    for (what, change) in [
        (
            "new text",
            json!({"theme": "light", "custom_css": {"text": "a {}"}}),
        ),
        (
            "switching it on",
            json!({"theme": "light", "custom_css": {"enabled": true}}),
        ),
    ] {
        let (status, body) = call(&app, &ada, "PUT", MINE, Some(change)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{what}: {body}");
        assert_eq!(body["error"]["code"], json!("CUSTOM_CSS_TURNED_OFF"));
    }
    let (_, body) = call(&app, &ada, "GET", MINE, None).await;
    assert_eq!(
        body["theme"],
        Value::Null,
        "a turned-off change saved its theme: {body}"
    );
    assert_eq!(
        body["site"],
        json!({"css": ":root { --site: 1px; }", "allow_user_css": false})
    );

    let off = json!({"theme": "light", "custom_css": {"enabled": false}});
    let (status, body) = call(&app, &ada, "PUT", MINE, Some(off)).await;
    assert_eq!(status, StatusCode::OK, "switching off: {body}");
    assert_eq!(body["custom_css"]["enabled"], json!(false));
    let (status, body) = call(&app, &ada, "PUT", MINE, Some(css(""))).await;
    assert_eq!(status, StatusCode::OK, "clearing: {body}");
    assert_eq!(body["theme"], json!("light"));
    assert_eq!(body["custom_css"]["text"], json!(""));
    app.stop().await;
}

/// The newest ten saves are kept, newest first. Saving the same text again,
/// clearing it and saving only a byte order mark add none, and a clear keeps
/// the history. A sheet of only U+0085 is not blank, so it is kept.
#[tokio::test]
async fn the_saves_keep_the_newest_ten() {
    let app = TestApp::start("appearance-saves").await;
    let ada = app.login_user("ada").await.authorization;
    for at in 1..=12 {
        let (status, body) = call(
            &app,
            &ada,
            "PUT",
            MINE,
            Some(css(&format!("a{{--n:{at}}}"))),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    call(&app, &ada, "PUT", MINE, Some(css("a{--n:12}"))).await;
    call(&app, &ada, "PUT", MINE, Some(json!({"theme": "dark"}))).await;
    call(&app, &ada, "PUT", MINE, Some(css(""))).await;
    let (_, body) = call(&app, &ada, "PUT", MINE, Some(css("\u{feff}"))).await;
    assert_eq!(
        body["custom_css"]["text"],
        json!(""),
        "a byte order mark alone is blank: {body}"
    );
    let (status, body) = call(&app, &ada, "GET", SAVES, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let kept: Vec<&str> = body
        .as_array()
        .unwrap()
        .iter()
        .map(|save| save["css"].as_str().unwrap())
        .collect();
    let wanted: Vec<String> = (3..=12).rev().map(|at| format!("a{{--n:{at}}}")).collect();
    assert_eq!(kept, wanted);
    let (_, body) = call(&app, &ada, "PUT", MINE, Some(css("\u{85}"))).await;
    assert_eq!(
        body["custom_css"]["text"],
        json!("\u{85}"),
        "a sheet of only U+0085 is kept as text: {body}"
    );
    app.stop().await;
}

/// The move from the browser writes once, while the account holds nothing,
/// and passes the same check.
#[tokio::test]
async fn the_browser_move_writes_once() {
    let app = TestApp::start("appearance-import").await;
    let ada = app.login_user("ada").await.authorization;
    let bob = app.login_user("bob").await.authorization;

    let (status, body) = call(
        &app,
        &ada,
        "POST",
        IMPORT,
        Some(css("a { b: image-set(x); }")),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let first = json!({"theme": "midnight", "custom_css": {"text": "a {}"}});
    let (status, body) = call(&app, &ada, "POST", IMPORT, Some(first)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        (body["imported"].clone(), body["stored"].clone()),
        (json!(true), json!(true))
    );
    assert_eq!(body["theme"], json!("midnight"));
    let again = json!({"theme": "light", "custom_css": {"text": "b {}"}});
    let (status, body) = call(&app, &ada, "POST", IMPORT, Some(again.clone())).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["imported"], json!(false));
    assert_eq!(body["theme"], json!("midnight"));
    assert_eq!(body["custom_css"]["text"], json!("a {}"));

    call(&app, &bob, "PUT", MINE, Some(json!({"theme": "dark"}))).await;
    let (_, body) = call(&app, &bob, "POST", IMPORT, Some(again)).await;
    assert_eq!(body["imported"], json!(false), "{body}");
    assert_eq!(body["theme"], json!("dark"));
    app.stop().await;
}

/// An admin's switch-off reaches only its user, keeps their text, and
/// makes no row for a user with none. The summary never holds the text.
#[tokio::test]
async fn an_admin_turns_off_one_users_css() {
    let app = TestApp::start("appearance-admin").await;
    let admin = app.login_admin().await;
    let ada = app.login_user("ada").await;
    let bob = app.login_user("bob").await;
    let cy = app.login_user("cyd").await;
    for who in [&ada, &bob] {
        call(
            &app,
            &who.authorization,
            "PUT",
            MINE,
            Some(css("a { color: red; }")),
        )
        .await;
    }
    let summary = |id: &str| format!("/api/v3/admin/users/{id}/custom-css");
    let turn_off = |id: &str| format!("/api/v3/admin/users/{id}/custom-css/turn-off");

    let (status, body) = call(&app, &admin, "GET", &summary(&bob.user_id), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["has_css"], json!(true));
    assert_eq!(body["length"], json!(17));
    let mut keys: Vec<&String> = body.as_object().unwrap().keys().collect();
    keys.sort();
    assert_eq!(keys, ["enabled", "has_css", "length", "saved_at"]);

    let (status, body) = call(&app, &admin, "POST", &turn_off(&bob.user_id), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["enabled"], json!(false));
    let (_, body) = call(&app, &bob.authorization, "GET", MINE, None).await;
    assert_eq!(body["custom_css"]["text"], json!("a { color: red; }"));
    assert_eq!(body["custom_css"]["enabled"], json!(false));
    let (_, body) = call(&app, &ada.authorization, "GET", MINE, None).await;
    assert_eq!(
        body["custom_css"]["enabled"],
        json!(true),
        "A was turned off: {body}"
    );

    let (status, body) = call(&app, &admin, "POST", &turn_off(&cy.user_id), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body,
        json!({"has_css": false, "enabled": true, "length": 0, "saved_at": null})
    );
    let (_, body) = call(&app, &cy.authorization, "GET", MINE, None).await;
    assert_eq!(body["stored"], json!(false), "a row was made: {body}");

    for (method, uri) in [("GET", summary("nobody")), ("POST", turn_off("nobody"))] {
        let (status, body) = call(&app, &admin, method, &uri, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {uri}: {body}");
    }
    app.stop().await;
}
