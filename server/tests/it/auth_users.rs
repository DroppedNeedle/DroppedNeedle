//! Users-slice briefs: role matrix, profile, sessions, recovery,
//! per-user Last.fm, and app passwords.
//!
//! Each test pins one behavior. The routers run against the memory fakes
//! with the test-principal layer standing in for the sibling session
//! middleware (auth resolution only).

use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use droppedneedle::auth::federated::password_import::PasswordHasher;
use droppedneedle::auth::federated::users::ProviderBinding;
use droppedneedle::auth::session::{middleware::CurrentSession, tokens};
use droppedneedle::auth::users::{
    Role, admin_router, clock_now, handlers,
    import::{self, ImportError},
    memory::{self, FakeUserDirectory, MemoryUserStore, TestRig},
    models, public_router, roles, services,
    stores::{
        BoxFuture, DirectoryError, DirectoryUser, FalliblePasswordHasher, HashError,
        LastFmStore as _, SessionManager as _, StoreError, UserStore,
    },
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

fn companion_principal_for(user: &models::UserRecord, session_id: &str) -> CurrentSession {
    memory::test_principal(user, session_id, true)
}

async fn call(
    app: Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value, axum::http::HeaderMap) {
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
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), 16 * 1024 * 1024)
        .await
        .unwrap();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        // Non-JSON bodies (avatar bytes) parse as null; status carries those.
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, json, headers)
}

fn error_code(body: &Value) -> &str {
    body.pointer("/error/code").and_then(Value::as_str).unwrap()
}

// ---------------------------------------------------------------------------
// Role matrix
// ---------------------------------------------------------------------------

