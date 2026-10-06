//! Federated sign-in end to end: OIDC, Jellyfin and Plex logins and
//! Last.fm linking run through the real app against local mock providers.
//! Also the pieces v2 import relies on: username derivation, dead sessions
//! after import, and imported app passwords.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Form, Json, Router, extract::Path, extract::Query, extract::State};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use droppedneedle::auth::federated::SessionIssuer;
use droppedneedle::auth::federated::fakes::{FakeHasher, FakeSessionIssuer, FakeUserStore};
use droppedneedle::auth::federated::jellyfin_login::emby_auth_header;
use droppedneedle::auth::federated::oidc::{
    AuthorizeParams, OidcConfig, RawClaims, authorize_url, form_encode, normalise_claims,
    pkce_challenge,
};
use droppedneedle::auth::federated::password_import::{
    LocalCredential, PasswordCheck, SESSIONS_SURVIVE_IMPORT, verify_and_maybe_rehash,
};
use droppedneedle::auth::federated::plex::plex_auth_url;
use droppedneedle::auth::federated::users::{
    FederatedProfile, derive_username, find_or_create_federated_user, username_base,
};
use droppedneedle::auth::wiring::Upstreams;
use droppedneedle::playback::forwarding::{ScrobbleCredentials as _, StoredScrobbleCredentials};
use droppedneedle::plugins::scrobble::MemoryListenBrainzLinkStore;
use droppedneedle::runtime_config::Secret;
use droppedneedle::runtime_config::secret_sections::{
    JellyfinConnection, LastFmSettings, OidcConnection, PlexConnection,
};
use ring::rand::SystemRandom;
use ring::signature::{RSA_PKCS1_SHA256, RsaKeyPair, RsaPublicKeyComponents};
use serde_json::{Value, json};

use crate::auth_e2e::{E2e, call};

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Serve `build(base_url)` on a loopback port and return the base URL. The
/// task ends with the test runtime.
async fn serve(build: impl FnOnce(String) -> Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("mock binds");
    let base = format!("http://{}", listener.local_addr().expect("mock address"));
    let router = build(base.clone());
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    base
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

/// One query parameter from a URL, form-decoded.
fn url_param(url: &str, key: &str) -> Option<String> {
    let query = url.split_once('?')?.1;
    query.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (name == key).then(|| form_decode(value))?
    })
}

fn form_decode(encoded: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(encoded.len());
    let mut chars = encoded.chars();
    while let Some(c) = chars.next() {
        match c {
            '+' => bytes.push(b' '),
            '%' => {
                let hi = chars.next()?.to_digit(16)?;
                let lo = chars.next()?.to_digit(16)?;
                bytes.push((hi * 16 + lo) as u8);
            }
            c => bytes.extend_from_slice(c.encode_utf8(&mut [0u8; 4]).as_bytes()),
        }
    }
    String::from_utf8(bytes).ok()
}

