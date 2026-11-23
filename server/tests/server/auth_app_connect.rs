//! App connect: a native app signs in through the browser and trades a
//! one-time code and its PKCE verifier at an OAuth 2.0 token endpoint for
//! a registered client's configured credential.
//!
//! The journey runs the whole flow through `create_app` with a cookie
//! session, the origin check and the request limiter, and signs in to
//! Subsonic or the native API with the result. The refusal table drives the routes over the
//! auth rig and its manual clock.

use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Request, StatusCode},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use droppedneedle_auth::app_connect::routes::*;
use droppedneedle_auth::app_connect::{AppConnectService, CODE_TTL_SECS};
use droppedneedle_auth::compat_auth::prod::CompatPasswords;
use droppedneedle_auth::compat_auth::subsonic::{SubsonicParams, authenticate};
use droppedneedle_auth::session::store::SessionKind;
use droppedneedle_auth::session::tokens::SESSION_MAX_AGE_SECS;
use droppedneedle_auth::users::Role;
use droppedneedle_kernel::mount::{Routes, routes};
use droppedneedle_kernel::runtime_config::sections::AppConnect;
use ring::rand::{SecureRandom as _, SystemRandom};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tower::ServiceExt as _;

use crate::auth_e2e::{E2e, HOST, login, set_cookie_token, setup_admin};
use crate::common::auth::{AuthRig, test_principal, with_test_principal};

const CLIENT: &str = "my-player";
const REDIRECT: &str = "com.example.player:/connect";
const STATE: &str = "app state/1";
const ORIGIN: &str = "http://e2e.test";
const TOKEN: &str = "/api/v3/auth/app-connect/token";
const AUTHORIZE: &str = "/api/v3/auth/app-connect/authorize";

/// A fresh PKCE pair, made the way an app makes one: 32 random bytes as
/// the verifier, its SHA-256 as the challenge.
fn pkce() -> (String, String) {
    let mut bytes = [0u8; 32];
    SystemRandom::new().fill(&mut bytes).unwrap();
    let verifier = URL_SAFE_NO_PAD.encode(bytes);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

fn apps() -> AppConnect {
    // Instances configured before credential kinds existed stored exactly these fields.
    serde_json::from_value(json!({ "clients": [{
        "client_id": CLIENT,
        "name": "My Player",
        "redirect_uris": [REDIRECT],
    }] }))
    .unwrap()
}

fn connect_request(challenge: &str) -> Value {
    json!({
        "response_type": "code",
        "client_id": CLIENT,
        "redirect_uri": REDIRECT,
        "code_challenge": challenge,
        "code_challenge_method": "S256",
        "state": STATE,
        "name": "Pixel 9",
    })
}

/// The page's Allow body for an app-password app: the request and the
/// kind it showed.
fn authorize_body(challenge: &str) -> Value {
    let mut body = connect_request(challenge);
    body["confirmed_credential_kind"] = json!("app_password");
    body
}

fn encode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

/// `pairs` as a query string or form body.
fn form(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(key, value)| format!("{key}={}", encode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

/// `request` as the `/connect` page's query.
fn request_uri(request: &Value) -> String {
    let pairs: Vec<(&str, &str)> = request
        .as_object()
        .unwrap()
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str().unwrap()))
        .collect();
    format!("/api/v3/auth/app-connect/request?{}", form(&pairs))
}

/// The form an app sends to trade `code`.
fn trade_form(code: &str, verifier: &str) -> String {
    client_trade_form(CLIENT, code, verifier)
}

fn client_trade_form(client_id: &str, code: &str, verifier: &str) -> String {
    form(&[
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", REDIRECT),
        ("client_id", client_id),
        ("code_verifier", verifier),
    ])
}

/// The one query value `key` of a redirect, decoded.
fn param(redirect: &str, key: &str) -> Option<String> {
    let query = redirect.split_once('?')?.1;
    url::form_urlencoded::parse(query.as_bytes())
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.into_owned())
}

enum Payload {
    None,
    Json(Value),
    Form(String),
}

async fn send(
    app: Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    payload: Payload,
) -> (StatusCode, Value, HeaderMap) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", HOST);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let body = match payload {
        Payload::None => Body::empty(),
        Payload::Json(json) => {
            builder = builder.header("content-type", "application/json");
            Body::from(json.to_string())
        }
        Payload::Form(form) => {
            builder = builder.header("content-type", "application/x-www-form-urlencoded");
            Body::from(form)
        }
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

