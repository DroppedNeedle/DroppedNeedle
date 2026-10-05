//! Federated logins: OIDC, Jellyfin, and the unified Plex journey against
//! scripted identity providers (no network), plus what v2 import must keep
//! working: dead sessions after import and imported app passwords.

use droppedneedle::auth::federated::fakes::{
    FakeHasher, FakeJellyfinIdp, FakeJellyfinLink, FakeOidcExchanges, FakeOidcIdp, FakeOidcStates,
    FakePlexLink, FakePlexPinClient, FakeSessionIssuer, FakeUserStore,
};
use droppedneedle::auth::federated::jellyfin_login::{
    JellyfinLogin, JellyfinProfile, NoopJellyfinLink, emby_auth_header, jellyfin_token_json,
};
use droppedneedle::auth::federated::oidc::{
    OidcLogin, OidcTokens, RawClaims, authorize_url, form_encode, normalise_claims,
    sealed_token_json,
};
use droppedneedle::auth::federated::password_import::{
    LocalCredential, PasswordCheck, SESSIONS_SURVIVE_IMPORT, verify_and_maybe_rehash,
};
use droppedneedle::auth::federated::plex::{
    PlexAccount, PlexJourney, PlexPoll, plex_auth_url, plex_token_json,
};
use droppedneedle::auth::federated::users::{
    FederatedUserStore, derive_username, find_or_create_federated_user, username_base,
};
use droppedneedle::auth::federated::{FederatedError, SessionIssuer, json_string};

fn oidc_claims() -> RawClaims {
    RawClaims {
        sub: Some("oidc-sub-1".to_owned()),
        email: Some("Jane.Doe@Example.com".to_owned()),
        email_verified: Some(true),
        name: Some("Jane Doe".to_owned()),
        preferred_username: None,
        nickname: None,
        picture: Some("https://idp.test/pic.png".to_owned()),
        avatar: None,
    }
}

fn oidc_tokens() -> OidcTokens {
    OidcTokens {
        access_token: "oidc-access-1".to_owned(),
        refresh_token: "oidc-refresh-1".to_owned(),
        id_token: "oidc-id-1".to_owned(),
    }
}

fn oidc_service(
    idp: FakeOidcIdp,
) -> (
    OidcLogin<FakeUserStore, FakeOidcIdp, FakeOidcStates, FakeOidcExchanges, FakeSessionIssuer>,
    FakeUserStore,
    FakeSessionIssuer,
    FakeOidcExchanges,
) {
    let users = FakeUserStore::new();
    let sessions = FakeSessionIssuer::new();
    let exchanges = FakeOidcExchanges::new();
    let service = OidcLogin::new(
        users.clone(),
        idp,
        FakeOidcStates::new(),
        exchanges.clone(),
        sessions.clone(),
    );
    (service, users, sessions, exchanges)
}

