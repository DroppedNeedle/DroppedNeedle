//! App connect: a native app signs in through the browser and trades a
//! one-time code and its PKCE verifier for an app password.
//!
//! The rig tests drive the three routes over the auth rig and its manual
//! clock (expiry, the mismatch matrix, replacing, refusals); the journey
//! runs the whole flow through `create_app` with a cookie session, the
//! origin check and the request limiter, and signs in to Subsonic with the
//! result.

use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Request, StatusCode},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use droppedneedle_auth::app_connect::AppConnectState;
use droppedneedle_auth::app_connect::codes::CODE_TTL_SECS;
use droppedneedle_auth::app_connect::routes::*;
use droppedneedle_auth::compat_auth::prod::CompatPasswords;
use droppedneedle_auth::compat_auth::subsonic::{SubsonicParams, authenticate};
use droppedneedle_auth::session::store::SessionKind;
use droppedneedle_auth::users::Role;
use droppedneedle_kernel::mount::{Routes, routes};
use droppedneedle_kernel::runtime_config::sections::{AppConnect, AppConnectClient};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tower::ServiceExt as _;

use crate::auth_e2e::{E2e, HOST, call, setup_admin};
use crate::common::auth::{AuthRig, test_principal, with_test_principal};

const CLIENT: &str = "my-player";
const REDIRECT: &str = "com.example.player:/connect";
const VERIFIER: &str = "a-verifier-of-the-app-that-is-long-enough-0123456789";
const STATE: &str = "app state/1";
const ORIGIN: &str = "http://e2e.test";

fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn apps() -> AppConnect {
    AppConnect {
        clients: vec![AppConnectClient {
            client_id: CLIENT.to_owned(),
            name: "My Player".to_owned(),
            redirect_uris: vec![REDIRECT.to_owned()],
        }],
    }
}

fn connect_request() -> Value {
    json!({
        "client_id": CLIENT,
        "redirect_uri": REDIRECT,
        "code_challenge": challenge(VERIFIER),
        "code_challenge_method": "S256",
        "state": STATE,
        "name": "Pixel 9",
    })
}

/// `connect_request` as the `/connect` page's query.
fn connect_query(request: &Value) -> String {
    let pairs: Vec<String> = request
        .as_object()
        .expect("object")
        .iter()
        .map(|(key, value)| {
            let value = value.as_str().expect("string");
            let encoded: String = value
                .bytes()
                .map(|byte| {
                    if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
                        char::from(byte).to_string()
                    } else {
                        format!("%{byte:02X}")
                    }
                })
                .collect();
            format!("{key}={encoded}")
        })
        .collect();
    format!("/api/v3/auth/app-connect/request?{}", pairs.join("&"))
}

fn token_body(code: &str, verifier: &str) -> Value {
    json!({
        "client_id": CLIENT,
        "redirect_uri": REDIRECT,
        "code": code,
        "code_verifier": verifier,
    })
}

/// The one query value `key` of a redirect, decoded.
fn param(redirect: &str, key: &str) -> Option<String> {
    let query = redirect.split_once('?')?.1;
    query.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (name == key).then(|| {
            let bytes = value.as_bytes();
            let mut out = Vec::new();
            let mut at = 0;
            while at < bytes.len() {
                if bytes[at] == b'%' {
                    let hex = std::str::from_utf8(&bytes[at + 1..at + 3]).expect("hex");
                    out.push(u8::from_str_radix(hex, 16).expect("hex digit"));
                    at += 3;
                } else {
                    out.push(bytes[at]);
                    at += 1;
                }
            }
            String::from_utf8(out).expect("utf-8")
        })
    })
}

struct Rig {
    rig: AuthRig,
    state: AppConnectState,
}

impl Rig {
    async fn new(tag: &str, apps: AppConnect) -> Self {
        let rig = AuthRig::new(tag).await;
        rig.config.save_blocking(apps).expect("app connect saves");
        let state = AppConnectState::new(rig.deps.clone(), rig.config.live());
        Self { rig, state }
    }