/// The caller's linked media accounts, as `service -> username`.
async fn linked_accounts(app: Router, token: &str) -> HashMap<String, String> {
    let (status, body, _) = call(
        app,
        "GET",
        "/api/v3/me/connections",
        &[("authorization", &bearer(token))],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["connections"]
        .as_array()
        .expect("connections list")
        .iter()
        .map(|link| {
            (
                link["service"].as_str().unwrap_or("").to_owned(),
                link["username"].as_str().unwrap_or("").to_owned(),
            )
        })
        .collect()
}

async fn providers(app: Router) -> Value {
    let (status, body, _) = call(app, "GET", "/api/v3/auth/providers", &[], None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

// ---------------------------------------------------------------------------
// OIDC against a mock provider with a real RSA key set
// ---------------------------------------------------------------------------

/// 2048-bit RSA test key (PKCS#1 DER). Test-only; signs the mock's tokens.
const TEST_RSA_KEY: &str = "\
MIIEpAIBAAKCAQEAsqwG5uoQ37JaUkuLm9qdf8BxTMVRb40BV1bTI4h/vwu65yLEvVp7XXNAmD2M2MRg6ymmuyNzly8LWwrj\
LwZ8Z7f+Mx0dNeepbI5bgb6PZLdVXmYYZ2A2McSEnY9VFggnwpQ1ARXH2aWxmXKW4w9Xfug5P/Ka9ncPEk+mLIc25N2Z+aDv\
iIGvsPlYLPasxllR2EOx1MbokOQDAV07CWjkk6Hj63LqYqYfdcZzdTptwrIbVp7yD04oNEESTSduKJQegqAgVy2/W74lQDmk\
6W7QFj2SVbIGPx2pEeYrvRhfxAHqSTifldKDtIkA2V3NxuqxBr66Y6CvR9YXUee9dRrQ0QIDAQABAoIBADeGrLJwhYPvepBe\
t+lcnFvKY6uXGsLPEF6jNgSx8/lcXN5d+MRb7UDSg32Mi0wGY5RRomZAEzklvqKxlH7Vxi25iX87CgvpjcaVyyxcG1YSf65R\
jj36MX1v9EK++5NYP3sB8iL/MNeb3cCxUuEHsIpZAwmzjKMRxxZKuHrYHqArE7D2+FoqeTHVW02oJotUQLl8o7zdSGtjj0qI\
GTtJ6oWkIODD5AUZ/JVpzWnMJwijN2QGsMp1AgIpUEmZ2PW/e++guI0gk8qUloP2BuQoPftvNrVikgK4SN08TpUldmrIw9UF\
SSpK6VK6RzIf5uL35IsML0S1xcz0cq4Jlobyyp0CgYEA+KJuNstpA3yIWIEECxT9ur5Sg3SR1dhD6qfcsOu9vVapVH+jF8mb\
LlNUxZyaOn/uL2BvoxG/SRwWc4gsyd8yhgCOg8uu46ISTubzEf9kBLS8GWqZA36fAPb2TmTMi4rGhe/iNNpT3jfuZDQaLDV9\
J+p+HerH8D1/UUd+2qjxs28CgYEAt/cFjlP0uER/g7zZDOp5t+KuwwousmgH7AdoIf/DpAhCbH4D4AJpWrMTInUE5Tufbrrr\
Vfbvw0ODnj4btGSgQgePQ9uCqvd+v/SwXAJMoYdsznhsiqxa3UeUxt1+OZrX4p+WENd5HulFYUzzYCveyqXQltRp9ByS72NC\
VCDsn78CgYBxuhKBu88gCiFvoivQSdy5Q38IpBRk5VRDjF61Ck+ywGYZwXw/UDdMHE/FSBl/sq6nOww1YdPGRDO8ysf52r8I\
bk7B2DP56VNIhfke/Vupj2YOliTBZXSjg1MsjozFM0gVUaF9nXQJTDod2XszR5Ak5uyjEJY9vFN1QSe0KtzLvwKBgQC3ZG1I\
fs+mZrrL5JZoNtOSikt4Kg11UxoapSOdSTCKtKUrLNDGHwFSJtT7c7amozKEG5kBwMMyYHq0ZOwPgIT2bjzXA9yWfVYBHHg3\
sR8dpDjG9+wUrk+C6poZSbNMz91JkZfzQCsBssC0iBbTF8jpMjXoNudNMLMWLFhyL8RUVwKBgQDMOmJyKPDRoMWWuDL7p6yB\
Qoa2ZE1vMWCcq2SepFdToPqO+PYLByDzeTHfVJIC78hUsjW5q/4UhHWX//jEgSqzk1dbjZenSBs6wCJXVR2NORniyvQf5DK5\
vBZYBrJDsPJ3BZfYCwwOMiKm4tvioD30TmVjtVMpouiW6BjT0AjFnA==";

const OIDC_CODE: &str = "auth-code-1";
const OIDC_CLIENT: &str = "droppedneedle";
const OIDC_SECRET: &str = "oidc-client-secret";

struct OidcMock {
    issuer: String,
    key: RsaKeyPair,
    /// Nonce the next token must carry (set by the test from the
    /// authorize URL, the way a browser would carry it).
    nonce: Mutex<String>,
    /// Form bodies the token endpoint received.
    token_requests: Mutex<Vec<HashMap<String, String>>>,
}

impl OidcMock {
    fn id_token(&self) -> String {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_secs();
        let header = json!({"alg": "RS256", "kid": "k1", "typ": "JWT"});
        let claims = json!({
            "iss": self.issuer, "aud": OIDC_CLIENT, "sub": "oidc-sub-1",
            "exp": now + 300, "iat": now,
            "nonce": self.nonce.lock().expect("nonce").clone(),
        });
        let signing_input = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header.to_string()),
            URL_SAFE_NO_PAD.encode(claims.to_string())
        );
        let mut signature = vec![0u8; self.key.public().modulus_len()];
        self.key
            .sign(
                &RSA_PKCS1_SHA256,
                &SystemRandom::new(),
                signing_input.as_bytes(),
                &mut signature,
            )
            .expect("mock signs");
        format!("{signing_input}.{}", URL_SAFE_NO_PAD.encode(signature))
    }
}

fn oidc_router(mock: Arc<OidcMock>) -> Router {
    async fn discovery(State(mock): State<Arc<OidcMock>>) -> Json<Value> {
        let base = &mock.issuer;
        Json(json!({
            "issuer": base,
            "authorization_endpoint": format!("{base}/authorize"),
            "token_endpoint": format!("{base}/token"),
            "userinfo_endpoint": format!("{base}/userinfo"),
            "jwks_uri": format!("{base}/jwks"),
        }))
    }
    async fn jwks(State(mock): State<Arc<OidcMock>>) -> Json<Value> {
        let public = RsaPublicKeyComponents::<Vec<u8>>::from(mock.key.public());
        Json(json!({"keys": [
            // A key of another type first: it must not hide the real one.
            {"kty": "oct", "kid": "other"},
            {"kty": "RSA", "kid": "k1", "alg": "RS256", "use": "sig",
             "n": URL_SAFE_NO_PAD.encode(&public.n), "e": URL_SAFE_NO_PAD.encode(&public.e)},
        ]}))
    }
    async fn token(
        State(mock): State<Arc<OidcMock>>,
        Form(form): Form<HashMap<String, String>>,
    ) -> Result<Json<Value>, StatusCode> {
        let ok = form.get("code").map(String::as_str) == Some(OIDC_CODE);
        mock.token_requests.lock().expect("log").push(form);
        if !ok {
            return Err(StatusCode::BAD_REQUEST);
        }
        Ok(Json(json!({
            "access_token": "oidc-access-1", "token_type": "Bearer",
            "refresh_token": "oidc-refresh-1", "id_token": mock.id_token(),
        })))
    }
    async fn userinfo(headers: HeaderMap) -> Result<Json<Value>, StatusCode> {
        if headers.get("authorization").and_then(|v| v.to_str().ok())
            != Some("Bearer oidc-access-1")
        {
            return Err(StatusCode::UNAUTHORIZED);
        }
        Ok(Json(json!({
            "sub": "oidc-sub-1", "email": "Jane.Doe@Example.com", "email_verified": true,
            "name": "Jane Doe", "picture": "https://idp.test/jane.png",
        })))
    }
    Router::new()
        .route("/.well-known/openid-configuration", get(discovery))
        .route("/jwks", get(jwks))
        .route("/token", post(token))
        .route("/userinfo", get(userinfo))
        .with_state(mock)
}

/// Start a login, act as the browser at the provider, and return the
/// response of the callback at `callback`.
async fn oidc_callback(
    app: &Router,
    mock: &OidcMock,
    callback: &str,
) -> (StatusCode, HeaderMap, String) {
    let (status, body, _) = call(
        app.clone(),
        "POST",
        "/api/v3/auth/oidc/authorize",
        &[],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let url = body["authorize_url"].as_str().expect("authorize url");
    assert!(
        url.starts_with(&format!("{}/authorize?", mock.issuer)),
        "{url}"
    );
    *mock.nonce.lock().expect("nonce") = url_param(url, "nonce").expect("nonce in url");
    let state = url_param(url, "state").expect("state in url");
    let (status, _, headers) = call(
        app.clone(),
        "GET",
        &format!("{callback}?code={OIDC_CODE}&state={}", form_encode(&state)),
        &[],
        None,
    )
    .await;
    (status, headers, url.to_owned())
}

#[tokio::test]
async fn oidc_sign_in_verifies_the_id_token_and_creates_the_first_admin() {
    let e2e = E2e::open("oidc").await;
    let key = RsaKeyPair::from_der(
        &base64::engine::general_purpose::STANDARD
            .decode(TEST_RSA_KEY)
            .expect("fixture decodes"),
    )
    .expect("fixture key loads");
    let mock_slot: Arc<Mutex<Option<Arc<OidcMock>>>> = Arc::new(Mutex::new(None));
    let slot = mock_slot.clone();
    let issuer = serve(move |issuer| {
        let mock = Arc::new(OidcMock {
            issuer,
            key,
            nonce: Mutex::new(String::new()),
            token_requests: Mutex::new(Vec::new()),
        });
        *slot.lock().expect("slot") = Some(mock.clone());
        oidc_router(mock)
    })
    .await;
    let mock = mock_slot.lock().expect("slot").clone().expect("mock built");
    e2e.store
        .save_secret(OidcConnection {
            enabled: true,
            issuer: issuer.clone(),
            client_id: OIDC_CLIENT.to_owned(),
            client_secret: Secret::new(OIDC_SECRET),
            scopes: "openid email profile".to_owned(),
            redirect_uri: "http://e2e.test/api/v3/auth/oidc/callback".to_owned(),
        })
        .expect("oidc settings save");
    assert_eq!(providers(e2e.router()).await["oidc"], true);

    // One app throughout: the one-time exchange codes live in its memory.
    let app = e2e.router();
    let (status, headers, url) = oidc_callback(&app, &mock, "/api/v3/auth/oidc/callback").await;
    assert_eq!(status, StatusCode::FOUND);
    let location = headers["location"].to_str().expect("ascii");
    let exchange_code = location
        .strip_prefix("/auth/callback?code=")
        .expect("relative SPA callback");

    // The code exchange proved the PKCE verifier and sent the secret.
    {
        let requests = mock.token_requests.lock().expect("log");
        let form = requests.last().expect("token request");
        let verifier = form.get("code_verifier").expect("verifier sent");
        assert_eq!(
            Some(pkce_challenge(verifier)),
            url_param(&url, "code_challenge")
        );
        assert_eq!(
            form.get("client_secret").map(String::as_str),
            Some(OIDC_SECRET)
        );
        assert_eq!(
            form.get("grant_type").map(String::as_str),
            Some("authorization_code")
        );
    }

    let (status, body, _) = call(
        app.clone(),
        "POST",
        "/api/v3/auth/oidc/exchange",
        &[],
        Some(json!({"code": exchange_code, "transport": "bearer"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let token = body["token"].as_str().expect("bearer token").to_owned();
    assert_eq!(body["role"], "admin", "first account is the admin: {body}");
    assert_eq!(body["email"], "jane.doe@example.com");
    assert_eq!(body["username"], "jane.doe");
    let (status, _, _) = call(
        app.clone(),
        "GET",
        "/api/v3/me",
        &[("authorization", &bearer(&token))],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // The exchange code is single use.
    let (status, _, _) = call(
        app.clone(),
        "POST",
        "/api/v3/auth/oidc/exchange",
        &[],
        Some(json!({"code": exchange_code, "transport": "bearer"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // A token minted for another login's nonce is refused.
    let (status, body, _) = call(
        app.clone(),
        "POST",
        "/api/v3/auth/oidc/authorize",
        &[],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let state = url_param(body["authorize_url"].as_str().expect("url"), "state").expect("state");
    *mock.nonce.lock().expect("nonce") = "nonce-from-another-login".to_owned();
    let (status, _, _) = call(
        app.clone(),
        "GET",
        &format!(
            "/api/v3/auth/oidc/callback?code={OIDC_CODE}&state={}",
            form_encode(&state)
        ),
        &[],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Providers set up for v2 redirect to its callback path; it still works.
    let (status, headers, _) = oidc_callback(&app, &mock, "/api/v1/auth/oidc/callback").await;
    assert_eq!(status, StatusCode::FOUND);
    assert!(
        headers["location"]
            .to_str()
            .is_ok_and(|location| location.starts_with("/auth/callback?code="))
    );
}

// ---------------------------------------------------------------------------
// Jellyfin against a mock server
// ---------------------------------------------------------------------------

const JELLYFIN_ADMIN_KEY: &str = "jf-admin-key";

fn jellyfin_router(base: String) -> Router {
    async fn authenticate(
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> Result<Json<Value>, StatusCode> {
        let auth = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("");
        // Jellyfin 10.11 only reads the MediaBrowser `Authorization` header.
        if !auth.starts_with("MediaBrowser Client=\"DroppedNeedle\"") {
            return Err(StatusCode::BAD_REQUEST);
        }
        if body["Username"] != "jane" || body["Pw"] != "right-password" {
            return Err(StatusCode::UNAUTHORIZED);
        }
        Ok(Json(json!({
            "User": {"Id": "jf-jane", "Name": "Jane", "HasPrimaryImage": true},
            "AccessToken": "jf-token-jane",
        })))
    }
    async fn users(headers: HeaderMap) -> Result<Json<Value>, StatusCode> {
        let expected = format!("MediaBrowser Token=\"{JELLYFIN_ADMIN_KEY}\"");
        if headers.get("authorization").and_then(|v| v.to_str().ok()) != Some(expected.as_str()) {
            return Err(StatusCode::UNAUTHORIZED);
        }
        Ok(Json(json!([
            {"Id": "jf-jane", "Name": "Jane"},
            {"Id": "jf-bob", "Name": "Bob"},
            {"Name": "No id, skipped"},
        ])))
    }
    let _ = base;
    Router::new()
        .route("/Users/AuthenticateByName", post(authenticate))
        .route("/Users", get(users))
}

#[tokio::test]
async fn jellyfin_sign_in_links_the_media_account_and_feeds_the_import() {
    let e2e = E2e::open("jellyfin").await;
    let server = serve(jellyfin_router).await;
    let mut settings = JellyfinConnection {
        jellyfin_url: server,
        api_key: Secret::new(JELLYFIN_ADMIN_KEY),
        user_id: String::new(),
        enabled: true,
        login_enabled: true,
    };
    e2e.store
        .save_secret(settings.clone())
        .expect("jellyfin settings save");
    assert_eq!(providers(e2e.router()).await["jellyfin"], true);

    let login = |password: &'static str| {
        let app = e2e.router();
        async move {
            call(
                app,
                "POST",
                "/api/v3/auth/jellyfin/login",
                &[],
                Some(json!({"username": "jane", "password": password, "transport": "bearer"})),
            )
            .await
        }
    };
    let (status, _, _) = login("wrong-password").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, body, _) = login("right-password").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["role"], "admin");
    let token = body["token"].as_str().expect("bearer token").to_owned();
    // Signing in linked the Jellyfin account for playback (v2 auto-link).
    let links = linked_accounts(e2e.router(), &token).await;
    assert_eq!(links.get("jellyfin").map(String::as_str), Some("Jane"));

    // The admin import lists the server's users, marks the signed-in one,
    // and imports the rest.
    let (status, body, _) = call(
        e2e.router(),
        "GET",
        "/api/v3/admin/import/jellyfin",
        &[("authorization", &bearer(&token))],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let candidates: HashMap<String, bool> = body["candidates"]
        .as_array()
        .expect("candidates")
        .iter()
        .map(|c| {
            (
                c["provider_uid"].as_str().unwrap_or("").to_owned(),
                c["already_imported"].as_bool().unwrap_or(false),
            )
        })
        .collect();
    assert_eq!(
        candidates,
        HashMap::from([("jf-jane".to_owned(), true), ("jf-bob".to_owned(), false)])
    );
    let (status, body, _) = call(
        e2e.router(),
        "POST",
        "/api/v3/admin/import",
        &[("authorization", &bearer(&token))],
        Some(json!({"provider": "jellyfin", "provider_uids": ["jf-bob"]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["total_imported"], 1);

    // Switching Jellyfin login off closes the route, not just the tab.
    settings.login_enabled = false;
    e2e.store.save_secret(settings).expect("settings save");
    assert_eq!(providers(e2e.router()).await["jellyfin"], false);
    let (status, _, _) = login("right-password").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

// ---------------------------------------------------------------------------
// Plex against a mock plex.tv plus a mock media server
// ---------------------------------------------------------------------------

const PLEX_ADMIN_TOKEN: &str = "plex-admin-token";
const PLEX_MACHINE: &str = "machine-1";

fn plex_token(headers: &HeaderMap) -> &str {
    headers
        .get("x-plex-token")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
}

fn plex_router(_base: String) -> Router {
    async fn create_pin(headers: HeaderMap) -> (StatusCode, Json<Value>) {
        assert!(headers.contains_key("x-plex-client-identifier"));
        (StatusCode::CREATED, Json(json!({"id": 42, "code": "ABCD"})))
    }
    async fn poll_pin(Path(id): Path<i64>) -> Json<Value> {
        // 42 is the member, 43 an outsider, anything else still pending.
        Json(match id {
            42 => json!({"id": 42, "authToken": "acct-member"}),
            43 => json!({"id": 43, "authToken": "acct-outsider"}),
            _ => json!({"id": id, "authToken": null}),
        })
    }
    async fn user(headers: HeaderMap) -> Result<Json<Value>, StatusCode> {
        match plex_token(&headers) {
            "acct-member" => Ok(Json(json!({
                "uuid": "plex-uuid-1", "email": "jane@example.com",
                "friendlyName": "Jane P", "thumb": "https://plex.test/jane.png",
            }))),
            "acct-outsider" => Ok(Json(json!({"uuid": "plex-uuid-9", "username": "stranger"}))),
            _ => Err(StatusCode::UNAUTHORIZED),
        }
    }
    async fn resources(headers: HeaderMap) -> Json<Value> {
        Json(match plex_token(&headers) {
            "acct-member" => json!([
                {"clientIdentifier": "phone-1", "provides": "player"},
                {"clientIdentifier": PLEX_MACHINE, "provides": "server", "accessToken": "srv-token-1"},
            ]),
            _ => json!([]),
        })
    }
    async fn identity(headers: HeaderMap) -> Result<Json<Value>, StatusCode> {
        if plex_token(&headers) != PLEX_ADMIN_TOKEN {
            return Err(StatusCode::UNAUTHORIZED);
        }
        Ok(Json(
            json!({"MediaContainer": {"machineIdentifier": PLEX_MACHINE}}),
        ))
    }
    async fn home_users(headers: HeaderMap) -> Result<Json<Value>, StatusCode> {
        if plex_token(&headers) != PLEX_ADMIN_TOKEN {
            return Err(StatusCode::UNAUTHORIZED);
        }
        Ok(Json(json!({"users": [
            {"uuid": "plex-uuid-1", "title": "Jane P"},
            {"uuid": "plex-uuid-3", "title": "Kid"},
        ]})))
    }
    async fn friends(headers: HeaderMap) -> Result<Json<Value>, StatusCode> {
        if plex_token(&headers) != PLEX_ADMIN_TOKEN {
            return Err(StatusCode::UNAUTHORIZED);
        }
        Ok(Json(
            json!([{"uuid": "plex-uuid-4", "username": "friend", "email": "f@example.com"}]),
        ))
    }
    Router::new()
        .route("/pins", post(create_pin))
        .route("/pins/{id}", get(poll_pin))
        .route("/user", get(user))
        .route("/resources", get(resources))
        .route("/identity", get(identity))
        .route("/home/users", get(home_users))
        .route("/friends", get(friends))
}

#[tokio::test]
async fn plex_pin_sign_in_gates_membership_links_and_imports() {
    let mock = serve(plex_router).await;
    let e2e = E2e::open("plex").await.with_upstreams(Upstreams {
        plex_tv: mock.clone(),
        ..Upstreams::default()
    });
    let mut settings = PlexConnection {
        plex_url: mock,
        plex_token: Secret::new(PLEX_ADMIN_TOKEN),
        enabled: true,
        login_enabled: true,
        music_library_ids: Vec::new(),
        scrobble_to_plex: true,
    };
    e2e.store
        .save_secret(settings.clone())
        .expect("plex settings save");
    assert_eq!(providers(e2e.router()).await["plex"], true);

    let (status, body, _) = call(e2e.router(), "POST", "/api/v3/auth/plex/start", &[], None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["pin_id"], 42);
    assert!(
        body["authorize_url"]
            .as_str()
            .is_some_and(|url| url.starts_with("https://app.plex.tv/auth#?clientID=")
                && url.contains("&code=ABCD")),
        "{body}"
    );

    let poll = |pin: i64| {
        let app = e2e.router();
        async move {
            call(
                app,
                "POST",
                "/api/v3/auth/plex/poll/login",
                &[],
                Some(json!({"pin_id": pin, "transport": "bearer"})),
            )
            .await
        }
    };
    let (status, body, _) = poll(44).await;
    assert_eq!(
        (status, body["completed"].clone()),
        (StatusCode::OK, json!(false))
    );
    // An account without access to the configured server is refused.
    let (status, _, _) = poll(43).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body, _) = poll(42).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["completed"], true);
    assert_eq!(body["user"]["role"], "admin");
    // plex.tv does not vouch for the address, so it is not stored.
    assert_eq!(body["user"]["email"], Value::Null);
    let token = body["token"].as_str().expect("bearer token").to_owned();
    let links = linked_accounts(e2e.router(), &token).await;
    assert_eq!(links.get("plex").map(String::as_str), Some("Jane P"));

    // The link flow stores the account server-side and returns only the name.
    let (status, _, _) = call(
        e2e.router(),
        "POST",
        "/api/v3/auth/plex/start?purpose=link",
        &[],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body, _) = call(
        e2e.router(),
        "POST",
        "/api/v3/auth/plex/poll/link",
        &[("authorization", &bearer(&token))],
        Some(json!({"pin_id": 42})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({"completed": true, "username": "Jane P"}));

    let (status, body, _) = call(
        e2e.router(),
        "GET",
        "/api/v3/admin/import/plex",
        &[("authorization", &bearer(&token))],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let mut uids: Vec<(String, bool)> = body["candidates"]
        .as_array()
        .expect("candidates")
        .iter()
        .map(|c| {
            (
                c["provider_uid"].as_str().unwrap_or("").to_owned(),
                c["already_imported"].as_bool().unwrap_or(false),
            )
        })
        .collect();
    uids.sort();
    assert_eq!(
        uids,
        vec![
            ("plex-uuid-1".to_owned(), true),
            ("plex-uuid-3".to_owned(), false),
            ("plex-uuid-4".to_owned(), false),
        ]
    );

    // With Plex login off, starting a login is refused outright.
    settings.login_enabled = false;
    e2e.store.save_secret(settings).expect("settings save");
    let (status, _, _) = call(e2e.router(), "POST", "/api/v3/auth/plex/start", &[], None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let (status, _, _) = poll(42).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

// ---------------------------------------------------------------------------
// Last.fm account linking against a mock web service
// ---------------------------------------------------------------------------

const LASTFM_KEY: &str = "lfm-key";
const LASTFM_SECRET: &str = "lfm-secret";
const LASTFM_SESSION_KEY: &str = "lfm-session-key";

fn lastfm_router(_base: String) -> Router {
    async fn api(Query(params): Query<HashMap<String, String>>) -> (StatusCode, Json<Value>) {
        let mut signed: Vec<(&String, &String)> = params
            .iter()
            .filter(|(key, _)| *key != "format" && *key != "api_sig")
            .collect();
        signed.sort();
        let mut text: String = signed.iter().map(|(k, v)| format!("{k}{v}")).collect();
        text.push_str(LASTFM_SECRET);
        let expected = format!("{:x}", md5::compute(text.as_bytes()));
        if params.get("api_sig") != Some(&expected)
            || params.get("api_key").map(String::as_str) != Some(LASTFM_KEY)
        {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({"error": 13, "message": "Invalid method signature supplied"})),
            );
        }
        match (
            params.get("method").map(String::as_str),
            params.get("token").map(String::as_str),
        ) {
            (Some("auth.getToken"), _) => (StatusCode::OK, Json(json!({"token": "lfm-token"}))),
            (Some("auth.getSession"), Some("lfm-token")) => (
                StatusCode::OK,
                Json(
                    json!({"session": {"name": "jane_lfm", "key": LASTFM_SESSION_KEY, "subscriber": 0}}),
                ),
            ),
            _ => (
                StatusCode::FORBIDDEN,
                Json(json!({"error": 14, "message": "Unauthorized Token"})),
            ),
        }
    }
    Router::new().route("/2.0/", get(api))
}

#[tokio::test]
async fn lastfm_links_with_the_instance_key_and_binds_tokens_to_the_user() {
    let mock = serve(lastfm_router).await;
    let e2e = E2e::open("lastfm").await.with_upstreams(Upstreams {
        lastfm: format!("{mock}/2.0/"),
        ..Upstreams::default()
    });
    // v2 model: the admin saves one app key pair for the whole instance.
    e2e.store
        .save_secret(LastFmSettings {
            enabled: true,
            api_key: Secret::new(LASTFM_KEY),
            shared_secret: Secret::new(LASTFM_SECRET),
        })
        .expect("lastfm settings save");
    // One app throughout: pending sign-in tokens live in its memory.
    let app = e2e.router();
    let (status, body, _) = call(
        app.clone(),
        "POST",
        "/api/v3/auth/setup",
        &[],
        Some(json!({
            "username": "jane", "password": "correct horse battery staple",
            "display_name": "Jane", "transport": "bearer",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let jane_id = body["user"]["id"].as_str().expect("user id").to_owned();
    let jane = bearer(body["token"].as_str().expect("bearer token"));
    let (status, body, _) = call(
        app.clone(),
        "POST",
        "/api/v3/admin/users",
        &[("authorization", &jane)],
        Some(json!({"username": "bob", "password": "another long passphrase"})),
    )
    .await;
    assert!(status.is_success(), "{status}: {body}");
    let (status, body, _) = call(
        app.clone(),
        "POST",
        "/api/v3/auth/login",
        &[],
        Some(json!({"username": "bob", "password": "another long passphrase", "transport": "bearer"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let bob = bearer(body["token"].as_str().expect("bearer token"));

    // Jane links without any key of her own, as in v2.
    let (status, body, _) = call(
        app.clone(),
        "POST",
        "/api/v3/me/connections/lastfm/token",
        &[("authorization", &jane)],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["auth_url"],
        "https://www.last.fm/api/auth/?api_key=lfm-key&token=lfm-token"
    );
    let session = |who: &str, token: &str| {
        let (app, who, token) = (app.clone(), who.to_owned(), token.to_owned());
        async move {
            call(
                app,
                "POST",
                "/api/v3/me/connections/lastfm/session",
                &[("authorization", &who)],
                Some(json!({ "token": token })),
            )
            .await
        }
    };
    // Bob cannot claim the token Jane asked for.
    let (status, _, _) = session(&bob, "lfm-token").await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, body, _) = session(&jane, "lfm-token").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["username"], "jane_lfm");

    let stored: Vec<String> =
        sqlx::query_scalar("SELECT connection_data FROM user_connections WHERE service = 'lastfm'")
            .fetch_all(e2e.pool())
            .await
            .expect("rows read");
    assert_eq!(stored.len(), 1);
    assert!(
        !stored[0].contains(LASTFM_SESSION_KEY),
        "the session key is sealed at rest"
    );

    // Scrobbling signs with the instance pair, since Jane has none.
    let users = e2e.users();
    let credentials = StoredScrobbleCredentials::new(
        Arc::new(MemoryListenBrainzLinkStore::new(users.crypto.clone())),
        users.lastfm.clone(),
        users.lastfm_switch.clone(),
        users.crypto.clone(),
    );
    let signing = credentials
        .lastfm_session(&jane_id)
        .await
        .expect("jane scrobbles");
    assert_eq!(
        (
            signing.api_key.as_str(),
            signing.shared_secret.as_str(),
            signing.session_key.as_str()
        ),
        (LASTFM_KEY, LASTFM_SECRET, LASTFM_SESSION_KEY)
    );
}

// ---------------------------------------------------------------------------
// Wire goldens and v2-import building blocks
// ---------------------------------------------------------------------------

#[test]
fn authorize_url_golden_pins_param_order_and_encoding() {
    let config = OidcConfig {
        enabled: true,
        issuer: "https://idp.test".to_owned(),
        client_id: "droppedneedle".to_owned(),
        client_secret: None,
        redirect_uri: "https://music.test/api/v3/auth/oidc/callback".to_owned(),
        scopes: "openid profile email".to_owned(),
    };
    let url = authorize_url(
        "https://idp.test/authorize",
        &config,
        &AuthorizeParams {
            state: "state 1",
            challenge: "ch+llenge",
            nonce: "n/1",
        },
    );
    assert_eq!(
        url,
        "https://idp.test/authorize?response_type=code&client_id=droppedneedle&redirect_uri=https%3A%2F%2Fmusic.test%2Fapi%2Fv3%2Fauth%2Foidc%2Fcallback&scope=openid+profile+email&state=state+1&code_challenge=ch%2Bllenge&code_challenge_method=S256&nonce=n%2F1"
    );
}

#[test]
fn claims_normalisation_keeps_v2_fallback_chain() {
    let base = RawClaims {
        sub: Some("s".to_owned()),
        ..RawClaims::default()
    };
    let name = |raw: RawClaims| normalise_claims(&raw).expect("claims").name;
    assert_eq!(name(base.clone()), "OIDC User");
    assert_eq!(
        name(RawClaims {
            email: Some("Local.Part@x.test".to_owned()),
            ..base.clone()
        }),
        "Local.Part"
    );
    assert_eq!(
        name(RawClaims {
            nickname: Some("nick".to_owned()),
            preferred_username: Some("pref".to_owned()),
            ..base.clone()
        }),
        "pref"
    );
    assert!(
        normalise_claims(&RawClaims::default()).is_err(),
        "sub is required"
    );
}

#[test]
fn emby_auth_header_bytes_are_pinned() {
    assert_eq!(
        emby_auth_header("dev-1"),
        "MediaBrowser Client=\"DroppedNeedle\", Device=\"DroppedNeedle\", DeviceId=\"dev-1\", Version=\"1.4.0\""
    );
}

#[test]
fn plex_auth_url_golden_is_shared_by_all_flows() {
    assert_eq!(
        plex_auth_url("client-1", "ABCD"),
        "https://app.plex.tv/auth#?clientID=client-1&code=ABCD&context%5Bdevice%5D%5Bproduct%5D=DroppedNeedle"
    );
}

#[tokio::test]
async fn federated_username_derivation_prefers_email_local_part() {
    // Every provider runs the email-first rule, so jane.doe@ beats any
    // display name.
    assert_eq!(
        username_base(Some("jane.doe@example.com"), "Jane Q. Elsewhere"),
        "jane.doe"
    );
    let users = FakeUserStore::new();
    assert_eq!(
        derive_username(&users, Some("jane.doe@example.com"), "Jane Q. Elsewhere")
            .await
            .unwrap(),
        ("jane.doe".to_owned(), "jane.doe".to_owned())
    );
}

#[tokio::test]
async fn unverified_email_never_claims_an_existing_account() {
    let users = FakeUserStore::new();
    let profile = |uid: &str, verified: bool| FederatedProfile {
        provider_uid: uid.to_owned(),
        display_name: "Sam".to_owned(),
        email: Some("sam@example.com".to_owned()),
        email_verified: verified,
        avatar_url: None,
        token_json: "{}".to_owned(),
    };
    let owner = find_or_create_federated_user(&users, "oidc", &profile("u1", true))
        .await
        .unwrap();
    let intruder = find_or_create_federated_user(&users, "plex", &profile("u2", false))
        .await
        .unwrap();
    assert_ne!(owner.id, intruder.id);
    assert_eq!(intruder.email, None, "an unverified address is not stored");
    assert_eq!(
        (owner.username.as_str(), intruder.username.as_str()),
        ("sam", "sam-2")
    );
    assert_eq!(intruder.role, "user");
    let linked = find_or_create_federated_user(&users, "jellyfin", &profile("u3", true))
        .await
        .unwrap();
    assert_eq!(linked.id, owner.id, "a verified address links");
}

const _: () = assert!(!SESSIONS_SURVIVE_IMPORT);

#[tokio::test]
async fn old_sessions_die_on_import_while_new_logins_work() {
    let sessions = FakeSessionIssuer::new();
    let live = sessions.issue_session("user-1", None).await.unwrap();
    assert!(sessions.is_live(&live));

    sessions.revoke_all();
    assert!(!sessions.is_live(&live));

    let hasher = FakeHasher::new();
    let stored = LocalCredential::imported_bcrypt(FakeHasher::bcrypt_fixture("pw"));
    assert!(matches!(
        verify_and_maybe_rehash(&hasher, "pw", Some(&stored)),
        PasswordCheck::Valid { .. }
    ));
    let relogin = sessions.issue_session("user-1", None).await.unwrap();
    assert!(sessions.is_live(&relogin));
}

#[tokio::test]
async fn imported_app_passwords_verify_on_both_compat_paths() {
    use droppedneedle::auth::compat_auth::fakes::FakeCompatPasswords;
    use droppedneedle::auth::compat_auth::jellyfin;
    use droppedneedle::auth::compat_auth::subsonic;

    let passwords = FakeCompatPasswords::new();
    passwords.add_user(
        "user-9",
        "imma",
        "Imma",
        "user",
        "account-password",
        &["imported-secret-1"],
    );

    let salt = "salty";
    let token = subsonic::md5_hex(&format!("imported-secret-1{salt}"));
    let params = subsonic::SubsonicParams::new(vec![
        ("u", "imma"),
        ("t", &token),
        ("s", salt),
        ("c", "symfonium"),
    ]);
    let principal = subsonic::authenticate(&passwords, &params).await.unwrap();
    assert_eq!(principal.user_id, "user-9");

    let hexed = format!("enc:{}", subsonic::hex_encode(b"imported-secret-1"));
    let params = subsonic::SubsonicParams::new(vec![("u", "Imma"), ("p", &hexed)]);
    let principal = subsonic::authenticate(&passwords, &params).await.unwrap();
    assert_eq!(principal.user_id, "user-9");

    let user = jellyfin::resolve_token(&passwords, Some("imported-secret-1"))
        .await
        .unwrap();
    assert_eq!(user.id, "user-9");
}