fn url_param(url: &str, key: &str) -> Option<String> {
    let needle = format!("{key}=");
    let start = url.find(needle.as_str())? + needle.len();
    let rest = &url[start..];
    let encoded = rest.split('&').next().unwrap_or("");
    form_decode(encoded)
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

#[test]
fn authorize_url_golden_pins_param_order_and_encoding() {
    let config = FakeOidcIdp::config();
    let url = authorize_url(
        "https://idp.test/authorize",
        &config,
        "state 1",
        "ch+llenge",
    );
    assert_eq!(
        url,
        "https://idp.test/authorize?response_type=code&client_id=droppedneedle&redirect_uri=https%3A%2F%2Fmusic.test%2Fapi%2Fv3%2Fauth%2Foidc%2Fcallback&scope=openid+profile+email&state=state+1&code_challenge=ch%2Bllenge&code_challenge_method=S256"
    );
    assert_eq!(form_encode("a b"), "a+b");
    assert_eq!(form_encode("state/+="), "state%2F%2B%3D");
    assert_eq!(form_encode("Zz-_.~09"), "Zz-_.~09");
}

#[tokio::test]
async fn oidc_callback_creates_first_user_as_admin_and_mints_exchange_code() {
    let idp = FakeOidcIdp::new(FakeOidcIdp::discovery_doc(), oidc_tokens(), oidc_claims());
    let states = FakeOidcStates::new();
    let (_, users, sessions, _) = oidc_service(idp.clone());
    let service = OidcLogin::new(
        users.clone(),
        idp,
        states.clone(),
        FakeOidcExchanges::new(),
        sessions.clone(),
    );
    let config = FakeOidcIdp::config();
    let url = service.build_authorize_url(&config).await.unwrap();
    let state = url_param(&url, "state").unwrap().to_owned();

    let exchange = service
        .handle_callback(&config, "auth-code-1", &state, Some("agent"))
        .await
        .unwrap();
    assert_eq!(exchange.len(), 44);

    let (user, token) = service.exchange_code(&exchange).await.unwrap();
    assert_eq!(user.role, "admin");
    assert_eq!(user.display_name, "Jane Doe");
    assert_eq!(user.email.as_deref(), Some("jane.doe@example.com"));
    assert_eq!(user.username, "jane.doe");
    assert!(sessions.is_live(&token));

    let binding = users
        .binding_id("oidc", "oidc-sub-1")
        .expect("binding created");
    let sealed = users.provider_tokens(&binding).expect("tokens stored");
    assert_eq!(sealed, sealed_token_json("oidc-access-1", "oidc-refresh-1"));

    assert!(matches!(
        service.exchange_code(&exchange).await,
        Err(FederatedError::Authentication(_))
    ));
}

#[tokio::test]
async fn oidc_rejects_bad_state_and_missing_sub() {
    let (service, _, _, _) = oidc_service(FakeOidcIdp::new(
        FakeOidcIdp::discovery_doc(),
        oidc_tokens(),
        oidc_claims(),
    ));
    let config = FakeOidcIdp::config();
    let denied = service
        .handle_callback(&config, "code", "no-such-state", None)
        .await;
    assert_eq!(
        denied,
        Err(FederatedError::Authentication(
            "Invalid or expired OIDC state".to_owned()
        ))
    );

    let mut claims = oidc_claims();
    claims.sub = None;
    let idp = FakeOidcIdp::new(FakeOidcIdp::discovery_doc(), oidc_tokens(), claims);
    assert_eq!(
        normalise_claims(&RawClaims::default()),
        Err(FederatedError::Authentication(
            "OIDC token missing 'sub' claim".to_owned()
        ))
    );
    let states = FakeOidcStates::new();
    let users = FakeUserStore::new();
    let retry = OidcLogin::new(
        users,
        idp,
        states.clone(),
        FakeOidcExchanges::new(),
        FakeSessionIssuer::new(),
    );
    let url = retry.build_authorize_url(&config).await.unwrap();
    let state = url_param(&url, "state").unwrap().to_owned();
    assert!(matches!(
        retry.handle_callback(&config, "code", &state, None).await,
        Err(FederatedError::Authentication(_))
    ));
}

#[tokio::test]
async fn oidc_links_email_and_refreshes_tokens_on_relogin() {
    let users = FakeUserStore::new();
    let pre = users
        .create_user(droppedneedle::auth::federated::users::NewFederatedUser {
            display_name: "Jane".to_owned(),
            email: Some("jane.doe@example.com".to_owned()),
            avatar_url: None,
            username: "jane".to_owned(),
            username_display: "jane".to_owned(),
        })
        .await
        .unwrap();
    let idp = FakeOidcIdp::new(FakeOidcIdp::discovery_doc(), oidc_tokens(), oidc_claims());
    let states = FakeOidcStates::new();
    let exchanges = FakeOidcExchanges::new();
    let sessions = FakeSessionIssuer::new();
    let service = OidcLogin::new(users.clone(), idp, states.clone(), exchanges, sessions);
    let config = FakeOidcIdp::config();

    let url = service.build_authorize_url(&config).await.unwrap();
    let state = url_param(&url, "state").unwrap().to_owned();
    let exchange = service
        .handle_callback(&config, "code-1", &state, None)
        .await
        .unwrap();
    let (linked, _) = service.exchange_code(&exchange).await.unwrap();
    assert_eq!(linked.id, pre.id);

    let url = service.build_authorize_url(&config).await.unwrap();
    let state = url_param(&url, "state").unwrap().to_owned();
    let exchange = service
        .handle_callback(&config, "code-2", &state, None)
        .await
        .unwrap();
    let (again, _) = service.exchange_code(&exchange).await.unwrap();
    assert_eq!(again.id, pre.id);
    let binding = users.binding_id("oidc", "oidc-sub-1").unwrap();
    assert!(
        users
            .provider_tokens(&binding)
            .unwrap()
            .contains("oidc-access-1")
    );
}

#[tokio::test]
async fn oidc_token_request_carries_secret_and_verifier() {
    let idp = FakeOidcIdp::new(FakeOidcIdp::discovery_doc(), oidc_tokens(), oidc_claims());
    let states = FakeOidcStates::new();
    let service = OidcLogin::new(
        FakeUserStore::new(),
        idp.clone(),
        states.clone(),
        FakeOidcExchanges::new(),
        FakeSessionIssuer::new(),
    );
    let config = FakeOidcIdp::config();
    let url = service.build_authorize_url(&config).await.unwrap();
    let state = url_param(&url, "state").unwrap().to_owned();
    service
        .handle_callback(&config, "code-9", &state, None)
        .await
        .unwrap();
    let request = idp
        .last_token_request
        .lock()
        .unwrap()
        .clone()
        .expect("token call made");
    assert_eq!(request.token_endpoint, "https://idp.test/token");
    assert_eq!(request.code, "code-9");
    assert_eq!(request.client_secret.as_deref(), Some("secret"));
    assert!(
        request
            .code_verifier
            .as_deref()
            .is_some_and(|v| v.len() == 43)
    );
    assert_eq!(
        idp.calls.lock().unwrap().clone(),
        vec!["discover", "discover", "exchange_code", "fetch_claims"]
    );
}

#[test]
fn claims_normalisation_keeps_v2_fallback_chain() {
    let full = normalise_claims(&RawClaims {
        sub: Some("s".to_owned()),
        email: Some("  A@X.test ".to_owned()),
        email_verified: None,
        name: None,
        preferred_username: Some("".to_owned()),
        nickname: Some("nick".to_owned()),
        picture: None,
        avatar: Some("av".to_owned()),
    })
    .unwrap();
    assert_eq!(full.email.as_deref(), Some("a@x.test"));
    assert_eq!(full.name, "nick");
    assert_eq!(full.thumb.as_deref(), Some("av"));

    let local = normalise_claims(&RawClaims {
        sub: Some("s".to_owned()),
        email: Some("local@x.test".to_owned()),
        ..RawClaims::default()
    })
    .unwrap();
    assert_eq!(local.name, "local");

    let anon = normalise_claims(&RawClaims {
        sub: Some("s".to_owned()),
        ..RawClaims::default()
    })
    .unwrap();
    assert_eq!(anon.name, "OIDC User");
    assert_eq!(anon.email, None);
    assert_eq!(json_string("a\"b\\c"), "\"a\\\"b\\\\c\"");
}

#[test]
fn emby_auth_header_bytes_are_pinned() {
    assert_eq!(
        emby_auth_header("client-9"),
        "MediaBrowser Client=\"DroppedNeedle\", Device=\"DroppedNeedle\", DeviceId=\"client-9\", Version=\"1.4.0\""
    );
    assert_eq!(jellyfin_token_json("tok"), "{\"access_token\":\"tok\"}");
    assert_eq!(plex_token_json("tok"), "{\"auth_token\":\"tok\"}");
}

fn jellyfin_profile() -> JellyfinProfile {
    JellyfinProfile {
        jellyfin_user_id: "jf-1".to_owned(),
        username: "JF User".to_owned(),
        access_token: "jf-token-1".to_owned(),
        avatar_url: Some("https://jf.test/Users/jf-1/Images/Primary".to_owned()),
    }
}

#[tokio::test]
async fn jellyfin_login_imports_user_links_connection_and_mints_session() {
    let mut idp = FakeJellyfinIdp::new(true);
    idp.accept("jfuser", "jfpass", jellyfin_profile());
    let users = FakeUserStore::new();
    let links = FakeJellyfinLink::new();
    let sessions = FakeSessionIssuer::new();
    let service = JellyfinLogin::new(users.clone(), idp, links.clone(), sessions.clone());

    let (user, token) = service
        .login("jfuser", "jfpass", Some("agent"))
        .await
        .unwrap();
    assert_eq!(user.role, "admin");
    assert_eq!(user.display_name, "JF User");
    assert_eq!(user.username, "jf-user");
    assert_eq!(user.email, None);
    assert!(sessions.is_live(&token));
    assert_eq!(
        links.linked.lock().unwrap().clone(),
        vec![(user.id.clone(), "jf-1".to_owned())]
    );
    let binding = users.binding_id("jellyfin", "jf-1").unwrap();
    assert!(
        users
            .provider_tokens(&binding)
            .unwrap()
            .contains("jf-token-1")
    );

    let (again, _) = service.login("jfuser", "jfpass", None).await.unwrap();
    assert_eq!(again.id, user.id);
}

#[tokio::test]
async fn jellyfin_login_rejects_bad_credentials_and_unconfigured_server() {
    let idp = FakeJellyfinIdp::new(true);
    let service = JellyfinLogin::new(
        FakeUserStore::new(),
        idp,
        NoopJellyfinLink,
        FakeSessionIssuer::new(),
    );
    assert_eq!(
        service.login("nobody", "nope", None).await,
        Err(FederatedError::Authentication(
            "Invalid Jellyfin username or password".to_owned()
        ))
    );

    let idp = FakeJellyfinIdp::new(false);
    let sessions = FakeSessionIssuer::new();
    let service = JellyfinLogin::new(
        FakeUserStore::new(),
        idp,
        NoopJellyfinLink,
        sessions.clone(),
    );
    assert_eq!(
        service.login("u", "p", None).await,
        Err(FederatedError::Authentication(
            "Jellyfin is not configured on this server".to_owned()
        ))
    );
    assert_eq!(sessions.issued(), 0);
}

#[test]
fn plex_auth_url_golden_is_shared_by_all_flows() {
    assert_eq!(
        plex_auth_url("cid-1", "pin-2"),
        "https://app.plex.tv/auth#?clientID=cid-1&code=pin-2&context%5Bdevice%5D%5Bproduct%5D=DroppedNeedle"
    );
}

fn plex_account() -> PlexAccount {
    PlexAccount {
        uuid: "plex-uuid-1".to_owned(),
        email: "plex@example.com".to_owned(),
        display_name: "Plex Person".to_owned(),
        thumb: None,
    }
}

fn plex_client() -> FakePlexPinClient {
    let mut client = FakePlexPinClient::new();
    client
        .accounts
        .insert("plex-token-1".to_owned(), plex_account());
    client.machine_id = Some("machine-1".to_owned());
    client
        .server_ids
        .insert("plex-token-1".to_owned(), vec!["machine-1".to_owned()]);
    client.server_tokens.insert(
        ("plex-token-1".to_owned(), "machine-1".to_owned()),
        "server-token-1".to_owned(),
    );
    client
}

#[tokio::test]
async fn plex_login_completes_with_membership_gate_session_and_link() {
    let client = plex_client();
    let users = FakeUserStore::new();
    let links = FakePlexLink::new();
    let sessions = FakeSessionIssuer::new();
    let journey = PlexJourney::new(
        users.clone(),
        client.clone(),
        links.clone(),
        sessions.clone(),
    );
    let (pin_id, _) = journey.start().await.unwrap();
    client.authorize_pin(pin_id, "plex-token-1");

    let PlexPoll::Complete((user, token)) =
        journey.poll_login(pin_id, Some("agent")).await.unwrap()
    else {
        panic!("expected a completed login");
    };
    assert_eq!(user.display_name, "Plex Person");
    // Plex does not vouch for the address, so it is not stored.
    assert_eq!(user.email, None);
    assert!(sessions.is_live(&token));
    assert_eq!(links.linked.lock().unwrap().len(), 1);
    let binding = users.binding_id("plex", "plex-uuid-1").unwrap();
    assert!(
        users
            .provider_tokens(&binding)
            .unwrap()
            .contains("plex-token-1")
    );
    assert!(
        client
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call == "server_access_token:machine-1")
    );
}