#[tokio::test]
async fn anonymous_user_route_is_401_with_bearer_challenge() {
    let (rig, _, _) = rig_with_admin().await;
    let (status, body, headers) = call(test_app(&rig, None), "GET", "/api/v3/me", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(error_code(&body), "UNAUTHORIZED");
    assert_eq!(headers.get("www-authenticate").unwrap(), "Bearer");
}

#[tokio::test]
async fn anonymous_admin_route_is_401_not_403() {
    let (rig, _, _) = rig_with_admin().await;
    let (status, _, _) = call(test_app(&rig, None), "GET", "/api/v3/admin/users", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn admin_routes_reject_user_and_trusted_with_403() {
    let (rig, admin, user) = rig_with_admin().await;
    let trusted = rig.seed_user("curator", Role::Trusted).await;
    rig.seed_session(&trusted.id, "sess-trusted").await;

    for (who, session) in [(&user, "sess-molly"), (&trusted, "sess-trusted")] {
        let app = test_app(&rig, Some(principal_for(who, session)));
        let (status, body, _) = call(app, "GET", "/api/v3/admin/users", None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(error_code(&body), "FORBIDDEN");
    }

    let app = test_app(&rig, Some(principal_for(&admin, "sess-admin")));
    let (status, _, _) = call(app, "GET", "/api/v3/admin/users", None).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn curator_probe_admits_trusted_and_admin_only() {
    let (rig, admin, user) = rig_with_admin().await;
    let trusted = rig.seed_user("curator", Role::Trusted).await;

    let probe = || {
        Router::new()
            .route("/probe", axum::routing::get(handlers::curator_probe))
            .with_state(rig.deps.clone())
    };

    let (status, _, _) = call(probe(), "GET", "/probe", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    for (who, session, expected) in [
        (&user, "sess-molly", StatusCode::FORBIDDEN),
        (&trusted, "sess-x", StatusCode::OK),
        (&admin, "sess-admin", StatusCode::OK),
    ] {
        let app = memory::with_test_principal(probe(), principal_for(who, session));
        let (status, _, _) = call(app, "GET", "/probe", None).await;
        assert_eq!(status, expected, "role {:?}", who.role);
    }
}

#[tokio::test]
async fn malformed_body_stays_inside_the_envelope() {
    let (rig, _, user) = rig_with_admin().await;
    let app = test_app(&rig, Some(principal_for(&user, "sess-molly")));
    let response = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/v3/me")
                .header("content-type", "application/json")
                .body(Body::from("{oops"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(error_code(&json), "INVALID_INPUT");
}

// ---------------------------------------------------------------------------
// Profile (trace A:494-501)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn profile_round_trip() {
    let (rig, _, user) = rig_with_admin().await;
    let app = || test_app(&rig, Some(principal_for(&user, "sess-molly")));

    let (status, body, _) = call(app(), "GET", "/api/v3/me", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["username"], json!("molly"));
    assert_eq!(body["display_name"], json!("molly"));
    assert_eq!(body["role"], json!("user"));
    assert_eq!(body["providers"], json!(["local"]));

    let (status, body, _) = call(
        app(),
        "PATCH",
        "/api/v3/me",
        Some(json!({"display_name": "Molly R"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["display_name"], json!("Molly R"));

    let (status, body, _) = call(
        app(),
        "PUT",
        "/api/v3/me/username",
        Some(json!({"username": "MollyR"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["username"], json!("mollyr"));
    assert_eq!(body["username_display"], json!("MollyR"));

    let (status, body, _) = call(
        app(),
        "PUT",
        "/api/v3/me/email",
        Some(json!({"email": "Molly@Example.com"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["email"], json!("molly@example.com"));

    let (status, body, _) = call(
        app(),
        "PUT",
        "/api/v3/me/email",
        Some(json!({"email": null})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["email"], Value::Null);
}

#[tokio::test]
async fn profile_rejects_bad_input_with_specific_messages() {
    let (rig, _, user) = rig_with_admin().await;
    let app = || test_app(&rig, Some(principal_for(&user, "sess-molly")));

    let (status, body, _) = call(
        app(),
        "PUT",
        "/api/v3/me/username",
        Some(json!({"username": "x"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["message"], json!("Invalid username"));

    let (status, body, _) = call(
        app(),
        "PUT",
        "/api/v3/me/username",
        Some(json!({"username": "admin"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["message"], json!("Username already taken"));

    let (status, _, _) = call(
        app(),
        "PATCH",
        "/api/v3/me",
        Some(json!({"display_name": "  "})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn password_change_needs_the_current_one() {
    let (rig, _, user) = rig_with_admin().await;
    let app = || test_app(&rig, Some(principal_for(&user, "sess-molly")));

    let (status, _, _) = call(
        app(),
        "POST",
        "/api/v3/me/password",
        Some(json!({"current_password": "wrong password here", "new_password": OTHER_PASSWORD})),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, _, _) = call(
        app(),
        "POST",
        "/api/v3/me/password",
        Some(json!({"current_password": PASSWORD, "new_password": "short"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _, _) = call(
        app(),
        "POST",
        "/api/v3/me/password",
        Some(json!({"current_password": PASSWORD, "new_password": OTHER_PASSWORD})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let cred = rig.users.local_credential(&user.id).await.unwrap().unwrap();
    assert!(
        rig.deps
            .passwords
            .verify_argon2id(OTHER_PASSWORD, &cred.hash)
    );
    assert!(!rig.deps.passwords.verify_argon2id(PASSWORD, &cred.hash));
}

#[tokio::test]
async fn local_password_set_covers_sso_accounts_once() {
    let rig = TestRig::new().unwrap();
    let now = clock_now(&rig.deps);
    rig.users
        .insert(models::UserRecord {
            id: "user-sso".to_owned(),
            username: Some("sso".to_owned()),
            username_display: Some("sso".to_owned()),
            display_name: "SSO User".to_owned(),
            email: None,
            avatar_url: None,
            role: Role::User,
            created_at: now,
            last_login_at: None,
        })
        .await
        .unwrap();
    let user = rig.users.get_by_id("user-sso").await.unwrap().unwrap();
    let app = || test_app(&rig, Some(principal_for(&user, "sess-sso")));

    let (status, _, _) = call(
        app(),
        "POST",
        "/api/v3/me/local-password",
        Some(json!({"new_password": OTHER_PASSWORD})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body, _) = call(
        app(),
        "POST",
        "/api/v3/me/local-password",
        Some(json!({"new_password": "yet another password here"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        body["error"]["message"],
        json!("A local password already exists; use change password instead")
    );
}

#[tokio::test]
async fn avatar_upload_serve_and_self_or_admin_read() {
    use base64::{Engine as _, engine::general_purpose::STANDARD as B64};

    let (rig, admin, user) = rig_with_admin().await;
    let bytes = png_bytes();
    let payload = json!({"content_type": "image/png", "image_base64": B64.encode(&bytes)});

    let app = test_app(&rig, Some(principal_for(&user, "sess-molly")));
    let (status, body, _) = call(app, "POST", "/api/v3/me/avatar", Some(payload)).await;
    assert_eq!(status, StatusCode::OK);
    let url = body["avatar_url"].as_str().unwrap().to_owned();
    assert!(url.starts_with(&format!("/api/v3/users/{}/avatar?v=", user.id)));

    // Owner reads the bytes back with the right type and cache header.
    let app = test_app(&rig, Some(principal_for(&user, "sess-molly")));
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/v3/users/{}/avatar", user.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers().get("content-type").unwrap(), "image/png");
    assert_eq!(
        response.headers().get("cache-control").unwrap(),
        "private, max-age=3600"
    );
    let served = axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    assert_eq!(served.to_vec(), bytes);

    // A stranger is forbidden; an admin reads anyone's.
    let stranger = rig.seed_user("stranger", Role::User).await;
    let app = test_app(&rig, Some(principal_for(&stranger, "sess-stranger")));
    let (status, _, _) = call(
        app,
        "GET",
        &format!("/api/v3/users/{}/avatar", user.id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let app = test_app(&rig, Some(principal_for(&admin, "sess-admin")));
    let (status, _, _) = call(
        app,
        "GET",
        &format!("/api/v3/users/{}/avatar", user.id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Unknown user id 404s even for the admin when no bytes exist.
    let app = test_app(&rig, Some(principal_for(&admin, "sess-admin")));
    let (status, _, _) = call(app, "GET", "/api/v3/users/user-nobody/avatar", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn avatar_rejects_bad_type_and_oversize() {
    use base64::{Engine as _, engine::general_purpose::STANDARD as B64};

    let (rig, _, user) = rig_with_admin().await;
    let app = || test_app(&rig, Some(principal_for(&user, "sess-molly")));

    let (status, _, _) = call(
        app(),
        "POST",
        "/api/v3/me/avatar",
        Some(json!({"content_type": "image/bmp", "image_base64": B64.encode(b"xx")})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let big = vec![0u8; 5 * 1024 * 1024 + 1];
    let (status, body, _) = call(
        app(),
        "POST",
        "/api/v3/me/avatar",
        Some(json!({"content_type": "image/png", "image_base64": B64.encode(&big)})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body["error"]["message"],
        json!("Image too large. Maximum size is 5 MB")
    );
}

// ---------------------------------------------------------------------------
// Sessions (R6 UI backend)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn session_list_marks_current_first() {
    let (rig, _, user) = rig_with_admin().await;
    rig.seed_session(&user.id, "sess-old").await;
    let app = test_app(&rig, Some(principal_for(&user, "sess-molly")));
    let (status, body, _) = call(app, "GET", "/api/v3/auth/sessions", None).await;
    assert_eq!(status, StatusCode::OK);
    let sessions = body["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0]["id"], json!("sess-molly"));
    assert_eq!(sessions[0]["current"], json!(true));
    assert_eq!(sessions[0]["kind"], json!("standard"));
    assert!(sessions[0].get("token_hash").is_none());
    assert_eq!(sessions[1]["current"], json!(false));
}

#[tokio::test]
async fn companion_remint_atomically_replaces_same_label() {
    let (rig, _, user) = rig_with_admin().await;
    let app = || test_app(&rig, Some(principal_for(&user, "sess-molly")));

    let (status, first, _) = call(
        app(),
        "POST",
        "/api/v3/auth/device-sessions",
        Some(json!({"label": "  Car   Display "})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(first["label"], json!("Car Display"));
    let first_token = first["token"].as_str().unwrap().to_owned();
    assert_eq!(
        first["expires_at"].as_i64().unwrap(),
        memory::TEST_NOW + tokens::SESSION_MAX_AGE_SECS
    );

    let (status, second, _) = call(
        app(),
        "POST",
        "/api/v3/auth/device-sessions",
        Some(json!({"label": "Car Display"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let second_token = second["token"].as_str().unwrap().to_owned();
    assert_ne!(first_token, second_token);

    // Old token no longer resolves; new one does; exactly one live row.
    assert!(
        rig.sessions
            .owner_by_hash(&tokens::hash_token(&first_token))
            .await
            .unwrap()
            .is_none()
    );
    let owner = rig
        .sessions
        .owner_by_hash(&tokens::hash_token(&second_token))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(owner.user_id, user.id);
    assert_eq!(owner.kind, roles::SessionKind::Companion);
    let rows = rig.sessions.list_for_user(&user.id).await.unwrap();
    let companions: Vec<_> = rows
        .iter()
        .filter(|row| row.kind == roles::SessionKind::Companion)
        .collect();
    assert_eq!(companions.len(), 1);

    // A different label coexists.
    let (status, _, _) = call(
        app(),
        "POST",
        "/api/v3/auth/device-sessions",
        Some(json!({"label": "Phone"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let rows = rig.sessions.list_for_user(&user.id).await.unwrap();
    assert_eq!(
        rows.iter()
            .filter(|row| row.kind == roles::SessionKind::Companion)
            .count(),
        2
    );
}

#[tokio::test]
async fn companion_tokens_cannot_mint_further_tokens() {
    let (rig, _, user) = rig_with_admin().await;
    let app = test_app(&rig, Some(companion_principal_for(&user, "sess-companion")));
    let (status, body, _) = call(
        app,
        "POST",
        "/api/v3/auth/device-sessions",
        Some(json!({"label": "Phone"})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        body["error"]["message"],
        json!("Companion tokens cannot mint further tokens")
    );
}

#[tokio::test]
async fn device_label_validation_collapses_whitespace() {
    let (rig, _, user) = rig_with_admin().await;
    let app = || test_app(&rig, Some(principal_for(&user, "sess-molly")));

    let (status, _, _) = call(
        app(),
        "POST",
        "/api/v3/auth/device-sessions",
        Some(json!({"label": "   "})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _, _) = call(
        app(),
        "POST",
        "/api/v3/auth/device-sessions",
        Some(json!({"label": "x".repeat(81)})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn revoke_own_session_and_logout_all() {
    let (rig, _, user) = rig_with_admin().await;
    let app = || test_app(&rig, Some(principal_for(&user, "sess-molly")));

    let (status, _, _) = call(app(), "DELETE", "/api/v3/auth/sessions/sess-admin", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _, _) = call(app(), "DELETE", "/api/v3/auth/sessions/sess-unknown", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    rig.seed_session(&user.id, "sess-extra").await;
    let (status, _, _) = call(app(), "DELETE", "/api/v3/auth/sessions/sess-extra", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _, _) = call(app(), "POST", "/api/v3/auth/logout-all", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, body, _) = call(app(), "GET", "/api/v3/auth/sessions", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["sessions"], json!([]));
}

#[tokio::test]
async fn admin_revokes_all_sessions_of_one_user() {
    let (rig, admin, user) = rig_with_admin().await;
    rig.seed_session(&user.id, "sess-extra").await;

    let app = test_app(&rig, Some(principal_for(&user, "sess-molly")));
    let (status, _, _) = call(
        app,
        "DELETE",
        &format!("/api/v3/admin/users/{}/sessions", user.id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let app = test_app(&rig, Some(principal_for(&admin, "sess-admin")));
    let (status, _, _) = call(
        app,
        "DELETE",
        "/api/v3/admin/users/user-nobody/sessions",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let app = test_app(&rig, Some(principal_for(&admin, "sess-admin")));
    let (status, _, _) = call(
        app,
        "DELETE",
        &format!("/api/v3/admin/users/{}/sessions", user.id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let app = test_app(&rig, Some(principal_for(&user, "sess-molly")));
    let (status, body, _) = call(app, "GET", "/api/v3/auth/sessions", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["sessions"], json!([]));
}

// ---------------------------------------------------------------------------
// Password recovery
// ---------------------------------------------------------------------------

#[tokio::test]
async fn recovery_mint_reset_and_single_use() {
    let (rig, admin, user) = rig_with_admin().await;

    let app = test_app(&rig, Some(principal_for(&admin, "sess-admin")));
    let (status, mint, _) = call(
        app,
        "POST",
        &format!("/api/v3/admin/users/{}/recovery-code", user.id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let code = mint["recovery_code"].as_str().unwrap().to_owned();
    assert_eq!(code.len(), 24); // 20 chars in XXXX- groups
    assert_eq!(
        mint["expires_at"].as_i64().unwrap(),
        memory::TEST_NOW + services::RECOVERY_TTL_SECS
    );

    // Case, dashes, and spaces are ignored.
    let sloppy = code.to_lowercase().replace('-', "  ");
    let app = test_app(&rig, None);
    let (status, _, _) = call(
        app,
        "POST",
        "/api/v3/auth/password-recovery/reset",
        Some(json!({"username": "MOLLY", "recovery_code": sloppy, "new_password": OTHER_PASSWORD})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let cred = rig.users.local_credential(&user.id).await.unwrap().unwrap();
    assert!(
        rig.deps
            .passwords
            .verify_argon2id(OTHER_PASSWORD, &cred.hash)
    );

    // The code is consumed: reuse fails with the uniform message.
    let app = test_app(&rig, None);
    let (status, body, _) = call(
        app,
        "POST",
        "/api/v3/auth/password-recovery/reset",
        Some(json!({"username": "molly", "recovery_code": code, "new_password": "a third password here!!!"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        body["error"]["message"],
        json!("Invalid or expired recovery code")
    );
}

#[tokio::test]
async fn recovery_failures_share_one_message() {
    let (rig, admin, user) = rig_with_admin().await;
    let app = test_app(&rig, Some(principal_for(&admin, "sess-admin")));
    let (_, mint, _) = call(
        app,
        "POST",
        &format!("/api/v3/admin/users/{}/recovery-code", user.id),
        None,
    )
    .await;
    let code = mint["recovery_code"].as_str().unwrap().to_owned();

    // Wrong username for a live code.
    let app = test_app(&rig, None);
    let (status, body, _) = call(
        app,
        "POST",
        "/api/v3/auth/password-recovery/reset",
        Some(json!({"username": "admin", "recovery_code": code, "new_password": OTHER_PASSWORD})),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        body["error"]["message"],
        json!("Invalid or expired recovery code")
    );

    // Unknown code.
    let app = test_app(&rig, None);
    let (status, body, _) = call(
        app,
        "POST",
        "/api/v3/auth/password-recovery/reset",
        Some(json!({"username": "molly", "recovery_code": "AAAA-BBBB-CCCC-DDDD-EEEE", "new_password": OTHER_PASSWORD})),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        body["error"]["message"],
        json!("Invalid or expired recovery code")
    );

    // Expired code.
    rig.clock
        .set(memory::TEST_NOW + services::RECOVERY_TTL_SECS + 1);
    let app = test_app(&rig, None);
    let (status, _, _) = call(
        app,
        "POST",
        "/api/v3/auth/password-recovery/reset",
        Some(json!({"username": "molly", "recovery_code": code, "new_password": OTHER_PASSWORD})),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn recovery_mint_guards_unknown_and_passwordless_accounts() {
    let (rig, admin, _) = rig_with_admin().await;
    let app = || test_app(&rig, Some(principal_for(&admin, "sess-admin")));

    let (status, _, _) = call(
        app(),
        "POST",
        "/api/v3/admin/users/user-nobody/recovery-code",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let now = clock_now(&rig.deps);
    rig.users
        .insert(models::UserRecord {
            id: "user-nolocal".to_owned(),
            username: Some("nolocal".to_owned()),
            username_display: Some("nolocal".to_owned()),
            display_name: "No Local".to_owned(),
            email: None,
            avatar_url: None,
            role: Role::User,
            created_at: now,
            last_login_at: None,
        })
        .await
        .unwrap();
    let (status, body, _) = call(
        app(),
        "POST",
        "/api/v3/admin/users/user-nolocal/recovery-code",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        body["error"]["message"],
        json!("Local password recovery is not available for this account")
    );
}

// ---------------------------------------------------------------------------
// Per-user Last.fm (R7)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn lastfm_link_flow_per_user() {
    let (rig, _, user) = rig_with_admin().await;
    let app = || test_app(&rig, Some(principal_for(&user, "sess-molly")));

    let (status, body, _) = call(app(), "GET", "/api/v3/me/connections/lastfm", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({"configured": false, "linked": false, "username": null})
    );

    // Token exchange needs stored credentials first.
    let (status, _, _) = call(app(), "POST", "/api/v3/me/connections/lastfm/token", None).await;
    assert_eq!(status, StatusCode::CONFLICT);

    let (status, body, _) = call(
        app(),
        "PUT",
        "/api/v3/me/connections/lastfm",
        Some(json!({"api_key": "user-api-key", "shared_secret": "user-shared-secret"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"configured": true, "linked": false}));
    // No secret material echoes back.
    assert!(!body.to_string().contains("user-api-key"));

    let (status, body, _) = call(app(), "POST", "/api/v3/me/connections/lastfm/token", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["token"], json!("fake-token"));
    assert!(body["auth_url"].as_str().unwrap().contains("last.fm"));

    // Unapproved tokens are a 409 naming the next step, not a 502.
    let (status, body, _) = call(
        app(),
        "POST",
        "/api/v3/me/connections/lastfm/session",
        Some(json!({"token": "fake-token"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("hasn't been approved")
    );

    rig.lastfm_client
        .approve("fake-token", "mollylfm", "session-key-1")
        .await;
    let (status, body, _) = call(
        app(),
        "POST",
        "/api/v3/me/connections/lastfm/session",
        Some(json!({"token": "fake-token"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"username": "mollylfm", "linked": true}));

    let (status, body, _) = call(app(), "GET", "/api/v3/me/connections/lastfm", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({"configured": true, "linked": true, "username": "mollylfm"})
    );

    // Stored secrets are ciphertext at rest.
    let link = rig.lastfm.get(&user.id).await.unwrap().unwrap();
    let key_cipher = link.api_key_encrypted.unwrap();
    let session_cipher = link.session_key_encrypted.unwrap();
    assert!(key_cipher.starts_with("v3:"));
    assert!(session_cipher.starts_with("v3:"));
    assert_eq!(
        rig.deps.crypto.decrypt(&key_cipher).unwrap(),
        "user-api-key"
    );
    assert_eq!(
        rig.deps.crypto.decrypt(&session_cipher).unwrap(),
        "session-key-1"
    );

    // The scrobble wiring point resolves the live session.
    let session = services::lastfm_scrobble_session(&rig.deps, &user.id)
        .await
        .unwrap();
    assert_eq!(session, ("mollylfm".to_owned(), "session-key-1".to_owned()));

    // Mask sentinel keeps the stored key; a real change unlinks.
    let (status, body, _) = call(
        app(),
        "PUT",
        "/api/v3/me/connections/lastfm",
        Some(json!({"api_key": "lastfm****", "shared_secret": "lastfm****"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"configured": true, "linked": true}));

    let (status, body, _) = call(
        app(),
        "PUT",
        "/api/v3/me/connections/lastfm",
        Some(json!({"api_key": "rotated-key", "shared_secret": "lastfm****"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"configured": true, "linked": false}));

    let (status, _, _) = call(app(), "DELETE", "/api/v3/me/connections/lastfm", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, body, _) = call(app(), "GET", "/api/v3/me/connections/lastfm", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["linked"], json!(false));
}

#[tokio::test]
async fn lastfm_upstream_fault_is_a_fixed_502() {
    use droppedneedle::auth::users::stores::LastFmError;

    let (rig, _, user) = rig_with_admin().await;
    rig.lastfm_client
        .fail_token_with(LastFmError::Transport)
        .await;
    let app = || test_app(&rig, Some(principal_for(&user, "sess-molly")));

    let (status, _, _) = call(
        app(),
        "PUT",
        "/api/v3/me/connections/lastfm",
        Some(json!({"api_key": "k", "shared_secret": "s"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body, _) = call(app(), "POST", "/api/v3/me/connections/lastfm/token", None).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(error_code(&body), "UPSTREAM_ERROR");
    assert_eq!(body["error"]["message"], json!("Upstream service error"));
    assert!(body["error"]["details"]["error_id"].is_string());
    assert!(!body.to_string().contains("last.fm"));
}

#[tokio::test]
async fn lastfm_disabled_switch_blocks_link_steps() {
    let (mut rig, _, user) = rig_with_admin().await;
    rig.deps.lastfm_switch = std::sync::Arc::new(memory::StaticLastFmSwitch(false));
    let app = || test_app(&rig, Some(principal_for(&user, "sess-molly")));

    let (status, body, _) = call(
        app(),
        "PUT",
        "/api/v3/me/connections/lastfm",
        Some(json!({"api_key": "k", "shared_secret": "s"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["message"], json!("Last.fm is disabled"));

    // Status stays readable so the UI can explain the state.
    let (status, _, _) = call(app(), "GET", "/api/v3/me/connections/lastfm", None).await;
    assert_eq!(status, StatusCode::OK);

    // The scrobble wiring point degrades to None while disabled.
    assert!(
        services::lastfm_scrobble_session(&rig.deps, &user.id)
            .await
            .is_none()
    );
}

#[tokio::test]
async fn admin_global_lastfm_pair_is_absent() {
    let (rig, admin, _) = rig_with_admin().await;
    let app = test_app(&rig, Some(principal_for(&admin, "sess-admin")));
    for uri in [
        "/api/v3/admin/lastfm/auth",
        "/api/v3/lastfm/auth/token",
        "/api/v3/lastfm/auth/session",
    ] {
        let (status, _, _) = call(app.clone(), "GET", uri, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "route {uri} must not exist");
        let (status, _, _) = call(app.clone(), "POST", uri, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "route {uri} must not exist");
    }
}

/// Minimal 1x1 PNG bytes for avatar briefs.
fn png_bytes() -> Vec<u8> {
    vec![
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x08, 0xD7, 0x63, 0xF8,
        0xFF, 0xFF, 0x3F, 0x00, 0x05, 0xFE, 0x02, 0xFE, 0xDC, 0xCC, 0x59, 0xE7, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ]
}

// ---------------------------------------------------------------------------
// App passwords (+ compat verification contract)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn app_password_crud_with_secret_shown_once() {
    let (rig, _, user) = rig_with_admin().await;
    let app = || test_app(&rig, Some(principal_for(&user, "sess-molly")));

    let (status, created, _) = call(
        app(),
        "POST",
        "/api/v3/me/app-passwords",
        Some(json!({"name": "Symfonium"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["name"], json!("Symfonium"));
    let secret = created["secret"].as_str().unwrap().to_owned();
    assert!(!secret.is_empty());
    let id = created["id"].as_str().unwrap().to_owned();

    let (status, _, _) = call(app(), "POST", "/api/v3/me/app-passwords", Some(json!({}))).await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, body, _) = call(app(), "GET", "/api/v3/me/app-passwords", None).await;
    assert_eq!(status, StatusCode::OK);
    let rows = body["app_passwords"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["name"], json!("Symfonium"));
    assert_eq!(rows[1]["name"], json!("App password"));
    assert!(!body.to_string().contains(&secret));

    let (status, _, _) = call(
        app(),
        "DELETE",
        &format!("/api/v3/me/app-passwords/{id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _, _) = call(app(), "GET", "/api/v3/me/app-passwords", None).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn app_password_revoke_is_owner_scoped_404() {
    let (rig, admin, user) = rig_with_admin().await;
    let other = rig.seed_user("other", Role::User).await;

    let app = test_app(&rig, Some(principal_for(&user, "sess-molly")));
    let (_, created, _) = call(
        app,
        "POST",
        "/api/v3/me/app-passwords",
        Some(json!({"name": "x"})),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_owned();

    rig.seed_session(&other.id, "sess-other").await;
    let app = test_app(&rig, Some(principal_for(&other, "sess-other")));
    let (status, _, _) = call(
        app,
        "DELETE",
        &format!("/api/v3/me/app-passwords/{id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let app = test_app(&rig, Some(principal_for(&user, "sess-molly")));
    let (status, _, _) = call(app, "DELETE", "/api/v3/me/app-passwords/app-unknown", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Admin oversight lists with owners and revokes across users.
    let app = test_app(&rig, Some(principal_for(&admin, "sess-admin")));
    let (status, body, _) = call(app, "GET", "/api/v3/admin/app-passwords", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["app_passwords"][0]["owner_username"], json!("molly"));

    let app = test_app(&rig, Some(principal_for(&admin, "sess-admin")));
    let (status, _, _) = call(
        app,
        "DELETE",
        &format!("/api/v3/admin/app-passwords/{id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let app = test_app(&rig, Some(principal_for(&admin, "sess-admin")));
    let (status, _, _) = call(
        app,
        "DELETE",
        &format!("/api/v3/admin/app-passwords/{id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn app_password_creation_caps_at_25() {
    let (rig, _, user) = rig_with_admin().await;
    let app = || test_app(&rig, Some(principal_for(&user, "sess-molly")));
    for _ in 0..25 {
        let (status, _, _) = call(app(), "POST", "/api/v3/me/app-passwords", Some(json!({}))).await;
        assert_eq!(status, StatusCode::CREATED);
    }
    let (status, body, _) = call(app(), "POST", "/api/v3/me/app-passwords", Some(json!({}))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("limit reached (25)")
    );
}

#[tokio::test]
async fn compat_verify_contract_never_raises_and_stamps_use() {
    let (rig, _, user) = rig_with_admin().await;
    let app = test_app(&rig, Some(principal_for(&user, "sess-molly")));
    let (_, created, _) = call(
        app,
        "POST",
        "/api/v3/me/app-passwords",
        Some(json!({"name": "Finamp"})),
    )
    .await;
    let secret = created["secret"].as_str().unwrap().to_owned();
    let id = created["id"].as_str().unwrap().to_owned();

    // Unknown, empty, and overlong secrets verify as None.
    assert!(
        services::verify_app_password(&rig.deps, "nope", None)
            .await
            .is_none()
    );
    assert!(
        services::verify_app_password(&rig.deps, "", None)
            .await
            .is_none()
    );
    assert!(
        services::verify_app_password(&rig.deps, &"x".repeat(1025), None)
            .await
            .is_none()
    );

    // The live secret resolves to the owner and stamps last use + client.
    let auth = services::verify_app_password(&rig.deps, &secret, Some("Finamp/1.0"))
        .await
        .unwrap();
    assert_eq!(auth.user_id, user.id);
    assert_eq!(auth.app_password_id, id);

    // Revoked rows stop verifying.
    let app = test_app(&rig, Some(principal_for(&user, "sess-molly")));
    let (status, _, _) = call(
        app,
        "DELETE",
        &format!("/api/v3/me/app-passwords/{id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(
        services::verify_app_password(&rig.deps, &secret, None)
            .await
            .is_none()
    );
}

#[tokio::test]
async fn app_passwords_never_verify_on_native_paths() {
    let (rig, _, user) = rig_with_admin().await;
    let app = test_app(&rig, Some(principal_for(&user, "sess-molly")));
    let (_, created, _) = call(
        app,
        "POST",
        "/api/v3/me/app-passwords",
        Some(json!({"name": "Jellify"})),
    )
    .await;
    let secret = created["secret"].as_str().unwrap().to_owned();

    // Compat verification accepts it ...
    assert!(
        services::verify_app_password(&rig.deps, &secret, None)
            .await
            .is_some()
    );
    // ... while the native session lookup (the only credential the sibling
    // middleware consults) resolves nothing for it.
    assert!(
        rig.sessions
            .owner_by_hash(&tokens::hash_token(&secret))
            .await
            .unwrap()
            .is_none()
    );
}

// ---------------------------------------------------------------------------
// Admin users
// ---------------------------------------------------------------------------

#[tokio::test]
async fn admin_user_lifecycle() {
    let (rig, admin, _) = rig_with_admin().await;
    let app = || test_app(&rig, Some(principal_for(&admin, "sess-admin")));

    let (status, body, _) = call(
        app(),
        "POST",
        "/api/v3/admin/users",
        Some(json!({"username": "Newbie", "password": "a fresh password 123", "role": "trusted"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["username"], json!("newbie"));
    assert_eq!(body["username_display"], json!("Newbie"));
    assert_eq!(body["role"], json!("trusted"));
    assert_eq!(body["display_name"], json!("Newbie"));
    let id = body["id"].as_str().unwrap().to_owned();

    // Conflicts stay vague: no username/email oracle.
    let (status, body, _) = call(
        app(),
        "POST",
        "/api/v3/admin/users",
        Some(json!({"username": "newbie", "password": "a fresh password 123"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["message"], json!("Could not create user"));

    let (status, body, _) = call(app(), "GET", "/api/v3/admin/users?limit=1&offset=0", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["users"].as_array().unwrap().len(), 1);
    assert_eq!(body["total"], json!(3));

    let (status, body, _) = call(app(), "GET", &format!("/api/v3/admin/users/{id}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], json!(id));

    let (status, _, _) = call(app(), "GET", "/api/v3/admin/users/user-nobody", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, body, _) = call(
        app(),
        "PUT",
        &format!("/api/v3/admin/users/{id}/role"),
        Some(json!({"role": "user"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["role"], json!("user"));

    let (status, _, _) = call(app(), "DELETE", &format!("/api/v3/admin/users/{id}"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _, _) = call(app(), "GET", &format!("/api/v3/admin/users/{id}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn admin_self_and_last_admin_guards() {
    let (rig, admin, user) = rig_with_admin().await;
    let app = || test_app(&rig, Some(principal_for(&admin, "sess-admin")));

    let (status, _, _) = call(
        app(),
        "PUT",
        &format!("/api/v3/admin/users/{}/role", admin.id),
        Some(json!({"role": "user"})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, _, _) = call(
        app(),
        "DELETE",
        &format!("/api/v3/admin/users/{}", admin.id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Promote a second admin, then demoting the first is allowed.
    let (status, _, _) = call(
        app(),
        "PUT",
        &format!("/api/v3/admin/users/{}/role", user.id),
        Some(json!({"role": "admin"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // The extractor resolves the fresh admin role from the store.
    let other_app = test_app(&rig, Some(principal_for(&user, "sess-molly")));
    let (status, _, _) = call(
        other_app,
        "PUT",
        &format!("/api/v3/admin/users/{}/role", admin.id),
        Some(json!({"role": "user"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // The demoted admin's next request resolves the new role: 403, not admin.
    let (status, _, _) = call(app(), "GET", "/api/v3/admin/users", None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn last_admin_cannot_be_demoted_or_deleted() {
    // Defense in depth below the self-guards: even a non-self caller cannot
    // remove the final admin. Pinned at the service layer (via HTTP the
    // extractor's 403 fires first, since any other admin existing means the
    // target is not the last one).
    let (rig, admin, _) = rig_with_admin().await;
    let ghost = roles::AuthContext {
        user_id: "ghost-admin".to_owned(),
        username: None,
        role: Role::Admin,
        session_id: "sess-ghost".to_owned(),
        session_kind: roles::SessionKind::Standard,
        via_cookie: false,
    };

    match services::admin_set_role(&rig.deps, &ghost, &admin.id, Role::User).await {
        Err(droppedneedle::auth::users::UsersError::Conflict { message }) => {
            assert_eq!(message, "Cannot remove the last admin account");
        }
        other => panic!("expected a last-admin conflict, got {other:?}"),
    }

    match services::admin_delete_user(&rig.deps, &ghost, &admin.id).await {
        Err(droppedneedle::auth::users::UsersError::Conflict { message }) => {
            assert_eq!(message, "Cannot delete the last admin account");
        }
        other => panic!("expected a last-admin conflict, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Breach screening seam
// ---------------------------------------------------------------------------

#[tokio::test]
async fn breached_passwords_reject_when_screening_is_on() {
    use droppedneedle::auth::users::{hibp::HibpScreen, memory::FakeHibpHttp, stores::HibpPolicy};

    let (mut rig, _, user) = rig_with_admin().await;
    // "password" is famously breached; its SHA-1 upper is the planted hit.
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

    let app = test_app(&rig, Some(principal_for(&user, "sess-molly")));
    let (status, _, _) = call(
        app,
        "POST",
        "/api/v3/me/password",
        Some(json!({"current_password": PASSWORD, "new_password": "password12xxxx"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // A clean password still passes with screening on.
    let app = test_app(&rig, Some(principal_for(&user, "sess-molly")));
    let (status, _, _) = call(
        app,
        "POST",
        "/api/v3/me/password",
        Some(json!({"current_password": PASSWORD, "new_password": "a clean password 456"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

// ---------------------------------------------------------------------------
// Fault-injection fakes
// ---------------------------------------------------------------------------

/// User store that fails chosen writes with `Internal`, delegating the rest
/// to the rig's memory store.
struct FailingUserStore {
    inner: Arc<MemoryUserStore>,
    fail_insert: bool,
    fail_credential: bool,
}

fn store_boom<T>() -> BoxFuture<'static, Result<T, StoreError>> {
    Box::pin(async { Err(StoreError::Internal("boom".to_owned())) })
}

impl UserStore for FailingUserStore {
    fn get_by_id<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<models::UserRecord>, StoreError>> {
        self.inner.get_by_id(id)
    }

    fn get_by_username<'a>(
        &'a self,
        username: &'a str,
    ) -> BoxFuture<'a, Result<Option<models::UserRecord>, StoreError>> {
        self.inner.get_by_username(username)
    }

    fn get_by_email<'a>(
        &'a self,
        email: &'a str,
    ) -> BoxFuture<'a, Result<Option<models::UserRecord>, StoreError>> {
        self.inner.get_by_email(email)
    }

    fn get_by_ids<'a>(
        &'a self,
        ids: &'a [String],
    ) -> BoxFuture<'a, Result<Vec<models::UserRecord>, StoreError>> {
        self.inner.get_by_ids(ids)
    }

    fn insert<'a>(&'a self, user: models::UserRecord) -> BoxFuture<'a, Result<(), StoreError>> {
        if self.fail_insert {
            return store_boom();
        }
        self.inner.insert(user)
    }

    fn update_profile<'a>(
        &'a self,
        id: &'a str,
        display_name: Option<&'a str>,
        avatar_url: Option<&'a str>,
    ) -> BoxFuture<'a, Result<bool, StoreError>> {
        self.inner.update_profile(id, display_name, avatar_url)
    }

    fn update_username<'a>(
        &'a self,
        id: &'a str,
        username: &'a str,
        username_display: &'a str,
    ) -> BoxFuture<'a, Result<bool, StoreError>> {
        self.inner.update_username(id, username, username_display)
    }

    fn update_email<'a>(
        &'a self,
        id: &'a str,
        email: Option<&'a str>,
    ) -> BoxFuture<'a, Result<bool, StoreError>> {
        self.inner.update_email(id, email)
    }

    fn set_role<'a>(&'a self, id: &'a str, role: Role) -> BoxFuture<'a, Result<bool, StoreError>> {
        self.inner.set_role(id, role)
    }

    fn touch_login<'a>(&'a self, id: &'a str, at: i64) -> BoxFuture<'a, Result<(), StoreError>> {
        self.inner.touch_login(id, at)
    }

    fn delete<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<bool, StoreError>> {
        self.inner.delete(id)
    }

    fn list<'a>(
        &'a self,
        limit: u64,
        offset: u64,
    ) -> BoxFuture<'a, Result<(Vec<models::UserRecord>, u64), StoreError>> {
        self.inner.list(limit, offset)
    }

    fn count_by_role<'a>(&'a self, role: Role) -> BoxFuture<'a, Result<u64, StoreError>> {
        self.inner.count_by_role(role)
    }

    fn provider_names<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Vec<String>, StoreError>> {
        self.inner.provider_names(id)
    }

    fn local_credential<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<models::LocalCredential>, StoreError>> {
        self.inner.local_credential(id)
    }

    fn insert_local_credential<'a>(
        &'a self,
        credential: models::LocalCredential,
    ) -> BoxFuture<'a, Result<(), StoreError>> {
        if self.fail_credential {
            return store_boom();
        }
        self.inner.insert_local_credential(credential)
    }

    fn replace_local_hash<'a>(
        &'a self,
        id: &'a str,
        expected_hash: &'a str,
        scheme: &'a str,
        new_hash: &'a str,
    ) -> BoxFuture<'a, Result<bool, StoreError>> {
        self.inner
            .replace_local_hash(id, expected_hash, scheme, new_hash)
    }

    fn complete_recovery_reset<'a>(
        &'a self,
        id: &'a str,
        expected_hash: &'a str,
        scheme: &'a str,
        new_hash: &'a str,
    ) -> BoxFuture<'a, Result<bool, StoreError>> {
        self.inner
            .complete_recovery_reset(id, expected_hash, scheme, new_hash)
    }

    fn get_provider_binding<'a>(
        &'a self,
        provider: &'a str,
        provider_uid: &'a str,
    ) -> BoxFuture<'a, Result<Option<ProviderBinding>, StoreError>> {
        self.inner.get_provider_binding(provider, provider_uid)
    }

    fn insert_provider_binding<'a>(
        &'a self,
        binding: ProviderBinding,
    ) -> BoxFuture<'a, Result<(), StoreError>> {
        self.inner.insert_provider_binding(binding)
    }
}

/// Hasher whose Argon2id entry point always fails, pinning the hashing
/// error path without touching the OS random source.
struct FailingHasher;

impl PasswordHasher for FailingHasher {
    fn verify_bcrypt(&self, _password: &str, _hash: &str) -> bool {
        false
    }

    fn verify_argon2id(&self, _password: &str, _hash: &str) -> bool {
        false
    }

    fn hash_argon2id(&self, _password: &str) -> String {
        "unpersistable".to_owned()
    }

    fn dummy_verify(&self) {}
}

impl FalliblePasswordHasher for FailingHasher {
    fn try_hash_argon2id(&self, _password: &str) -> Result<String, HashError> {
        Err(HashError::RngUnavailable)
    }
}

// ---------------------------------------------------------------------------
// Fix-up briefs: sessions, recovery, creation faults, hashing, Last.fm
// ---------------------------------------------------------------------------

#[tokio::test]
async fn companion_concurrent_remint_leaves_exactly_one_live_row() {
    let (rig, _, user) = rig_with_admin().await;
    let mut handles = Vec::new();
    for n in 0..8u32 {
        let sessions = Arc::clone(&rig.sessions);
        let user_id = user.id.clone();
        handles.push(tokio::spawn(async move {
            let id = format!("sess-race-{n}");
            let digest = tokens::hash_token(&format!("race-token-{n}"));
            sessions
                .replace_companion(
                    &id,
                    &user_id,
                    &digest,
                    "phone",
                    memory::TEST_NOW,
                    memory::TEST_NOW + tokens::SESSION_MAX_AGE_SECS,
                )
                .await
        }));
    }
    for handle in handles {
        handle.await.unwrap().unwrap();
    }
    let rows = rig.sessions.list_for_user(&user.id).await.unwrap();
    assert_eq!(
        rows.iter()
            .filter(|row| row.kind == roles::SessionKind::Companion)
            .count(),
        1,
        "concurrent remints converge on one live same-label row"
    );
}

#[tokio::test]
async fn recovery_reset_revokes_all_prior_sessions() {
    let (rig, admin, user) = rig_with_admin().await;
    rig.seed_session(&user.id, "sess-extra").await;

    let app = test_app(&rig, Some(principal_for(&admin, "sess-admin")));
    let (status, mint, _) = call(
        app,
        "POST",
        &format!("/api/v3/admin/users/{}/recovery-code", user.id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let code = mint["recovery_code"].as_str().unwrap().to_owned();

    let app = test_app(&rig, None);
    let (status, _, _) = call(
        app,
        "POST",
        "/api/v3/auth/password-recovery/reset",
        Some(json!({"username": "molly", "recovery_code": code, "new_password": OTHER_PASSWORD})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Both prior sessions are dead; the new password verifies.
    assert!(
        rig.sessions
            .list_for_user(&user.id)
            .await
            .unwrap()
            .is_empty()
    );
    let cred = rig.users.local_credential(&user.id).await.unwrap().unwrap();
    assert!(
        rig.deps
            .passwords
            .verify_argon2id(OTHER_PASSWORD, &cred.hash)
    );
}

#[tokio::test]
async fn admin_create_store_fault_is_a_fixed_500() {
    let (mut rig, admin, _) = rig_with_admin().await;
    rig.deps.users = Arc::new(FailingUserStore {
        inner: Arc::clone(&rig.users),
        fail_insert: true,
        fail_credential: false,
    });
    let app = test_app(&rig, Some(principal_for(&admin, "sess-admin")));
    let (status, body, _) = call(
        app,
        "POST",
        "/api/v3/admin/users",
        Some(json!({"username": "newbie", "password": "a fresh password 123"})),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(error_code(&body), "INTERNAL_ERROR");
    assert_eq!(body["error"]["message"], json!("Internal server error"));
    assert!(body["error"]["details"]["error_id"].is_string());
}

#[tokio::test]
async fn admin_create_compensates_credential_failure() {
    let (mut rig, admin, _) = rig_with_admin().await;
    rig.deps.users = Arc::new(FailingUserStore {
        inner: Arc::clone(&rig.users),
        fail_insert: false,
        fail_credential: true,
    });
    let app = test_app(&rig, Some(principal_for(&admin, "sess-admin")));
    let (status, _, _) = call(
        app,
        "POST",
        "/api/v3/admin/users",
        Some(json!({"username": "newbie", "password": "a fresh password 123"})),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        rig.users.get_by_username("newbie").await.unwrap().is_none(),
        "no orphaned passwordless row survives a credential failure"
    );
}

#[tokio::test]
async fn password_hash_failure_fails_the_request() {
    use droppedneedle::auth::users::UsersError;

    let (mut rig, _, _) = rig_with_admin().await;
    rig.deps.passwords = Arc::new(FailingHasher);
    match services::admin_create_user(
        &rig.deps,
        "newbie",
        "a fresh password 123",
        None,
        None,
        Role::User,
    )
    .await
    {
        Err(UsersError::Internal { .. }) => {}
        other => panic!("expected a fixed 500, got {other:?}"),
    }
    assert!(rig.users.get_by_username("newbie").await.unwrap().is_none());
}

#[tokio::test]
async fn lastfm_resubmit_identical_plaintext_keeps_link() {
    let (rig, _, user) = rig_with_admin().await;
    let app = || test_app(&rig, Some(principal_for(&user, "sess-molly")));

    let (status, _, _) = call(
        app(),
        "PUT",
        "/api/v3/me/connections/lastfm",
        Some(json!({"api_key": "user-api-key", "shared_secret": "user-shared-secret"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    rig.lastfm_client
        .approve("fake-token", "mollylfm", "session-key-1")
        .await;
    let (status, _, _) = call(
        app(),
        "POST",
        "/api/v3/me/connections/lastfm/session",
        Some(json!({"token": "fake-token"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Resubmitting the identical plaintext (not the mask) keeps the link:
    // the change check compares decrypted values, not ciphertext.
    let (status, body, _) = call(
        app(),
        "PUT",
        "/api/v3/me/connections/lastfm",
        Some(json!({"api_key": "user-api-key", "shared_secret": "user-shared-secret"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"configured": true, "linked": true}));
    let (status, body, _) = call(app(), "GET", "/api/v3/me/connections/lastfm", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["username"], json!("mollylfm"));

    // A real change still unlinks.
    let (status, body, _) = call(
        app(),
        "PUT",
        "/api/v3/me/connections/lastfm",
        Some(json!({"api_key": "user-api-key", "shared_secret": "rotated-secret"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"configured": true, "linked": false}));
}

// ---------------------------------------------------------------------------
// Admin user import (Jellyfin/Plex)
// ---------------------------------------------------------------------------

fn directory_user(
    uid: &str,
    display_name: &str,
    email: Option<&str>,
    avatar_url: Option<&str>,
) -> DirectoryUser {
    DirectoryUser {
        provider_uid: uid.to_owned(),
        display_name: display_name.to_owned(),
        avatar_url: avatar_url.map(str::to_owned),
        email: email.map(str::to_owned),
    }
}

#[tokio::test]
async fn import_lists_candidates_with_imported_flags() {
    let (rig, admin, _) = rig_with_admin().await;
    rig.users
        .insert_provider_binding(ProviderBinding {
            id: "bind-1".to_owned(),
            user_id: admin.id.clone(),
            provider: "jellyfin".to_owned(),
            provider_uid: "jf-1".to_owned(),
        })
        .await
        .unwrap();

    let jellyfin = FakeUserDirectory::jellyfin(vec![
        directory_user(
            "jf-1",
            "JF One",
            None,
            Some("https://jf/Users/jf-1/Images/Primary"),
        ),
        directory_user("jf-2", "JF Two", None, None),
    ]);
    let listed = import::list_import_candidates(&rig.deps, &jellyfin)
        .await
        .unwrap();
    assert_eq!(listed.candidates.len(), 2);
    assert_eq!(listed.candidates[0].provider, "jellyfin");
    assert!(listed.candidates[0].already_imported);
    assert!(!listed.candidates[1].already_imported);

    let plex = FakeUserDirectory::plex(vec![directory_user(
        "px-1",
        "Plex One",
        Some("px1@example.com"),
        Some("https://plex/thumb/1"),
    )]);
    let listed = import::list_import_candidates(&rig.deps, &plex)
        .await
        .unwrap();
    assert_eq!(
        listed.candidates[0].email.as_deref(),
        Some("px1@example.com")
    );
    assert!(!listed.candidates[0].already_imported);

    // Directory faults are 503s with the federated fixed body.
    jellyfin.fail_with(DirectoryError::Transport).await;
    match import::list_import_candidates(&rig.deps, &jellyfin).await {
        Err(ImportError::Unavailable { .. }) => {}
        other => panic!("expected a 503, got {other:?}"),
    }
}

#[tokio::test]
async fn import_creates_links_and_skips() {
    let (rig, _, _) = rig_with_admin().await;
    let linked = rig.seed_user("linked", Role::User).await;
    rig.users
        .update_email(&linked.id, Some("sam@example.com"))
        .await
        .unwrap();

    let plex = FakeUserDirectory::plex(vec![
        directory_user(
            "px-new",
            "New Person",
            Some("new@example.com"),
            Some("https://plex/thumb/new"),
        ),
        directory_user("px-link", "Sam Linked", Some("sam@example.com"), None),
    ]);
    let batch = import::import_users(
        &rig.deps,
        &plex,
        &[
            "px-new".to_owned(),
            "px-link".to_owned(),
            "px-unknown".to_owned(),
            "px-new".to_owned(),
        ],
    )
    .await
    .unwrap();
    assert_eq!(batch.total_imported, 1);
    assert_eq!(batch.imported.len(), 1);
    assert_eq!(batch.linked.len(), 1);
    assert_eq!(
        batch.skipped,
        vec!["px-unknown".to_owned(), "px-new".to_owned()]
    );

    // The created account: forced role, kept email and thumb, bound provider.
    let created = &batch.imported[0];
    assert_eq!(created.role, Role::User);
    assert_eq!(created.email.as_deref(), Some("new@example.com"));
    assert_eq!(
        created.avatar_url.as_deref(),
        Some("https://plex/thumb/new")
    );
    assert_eq!(created.providers, vec!["plex".to_owned()]);

    // The linked account is untouched: same id, name, and username, with the
    // new provider added next to its local one.
    assert_eq!(batch.linked[0].id, linked.id);
    let reread = rig.users.get_by_id(&linked.id).await.unwrap().unwrap();
    assert_eq!(reread.display_name, "linked");
    assert_eq!(reread.username.as_deref(), Some("linked"));
    let names = rig.users.provider_names(&linked.id).await.unwrap();
    assert_eq!(names, vec!["local".to_owned(), "plex".to_owned()]);
}

#[tokio::test]
async fn import_derives_usernames_with_suffix_dedup() {
    let (rig, _, _) = rig_with_admin().await;
    rig.seed_user("jane", Role::User).await;

    let jellyfin = FakeUserDirectory::jellyfin(vec![directory_user(
        "jf-9",
        "Jane",
        None,
        Some("https://jf/Users/jf-9/Images/Primary"),
    )]);
    let batch = import::import_users(&rig.deps, &jellyfin, &["jf-9".to_owned()])
        .await
        .unwrap();
    assert_eq!(batch.total_imported, 1);
    assert_eq!(
        batch.imported[0].username.as_deref(),
        Some("jane-2"),
        "taken display base dedups with a numeric suffix"
    );
    assert_eq!(
        batch.imported[0].username_display.as_deref(),
        Some("Jane-2")
    );
    assert!(
        batch.imported[0].avatar_url.is_none(),
        "the unguarded Jellyfin picker URL never persists"
    );
}

#[tokio::test]
async fn import_endpoints_are_admin_only_and_503_without_live_clients() {
    let (rig, admin, user) = rig_with_admin().await;

    for (method, uri, body) in [
        ("GET", "/api/v3/admin/import/jellyfin", None),
        ("GET", "/api/v3/admin/import/plex", None),
        (
            "POST",
            "/api/v3/admin/import",
            Some(json!({"provider": "plex", "provider_uids": []})),
        ),
    ] {
        let (status, _, _) = call(test_app(&rig, None), method, uri, body.clone()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri} anon");
        let app = test_app(&rig, Some(principal_for(&user, "sess-molly")));
        let (status, _, _) = call(app, method, uri, body).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri} user");
    }

    // No live client yet: every admin call is an honest fixed-body 503.
    let app = || test_app(&rig, Some(principal_for(&admin, "sess-admin")));
    for (method, uri, body) in [
        ("GET", "/api/v3/admin/import/jellyfin", None),
        ("GET", "/api/v3/admin/import/plex", None),
        (
            "POST",
            "/api/v3/admin/import",
            Some(json!({"provider": "plex", "provider_uids": ["px-1"]})),
        ),
    ] {
        let (status, response, _) = call(app(), method, uri, body).await;
        assert_eq!(
            status,
            StatusCode::SERVICE_UNAVAILABLE,
            "{method} {uri} admin"
        );
        assert_eq!(error_code(&response), "UPSTREAM_ERROR");
        assert_eq!(
            response["error"]["message"],
            json!("Upstream service error")
        );
        assert!(response["error"]["details"]["error_id"].is_string());
    }
}

#[tokio::test]
async fn import_rejects_unknown_provider_with_400() {
    let (rig, admin, _) = rig_with_admin().await;
    let app = test_app(&rig, Some(principal_for(&admin, "sess-admin")));
    let (status, body, _) = call(
        app,
        "POST",
        "/api/v3/admin/import",
        Some(json!({"provider": "okta", "provider_uids": ["u1"]})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body["error"]["message"],
        json!("Unsupported import provider")
    );
}
