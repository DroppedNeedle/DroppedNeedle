//! Shared compat infrastructure briefs (stage 9, shared slice).
//!
//! Brief-first: each test states the contract, then pins it. Auth
//! behavior reuses the stage-3 store (`FakeCompatPasswords`) — this
//! slice adds no credential logic of its own.

use droppedneedle::auth::compat_auth::{fakes::FakeCompatPasswords, jellyfin, subsonic};
use droppedneedle::compat::shared::{auth, cors, extensions, path_case, ratelimit, redact};

fn seeded_store() -> FakeCompatPasswords {
    let store = FakeCompatPasswords::new();
    store.add_user(
        "user-1",
        "ada",
        "Ada",
        "user",
        "account-password-never-verifies",
        &["app-secret-1"],
    );
    store
}

fn params(entries: Vec<(&str, &str)>) -> subsonic::SubsonicParams {
    subsonic::SubsonicParams::new(entries)
}

// --- Subsonic auth matrix (stage-3 schemes, brief order 10/40/43/44/50/70) ---

#[tokio::test]
async fn brief_subsonic_token_password_and_apikey_all_verify() {
    let store = seeded_store();
    let salt = "pepper";
    let token = subsonic::md5_hex(&format!("app-secret-1{salt}"));
    let via_token = subsonic::authenticate(
        &store,
        &params(vec![
            ("u", "ada"),
            ("t", &token),
            ("s", salt),
            ("c", "feishin"),
        ]),
    )
    .await
    .expect("token scheme verifies");
    assert_eq!(via_token.user_id, "user-1");

    let via_password =
        subsonic::authenticate(&store, &params(vec![("u", "ada"), ("p", "app-secret-1")]))
            .await
            .expect("password scheme verifies");
    assert_eq!(via_password.user_id, "user-1");

    let via_key = subsonic::authenticate(&store, &params(vec![("apiKey", "app-secret-1")]))
        .await
        .expect("lone apiKey verifies");
    assert_eq!(via_key.user_id, "user-1");
}

#[tokio::test]
async fn brief_subsonic_code_10_conflicts_and_duplicates() {
    let store = seeded_store();
    for entries in [
        vec![("u", "ada"), ("t", "abc")],
        vec![("u", "ada"), ("s", "abc")],
        vec![("u", "ada"), ("u", "ada"), ("p", "app-secret-1")],
        vec![("u", "ada"), ("p", "app-secret-1"), ("t", "a"), ("s", "b")],
        vec![("apiKey", "app-secret-1"), ("p", "app-secret-1")],
        vec![("u", "ada")],
    ] {
        let denied = subsonic::authenticate(&store, &params(entries))
            .await
            .expect_err("conflict must deny");
        assert_eq!(denied.code, subsonic::PARAM_MISSING);
    }
}

#[tokio::test]
async fn brief_subsonic_code_40_bad_credential() {
    let store = seeded_store();
    for entries in [
        vec![("u", "ada"), ("p", "wrong")],
        vec![("u", "ada"), ("t", &"0".repeat(32)), ("s", "salt")],
        vec![("u", "nobody"), ("p", "app-secret-1")],
        vec![("u", "ada"), ("p", "account-password-never-verifies")],
    ] {
        let denied = subsonic::authenticate(&store, &params(entries))
            .await
            .expect_err("bad credential must deny");
        assert_eq!(denied.code, subsonic::WRONG_CREDENTIALS);
    }
}

#[tokio::test]
async fn brief_subsonic_code_43_apikey_plus_user() {
    let store = seeded_store();
    let denied = subsonic::authenticate(
        &store,
        &params(vec![("u", "ada"), ("apiKey", "app-secret-1")]),
    )
    .await
    .expect_err("apiKey plus u must deny");
    assert_eq!(denied.code, subsonic::CONFLICTING_AUTH);
}

#[tokio::test]
async fn brief_subsonic_code_44_bad_apikey() {
    let store = seeded_store();
    let denied = subsonic::authenticate(&store, &params(vec![("apiKey", "nope")]))
        .await
        .expect_err("bad apiKey must deny");
    assert_eq!(denied.code, subsonic::INVALID_APIKEY);
}

#[test]
fn brief_subsonic_code_50_stays_enveloped_on_binary_dispatch() {
    assert!(subsonic::dispatch_uses_envelope(
        subsonic::NOT_AUTHORIZED,
        "stream"
    ));
    let rendered = subsonic::SubsonicDenied::new(subsonic::NOT_AUTHORIZED).render(
        subsonic::SubsonicFormat::Json,
        None,
        "DroppedNeedle",
        "3.0.0",
    );
    assert_eq!(rendered.status, 200);
    assert_eq!(rendered.content_type, "application/json");
    assert!(rendered.body_text().contains("\"code\":50"));
}