#[tokio::test]
async fn plex_rejects_non_members_and_unverifiable_servers() {
    let mut client = plex_client();
    client
        .server_ids
        .insert("plex-token-1".to_owned(), vec!["other-machine".to_owned()]);
    let journey = PlexJourney::new(
        FakeUserStore::new(),
        client.clone(),
        FakePlexLink::new(),
        FakeSessionIssuer::new(),
    );
    let (pin_id, _) = journey.start().await.unwrap();
    client.authorize_pin(pin_id, "plex-token-1");
    assert_eq!(
        journey.poll_login(pin_id, None).await,
        Err(FederatedError::Authentication(
            "Your Plex account does not have access to this server".to_owned()
        ))
    );

    let mut client = FakePlexPinClient::new();
    client
        .accounts
        .insert("plex-token-1".to_owned(), plex_account());
    let journey = PlexJourney::new(
        FakeUserStore::new(),
        client.clone(),
        FakePlexLink::new(),
        FakeSessionIssuer::new(),
    );
    let (pin_id, _) = journey.start().await.unwrap();
    client.authorize_pin(pin_id, "plex-token-1");
    assert_eq!(
        journey.poll_link(pin_id).await,
        Err(FederatedError::Authentication(
            "Could not verify the configured Plex server".to_owned()
        ))
    );
}