fn refused(error: &str) -> (StatusCode, Value) {
    (StatusCode::BAD_REQUEST, json!({ "error": error }))
}

/// The whole flow through the real app: off until an admin lists the app,
/// a cookie session allows it (with the origin check), the app trades the
/// code with no session, Subsonic takes the result, signing in again on the
/// device replaces it, and the limiter holds the public trade to its own
/// budget.
#[tokio::test]
async fn journey_app_connect_through_the_app() {
    let e2e = E2e::open("app-connect").await;
    let app = e2e.router();
    let (owner, session) =
        setup_admin(app.clone(), "e2e-owner", "e2e-owner-password-1", "cookie").await;
    let cookie = format!("droppedneedle_sid={}", session.as_str().unwrap());
    let signed_in = [("cookie", cookie.as_str()), ("origin", ORIGIN)];
    let (verifier, challenge) = pkce();
    let compat = CompatPasswords::new(e2e.users(), e2e.crypto());

    let trade = |code: &str| {
        send(
            app.clone(),
            "POST",
            TOKEN,
            &[],
            Payload::Form(trade_form(code, &verifier)),
        )
    };
    let (status, body, _) = trade("never-issued").await;
    assert_eq!((status, body), refused("invalid_client"), "off");

    // The admin lists the app. The harness gives the settings route a store
    // of its own, so the app's store gets it directly, as the one
    // production store would.
    let (status, body, _) = send(
        app.clone(),
        "PUT",
        "/api/v3/settings/app-connect",
        &signed_in,
        Payload::Json(serde_json::to_value(apps()).unwrap()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    e2e.store.save_blocking(apps()).unwrap();

    let (status, view, _) = send(
        app.clone(),
        "GET",
        &request_uri(&connect_request(&challenge)),
        &signed_in,
        Payload::None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(
        view,
        json!({
            "client_id": CLIENT,
            "client_name": "My Player",
            "credential_kind": "app_password",
            "device_name": "Pixel 9",
            "password_name": "My Player · Pixel 9",
            "replaces_existing": false,
            "at_limit": false,
            "cancel_redirect": "com.example.player:/connect?error=access_denied&state=app%20state%2F1",
        })
    );

    // A cookie allow from another origin is refused by the session gate.
    let (status, _, _) = send(
        app.clone(),
        "POST",
        AUTHORIZE,
        &[("cookie", &cookie), ("origin", "http://evil.test")],
        Payload::Json(authorize_body(&challenge)),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let allow = || async {
        let (status, allowed, headers) = send(
            app.clone(),
            "POST",
            AUTHORIZE,
            &signed_in,
            Payload::Json(authorize_body(&challenge)),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{allowed}");
        assert_eq!(headers["cache-control"], "no-store");
        let redirect = allowed["redirect"].as_str().unwrap();
        assert!(redirect.starts_with("com.example.player:/connect?code="));
        assert_eq!(param(redirect, "state").as_deref(), Some(STATE));
        param(redirect, "code").unwrap()
    };
    let code = allow().await;
    let (status, minted, headers) = trade(&code).await;
    assert_eq!(status, StatusCode::OK, "{minted}");
    assert_eq!(headers["cache-control"], "no-store");
    assert_eq!(headers["pragma"], "no-cache");
    assert_eq!(minted["token_type"], json!("Bearer"));
    assert_eq!(minted["username"], json!("e2e-owner"));
    assert!(minted.get("expires_in").is_none());
    assert_eq!(minted["credential_kind"], json!("app_password"));
    let first = minted["access_token"].as_str().unwrap().to_owned();

    // A code trades once.
    let (status, body, _) = trade(&code).await;
    assert_eq!((status, body), refused("invalid_grant"));

    // Listed under the user's app passwords like one made by hand, and
    // Subsonic takes it as the user.
    let (_, listed, _) = send(
        app.clone(),
        "GET",
        "/api/v3/me/app-passwords",
        &signed_in,
        Payload::None,
    )
    .await;
    assert_eq!(
        listed["app_passwords"][0]["name"],
        json!("My Player · Pixel 9"),
        "{listed}"
    );
    let principal = authenticate(
        &compat,
        &SubsonicParams::new(vec![("apiKey", first.as_str())]),
    )
    .await
    .expect("the access token is an app password");
    assert_eq!(principal.user_id, owner["id"].as_str().unwrap());

    // Signing in again on the same device replaces the password.
    let code = allow().await;
    let (status, minted, _) = trade(&code).await;
    assert_eq!(status, StatusCode::OK, "{minted}");
    let second = minted["access_token"].as_str().unwrap();
    assert!(
        authenticate(
            &compat,
            &SubsonicParams::new(vec![("apiKey", first.as_str())])
        )
        .await
        .is_err(),
        "the old one is revoked"
    );
    assert!(
        authenticate(&compat, &SubsonicParams::new(vec![("apiKey", second)]))
            .await
            .is_ok()
    );

    // The public trade has its own small budget per address.
    let mut limited = None;
    for _ in 0..20 {
        let (status, _, headers) = trade("guess").await;
        if status == StatusCode::TOO_MANY_REQUESTS {
            limited = Some(headers);
            break;
        }
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    assert!(
        limited
            .expect("the trade is limited")
            .contains_key("retry-after")
    );
}

struct Rig {
    rig: AuthRig,
    service: AppConnectService,
}

impl Rig {
    async fn new(tag: &str) -> Self {
        let rig = AuthRig::new(tag).await;
        rig.config.save_blocking(apps()).unwrap();
        let service = AppConnectService::new(rig.deps.clone(), rig.config.live());
        Self { rig, service }
    }

    fn router(&self, user_id: &str, kind: SessionKind) -> Router {
        let app = Routes::new()
            .user(routes!(app_connect_request_handler))
            .user(routes!(app_connect_authorize_handler))
            .public(routes!(app_connect_token_handler))
            .with_state(self.service.clone())
            .into_router();
        with_test_principal(app, test_principal(user_id, Role::User, "sess-1", kind))
    }

    async fn call(
        &self,
        user_id: &str,
        method: &str,
        uri: &str,
        payload: Payload,
    ) -> (StatusCode, Value) {
        let (status, body, _) = send(
            self.router(user_id, SessionKind::Standard),
            method,
            uri,
            &[],
            payload,
        )
        .await;
        (status, body)
    }

    /// Allow a request with `challenge` for `user_id`; the code handed back.
    async fn code_for(&self, user_id: &str, challenge: &str) -> String {
        let (status, body) = self
            .call(
                user_id,
                "POST",
                AUTHORIZE,
                Payload::Json(authorize_body(challenge)),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        param(body["redirect"].as_str().unwrap(), "code").unwrap()
    }

    async fn trade(&self, form: String) -> (StatusCode, Value) {
        self.call("nobody", "POST", TOKEN, Payload::Form(form))
            .await
    }

    async fn live_names(&self, user_id: &str) -> Vec<String> {
        let rows = self.rig.deps.app_passwords.list_active_by_user(user_id);
        rows.await
            .unwrap()
            .into_iter()
            .map(|row| row.name)
            .collect()
    }
}

/// Each way a trade or an allow is refused, and that a refused trade burns
/// its code and mints nothing.
#[tokio::test]
async fn app_connect_refusals() {
    let rig = Rig::new("app-connect-refusals").await;
    let molly = rig.rig.seed_user("molly", Role::User).await;
    let other = rig.rig.seed_user("other", Role::User).await;
    let (verifier, challenge) = pkce();
    let (other_verifier, _) = pkce();

    // Wrong verifier, then the right one: the code went with the first try.
    let code = rig.code_for(&molly.id, &challenge).await;
    assert_eq!(
        rig.trade(trade_form(&code, &other_verifier)).await,
        refused("invalid_grant")
    );
    assert_eq!(
        rig.trade(trade_form(&code, &verifier)).await,
        refused("invalid_grant"),
        "a refused trade burns the code"
    );

    // Expired after two minutes.
    let code = rig.code_for(&molly.id, &challenge).await;
    rig.rig.clock().advance(CODE_TTL_SECS * 1000);
    assert_eq!(
        rig.trade(trade_form(&code, &verifier)).await,
        refused("invalid_grant")
    );

    // A code traded at another address than it was sent to.
    let code = rig.code_for(&molly.id, &challenge).await;
    let elsewhere = form(&[
        ("grant_type", "authorization_code"),
        ("code", &code),
        ("redirect_uri", "com.example.player:/other"),
        ("client_id", CLIENT),
        ("code_verifier", &verifier),
    ]);
    assert_eq!(rig.trade(elsewhere).await, refused("invalid_grant"));

    // A code mints for the user who allowed it, whoever trades it, and
    // no other account gains a password.
    let code = rig.code_for(&molly.id, &challenge).await;
    let (status, body) = rig.trade(trade_form(&code, &verifier)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["username"], json!("molly"));
    assert_eq!(rig.live_names(&molly.id).await, ["My Player · Pixel 9"]);
    assert!(rig.live_names(&other.id).await.is_empty());

    // An unlisted address is refused with nothing handed back.
    let mut unlisted = connect_request(&challenge);
    unlisted["redirect_uri"] = json!("https://evil.example/");
    let (status, body) = rig
        .call(&molly.id, "GET", &request_uri(&unlisted), Payload::None)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["details"], Value::Null, "{body}");
    let mut unlisted_allow = unlisted;
    unlisted_allow["confirmed_credential_kind"] = json!("app_password");
    let (status, _) = rig
        .call(&molly.id, "POST", AUTHORIZE, Payload::Json(unlisted_allow))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // A listed app that asks for something other than a code gets the
    // error handed back to it.
    let mut implicit = connect_request(&challenge);
    implicit["response_type"] = json!("token");
    let (status, body) = rig
        .call(&molly.id, "GET", &request_uri(&implicit), Payload::None)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(
        body["error"]["details"]["redirect"],
        json!("com.example.player:/connect?error=unsupported_response_type&state=app%20state%2F1")
    );

    // A device session cannot mint a permanent credential.
    let (status, _, _) = send(
        rig.router(&molly.id, SessionKind::Companion),
        "POST",
        AUTHORIZE,
        &[],
        Payload::Json(authorize_body(&challenge)),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // What is not a trade.
    let code = rig.code_for(&molly.id, &challenge).await;
    let password_grant = trade_form(&code, &verifier)
        .replace("grant_type=authorization_code", "grant_type=password");
    assert_eq!(
        rig.trade(password_grant).await,
        refused("unsupported_grant_type")
    );
    let twice = format!("{}&code=again", trade_form(&code, &verifier));
    assert_eq!(rig.trade(twice).await, refused("invalid_request"));
    let as_json = json!({
        "grant_type": "authorization_code",
        "code": code,
        "redirect_uri": REDIRECT,
        "client_id": CLIENT,
        "code_verifier": verifier,
    });
    assert_eq!(
        rig.call("nobody", "POST", TOKEN, Payload::Json(as_json))
            .await,
        refused("invalid_request")
    );
    let stranger = trade_form(&code, &verifier).replace(CLIENT, "stranger");
    assert_eq!(rig.trade(stranger).await, refused("invalid_client"));
    assert_eq!(
        rig.live_names(&molly.id).await,
        ["My Player · Pixel 9"],
        "nothing refused minted"
    );

    // Off: the page's routes are 404 and every app is unknown.
    rig.rig.config.save_blocking(AppConnect::default()).unwrap();
    let (status, _) = rig
        .call(
            &molly.id,
            "GET",
            &request_uri(&connect_request(&challenge)),
            Payload::None,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = rig
        .call(
            &molly.id,
            "POST",
            AUTHORIZE,
            Payload::Json(authorize_body(&challenge)),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        rig.trade(trade_form("any", &verifier)).await,
        refused("invalid_client")
    );
}

fn native_apps(client_ids: &[&str]) -> AppConnect {
    // Use the settings wire representation an instance admin saves.
    let clients: Vec<_> = client_ids
        .iter()
        .map(|id| {
            json!({
                "client_id": id,
                "name": "My Player",
                "redirect_uris": [REDIRECT],
                "credential_kind": "device_session",
            })
        })
        .collect();
    serde_json::from_value(json!({ "clients": clients })).unwrap()
}

/// Follow the handoff and return the bearer header exactly as a native client would use it.
async fn native_bearer(app: &Router, browser: &[(&str, &str)], client_id: &str) -> String {
    let (verifier, challenge) = pkce();
    let mut request = connect_request(&challenge);
    request["client_id"] = json!(client_id);
    let (status, view, _) = send(
        app.clone(),
        "GET",
        &request_uri(&request),
        browser,
        Payload::None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["credential_kind"], "device_session");
    request["confirmed_credential_kind"] = view["credential_kind"].clone();
    let (status, allowed, headers) = send(
        app.clone(),
        "POST",
        AUTHORIZE,
        browser,
        Payload::Json(request),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["cache-control"], "no-store");
    let redirect = allowed["redirect"].as_str().expect("redirect");
    assert_eq!(param(redirect, "state").as_deref(), Some(STATE));
    let code = param(redirect, "code").expect("code in redirect");
    let (status, token, headers) = send(
        app.clone(),
        "POST",
        TOKEN,
        &[],
        Payload::Form(client_trade_form(client_id, &code, &verifier)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["cache-control"], "no-store");
    assert_eq!(headers["pragma"], "no-cache");
    assert_eq!(token["token_type"], "Bearer");
    assert_eq!(token["expires_in"], SESSION_MAX_AGE_SECS);
    assert_eq!(token["credential_kind"], "device_session");
    format!(
        "Bearer {}",
        token["access_token"].as_str().expect("access token")
    )
}

#[tokio::test]
async fn journey_app_connect_device_session() {
    const SECOND: &str = "another-player";
    let (e2e, clock) = E2e::with_manual_clock("native-app-connect").await;
    let app = e2e.router();
    let (_, session) = setup_admin(app.clone(), "owner", "owner-password-1234", "bearer").await;
    let admin_bearer = format!("Bearer {}", session.as_str().unwrap());
    let admin = [("authorization", admin_bearer.as_str())];
    let (status, listener, _) = send(
        app.clone(),
        "POST",
        "/api/v3/admin/users",
        &admin,
        Payload::Json(json!({"username": "listener", "password": "listener-password-1234"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, _, headers) =
        login(app.clone(), "listener", "listener-password-1234", "cookie").await;
    assert_eq!(status, StatusCode::OK);
    let cookie = format!(
        "droppedneedle_sid={}",
        set_cookie_token(&headers).expect("cookie")
    );
    let browser = [("cookie", cookie.as_str()), ("origin", ORIGIN)];
    e2e.store
        .save_blocking(native_apps(&[CLIENT, SECOND]))
        .unwrap();

    // A manually minted session can have exactly the same visible label.
    let (status, manual, _) = send(
        app.clone(),
        "POST",
        "/api/v3/auth/device-sessions",
        &browser,
        Payload::Json(json!({"label": "My Player · Pixel 9"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let manual = format!("Bearer {}", manual["token"].as_str().unwrap());

    // Same display name and device name across two separate registrations.
    let first = native_bearer(&app, &browser, CLIENT).await;
    // These are separate sign-ins. Allow the per-address budget to refill
    // between them, as it would while a person opens the next app.
    clock.advance(3_000);
    let second = native_bearer(&app, &browser, SECOND).await;
    clock.advance(3_000);
    let replacement = native_bearer(&app, &browser, CLIENT).await;
    let me = |bearer: String| {
        let app = app.clone();
        async move {
            let (status, body, _) = send(
                app,
                "GET",
                "/api/v3/me",
                &[("authorization", &bearer)],
                Payload::None,
            )
            .await;
            (status, body["id"].clone())
        }
    };
    assert_eq!(me(first).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(
        me(second.clone()).await,
        (StatusCode::OK, listener["id"].clone())
    );
    assert_eq!(
        me(replacement.clone()).await,
        (StatusCode::OK, listener["id"].clone())
    );
    let (_, listed, _) = send(
        app.clone(),
        "GET",
        "/api/v3/me/app-passwords",
        &browser,
        Payload::None,
    )
    .await;
    assert_eq!(listed["app_passwords"], json!([]));

    // Identify the current device session using that device's own token.
    let device = [("authorization", replacement.as_str())];
    let (status, _, _) = send(
        app.clone(),
        "GET",
        "/api/v3/admin/users",
        &device,
        Payload::None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "the app keeps the listener's role"
    );

    let (status, listed, _) = send(
        app.clone(),
        "GET",
        "/api/v3/auth/sessions",
        &device,
        Payload::None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let sessions = listed["sessions"].as_array().unwrap();
    assert_eq!(
        sessions
            .iter()
            .filter(|row| row["kind"] == "companion")
            .count(),
        3
    );
    let current = sessions.iter().find(|row| row["current"] == true).unwrap();
    assert_eq!(current["kind"], "companion");
    let uri = format!("/api/v3/auth/sessions/{}", current["id"].as_str().unwrap());
    let (status, _, _) = send(
        app.clone(),
        "POST",
        "/api/v3/auth/device-sessions",
        &device,
        Payload::Json(json!({"label": "another device"})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, _, _) = send(app.clone(), "DELETE", &uri, &admin, Payload::None).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "own-session revocation is scoped to the caller"
    );
    let (status, _, _) = send(app.clone(), "DELETE", &uri, &browser, Payload::None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(me(replacement).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(me(second).await.0, StatusCode::OK);
    assert_eq!(me(manual).await.0, StatusCode::OK);
}

#[tokio::test]
async fn app_connect_consent_is_bound_to_the_registered_kind() {
    let rig = Rig::new("app-connect-credential-kind").await;
    let user = rig.rig.seed_user("molly", Role::User).await;
    let (verifier, challenge) = pkce();
    let password_code = rig.code_for(&user.id, &challenge).await;
    rig.rig
        .config
        .save_blocking(native_apps(&[CLIENT]))
        .unwrap();
    assert_eq!(
        rig.trade(trade_form(&password_code, &verifier)).await,
        refused("invalid_grant")
    );

    // A stale confirmation is rejected even when the client ID and
    // redirect have stayed the same.
    let mut request = connect_request(&challenge);
    request["confirmed_credential_kind"] = json!("app_password");
    let (status, _, _) = send(
        rig.router(&user.id, SessionKind::Standard),
        "POST",
        AUTHORIZE,
        &[],
        Payload::Json(request.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    request["confirmed_credential_kind"] = json!("device_session");
    let (status, _, _) = send(
        rig.router(&user.id, SessionKind::Companion),
        "POST",
        AUTHORIZE,
        &[],
        Payload::Json(request.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, allowed) = rig
        .call(&user.id, "POST", AUTHORIZE, Payload::Json(request))
        .await;
    assert_eq!(status, StatusCode::OK);
    let code = param(allowed["redirect"].as_str().unwrap(), "code").unwrap();
    rig.rig.config.save_blocking(apps()).unwrap();
    assert_eq!(
        rig.trade(trade_form(&code, &verifier)).await,
        refused("invalid_grant")
    );
    assert!(rig.live_names(&user.id).await.is_empty());
}

/// The consent view reports the cap, but grants already issued still have to
/// obey it when their token trades arrive together.
#[tokio::test]
async fn app_connect_device_session_limit() {
    let rig = Rig::new("app-connect-device-cap").await;
    let user = rig.rig.seed_user("molly", Role::User).await;
    rig.rig
        .config
        .save_blocking(native_apps(&[CLIENT]))
        .unwrap();

    let allow = |name: String| {
        let rig = &rig;
        let user_id = &user.id;
        async move {
            let (verifier, challenge) = pkce();
            let mut request = connect_request(&challenge);
            request["name"] = json!(name);
            request["confirmed_credential_kind"] = json!("device_session");
            let (status, allowed) = rig
                .call(user_id, "POST", AUTHORIZE, Payload::Json(request))
                .await;
            assert_eq!(status, StatusCode::OK);
            let code = param(allowed["redirect"].as_str().unwrap(), "code").unwrap();
            trade_form(&code, &verifier)
        }
    };
    for n in 0..24 {
        assert_eq!(
            rig.trade(allow(format!("Device {n}")).await).await.0,
            StatusCode::OK
        );
    }
    let a = allow("Next device".to_owned()).await;
    let b = allow("Another device".to_owned()).await;
    let (a, b) = tokio::join!(rig.trade(a), rig.trade(b));
    let (success, failure) = if a.0 == StatusCode::OK {
        (a, b)
    } else {
        (b, a)
    };
    assert_eq!(success.0, StatusCode::OK);
    assert_eq!(failure, refused("invalid_grant"));

    let mut request = connect_request(&pkce().1);
    let view = |request: Value| {
        let rig = &rig;
        let user_id = &user.id;
        async move {
            rig.call(user_id, "GET", &request_uri(&request), Payload::None)
                .await
        }
    };
    request["name"] = json!("Brand new");
    let (status, page) = view(request.clone()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["at_limit"], true);
    request["name"] = json!("Device 0");
    let (status, page) = view(request).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["replaces_existing"], true);
    assert_eq!(page["at_limit"], false);
    assert_eq!(
        rig.trade(allow("Device 0".to_owned()).await).await.0,
        StatusCode::OK
    );
}
