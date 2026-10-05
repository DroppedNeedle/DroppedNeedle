//! Platform routes: cover headers, ETag revalidation, placeholders and
//! warming, and the wrapped API key gate (exact match, no trimming, no
//! key material in rejections).

use droppedneedle::reads::platform;

use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use platform::{
    covers::{CoversState, FakeCoverArt},
    wrapped::{
        FakeWrappedData, ServerWrappedResponse, UserWrappedResponse, WrappedAlbum, WrappedArtist,
        WrappedGenre, WrappedState, WrappedTrack, WrappedUserSummary,
    },
};
use tower::ServiceExt as _;

/// 1x1 PNG standing in for fetched art.
const PNG_BYTES: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0a, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae,
    0x42, 0x60, 0x82,
];

const WRAPPED_KEY: &str = "test-secret-key";

fn covers_app(fake: FakeCoverArt) -> Router {
    platform::covers::routes(CoversState::new(Arc::new(fake)))
}

fn covers_fixture() -> FakeCoverArt {
    FakeCoverArt::empty()
        .with_release_group(
            "rg-1",
            Some("500"),
            PNG_BYTES.to_vec(),
            "image/png",
            "audiodb",
        )
        .with_release_group("rg-orig", None, PNG_BYTES.to_vec(), "image/png", "audiodb")
        .with_release(
            "rel-caa",
            Some("500"),
            PNG_BYTES.to_vec(),
            "image/jpeg",
            "cover-art-archive",
        )
        .with_artist("art-1", None, PNG_BYTES.to_vec(), "image/png", "audiodb")
        .warming_release_group("rg-warm", Some("500"))
        .warming_release("rel-warm")
        .warming_artist("art-warm", None)
}

fn user_payload() -> UserWrappedResponse {
    UserWrappedResponse {
        user_id: "u-1".to_owned(),
        display_name: "Ada".to_owned(),
        year: 2026,
        has_data: true,
        top_artists: vec![WrappedArtist {
            name: "A".to_owned(),
            listen_count: 42,
            artist_mbid: None,
        }],
        top_tracks: vec![WrappedTrack {
            name: "T".to_owned(),
            artist_name: "A".to_owned(),
            listen_count: 10,
        }],
        top_albums: vec![WrappedAlbum {
            name: "B".to_owned(),
            artist_name: "A".to_owned(),
            listen_count: 8,
            mbid: None,
        }],
        top_genres: vec![WrappedGenre {
            genre: "rock".to_owned(),
            listen_count: 50,
        }],
        loved_tracks_count: 7,
        total_listens_estimated: 42,
    }
}

fn wrapped_app(api_key: &str) -> Router {
    let mut per_user = HashMap::new();
    per_user.insert("u-1".to_owned(), user_payload());
    let data = FakeWrappedData::new(
        2026,
        vec![WrappedUserSummary {
            id: "u-1".to_owned(),
            display_name: "Ada".to_owned(),
            has_listenbrainz: true,
            email: Some("ada@example.invalid".to_owned()),
        }],
        per_user,
        ServerWrappedResponse {
            year: 2026,
            total_users_tracked: 1,
            total_listens_estimated: 42,
            leaderboard: vec![platform::wrapped::WrappedLeaderboardEntry {
                display_name: "Ada".to_owned(),
                listen_count: 42,
            }],
            top_artist_sitewide: None,
            top_album_sitewide: None,
        },
    );
    platform::wrapped::routes(WrappedState::new(api_key.to_owned(), Arc::new(data)))
}

async fn get(
    app: Router,
    uri: &str,
    headers: &[(&str, &str)],
) -> (StatusCode, Vec<(String, String)>, Vec<u8>) {
    let mut builder = Request::builder().uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = app
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers: Vec<(String, String)> = response
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_owned()))
        .collect();
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap()
        .to_vec();
    (status, headers, body)
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

// Covers.

#[tokio::test]
async fn release_group_cover_hit_serves_bytes_with_headers() {
    let (status, headers, body) = get(
        covers_app(covers_fixture()),
        "/covers/release-group/rg-1",
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, PNG_BYTES);
    assert_eq!(header(&headers, "content-type"), Some("image/png"));
    assert_eq!(header(&headers, "x-cover-source"), Some("audiodb"));
    assert_eq!(
        header(&headers, "cache-control"),
        Some("public, max-age=31536000, immutable")
    );
    let etag = header(&headers, "etag").unwrap();
    assert!(etag.starts_with('"') && etag.ends_with('"'));
}

#[tokio::test]
async fn release_group_cover_honors_if_none_match() {
    let app = || covers_app(covers_fixture());
    let (_, headers, _) = get(app(), "/covers/release-group/rg-1", &[]).await;
    let etag = header(&headers, "etag").unwrap().to_owned();
    for candidate in [etag.clone(), format!("W/{etag}"), "*".to_owned()] {
        let (status, headers, body) = get(
            app(),
            "/covers/release-group/rg-1",
            &[("if-none-match", &candidate)],
        )
        .await;
        assert_eq!(status, StatusCode::NOT_MODIFIED, "candidate {candidate}");
        assert!(body.is_empty());
        assert_eq!(header(&headers, "etag"), Some(etag.as_str()));
    }
    let (status, _, body) = get(
        app(),
        "/covers/release-group/rg-1",
        &[("if-none-match", "\"other\"")],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, PNG_BYTES);
}