#[tokio::test]
async fn plex_link_returns_profile_only_and_connect_returns_raw_token() {
    let client = plex_client();
    let users = FakeUserStore::new();
    let sessions = FakeSessionIssuer::new();
    let journey = PlexJourney::new(
        users.clone(),
        client.clone(),
        FakePlexLink::new(),
        sessions.clone(),
    );

    let (pin_id, _) = journey.start().await.unwrap();
    client.authorize_pin(pin_id, "plex-token-1");
    let PlexPoll::Complete(profile) = journey.poll_link(pin_id).await.unwrap() else {
        panic!("expected a completed link");
    };
    assert_eq!(profile.uuid, "plex-uuid-1");
    assert_eq!(profile.server_access_token, "server-token-1");
    assert_eq!(sessions.issued(), 0);
    assert!(
        users
            .get_provider("plex", "plex-uuid-1")
            .await
            .unwrap()
            .is_none(),
        "linking creates no account"
    );

    let (pin_id, _) = journey.start().await.unwrap();
    client.authorize_pin(pin_id, "plex-token-1");
    assert!(matches!(
        journey.poll_connect(pin_id).await.unwrap(),
        PlexPoll::Complete(token) if token == "plex-token-1"
    ));
}

#[tokio::test]
async fn federated_username_derivation_prefers_email_local_part() {
    // INTENDED unification (see derive_username): every provider runs the
    // email-first rule, so jane.doe@ beats any display name.
    assert_eq!(
        username_base(Some("jane.doe@example.com"), "Jane Q. Elsewhere"),
        "jane.doe"
    );
    assert_eq!(
        username_base(Some("jane.doe@"), "Jane Q. Elsewhere"),
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
async fn duplicate_username_surfaces_for_rederivation() {
    let users = FakeUserStore::new();
    let profile = droppedneedle::auth::federated::users::FederatedProfile {
        provider_uid: "u1".to_owned(),
        display_name: "Sam".to_owned(),
        email: None,
        email_verified: false,
        avatar_url: None,
        token_json: "{}".to_owned(),
    };
    let first = find_or_create_federated_user(&users, "oidc", &profile)
        .await
        .unwrap();
    let profile = droppedneedle::auth::federated::users::FederatedProfile {
        provider_uid: "u2".to_owned(),
        display_name: "Sam".to_owned(),
        email: None,
        email_verified: false,
        avatar_url: None,
        token_json: "{}".to_owned(),
    };
    let second = find_or_create_federated_user(&users, "oidc", &profile)
        .await
        .unwrap();
    assert_eq!(first.username, "sam");
    assert_eq!(second.username, "sam-2");
    assert_eq!(second.role, "user");
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

// ---------------------------------------------------------------------------
// OIDC over HTTP
// ---------------------------------------------------------------------------

mod oidc_http {
    use axum::{
        Router,
        body::Body,
        http::{HeaderMap, Request, StatusCode},
    };
    use droppedneedle::auth::federated::fakes::{
        FakeOidcExchanges, FakeOidcIdp, FakeOidcStates, FakeSessionIssuer, FakeUserStore,
    };
    use droppedneedle::auth::federated::oidc::OidcLogin;
    use droppedneedle::auth::routes::federated::{OidcRouteState, StaticOidcConfig, oidc_router};
    use droppedneedle::auth::session::cookies::COOKIE_NAME;
    use droppedneedle::auth::users::memory::TestRig;
    use serde_json::{Value, json};
    use tower::ServiceExt as _;

    fn app() -> Router {
        let login = OidcLogin::new(
            FakeUserStore::new(),
            FakeOidcIdp::new(
                FakeOidcIdp::discovery_doc(),
                super::oidc_tokens(),
                super::oidc_claims(),
            ),
            FakeOidcStates::new(),
            FakeOidcExchanges::new(),
            FakeSessionIssuer::new(),
        );
        let ids = TestRig::new().expect("test rig").deps.ids;
        let state = OidcRouteState::new(login, StaticOidcConfig(FakeOidcIdp::config()), ids, "");
        Router::new().nest("/api/v3", oidc_router(state))
    }

    async fn send(app: &Router, request: Request<Body>) -> (StatusCode, HeaderMap, Value) {
        let response = app.clone().oneshot(request).await.expect("responds");
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("body reads");
        (
            status,
            headers,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    /// Authorize, then take the callback redirect's one-time exchange code.
    async fn exchange_code(app: &Router) -> String {
        let request = Request::post("/api/v3/auth/oidc/authorize")
            .body(Body::empty())
            .expect("request builds");
        let (_, _, body) = send(app, request).await;
        let url = body["authorize_url"].as_str().expect("url");
        let state = super::url_param(url, "state").expect("state");
        let request = Request::get(format!(
            "/api/v3/auth/oidc/callback?code=authcode-1&state={state}"
        ))
        .body(Body::empty())
        .expect("request builds");
        let (status, headers, _) = send(app, request).await;
        assert_eq!(status, StatusCode::FOUND);
        let location = headers["location"].to_str().expect("ascii").to_owned();
        assert!(location.starts_with("/auth/callback?code="), "{location}");
        location.split("code=").nth(1).expect("code").to_owned()
    }

    fn exchange(code: &str, proto: &str, peer: Option<&str>) -> Request<Body> {
        let mut request = Request::post("/api/v3/auth/oidc/exchange")
            .header("content-type", "application/json")
            .header("x-forwarded-proto", proto)
            .body(Body::from(json!({"code": code}).to_string()))
            .expect("request builds");
        if let Some(peer) = peer {
            let addr: std::net::SocketAddr = peer.parse().expect("peer");
            request
                .extensions_mut()
                .insert(axum::extract::ConnectInfo(addr));
        }
        request
    }

    #[tokio::test]
    async fn oidc_login_mints_a_cookie_session_once() {
        let app = app();
        let code = exchange_code(&app).await;
        let (status, headers, body) = send(&app, exchange(&code, "http", None)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(
            body.get("token").is_none(),
            "cookie mode keeps the token out"
        );
        let cookie = headers["set-cookie"].to_str().expect("ascii");
        assert!(cookie.starts_with(&format!("{COOKIE_NAME}=")), "{cookie}");
        // Replaying the code fails: it is single use.
        let (status, _, _) = send(&app, exchange(&code, "http", None)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn forwarded_proto_marks_secure_only_from_trusted_proxies() {
        let app = app();
        for (peer, secure) in [
            (None, false),
            (Some("203.0.113.9:1"), false),
            (Some("127.0.0.1:1"), true),
        ] {
            let code = exchange_code(&app).await;
            let (status, headers, _) = send(&app, exchange(&code, "https", peer)).await;
            assert_eq!(status, StatusCode::OK);
            let cookie = headers["set-cookie"].to_str().expect("ascii");
            assert_eq!(cookie.contains("Secure"), secure, "{peer:?}: {cookie}");
        }
    }
}
