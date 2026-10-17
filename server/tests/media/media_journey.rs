//! The media journey through the whole app against the in-repo mock Plex
//! server: save a Plex connection, browse its albums, stream whole bytes,
//! seek a range, then start, progress and stop playback and find the play
//! in history.

use axum::http::StatusCode;
use droppedneedle_testkit::remotes::{PLEX_TOKEN, canned_stream_bytes};
use serde_json::json;

use crate::common::TestApp;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn media_connect_browse_play_seek_stop() {
    let app = TestApp::start("media-plex").await;
    let auth = app.login_admin().await;

    // Seed one four-minute catalog track behind the playback lifecycle.
    seed_track(&app);

    // Connect: the admin saves the Plex server; the mock accepts its token.
    let (status, body) = app
        .call(
            "PUT",
            "/api/v3/settings/plex",
            &[("authorization", auth.as_str())],
            Some(json!({
                "plex_url": app.plex.base_url,
                "plex_token": PLEX_TOKEN,
                "enabled": true,
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = app
        .call(
            "GET",
            "/api/v3/remotes/plex/connection",
            &[("authorization", auth.as_str())],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["connected"], true);
    assert_eq!(body["source"], "plex");

    // Browse: the mock catalog answers through the unified adapter.
    let (status, body) = app
        .call(
            "GET",
            "/api/v3/remotes/plex/albums",
            &[("authorization", auth.as_str())],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["total"], 3,
        "mock sections hold exactly three albums: {body}"
    );
    assert_eq!(body["items"][0]["source"], "plex");

    // Play: whole canned bytes with the upstream content type.
    let expected = canned_stream_bytes(b'P');
    let (status, headers, bytes) = app
        .raw(
            "GET",
            "/api/v3/stream/plex/library/parts/1/file.mp3",
            &[("authorization", auth.as_str())],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers.get("content-type").and_then(|v| v.to_str().ok()),
        Some("audio/mpeg")
    );
    assert_eq!(bytes, expected);

    // Seek: an exact 100-byte slice with byte-exact framing.
    let (status, headers, bytes) = app
        .raw(
            "GET",
            "/api/v3/stream/plex/library/parts/1/file.mp3",
            &[("authorization", auth.as_str()), ("range", "bytes=100-199")],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        headers.get("content-range").and_then(|v| v.to_str().ok()),
        Some("bytes 100-199/1024")
    );
    assert_eq!(bytes, expected[100..200]);

    // Start: the session opens and presence shows one listener.
    let (status, body) = app
        .call(
            "POST",
            "/api/v3/playback/start",
            &[("authorization", auth.as_str())],
            Some(json!({
                "track_id": "journey-track-1",
                "source": "plex",
                "device": "web",
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["accepted"], true);
    let (status, body) = app
        .call(
            "GET",
            "/api/v3/now-playing",
            &[("authorization", auth.as_str())],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["sessions"].as_array().expect("sessions").len(), 1);

    // Progress then stop past threshold: the play counts and clears.
    let (status, body) = app
        .call(
            "POST",
            "/api/v3/playback/progress",
            &[("authorization", auth.as_str())],
            Some(json!({
                "track_id": "journey-track-1",
                "source": "plex",
                "device": "web",
                "position_ms": 120_000,
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = app
        .call(
            "POST",
            "/api/v3/playback/stop",
            &[("authorization", auth.as_str())],
            Some(json!({
                "track_id": "journey-track-1",
                "source": "plex",
                "device": "web",
                "position_ms": 240_000,
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["scrobbled"], true);
    let (status, body) = app
        .call(
            "GET",
            "/api/v3/now-playing",
            &[("authorization", auth.as_str())],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["sessions"].as_array().expect("sessions").len(), 0);

    // Reported: exactly one play-history row for the journey track.
    let count: i64 = app
        .db()
        .query_row(
            "SELECT COUNT(*) FROM library_play_history WHERE track_name = 'Meridian Dawn'",
            [],
            |row| row.get(0),
        )
        .expect("history counts");
    assert_eq!(count, 1);
    app.stop().await;
}

/// A player streaming from a media server that refuses connections reads
/// the closed-list `REMOTE_UNAVAILABLE`, not the generic internal error.
#[tokio::test]
async fn a_stream_from_a_server_that_is_down_answers_remote_unavailable() {
    let app = TestApp::start("media-plex-down").await;
    let auth = app.login_admin().await;
    let closed = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("loopback binds");
    let plex_url = format!("http://{}", closed.local_addr().expect("has an address"));
    drop(closed);
    let (status, body) = app
        .call(
            "PUT",
            "/api/v3/settings/plex",
            &[("authorization", auth.as_str())],
            Some(json!({
                "plex_url": plex_url,
                "plex_token": PLEX_TOKEN,
                "enabled": true,
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, _, bytes) = app
        .raw(
            "GET",
            "/api/v3/stream/plex/library/parts/1/file.mp3",
            &[
                ("authorization", auth.as_str()),
                ("x-request-id", "stream-down-1"),
            ],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    let body: serde_json::Value = serde_json::from_slice(&bytes).expect("body is json");
    assert_eq!(
        body,
        json!({"error": {
            "code": "REMOTE_UNAVAILABLE",
            "message": "Your media server did not answer or returned an error.",
            "details": {"error_id": "stream-down-1"},
        }})
    );
    app.stop().await;
}

/// Minimal artist/album/track rows behind the playback lifecycle.
fn seed_track(app: &TestApp) {
    let seed = [
        "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at) \
         VALUES ('journey-artist-1', 'Aurora Current', 'aurora current', 'group', 0, 0)",
        "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_id, grouping_source, created_at, updated_at) \
         VALUES ('journey-album-1', 'root-1', 'aurora current/neon meridian', 'Neon Meridian', \
         'neon meridian', 'journey-artist-1', 'manual', 0, 0)",
        "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
         path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
         album_title, album_title_folded, file_format, duration_seconds, ingest_source, \
         imported_at, membership_source) \
         VALUES ('journey-track-1', 'journey-album-1', 'root-1', '/music/meridian.flac', \
         'meridian.flac', 'hash-1', 1024, 0, 'rev-1', 'Meridian Dawn', 'meridian dawn', \
         'Neon Meridian', 'neon meridian', 'flac', 240.0, 'seed', 0, 'manual')",
    ]
    .join(";\n");
    app.db().execute_batch(&seed).expect("seed batch runs");
}
