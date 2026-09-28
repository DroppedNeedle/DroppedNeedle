//! Stage 3 federated briefs: OIDC, Jellyfin, and unified Plex logins
//! against scripted fakes (no live IdP calls anywhere), the bcrypt-import
//! to Argon2id rehash, dead sessions on import, and post-import
//! app-password verification through the compat contracts.

#[allow(dead_code)]
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

use droppedneedle::auth::federated::fakes::{
    FakeHasher, FakeJellyfinIdp, FakeJellyfinLink, FakeOidcExchanges, FakeOidcIdp, FakeOidcStates,
    FakePlexLink, FakePlexPinClient, FakeSessionIssuer, FakeUserStore,
};
use droppedneedle::auth::federated::jellyfin_login::{
    JellyfinLogin, JellyfinProfile, NoopJellyfinLink, emby_auth_header, jellyfin_token_json,
};
use droppedneedle::auth::federated::oidc::{
    DISCOVERY_SUFFIX, EXCHANGE_TTL_SECS, OidcLogin, OidcStateStore, OidcTokens, RANDOM_BYTES,
    RawClaims, STATE_TTL_SECS, authorize_url, form_encode, jwt_payload_claims, normalise_claims,
    pkce_challenge, sealed_token_json,
};
use droppedneedle::auth::federated::password_import::{
    HashScheme, LocalCredential, PasswordCheck, SESSIONS_SURVIVE_IMPORT, verify_and_maybe_rehash,
};
use droppedneedle::auth::federated::plex::{
    NoopPlexLink, PlexAccount, PlexJourney, PlexPoll, PlexPurpose, PlexStartDenied, plex_auth_url,
    plex_token_json,
};
use droppedneedle::auth::federated::users::{
    CREATE_RETRIES, FederatedUserStore, PROVIDER_JELLYFIN, PROVIDER_OIDC, PROVIDER_PLEX,
    ROLE_ADMIN, ROLE_USER, derive_username, find_or_create_federated_user, username_base,
};
use droppedneedle::auth::federated::{FederatedError, SessionIssuer, json_string};

