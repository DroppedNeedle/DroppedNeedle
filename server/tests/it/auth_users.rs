//! Users routes over the test rig: the behaviors that live in the users
//! handlers and services (credentials, ownership, recovery, Last.fm, admin
//! guards, user import). The test-principal layer stands in for the
//! session middleware; transactional store guards are pinned on SQLite in
//! `auth_stores`, and the role matrix in `auth_e2e`.

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use droppedneedle::auth::session::middleware::CurrentSession;
use droppedneedle::auth::users::{
    Role, admin_router, import,
    memory::{self, FakeUserDirectory, TestRig},
    models, public_router,
    stores::{DirectoryUser, SessionManager as _, UserStore as _},
    users_router,
};
use serde_json::{Value, json};
use tower::ServiceExt as _;

const PASSWORD: &str = "correct horse battery staple";
const OTHER_PASSWORD: &str = "another fine password here";

fn test_app(rig: &TestRig, principal: Option<CurrentSession>) -> Router {
    let app = Router::new()
        .nest("/api/v3", users_router(rig.deps.clone()))
        .nest("/api/v3", admin_router(rig.deps.clone()))
        .nest("/api/v3", public_router(rig.deps.clone()));
    match principal {
        Some(ctx) => memory::with_test_principal(app, ctx),
        None => app,
    }
}

async fn rig_with_admin() -> (TestRig, models::UserRecord, models::UserRecord) {
    let rig = TestRig::new().unwrap();
    let admin = rig.seed_user("admin", Role::Admin).await;
    rig.seed_session(&admin.id, "sess-admin").await;
    let user = rig.seed_user("molly", Role::User).await;
    rig.seed_session(&user.id, "sess-molly").await;
    (rig, admin, user)
}

fn principal_for(user: &models::UserRecord, session_id: &str) -> CurrentSession {
    memory::test_principal(user, session_id, false)
}