#[test]
fn brief_subsonic_code_70_goes_text_on_binary_dispatch() {
    assert!(!subsonic::dispatch_uses_envelope(
        subsonic::NOT_FOUND,
        "stream"
    ));
    let rendered =
        subsonic::render_binary_error(subsonic::NOT_FOUND, "The requested data was not found.");
    assert_eq!(rendered.status, 404);
    assert_eq!(rendered.content_type, "text/plain");
}

#[test]
fn brief_subsonic_getavatar_is_the_only_text_403() {
    assert!(!subsonic::avatar_is_self(&["ada", "Ada"], Some("grace")));
    let rendered =
        subsonic::render_binary_error(subsonic::NOT_AUTHORIZED, subsonic::AVATAR_FORBIDDEN_MESSAGE);
    assert_eq!(rendered.status, 403);
    assert_eq!(rendered.content_type, "text/plain");
    assert!(subsonic::avatar_is_self(&["ada", "Ada"], Some("ADA")));
}

// --- Jellyfin auth matrix ---

#[tokio::test]
async fn brief_jellyfin_401_missing_or_bad_token() {
    let store = seeded_store();
    for token in [None, Some(""), Some("bogus")] {
        let denied = jellyfin::resolve_token(&store, token)
            .await
            .expect_err("must deny");
        assert_eq!(denied.status(), jellyfin::UNAUTHORIZED);
        assert!(denied.body().is_empty());
    }
    let user = jellyfin::resolve_token(&store, Some("app-secret-1"))
        .await
        .expect("valid token resolves");
    assert_eq!(user.id, "user-1");
}

#[test]
fn brief_jellyfin_token_extraction_order() {
    let header = jellyfin::JellyfinRequest {
        authorization: Some("MediaBrowser Token=\"from-header\", Client=\"Finamp\""),
        emby_token: Some("from-direct-header"),
        query_apikey: Some("from-query"),
        ..Default::default()
    };
    assert_eq!(
        jellyfin::extract_token(&header).as_deref(),
        Some("from-header")
    );
    let direct = jellyfin::JellyfinRequest {
        emby_token: Some("from-direct-header"),
        query_apikey: Some("from-query"),
        ..Default::default()
    };
    assert_eq!(
        jellyfin::extract_token(&direct).as_deref(),
        Some("from-direct-header")
    );
    let query = jellyfin::JellyfinRequest {
        query_api_key: Some("from-query"),
        ..Default::default()
    };
    assert_eq!(
        jellyfin::extract_token(&query).as_deref(),
        Some("from-query")
    );
}

#[tokio::test]
async fn brief_jellyfin_login_echoes_password_verbatim() {
    let store = seeded_store();
    let user = jellyfin::authenticate_by_name(&store, "ada", "app-secret-1", Some("Finamp"))
        .await
        .expect("login verifies");
    let echo = jellyfin::login_echo_json(
        &user,
        "app-secret-1",
        &jellyfin::SessionFacts {
            id: "session-hex".to_owned(),
            client: Some("Finamp".to_owned()),
            device_name: None,
            device_id: None,
            last_activity: "2026-09-28T12:00:00.123456+00:00".to_owned(),
        },
    );
    assert!(echo.contains("\"AccessToken\":\"app-secret-1\""));
    assert!(echo.contains("\"ServerId\":\""));
    assert_eq!(jellyfin::server_id().len(), 32);
    jellyfin::authenticate_by_name(&store, "ada", "wrong", None)
        .await
        .expect_err("bad login denies");
}

#[test]
fn brief_jellyfin_anon_set() {
    let anon = [
        ("GET", "/jellyfin/System/Info/Public"),
        ("GET", "/jellyfin/QuickConnect/Enabled"),
        ("POST", "/jellyfin/Sessions/Logout"),
        ("POST", "/jellyfin/Users/AuthenticateByName"),
        ("POST", "/jellyfin/users/authenticatebyname"),
        ("GET", "/jellyfin/Items/abc/Images/Primary"),
        ("GET", "/jellyfin/Items/abc/Images/Primary/0"),
        ("GET", "/jellyfin/Audio/abc/universal"),
        ("GET", "/jellyfin/Audio/abc/stream"),
        ("GET", "/jellyfin/Audio/abc/stream.mp3"),
        ("HEAD", "/jellyfin/Audio/abc/stream"),
    ];
    for (method, path) in anon {
        assert!(auth::jellyfin_is_anonymous(method, path), "{method} {path}");
    }
    let authed = [
        ("GET", "/jellyfin/System/Info"),
        ("GET", "/jellyfin/Users/me"),
        ("GET", "/jellyfin/Items"),
        ("GET", "/jellyfin/UserItems/Latest"),
        ("GET", "/jellyfin/Items/abc"),
        ("POST", "/jellyfin/Items/abc/PlaybackInfo"),
        ("GET", "/jellyfin/Audio/abc"),
        ("DELETE", "/jellyfin/Items/abc/Images/Primary"),
    ];
    for (method, path) in authed {
        assert!(
            !auth::jellyfin_is_anonymous(method, path),
            "{method} {path}"
        );
    }
    assert!(auth::subsonic_is_public("getOpenSubsonicExtensions"));
    assert!(auth::subsonic_is_public("getopensubsonicextensions.view"));
    assert!(!auth::subsonic_is_public("stream"));
    assert!(!auth::subsonic_is_public("ping"));
}

