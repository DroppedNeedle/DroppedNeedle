//! Live event stream journey through the real app: two signed-in users
//! open `/api/v3/events/stream`. A now-playing heartbeat reaches both as a
//! `snapshot`, a notice for one user reaches only that user, and a tab
//! opened later replays current state plus its own recent notice.

use std::time::Duration;

use axum::Router;
use axum::body::{Body, BodyDataStream};
use axum::http::{Request, StatusCode};
use droppedneedle::create_app;
use droppedneedle::events::{PlaylistImported, UserNotice};
use futures_util::StreamExt as _;
use serde_json::{Value, json};
use tower::ServiceExt as _;

use crate::auth_e2e::{E2e, HOST, call, login, setup_admin};

/// One open stream, read frame by frame.
struct Stream {
    body: BodyDataStream,
    buffer: String,
}

impl Stream {
    async fn open(app: Router, token: &str) -> Self {
        let request = Request::builder()
            .uri("/api/v3/events/stream")
            .header("host", HOST)
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .expect("request builds");
        let response = app.oneshot(request).await.expect("router responds");
        assert_eq!(response.status(), StatusCode::OK);
        let headers = response.headers();
        assert_eq!(headers["content-type"], "text/event-stream");
        assert_eq!(headers["x-accel-buffering"], "no");
        assert!(
            headers.get("content-encoding").is_none(),
            "the stream is never compressed"
        );
        Self {
            body: response.into_body().into_data_stream(),
            buffer: String::new(),
        }
    }

    /// The next frame, without its blank-line terminator.
    async fn frame(&mut self) -> String {
        loop {
            if let Some(end) = self.buffer.find("\n\n") {
                let frame = self.buffer[..end].to_owned();
                self.buffer.drain(..end + 2);
                return frame;
            }
            let chunk = tokio::time::timeout(Duration::from_secs(5), self.body.next())
                .await
                .expect("a frame arrives in time")
                .expect("the stream stays open")
                .expect("the chunk reads");
            self.buffer
                .push_str(std::str::from_utf8(&chunk).expect("frames are text"));
        }
    }

    /// The next event as (name, JSON payload).
    async fn event(&mut self) -> (String, Value) {
        let frame = self.frame().await;
        let mut name = String::new();
        let mut data = Value::Null;
        for line in frame.lines() {
            if let Some(value) = line.strip_prefix("event: ") {
                name = value.to_owned();
            } else if let Some(value) = line.strip_prefix("data: ") {
                data = serde_json::from_str(value).expect("data is JSON");
            }
        }
        assert!(!name.is_empty(), "an event frame: {frame:?}");
        (name, data)
    }
}

#[tokio::test]
async fn events_stream_journey_presence_notices_and_replay() {
    let e2e = E2e::open("events-stream").await;
    let state = e2e.state();
    let hub = state.events.clone();
    let app = create_app(state);

    let (_, admin_token) =
        setup_admin(app.clone(), "e2e-owner", "e2e-owner-password-1", "bearer").await;
    let admin_token = admin_token.as_str().expect("admin token").to_owned();
    let admin_auth = format!("Bearer {admin_token}");
    let (status, body, _) = call(
        app.clone(),
        "POST",
        "/api/v3/admin/users",
        &[("authorization", admin_auth.as_str())],
        Some(json!({"username": "molly", "password": "molly-password-1234"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let molly_id = body["id"].as_str().expect("molly id").to_owned();
    let (status, body, _) = login(app.clone(), "molly", "molly-password-1234", "bearer").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let molly_token = body["token"].as_str().expect("molly token").to_owned();

    // Both streams open with the reconnect delay.
    let mut admin = Stream::open(app.clone(), &admin_token).await;
    let mut molly = Stream::open(app.clone(), &molly_token).await;
    assert_eq!(admin.frame().await, "retry: 5000");
    assert_eq!(molly.frame().await, "retry: 5000");

    // A heartbeat goes to everyone as a presence snapshot.
    let (status, body, _) = call(
        app.clone(),
        "POST",
        "/api/v3/now-playing",
        &[("authorization", admin_auth.as_str())],
        Some(json!({"track_name": "Meridian Dawn", "artist_name": "The Tests"})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    for stream in [&mut admin, &mut molly] {
        let (name, data) = stream.event().await;
        assert_eq!(name, "snapshot");
        assert_eq!(data["sessions"].as_array().expect("sessions").len(), 1);
    }

    // A notice for molly reaches molly only: the admin's next event is the
    // snapshot after the heartbeat clears.
    hub.notify(
        &molly_id,
        UserNotice::PlaylistImported(PlaylistImported {
            playlist_id: "playlist-1".to_owned(),
            event_id: "import-1".to_owned(),
        }),
    );
    let (name, data) = molly.event().await;
    assert_eq!(name, "playlist_imported");
    assert_eq!(
        data,
        json!({"playlist_id": "playlist-1", "event_id": "import-1"})
    );
    let (status, body, _) = call(
        app.clone(),
        "DELETE",
        "/api/v3/now-playing",
        &[("authorization", admin_auth.as_str())],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let (name, data) = admin.event().await;
    assert_eq!(name, "snapshot", "the admin never sees molly's notice");
    assert_eq!(data["sessions"].as_array().expect("sessions").len(), 0);

    // A tab molly opens now replays current state and her recent notice;
    // one the admin opens replays the state alone.
    let mut late = Stream::open(app.clone(), &molly_token).await;
    assert_eq!(late.frame().await, "retry: 5000");
    let mut replayed = vec![late.event().await.0, late.event().await.0];
    replayed.sort();
    assert_eq!(replayed, ["playlist_imported", "snapshot"]);
    let mut late_admin = Stream::open(app.clone(), &admin_token).await;
    assert_eq!(late_admin.frame().await, "retry: 5000");
    assert_eq!(late_admin.event().await.0, "snapshot");
    hub.close();
    assert!(
        tokio::time::timeout(Duration::from_secs(5), late_admin.body.next())
            .await
            .expect("the stream ends in time")
            .is_none(),
        "closing the hub ends the stream with no notice replayed"
    );
}
