//! YouTube links over a scratch database and the fake YouTube search:
//! generate an album and its tracks, reuse saved links without searching,
//! edit and paste links by hand, and delete them again.

use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use droppedneedle::auth::session::extract::Transport;
use droppedneedle::auth::session::middleware::CurrentSession;
use droppedneedle::auth::session::store::SessionKind;
use droppedneedle::auth::users::memory::with_test_principal;
use droppedneedle::db::{DbConfig, open_runtime};
use droppedneedle::reads::collections::db::CollectionsDb;
use droppedneedle::reads::discover::fakes::FakeYouTube;
use droppedneedle::reads::youtube::{self, YouTubeLinks, store::LinkStore};
use serde_json::{Value, json};
use tower::ServiceExt as _;

use crate::common::{FixedIdGenerator, ScratchDir};

async fn call(app: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    let body = match body {
        Some(json) => {
            builder = builder.header("content-type", "application/json");
            Body::from(json.to_string())
        }
        None => Body::empty(),
    };
    let response = app
        .clone()
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

#[tokio::test]
async fn youtube_links_journey() {
    let dir = ScratchDir::new("youtube");
    let runtime = open_runtime(&DbConfig::new(&dir.join("app.db")))
        .await
        .unwrap();
    let links = YouTubeLinks::new(
        LinkStore::new(CollectionsDb::new(
            runtime.pool().clone(),
            runtime.lane().clone(),
        )),
        Arc::new(FakeYouTube::configured()),
        Arc::new(FixedIdGenerator::new(
            "0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0",
        )),
    );
    let app = with_test_principal(
        Router::new().nest("/api/v3", youtube::router(links)),
        CurrentSession {
            user_id: "u-ada".to_owned(),
            session_id: "sess-test".to_owned(),
            kind: SessionKind::Standard,
            transport: Transport::Bearer,
        },
    );

    // Nothing saved yet: 204, not an error.
    let (status, _) = call(&app, "GET", "/api/v3/youtube/link/rg-1", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Album link: searched, saved, and answered with today's budget.
    let generate = json!({
        "artist_name": "Portishead", "album_name": "Dummy",
        "album_id": "rg-1", "cover_url": "https://covers/rg-1.jpg"
    });
    let (status, body) = call(
        &app,
        "POST",
        "/api/v3/youtube/generate",
        Some(generate.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let video = body["link"]["video_id"].as_str().unwrap().to_owned();
    assert_eq!(
        body["link"]["embed_url"],
        format!("https://www.youtube.com/embed/{video}")
    );
    assert_eq!(body["link"]["is_manual"], false);
    assert_eq!(
        body["quota"],
        json!({"used": 120, "limit": 10000, "remaining": 9880, "date": body["quota"]["date"]})
    );
    // Asking again answers the saved row unchanged.
    let created = body["link"]["created_at"].clone();
    let (_, again) = call(&app, "POST", "/api/v3/youtube/generate", Some(generate)).await;
    assert_eq!(again["link"]["created_at"], created);

    // No video found is a 404 with a readable message.
    let (status, body) = call(
        &app,
        "POST",
        "/api/v3/youtube/generate",
        Some(json!({"artist_name": "X", "album_name": "no-video-here", "album_id": "rg-9"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("no-video-here")
    );

    // Batch: one found, one with no video.
    let (status, body) = call(
        &app,
        "POST",
        "/api/v3/youtube/generate-tracks",
        Some(json!({
            "album_id": "rg-1", "album_name": "Dummy", "artist_name": "Portishead",
            "tracks": [
                {"track_name": "Roads", "track_number": 5},
                {"track_name": "no-video-here", "track_number": 6, "disc_number": 1}
            ]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["track_links"].as_array().unwrap().len(), 1);
    assert_eq!(body["track_links"][0]["disc_number"], 1);
    assert_eq!(
        body["failed"],
        json!([{"disc_number": 1, "track_number": 6, "track_name": "no-video-here", "reason": "No video found"}])
    );
    let roads = body["track_links"][0].clone();
    // A saved track answers without a new row.
    let (_, body) = call(
        &app,
        "POST",
        "/api/v3/youtube/generate-track",
        Some(json!({
            "album_id": "rg-1", "album_name": "Dummy", "artist_name": "Portishead",
            "track_name": "Roads", "track_number": 5
        })),
    )
    .await;
    assert_eq!(body["track_link"], roads);
    let (_, list) = call(&app, "GET", "/api/v3/youtube/links", None).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["track_count"], 1);
    assert_eq!(list[0]["video_id"], video);

    // A track-only album gets an entry with no album video; removing its
    // last track removes the empty entry too.
    let (status, body) = call(
        &app,
        "POST",
        "/api/v3/youtube/generate-track",
        Some(json!({
            "album_id": "rg-2", "album_name": "Third", "artist_name": "Portishead",
            "track_name": "Machine Gun", "track_number": 3, "disc_number": 2
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, entry) = call(&app, "GET", "/api/v3/youtube/link/rg-2", None).await;
    assert_eq!(entry["video_id"], Value::Null);
    assert_eq!(entry["track_count"], 1);
    let (status, _) = call(&app, "DELETE", "/api/v3/youtube/track-link/rg-2/2/3", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(&app, "GET", "/api/v3/youtube/link/rg-2", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Edit: new video from a URL, cover cleared, creation time kept.
    let (status, body) = call(
        &app,
        "PUT",
        "/api/v3/youtube/link/rg-1",
        Some(json!({"youtube_url": "https://youtu.be/dQw4w9WgXcQ?si=x", "cover_url": null})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["video_id"], "dQw4w9WgXcQ");
    assert_eq!(body["cover_url"], Value::Null);
    assert_eq!(body["album_name"], "Dummy");
    assert_eq!(body["created_at"], created);
    let (status, body) = call(
        &app,
        "PUT",
        "/api/v3/youtube/link/rg-1",
        Some(json!({"youtube_url": "https://example.com/nope"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "INVALID_INPUT");
    let (status, _) = call(&app, "PUT", "/api/v3/youtube/link/rg-x", Some(json!({}))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Manual link with no album behind it gets a made-up id.
    let (status, body) = call(
        &app,
        "POST",
        "/api/v3/youtube/manual",
        Some(json!({
            "album_name": "Live", "artist_name": "Portishead",
            "youtube_url": "https://www.youtube.com/watch?v=abcdefghijk"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["album_id"], "manual-0f1e2d3c4b5a");
    assert_eq!(body["is_manual"], true);

    // Deleting an album link takes its track links with it.
    let (status, _) = call(&app, "DELETE", "/api/v3/youtube/link/rg-1", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, tracks) = call(&app, "GET", "/api/v3/youtube/track-links/rg-1", None).await;
    assert_eq!(tracks, json!([]));
    let (_, list) = call(&app, "GET", "/api/v3/youtube/links", None).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
}