// --- Kill switches ---
//
// The gates live in the slices (`Settings.enabled` per protocol), pinned at
// the production edge in `compat_wiring.rs` (`kill_switches_default_off`);
// there is no shared enablement helper left to brief here.

// --- Redaction ---

#[test]
fn brief_redaction_masks_secrets_keeps_rest() {
    assert_eq!(
        redact::redact_request_target("/subsonic/rest/ping?u=ada&p=hunter2&t=tok&s=salt"),
        "/subsonic/rest/ping?u=ada&p=***&t=***&s=***"
    );
    assert_eq!(
        redact::redact_request_target("/jellyfin/Audio/x/stream?api_key=secret&static=true"),
        "/jellyfin/Audio/x/stream?api_key=***&static=true"
    );
    assert_eq!(
        redact::redact_request_target("/subsonic/rest/stream?transcodeParams=blob&id=tr-1"),
        "/subsonic/rest/stream?transcodeParams=***&id=tr-1"
    );
    assert_eq!(
        redact::redact_request_target(
            "/subsonic/rest/ping?P=A&T=B&ApiKey=C&TOKEN=D&PW=E&PASSWORD=F"
        ),
        "/subsonic/rest/ping?P=***&T=***&ApiKey=***&TOKEN=***&PW=***&PASSWORD=***"
    );
    assert_eq!(
        redact::redact_request_target("/subsonic/rest/ping?%70=%68i&id=%70"),
        "/subsonic/rest/ping?p=***&id=p"
    );
    assert_eq!(
        redact::redact_request_target("/subsonic/rest/ping?id=1&id=2&p=x"),
        "/subsonic/rest/ping?id=1&id=2&p=***"
    );
    assert_eq!(
        redact::redact_request_target("/subsonic/rest/ping"),
        "/subsonic/rest/ping"
    );
}

// --- CORS ---

#[test]
fn brief_cors_star_creds_off_preflight_pre_auth() {
    const {
        assert!(!cors::ALLOWS_CREDENTIALS);
    }
    assert!(cors::is_preflight("OPTIONS", "/subsonic/rest/ping"));
    assert!(cors::is_preflight(
        "OPTIONS",
        "/jellyfin/System/Info/Public"
    ));
    assert!(!cors::is_preflight("GET", "/subsonic/rest/ping"));
    assert!(!cors::is_preflight("OPTIONS", "/api/users"));
    assert_eq!(cors::PREFLIGHT_STATUS, 204);
    let origin = cors::HEADERS
        .iter()
        .find(|(key, _)| *key == "Access-Control-Allow-Origin")
        .map(|(_, value)| *value);
    assert_eq!(origin, Some("*"));
    assert!(
        cors::HEADERS
            .iter()
            .all(|(key, _)| *key != "Access-Control-Allow-Credentials")
    );
}

// --- Case-insensitive paths ---

#[test]
fn brief_paths_casefold_to_registered_casing() {
    let routes = [
        "/jellyfin/Users/AuthenticateByName",
        "/jellyfin/System/Info/Public",
        "/jellyfin/Audio/{id}/stream",
    ];
    assert_eq!(
        path_case::canonicalize(&routes, "/jellyfin/users/authenticatebyname").as_deref(),
        Some("/jellyfin/Users/AuthenticateByName")
    );
    assert_eq!(
        path_case::canonicalize(&routes, "/jellyfin/System/Info/Public"),
        None
    );
    assert_eq!(
        path_case::canonicalize(&routes, "/JELLYFIN/audio/AbC/stReAm").as_deref(),
        Some("/jellyfin/Audio/AbC/stream")
    );
    assert_eq!(path_case::canonicalize(&routes, "/api/Users"), None);
}

// --- Rate limits + backoff ---

#[test]
fn brief_limits_public_burst_then_429_with_retry_after() {
    let mut limits = ratelimit::CompatRateLimits::new();
    for _ in 0..20 {
        assert_eq!(limits.public_retry_after("10.0.0.1", 0.0), None);
    }
    let retry = limits.public_retry_after("10.0.0.1", 0.0);
    assert!(retry.is_some_and(|secs| secs >= 1));
    assert_eq!(limits.public_retry_after("10.0.0.2", 0.0), None);
}