async fn call(app: Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    let body = match body {
        Some(json) => {
            builder = builder.header("content-type", "application/json");
            Body::from(json.to_string())
        }
        None => Body::empty(),
    };
    let response = app.oneshot(builder.body(body).unwrap()).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 16 * 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn password_change_needs_the_current_one_and_ends_other_sessions() {
    let (rig, _, user) = rig_with_admin().await;
    rig.seed_session(&user.id, "sess-phone").await;
    let app = || test_app(&rig, Some(principal_for(&user, "sess-molly")));
    let change = |current: &str, next: &str| {
        Some(json!({"current_password": current, "new_password": next}))
    };

    let (status, _) = call(
        app(),
        "POST",
        "/api/v3/me/password",
        change("wrong password here", OTHER_PASSWORD),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = call(
        app(),
        "POST",
        "/api/v3/me/password",
        change(PASSWORD, "short"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = call(
        app(),
        "POST",
        "/api/v3/me/password",
        change(PASSWORD, OTHER_PASSWORD),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Every other session died with the old password; this one stays.
    let live: Vec<String> = rig
        .sessions
        .list_for_user(&user.id)
        .await
        .unwrap()
        .into_iter()
        .map(|session| session.id)
        .collect();
    assert_eq!(live, vec!["sess-molly".to_owned()]);
}

#[tokio::test]
async fn device_tokens_cannot_create_credentials() {
    let (rig, _, user) = rig_with_admin().await;
    let app = || {
        test_app(
            &rig,
            Some(memory::test_principal(&user, "sess-device", true)),
        )
    };
    for (uri, body) in [
        ("/api/v3/auth/device-sessions", json!({"label": "Phone"})),
        ("/api/v3/me/app-passwords", json!({"name": "forever"})),
        (
            "/api/v3/me/local-password",
            json!({"new_password": OTHER_PASSWORD}),
        ),
    ] {
        let (status, body) = call(app(), "POST", uri, Some(body)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri}: {body}");
    }
}

#[tokio::test]
async fn first_local_password_is_set_once() {
    let rig = TestRig::new().unwrap();
    let user = models::UserRecord {
        id: "user-sso".to_owned(),
        username: Some("sso".to_owned()),
        username_display: Some("sso".to_owned()),
        display_name: "SSO User".to_owned(),
        email: None,
        avatar_url: None,
        role: Role::User,
        created_at: memory::TEST_NOW,
        last_login_at: None,
    };
    rig.users.insert(user.clone()).await.unwrap();
    let app = || test_app(&rig, Some(principal_for(&user, "sess-sso")));
    let body = Some(json!({"new_password": OTHER_PASSWORD}));
    let (status, _) = call(app(), "POST", "/api/v3/me/local-password", body.clone()).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(app(), "POST", "/api/v3/me/local-password", body).await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn foreign_sessions_and_app_passwords_are_404_not_403() {
    let (rig, _, user) = rig_with_admin().await;
    let other = rig.seed_user("other", Role::User).await;
    let (_, created) = call(
        test_app(&rig, Some(principal_for(&user, "sess-molly"))),
        "POST",
        "/api/v3/me/app-passwords",
        Some(json!({"name": "x"})),
    )
    .await;
    let app_password = created["id"].as_str().unwrap().to_owned();
    let stranger = || test_app(&rig, Some(principal_for(&other, "sess-other")));
    for uri in [
        format!("/api/v3/me/app-passwords/{app_password}"),
        "/api/v3/auth/sessions/sess-molly".to_owned(),
    ] {
        let (status, _) = call(stranger(), "DELETE", &uri, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
    }
}

#[tokio::test]
async fn avatars_are_self_or_admin() {
    use base64::{Engine as _, engine::general_purpose::STANDARD as B64};

    let (rig, admin, user) = rig_with_admin().await;
    let png = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    let payload = json!({"content_type": "image/png", "image_base64": B64.encode(png)});
    let (status, body) = call(
        test_app(&rig, Some(principal_for(&user, "sess-molly"))),
        "POST",
        "/api/v3/me/avatar",
        Some(payload),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let stranger = rig.seed_user("stranger", Role::User).await;
    let uri = format!("/api/v3/users/{}/avatar", user.id);
    for (who, session, expected) in [
        (&user, "sess-molly", StatusCode::OK),
        (&admin, "sess-admin", StatusCode::OK),
        (&stranger, "sess-stranger", StatusCode::FORBIDDEN),
    ] {
        let (status, _) = call(
            test_app(&rig, Some(principal_for(who, session))),
            "GET",
            &uri,
            None,
        )
        .await;
        assert_eq!(status, expected, "{}", who.id);
    }
}

#[tokio::test]
async fn avatar_upload_rejects_svg_and_oversize() {
    use base64::{Engine as _, engine::general_purpose::STANDARD as B64};

    let (rig, _, user) = rig_with_admin().await;
    let svg = br#"<svg xmlns="http://www.w3.org/2000/svg"><script>x()</script></svg>"#;
    let big = vec![0u8; 5 * 1024 * 1024 + 1];
    for (content_type, bytes) in [("image/svg+xml", svg.to_vec()), ("image/png", big)] {
        let (status, body) = call(
            test_app(&rig, Some(principal_for(&user, "sess-molly"))),
            "POST",
            "/api/v3/me/avatar",
            Some(json!({"content_type": content_type, "image_base64": B64.encode(&bytes)})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{content_type}: {body}");
    }
}

#[tokio::test]
async fn recovery_failures_share_one_message() {
    let (rig, admin, user) = rig_with_admin().await;
    let (_, minted) = call(
        test_app(&rig, Some(principal_for(&admin, "sess-admin"))),
        "POST",
        &format!("/api/v3/admin/users/{}/recovery-code", user.id),
        None,
    )
    .await;
    let code = minted["recovery_code"].as_str().unwrap().to_owned();
    for (username, code) in [
        ("admin", code.as_str()),
        ("molly", "AAAA-BBBB-CCCC-DDDD-EEEE"),
    ] {
        let (status, body) = call(
            test_app(&rig, None),
            "POST",
            "/api/v3/auth/password-recovery/reset",
            Some(json!({"username": username, "recovery_code": code, "new_password": OTHER_PASSWORD})),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(
            body["error"]["message"],
            json!("Invalid or expired recovery code")
        );
    }
}

#[tokio::test]
async fn lastfm_link_flow_keeps_secrets_sealed() {
    let (rig, _, user) = rig_with_admin().await;
    let app = || test_app(&rig, Some(principal_for(&user, "sess-molly")));
    let credentials =
        |key: &str, secret: &str| Some(json!({"api_key": key, "shared_secret": secret}));

    let (status, body) = call(
        app(),
        "PUT",
        "/api/v3/me/connections/lastfm",
        credentials("user-api-key", "user-shared-secret"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!body.to_string().contains("user-api-key"), "{body}");
    let (status, _) = call(app(), "POST", "/api/v3/me/connections/lastfm/token", None).await;
    assert_eq!(status, StatusCode::OK);
    // An unapproved token is a 409 naming the next step, not an outage.
    let (status, _) = call(
        app(),
        "POST",
        "/api/v3/me/connections/lastfm/session",
        Some(json!({"token": "fake-token"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    rig.lastfm_client
        .approve("fake-token", "mollylfm", "session-key-1")
        .await;
    let (status, body) = call(
        app(),
        "POST",
        "/api/v3/me/connections/lastfm/session",
        Some(json!({"token": "fake-token"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"username": "mollylfm", "linked": true}));
    use droppedneedle::auth::users::stores::LastFmStore as _;
    let link = rig.lastfm.get(&user.id).await.unwrap().unwrap();
    assert!(link.session_key_encrypted.unwrap().starts_with("v3:"));

    // The mask and an identical resubmit keep the link; a new key unlinks.
    for (key, secret, linked) in [
        ("lastfm****", "lastfm****", true),
        ("user-api-key", "user-shared-secret", true),
        ("rotated-key", "lastfm****", false),
    ] {
        let (status, body) = call(
            app(),
            "PUT",
            "/api/v3/me/connections/lastfm",
            credentials(key, secret),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["linked"], json!(linked), "{key}");
    }
}

#[tokio::test]
async fn admins_cannot_demote_or_delete_themselves() {
    let (rig, admin, _) = rig_with_admin().await;
    let app = || test_app(&rig, Some(principal_for(&admin, "sess-admin")));
    let (status, _) = call(
        app(),
        "PUT",
        &format!("/api/v3/admin/users/{}/role", admin.id),
        Some(json!({"role": "user"})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = call(
        app(),
        "DELETE",
        &format!("/api/v3/admin/users/{}", admin.id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn breached_passwords_reject_when_screening_is_on() {
    use droppedneedle::auth::users::{hibp::HibpScreen, memory::FakeHibpHttp, stores::HibpPolicy};

    let (mut rig, _, user) = rig_with_admin().await;
    let digest = droppedneedle::auth::users::hibp::sha1_hex_upper(b"password12xxxx");
    rig.deps.screen = std::sync::Arc::new(HibpScreen::new(std::sync::Arc::new(
        FakeHibpHttp::with_hits(&[digest.as_str()]),
    )));
    rig.deps.security = std::sync::Arc::new(memory::StaticSecurityPolicy {
        policy: HibpPolicy {
            check: true,
            local_path: String::new(),
        },
    });
    let (status, _) = call(
        test_app(&rig, Some(principal_for(&user, "sess-molly"))),
        "POST",
        "/api/v3/me/password",
        Some(json!({"current_password": PASSWORD, "new_password": "password12xxxx"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn import_creates_new_accounts_and_links_known_emails() {
    let (rig, _, _) = rig_with_admin().await;
    let known = rig.seed_user("known", Role::User).await;
    rig.users
        .update_email(&known.id, Some("sam@example.com"))
        .await
        .unwrap();
    let directory_user = |uid: &str, email: &str| DirectoryUser {
        provider_uid: uid.to_owned(),
        display_name: uid.to_owned(),
        avatar_url: None,
        email: Some(email.to_owned()),
    };
    let plex = FakeUserDirectory::plex(vec![
        directory_user("px-new", "new@example.com"),
        directory_user("px-link", "sam@example.com"),
    ]);
    let batch = import::import_users(
        &rig.deps,
        &plex,
        &[
            "px-new".to_owned(),
            "px-link".to_owned(),
            "px-unknown".to_owned(),
        ],
    )
    .await
    .unwrap();
    assert_eq!(batch.imported.len(), 1);
    assert_eq!(batch.imported[0].role, Role::User);
    assert_eq!(batch.linked[0].id, known.id);
    assert_eq!(batch.skipped, vec!["px-unknown".to_owned()]);
}