    fn router(&self, user_id: &str, kind: SessionKind) -> Router {
        let app = Routes::new()
            .user(routes!(app_connect_request_handler))
            .user(routes!(app_connect_authorize_handler))
            .public(routes!(app_connect_token_handler))
            .with_state(self.state.clone())
            .into_router();
        with_test_principal(app, test_principal(user_id, Role::User, "sess-1", kind))
    }

    async fn call(
        &self,
        user_id: &str,
        method: &str,
        uri: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value, HeaderMap) {
        send(
            self.router(user_id, SessionKind::Standard),
            method,
            uri,
            &[],
            body,
        )
        .await
    }

    /// Allow `request` for `user_id` and return the code handed back.
    async fn code_for(&self, user_id: &str, request: Value) -> String {
        let (status, body, _) = self
            .call(
                user_id,
                "POST",
                "/api/v3/auth/app-connect/authorize",
                Some(request),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let redirect = body["redirect"].as_str().expect("redirect");
        param(redirect, "code").expect("code")
    }

    async fn trade(&self, body: Value) -> (StatusCode, Value) {
        let (status, body, _) = self
            .call(
                "nobody",
                "POST",
                "/api/v3/auth/app-connect/token",
                Some(body),
            )
            .await;
        (status, body)
    }

    async fn live_names(&self, user_id: &str) -> Vec<String> {
        self.rig
            .deps
            .app_passwords
            .list_active_by_user(user_id)
            .await
            .expect("list")
            .into_iter()
            .map(|row| row.name)
            .collect()
    }
}

async fn send(
    app: Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: Option<Value>,
) -> (StatusCode, Value, HeaderMap) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", HOST);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let body = match body {
        Some(json) => {
            builder = builder.header("content-type", "application/json");
            Body::from(json.to_string())
        }
        None => Body::empty(),
    };
    let response = app.oneshot(builder.body(body).unwrap()).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        headers,
    )
}

#[tokio::test]
async fn the_request_view_names_the_app_device_and_password() {
    let rig = Rig::new("app-connect-view", apps()).await;
    let user = rig.rig.seed_user("molly", Role::User).await;
    let (status, body, _) = rig
        .call(&user.id, "GET", &connect_query(&connect_request()), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body,
        json!({
            "client_id": CLIENT,
            "client_name": "My Player",
            "device_name": "Pixel 9",
            "password_name": "My Player · Pixel 9",
            "replaces_existing": false,
            "at_limit": false,
            "cancel_redirect": "com.example.player:/connect?error=access_denied&state=app%20state%2F1",
        })
    );

    // An unknown app or address gets no address back; a malformed request
    // of a known one gets its error address in the details.
    let mut unknown = connect_request();
    unknown["redirect_uri"] = json!("https://evil.example/");
    let (status, body, _) = rig
        .call(&user.id, "GET", &connect_query(&unknown), None)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["details"], Value::Null, "{body}");
    let mut plain = connect_request();
    plain["code_challenge_method"] = json!("plain");
    let (status, body, _) = rig
        .call(&user.id, "GET", &connect_query(&plain), None)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(
        body["error"]["details"]["redirect"],
        json!("com.example.player:/connect?error=invalid_request&state=app%20state%2F1")
    );
}