#[test]
fn brief_limits_principal_browse_looser_than_mutation() {
    let mut limits = ratelimit::CompatRateLimits::new();
    for _ in 0..120 {
        assert_eq!(limits.principal_retry_after("user:1", false, 0.0), None);
    }
    assert!(limits.principal_retry_after("user:1", false, 0.0).is_some());
    for _ in 0..20 {
        assert_eq!(limits.principal_retry_after("user:1", true, 0.0), None);
    }
    assert!(limits.principal_retry_after("user:1", true, 0.0).is_some());
}

#[test]
fn brief_backoff_five_failures_lock_out_then_double() {
    let mut limits = ratelimit::CompatRateLimits::new();
    for second in 0..4 {
        assert_eq!(
            auth::record_auth_denial(&mut limits, "10.0.0.9", f64::from(second)),
            None
        );
    }
    assert_eq!(
        auth::record_auth_denial(&mut limits, "10.0.0.9", 4.0),
        Some(10)
    );
    assert_eq!(
        auth::auth_locked_out(&mut limits, "10.0.0.9", 4.0),
        Some(10)
    );
    assert_eq!(auth::auth_locked_out(&mut limits, "10.0.0.9", 5.0), Some(9));
    assert_eq!(auth::auth_locked_out(&mut limits, "10.0.0.9", 15.0), None);
    for second in 20..24 {
        auth::record_auth_denial(&mut limits, "10.0.0.9", f64::from(second));
    }
    assert_eq!(
        auth::record_auth_denial(&mut limits, "10.0.0.9", 24.0),
        Some(20)
    );
}

#[test]
fn brief_limits_media_exempt_mutation_classified() {
    assert!(ratelimit::is_media_request("/subsonic/rest/stream.view"));
    assert!(ratelimit::is_media_request("/subsonic/rest/download"));
    assert!(!ratelimit::is_media_request("/subsonic/rest/getCoverArt"));
    assert!(ratelimit::is_media_request("/jellyfin/audio/abc/stream"));
    assert!(!ratelimit::is_media_request("/jellyfin/Items"));
    assert!(ratelimit::is_mutation_request("DELETE", "/jellyfin/x"));
    assert!(ratelimit::is_mutation_request(
        "POST",
        "/subsonic/rest/star"
    ));
    assert!(!ratelimit::is_mutation_request(
        "POST",
        "/jellyfin/Users/AuthenticateByName"
    ));
    assert!(!ratelimit::is_mutation_request(
        "POST",
        "/jellyfin/Items/abc/PlaybackInfo"
    ));
    assert!(!ratelimit::is_mutation_request("GET", "/jellyfin/Items"));
    assert_eq!(
        auth::principal_label(Some("user-1"), "10.0.0.1"),
        "user:user-1"
    );
    assert_eq!(auth::principal_label(None, "10.0.0.1"), "ip:10.0.0.1");
    assert_eq!(auth::principal_label(Some(""), "10.0.0.1"), "ip:10.0.0.1");
}

// --- Guards ---
//
// Deleted: the shared guard duplicates had zero production callers (the
// slices carry the single copies), so their pins moved with them. The
// transcode-hint mapping survives in exactly one spelling
// (`subsonic::stream::transcode_hint`), briefed by the Subsonic suite's
// `song_child_carries_transcode_hint_when_enabled`.

// --- Extensions: the ONE choice ---

#[test]
fn brief_extensions_matrix_wins_transcoding_unadvertised() {
    assert_eq!(extensions::ADVERTISED.len(), 3);
    assert!(extensions::is_advertised("apiKeyAuthentication"));
    assert!(extensions::is_advertised("formPost"));
    assert!(extensions::is_advertised("transcodeOffset"));
    assert!(!extensions::is_advertised("transcoding"));
    assert!(
        extensions::IMPLEMENTED_UNADVERTISED
            .iter()
            .any(|ext| ext.name == "transcoding")
    );
    assert!(extensions::NEVER_ADVERTISED.contains(&"podcasts"));
    assert!(extensions::NEVER_ADVERTISED.contains(&"video"));
}

#[test]
fn brief_limits_clear_and_reset_release_buckets() {
    let mut limits = ratelimit::CompatRateLimits::new();
    for _ in 0..120 {
        assert_eq!(limits.principal_retry_after("user:7", false, 0.0), None);
    }
    assert!(limits.principal_retry_after("user:7", false, 0.0).is_some());
    limits.clear_principal("user:7");
    assert_eq!(limits.principal_retry_after("user:7", false, 0.0), None);
    limits.reset();
    assert_eq!(auth::auth_locked_out(&mut limits, "10.0.0.9", 0.0), None);
}