#[tokio::test]
async fn release_group_cover_miss_serves_album_placeholder() {
    let (status, headers, body) = get(
        covers_app(covers_fixture()),
        "/covers/release-group/unknown",
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(header(&headers, "content-type"), Some("image/svg+xml"));
    assert_eq!(header(&headers, "x-cover-source"), Some("placeholder"));
    assert_eq!(
        header(&headers, "cache-control"),
        Some("public, max-age=300")
    );
    assert!(body.starts_with(b"<svg"));
}

#[tokio::test]
async fn release_group_cover_warming_answers_202() {
    let (status, headers, body) = get(
        covers_app(covers_fixture()),
        "/covers/release-group/rg-warm",
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert!(body.is_empty());
    assert_eq!(header(&headers, "cache-control"), Some("no-store"));
    assert_eq!(header(&headers, "x-cover-source"), Some("warming"));
}

// Wrapped.

#[tokio::test]
async fn wrapped_trio_shapes_with_valid_key() {
    let key = [("x-wrapped-api-key", WRAPPED_KEY)];
    let (status, _, body) = get(wrapped_app(WRAPPED_KEY), "/wrapped/users", &key).await;
    assert_eq!(status, StatusCode::OK);
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        json,
        serde_json::json!({"year": 2026, "users": [{
            "id": "u-1",
            "display_name": "Ada",
            "has_listenbrainz": true,
            "email": "ada@example.invalid",
        }]})
    );
    let (status, _, body) = get(wrapped_app(WRAPPED_KEY), "/wrapped/user/u-1", &key).await;
    assert_eq!(status, StatusCode::OK);
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["user_id"], "u-1");
    assert_eq!(json["has_data"], true);
    assert_eq!(json["top_artists"][0]["name"], "A");
    assert_eq!(json["top_tracks"][0]["artist_name"], "A");
    assert_eq!(json["top_albums"][0]["listen_count"], 8);
    assert_eq!(json["top_genres"][0]["genre"], "rock");
    assert_eq!(json["loved_tracks_count"], 7);
    assert_eq!(json["total_listens_estimated"], 42);
    let (status, _, body) = get(wrapped_app(WRAPPED_KEY), "/wrapped/server", &key).await;
    assert_eq!(status, StatusCode::OK);
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["year"], 2026);
    assert_eq!(json["total_users_tracked"], 1);
    assert_eq!(json["leaderboard"][0]["display_name"], "Ada");
    assert!(json.get("top_artist_sitewide").is_none());
}

#[tokio::test]
async fn wrapped_rejects_missing_key_on_every_route() {
    for uri in ["/wrapped/users", "/wrapped/user/u-1", "/wrapped/server"] {
        let (status, headers, body) = get(wrapped_app(WRAPPED_KEY), uri, &[]).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri}");
        assert!(header(&headers, "www-authenticate").is_none(), "{uri}");
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"error": {
                "code": "UNAUTHORIZED",
                "message": "Invalid or missing wrapped API key",
                "details": null,
            }}),
            "{uri}"
        );
    }
}

#[tokio::test]
async fn wrapped_rejects_everything_when_unconfigured() {
    let (status, _, _) = get(wrapped_app(""), "/wrapped/users", &[]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, _) = get(
        wrapped_app(""),
        "/wrapped/server",
        &[("x-wrapped-api-key", "anything")],
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn wrapped_rejects_padded_values_without_trimming() {
    for value in [" test-secret-key", "test-secret-key ", " test-secret-key "] {
        let (status, _, _) = get(
            wrapped_app(WRAPPED_KEY),
            "/wrapped/users",
            &[("x-wrapped-api-key", value)],
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "value {value:?}");
    }
}

// Leaks.

#[tokio::test]
async fn wrapped_rejection_echoes_no_key_material() {
    let (status, _, body) = get(
        wrapped_app("expected-dn-secret-marker-9f31"),
        "/wrapped/users",
        &[(
            "x-wrapped-api-key",
            "presented-/srv/secrets/droppedneedle.key",
        )],
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let text = String::from_utf8(body).unwrap();
    assert!(!text.contains("dn-secret-marker-9f31"), "{text}");
    assert!(!text.contains("/srv/secrets/droppedneedle.key"), "{text}");
}

#[tokio::test]
async fn covers_400_carries_only_caller_input() {
    let (status, _, body) = get(
        covers_app(covers_fixture()),
        "/covers/release/rel-1?size=%2Fsrv%2Fsecrets%2Fx",
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let text = String::from_utf8(body).unwrap();
    assert!(text.contains("Unsupported size '/srv/secrets/x'"), "{text}");
    assert!(!text.contains("placeholder"), "{text}");
    assert!(!text.contains("cover-art-archive"), "{text}");
}

/// The covers debug route stays out of the mounted platform router.
#[tokio::test]
async fn covers_debug_route_stays_out() {
    use platform::version::{FakeReleases, VersionInfo, VersionState};
    let releases = FakeReleases::new(
        VersionInfo {
            version: "1.2.0".to_owned(),
            build_date: None,
        },
        None,
        Vec::new(),
    );
    let app = platform::platform_router(platform::PlatformState::new(
        CoversState::new(Arc::new(covers_fixture())),
        VersionState::new(Arc::new(releases)),
        WrappedState::new(
            WRAPPED_KEY.to_owned(),
            Arc::new(FakeWrappedData::empty(2026)),
        ),
    ));
    let (status, _, _) = get(app, "/covers/debug/artist/art-1", &[]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