#[tokio::test]
async fn a_code_trades_once_for_exactly_its_app_address_and_verifier() {
    let rig = Rig::new("app-connect-trade", apps()).await;
    let user = rig.rig.seed_user("molly", Role::User).await;

    // The request's state comes back unchanged next to the code.
    let (status, body, headers) = rig
        .call(
            &user.id,
            "POST",
            "/api/v3/auth/app-connect/authorize",
            Some(connect_request()),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(headers["cache-control"], "no-store");
    let redirect = body["redirect"].as_str().unwrap();
    assert!(
        redirect.starts_with("com.example.player:/connect?code="),
        "{redirect}"
    );
    assert_eq!(param(redirect, "state").as_deref(), Some(STATE));
    let code = param(redirect, "code").unwrap();
    assert_eq!(code.len(), 43);

    let (status, body) = rig.trade(token_body(&code, VERIFIER)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let secret = body["app_password"].as_str().unwrap().to_owned();
    assert_eq!(body["name"], json!("My Player · Pixel 9"));
    assert_eq!(body["username"], json!("molly"));
    let row = rig
        .rig
        .deps
        .app_passwords
        .get_by_id(body["id"].as_str().unwrap())
        .await
        .unwrap()
        .expect("the row exists");
    assert_eq!(row.user_id, user.id);

    // The password signs in to Subsonic as the user.
    let compat = CompatPasswords::new(rig.rig.deps.clone(), Arc::clone(&rig.rig.platform.crypto));
    let params = SubsonicParams::new(vec![("apiKey", secret.as_str())]);
    let principal = authenticate(&compat, &params).await.expect("apiKey works");
    assert_eq!(principal.user_id, user.id);

    // Replay of a traded code.
    assert_eq!(
        rig.trade(token_body(&code, VERIFIER)).await,
        (StatusCode::BAD_REQUEST, json!({"error": "invalid_grant"}))
    );

    // Each mismatch is the same invalid_grant, and burns the code: the
    // right trade after it fails too.
    for (field, value) in [
        (
            "code_verifier",
            "another-verifier-that-is-also-long-enough-01234567",
        ),
        ("client_id", "other"),
        ("redirect_uri", "com.example.player:/other"),
    ] {
        let code = rig.code_for(&user.id, connect_request()).await;
        let mut wrong = token_body(&code, VERIFIER);
        wrong[field] = json!(value);
        assert_eq!(
            rig.trade(wrong).await,
            (StatusCode::BAD_REQUEST, json!({"error": "invalid_grant"})),
            "{field}"
        );
        assert_eq!(
            rig.trade(token_body(&code, VERIFIER)).await,
            (StatusCode::BAD_REQUEST, json!({"error": "invalid_grant"})),
            "{field}: the code is burnt"
        );
    }
    assert_eq!(
        rig.trade(token_body("never-issued", VERIFIER)).await,
        (StatusCode::BAD_REQUEST, json!({"error": "invalid_grant"}))
    );

    // A malformed body is invalid_request, also the empty object the app
    // sends to learn whether the feature is on.
    for body in [json!({}), json!({"code": 1}), json!([])] {
        let (status, answer, headers) = rig
            .call(
                "nobody",
                "POST",
                "/api/v3/auth/app-connect/token",
                Some(body),
            )
            .await;
        assert_eq!(
            (status, answer),
            (StatusCode::BAD_REQUEST, json!({"error": "invalid_request"}))
        );
        assert_eq!(headers["cache-control"], "no-store");
    }
}

#[tokio::test]
async fn a_code_expires_after_two_minutes() {
    let rig = Rig::new("app-connect-expiry", apps()).await;
    let user = rig.rig.seed_user("molly", Role::User).await;
    let in_time = rig.code_for(&user.id, connect_request()).await;
    let late = rig.code_for(&user.id, connect_request()).await;
    rig.rig.clock.advance((CODE_TTL_SECS - 1) * 1000);
    assert_eq!(
        rig.trade(token_body(&in_time, VERIFIER)).await.0,
        StatusCode::OK
    );
    rig.rig.clock.advance(1000);
    assert_eq!(
        rig.trade(token_body(&late, VERIFIER)).await,
        (StatusCode::BAD_REQUEST, json!({"error": "invalid_grant"}))
    );
}

#[tokio::test]
async fn signing_in_again_on_the_device_replaces_its_password() {
    let rig = Rig::new("app-connect-replace", apps()).await;
    let user = rig.rig.seed_user("molly", Role::User).await;
    let other = rig.rig.seed_user("other", Role::User).await;
    let compat = CompatPasswords::new(rig.rig.deps.clone(), Arc::clone(&rig.rig.platform.crypto));

    let code = rig.code_for(&user.id, connect_request()).await;
    let first = rig.trade(token_body(&code, VERIFIER)).await.1;
    let code = rig.code_for(&other.id, connect_request()).await;
    rig.trade(token_body(&code, VERIFIER)).await;

    // The page says it replaces one.
    let (_, view, _) = rig
        .call(&user.id, "GET", &connect_query(&connect_request()), None)
        .await;
    assert_eq!(view["replaces_existing"], json!(true));

    let code = rig.code_for(&user.id, connect_request()).await;
    let second = rig.trade(token_body(&code, VERIFIER)).await.1;
    assert_ne!(first["id"], second["id"]);
    assert_eq!(rig.live_names(&user.id).await, ["My Player · Pixel 9"]);
    assert_eq!(
        rig.live_names(&other.id).await,
        ["My Player · Pixel 9"],
        "another user's stays"
    );
    let old = SubsonicParams::new(vec![("apiKey", first["app_password"].as_str().unwrap())]);
    assert!(
        authenticate(&compat, &old).await.is_err(),
        "the old one is revoked"
    );
    let new = SubsonicParams::new(vec![("apiKey", second["app_password"].as_str().unwrap())]);
    assert!(authenticate(&compat, &new).await.is_ok());

    // Another device keeps its own.
    let mut tablet = connect_request();
    tablet["name"] = json!("Tab S9");
    let code = rig.code_for(&user.id, tablet).await;
    rig.trade(token_body(&code, VERIFIER)).await;
    let mut names = rig.live_names(&user.id).await;
    names.sort();
    assert_eq!(names, ["My Player · Pixel 9", "My Player · Tab S9"]);
}

#[tokio::test]
async fn refusals_before_a_code_is_issued() {
    let rig = Rig::new("app-connect-refusals", apps()).await;
    let user = rig.rig.seed_user("molly", Role::User).await;

    // A device session cannot mint a permanent credential.
    let (status, body, _) = send(
        rig.router(&user.id, SessionKind::Companion),
        "POST",
        "/api/v3/auth/app-connect/authorize",
        &[],
        Some(connect_request()),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    // Unknown app, malformed request.
    let mut unknown = connect_request();
    unknown["client_id"] = json!("other");
    let mut no_state = connect_request();
    no_state["state"] = json!("");
    for request in [unknown, no_state] {
        let (status, body, _) = rig
            .call(
                &user.id,
                "POST",
                "/api/v3/auth/app-connect/authorize",
                Some(request),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    }

    // At the app-password limit the page says so and allow is refused.
    for n in 0..25 {
        rig.rig
            .deps
            .app_passwords
            .insert_capped(
                droppedneedle_auth::users::models::AppPasswordRecord {
                    id: format!("ap-{n}"),
                    user_id: user.id.clone(),
                    name: format!("manual {n}"),
                    secret_sha256: format!("{n:064}"),
                    secret_encrypted: String::new(),
                    created_at: 0,
                    last_used_at: None,
                    last_client: None,
                },
                25,
            )
            .await
            .unwrap();
    }
    let (_, view, _) = rig
        .call(&user.id, "GET", &connect_query(&connect_request()), None)
        .await;
    assert_eq!(view["at_limit"], json!(true));
    let (status, body, _) = rig
        .call(
            &user.id,
            "POST",
            "/api/v3/auth/app-connect/authorize",
            Some(connect_request()),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(rig.state.codes.is_empty());
}

#[tokio::test]
async fn a_code_is_void_once_its_app_is_unlisted_or_its_user_gone() {
    let rig = Rig::new("app-connect-void", apps()).await;
    let user = rig.rig.seed_user("molly", Role::User).await;
    let unlisted = rig.code_for(&user.id, connect_request()).await;
    let mut moved = apps();
    moved.clients[0].redirect_uris = vec!["com.example.player:/moved".to_owned()];
    rig.rig.config.save_blocking(moved).unwrap();
    assert_eq!(
        rig.trade(token_body(&unlisted, VERIFIER)).await,
        (StatusCode::BAD_REQUEST, json!({"error": "invalid_grant"}))
    );

    rig.rig.config.save_blocking(apps()).unwrap();
    let orphan = rig.code_for(&user.id, connect_request()).await;
    rig.rig.deps.users.delete(&user.id).await.unwrap();
    assert_eq!(
        rig.trade(token_body(&orphan, VERIFIER)).await,
        (StatusCode::BAD_REQUEST, json!({"error": "invalid_grant"}))
    );
    assert!(rig.live_names(&user.id).await.is_empty());
}

#[tokio::test]
async fn every_route_is_404_while_no_app_is_listed() {
    let rig = Rig::new("app-connect-off", AppConnect::default()).await;
    let user = rig.rig.seed_user("molly", Role::User).await;
    for (method, uri, body) in [
        ("GET", connect_query(&connect_request()), None),
        (
            "POST",
            "/api/v3/auth/app-connect/authorize".to_owned(),
            Some(connect_request()),
        ),
        (
            "POST",
            "/api/v3/auth/app-connect/token".to_owned(),
            Some(json!({})),
        ),
        (
            "POST",
            "/api/v3/auth/app-connect/token".to_owned(),
            Some(token_body("x", VERIFIER)),
        ),
    ] {
        let (status, body, _) = rig.call(&user.id, method, &uri, body).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {uri}: {body}");
    }
}

/// The whole flow through the real app: a cookie session allows (with the
/// origin check), the app trades without one, Subsonic takes the result,
/// and the limiter holds the public trade to its own budget.
#[tokio::test]
async fn journey_app_connect_through_the_app() {
    let e2e = E2e::open("app-connect").await;
    let app = e2e.router();
    let (owner, session) =
        setup_admin(app.clone(), "e2e-owner", "e2e-owner-password-1", "cookie").await;
    let cookie = format!("droppedneedle_sid={}", session.as_str().unwrap());

    // Off until an app is listed.
    let (status, _, _) = call(
        app.clone(),
        "POST",
        "/api/v3/auth/app-connect/token",
        &[],
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // The admin lists the app. The settings route takes it; the harness
    // gives that route a store of its own, so the app's store gets it
    // directly, as the one production store would.
    let (status, body, _) = call(
        app.clone(),
        "PUT",
        "/api/v3/settings/app-connect",
        &[("cookie", &cookie), ("origin", ORIGIN)],
        Some(serde_json::to_value(apps()).unwrap()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    e2e.store.save_blocking(apps()).expect("app connect saves");
    let (status, _, _) = call(
        app.clone(),
        "POST",
        "/api/v3/auth/app-connect/token",
        &[],
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "on now: anything but 404");

    let (status, view, _) = call(
        app.clone(),
        "GET",
        &connect_query(&connect_request()),
        &[("cookie", &cookie)],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{view}");

    // A cookie allow from another origin is refused by the session gate.
    let (status, _, _) = call(
        app.clone(),
        "POST",
        "/api/v3/auth/app-connect/authorize",
        &[("cookie", &cookie), ("origin", "http://evil.test")],
        Some(connect_request()),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, allowed, _) = call(
        app.clone(),
        "POST",
        "/api/v3/auth/app-connect/authorize",
        &[("cookie", &cookie), ("origin", ORIGIN)],
        Some(connect_request()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{allowed}");
    let code = param(allowed["redirect"].as_str().unwrap(), "code").unwrap();

    let (status, minted, headers) = call(
        app.clone(),
        "POST",
        "/api/v3/auth/app-connect/token",
        &[],
        Some(token_body(&code, VERIFIER)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{minted}");
    assert_eq!(headers["cache-control"], "no-store");
    assert_eq!(minted["username"], json!("e2e-owner"));

    // Listed under the user's app passwords like one made by hand.
    let (_, listed, _) = call(
        app.clone(),
        "GET",
        "/api/v3/me/app-passwords",
        &[("cookie", &cookie)],
        None,
    )
    .await;
    assert_eq!(
        listed["app_passwords"][0]["name"],
        json!("My Player · Pixel 9"),
        "{listed}"
    );
    assert_eq!(listed["app_passwords"][0]["id"], minted["id"]);

    let compat = CompatPasswords::new(e2e.users(), e2e.crypto());
    let params = SubsonicParams::new(vec![("apiKey", minted["app_password"].as_str().unwrap())]);
    let principal = authenticate(&compat, &params).await.expect("apiKey works");
    assert_eq!(principal.user_id, owner["id"].as_str().unwrap());

    // The public trade has its own small budget per address.
    let mut limited = None;
    for _ in 0..20 {
        let (status, _, headers) = call(
            app.clone(),
            "POST",
            "/api/v3/auth/app-connect/token",
            &[],
            Some(token_body("guess", VERIFIER)),
        )
        .await;
        if status == StatusCode::TOO_MANY_REQUESTS {
            limited = Some(headers);
            break;
        }
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    let limited = limited.expect("the trade is limited");
    assert!(limited.contains_key("retry-after"));
}