fn oidc_claims() -> RawClaims {
    RawClaims {
        sub: Some("oidc-sub-1".to_owned()),
        email: Some("Jane.Doe@Example.com".to_owned()),
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
fn oidc_constants_match_v2() {
    assert_eq!(DISCOVERY_SUFFIX, "/.well-known/openid-configuration");
    assert_eq!(STATE_TTL_SECS, 600);
    assert_eq!(EXCHANGE_TTL_SECS, 60);
    assert_eq!(RANDOM_BYTES, 32);
    assert_eq!(CREATE_RETRIES, 20);
    assert_eq!((ROLE_ADMIN, ROLE_USER), ("admin", "user"));
    assert_eq!(
        (PROVIDER_OIDC, PROVIDER_JELLYFIN, PROVIDER_PLEX),
        ("oidc", "jellyfin", "plex")
    );
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

#[test]
fn providers_flag_needs_enabled_issuer_and_client() {
    let mut config = FakeOidcIdp::config();
    assert!(OidcLogin::<
        FakeUserStore,
        FakeOidcIdp,
        FakeOidcStates,
        FakeOidcExchanges,
        FakeSessionIssuer,
    >::providers_flag(&config));
    config.enabled = false;
    assert!(!OidcLogin::<
        FakeUserStore,
        FakeOidcIdp,
        FakeOidcStates,
        FakeOidcExchanges,
        FakeSessionIssuer,
    >::providers_flag(&config));
}

#[tokio::test]
async fn oidc_authorize_stores_pkce_state_for_callback() {
    let idp = FakeOidcIdp::new(FakeOidcIdp::discovery_doc(), oidc_tokens(), oidc_claims());
    let states = FakeOidcStates::new();
    let service = OidcLogin::new(
        FakeUserStore::new(),
        idp,
        states.clone(),
        FakeOidcExchanges::new(),
        FakeSessionIssuer::new(),
    );
    let config = FakeOidcIdp::config();
    let url = service.build_authorize_url(&config).await.unwrap();
    assert!(url.starts_with("https://idp.test/authorize?response_type=code&"));
    let state = url_param(&url, "state").unwrap().to_owned();
    let challenge = url_param(&url, "code_challenge").unwrap().to_owned();
    assert_eq!(state.len(), 44);
    let verifier = states
        .consume_state(&state)
        .await
        .unwrap()
        .expect("state stored");
    assert_eq!(verifier.len(), 43);
    assert_eq!(pkce_challenge(&verifier), challenge);
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
            role: "user".to_owned(),
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
fn jwt_payload_decode_pins_compact_parsing() {
    let payload = URL_SAFE_NO_PAD.encode(br#"{"sub":"u1","email":"J@X.test","nickname":"J"}"#);
    let token = format!("e30.{payload}.sig");
    let claims = jwt_payload_claims(&token).expect("decodes");
    assert_eq!(claims.sub.as_deref(), Some("u1"));
    assert_eq!(claims.nickname.as_deref(), Some("J"));
    assert!(jwt_payload_claims("only.two").is_none());
    assert!(jwt_payload_claims("a.b.c.d").is_none());
    assert!(jwt_payload_claims("a.!!!.c").is_none());
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

#[tokio::test]
async fn jellyfin_credential_check_has_no_side_effects() {
    let mut idp = FakeJellyfinIdp::new(true);
    idp.accept("jfuser", "jfpass", jellyfin_profile());
    let users = FakeUserStore::new();
    let sessions = FakeSessionIssuer::new();
    let service = JellyfinLogin::new(
        users.clone(),
        idp,
        FakeJellyfinLink::new(),
        sessions.clone(),
    );
    let profile = service
        .authenticate_credentials("jfuser", "jfpass")
        .await
        .unwrap();
    assert_eq!(profile.jellyfin_user_id, "jf-1");
    assert!(!users.has_any_users().await.unwrap());
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
async fn plex_start_and_pending_poll_have_no_side_effects() {
    let client = FakePlexPinClient::new();
    let users = FakeUserStore::new();
    let sessions = FakeSessionIssuer::new();
    let journey = PlexJourney::new(users.clone(), client, FakePlexLink::new(), sessions.clone());
    let (pin_id, url) = journey.start().await.unwrap();
    assert!(url.starts_with("https://app.plex.tv/auth#?clientID=test-client-id&code=code-"));
    assert!(matches!(
        journey.poll_login(pin_id, None).await.unwrap(),
        PlexPoll::Pending
    ));
    assert!(matches!(
        journey.poll_link(pin_id).await.unwrap(),
        PlexPoll::Pending
    ));
    assert!(matches!(
        journey.poll_connect(pin_id).await.unwrap(),
        PlexPoll::Pending
    ));
    assert!(!users.has_any_users().await.unwrap());
    assert_eq!(sessions.issued(), 0);
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
    assert_eq!(user.email.as_deref(), Some("plex@example.com"));
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
async fn plex_login_skips_gate_when_no_server_is_configured() {
    let mut client = FakePlexPinClient::new();
    client
        .accounts
        .insert("plex-token-1".to_owned(), plex_account());
    let journey = PlexJourney::new(
        FakeUserStore::new(),
        client.clone(),
        NoopPlexLink,
        FakeSessionIssuer::new(),
    );
    let (pin_id, _) = journey.start().await.unwrap();
    client.authorize_pin(pin_id, "plex-token-1");
    assert!(matches!(
        journey.poll_login(pin_id, None).await.unwrap(),
        PlexPoll::Complete(_)
    ));
    assert!(
        !client
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call.starts_with("account_server_ids"))
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
    assert!(!users.has_any_users().await.unwrap());

    let (pin_id, _) = journey.start().await.unwrap();
    client.authorize_pin(pin_id, "plex-token-1");
    assert!(matches!(
        journey.poll_connect(pin_id).await.unwrap(),
        PlexPoll::Complete(token) if token == "plex-token-1"
    ));
}

#[tokio::test]
async fn plex_maps_provider_failures_to_v2_messages() {
    let mut client = FakePlexPinClient::new();
    client.fail_create = true;
    let journey = PlexJourney::new(
        FakeUserStore::new(),
        client,
        FakePlexLink::new(),
        FakeSessionIssuer::new(),
    );
    assert_eq!(
        journey.start().await,
        Err(FederatedError::Authentication(
            "Could not start Plex authentication".to_owned()
        ))
    );

    let mut client = plex_client();
    client.failing_accounts.push("plex-token-1".to_owned());
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
            "Could not verify Plex account".to_owned()
        ))
    );
}

#[tokio::test]
async fn plex_start_gates_link_and_connect_when_unconfigured() {
    // No machine id means Plex is unconfigured: login starts fine, link and
    // connect refuse.
    let journey = PlexJourney::new(
        FakeUserStore::new(),
        FakePlexPinClient::new(),
        FakePlexLink::new(),
        FakeSessionIssuer::new(),
    );
    assert!(journey.start_for_purpose(PlexPurpose::Login).await.is_ok());
    assert_eq!(
        journey.start_for_purpose(PlexPurpose::Link).await,
        Err(PlexStartDenied::NotConfigured)
    );
    assert_eq!(
        journey.start_for_purpose(PlexPurpose::Connect).await,
        Err(PlexStartDenied::NotConfigured)
    );

    // Configured servers mint for every purpose.
    let journey = PlexJourney::new(
        FakeUserStore::new(),
        plex_client(),
        FakePlexLink::new(),
        FakeSessionIssuer::new(),
    );
    assert!(journey.start_for_purpose(PlexPurpose::Link).await.is_ok());
    assert!(
        journey
            .start_for_purpose(PlexPurpose::Connect)
            .await
            .is_ok()
    );

    // PIN-creation failure is a start failure, not a config refusal.
    let mut failing = FakePlexPinClient::new();
    failing.fail_create = true;
    let journey = PlexJourney::new(
        FakeUserStore::new(),
        failing,
        FakePlexLink::new(),
        FakeSessionIssuer::new(),
    );
    assert_eq!(
        journey.start_for_purpose(PlexPurpose::Login).await,
        Err(PlexStartDenied::StartFailed(
            FederatedError::Authentication("Could not start Plex authentication".to_owned())
        ))
    );
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

#[test]
fn hash_scheme_tags_round_trip_and_never_sniff() {
    assert_eq!(HashScheme::Bcrypt.as_tag(), "bcrypt");
    assert_eq!(HashScheme::Argon2id.as_tag(), "argon2id");
    assert_eq!(HashScheme::from_tag("bcrypt"), Some(HashScheme::Bcrypt));
    assert_eq!(HashScheme::from_tag("argon2id"), Some(HashScheme::Argon2id));
    assert_eq!(HashScheme::from_tag("$2b$12$abc"), None);
    assert_eq!(HashScheme::from_tag(""), None);
}

#[test]
fn bcrypt_login_verifies_once_then_rehashes_to_argon2id() {
    let hasher = FakeHasher::new();
    let mut stored = LocalCredential::imported_bcrypt(FakeHasher::bcrypt_fixture("old-password"));

    let PasswordCheck::Valid { upgraded } =
        verify_and_maybe_rehash(&hasher, "old-password", Some(&stored))
    else {
        panic!("the imported password must verify");
    };
    let upgraded = upgraded.expect("bcrypt success must rehash");
    assert_eq!(upgraded.scheme, HashScheme::Argon2id);
    stored = upgraded;
    assert_eq!(stored.scheme.as_tag(), "argon2id");

    assert_eq!(
        verify_and_maybe_rehash(&hasher, "old-password", Some(&stored)),
        PasswordCheck::Valid { upgraded: None }
    );
    assert_eq!(
        verify_and_maybe_rehash(&hasher, "wrong", Some(&stored)),
        PasswordCheck::Invalid
    );
}

#[test]
fn wrong_password_never_rehashes_and_unknown_users_cost_one_dummy() {
    let hasher = FakeHasher::new();
    let stored = LocalCredential::imported_bcrypt(FakeHasher::bcrypt_fixture("old-password"));
    assert_eq!(
        verify_and_maybe_rehash(&hasher, "wrong", Some(&stored)),
        PasswordCheck::Invalid
    );
    assert_eq!(stored.scheme, HashScheme::Bcrypt);

    assert_eq!(
        verify_and_maybe_rehash(&hasher, "anything", None),
        PasswordCheck::Invalid
    );
    assert_eq!(hasher.dummy_calls(), 1);
}

const _: () = assert!(!SESSIONS_SURVIVE_IMPORT);

#[test]
fn session_issuer_starts_empty() {
    assert_eq!(FakeSessionIssuer::new().issued(), 0);
}

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

#[test]
fn error_messages_stay_stable() {
    assert_eq!(
        format!("{}", FederatedError::RngUnavailable),
        "random source unavailable"
    );
    assert_eq!(
        format!("{}", FederatedError::StoreUnavailable("db down".to_owned())),
        "store unavailable: db down"
    );
    assert_eq!(
        format!("{}", FederatedError::UsernameTaken),
        "username taken"
    );
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
