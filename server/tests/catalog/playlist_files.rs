//! Playlist files: a playlist exported as M3U or CSV reads back into a new
//! playlist with the same tracks, and the import reports what it left out
//! and holds to its limits.

use axum::http::StatusCode;
use serde_json::{Value, json};

use crate::common::{Login, TestApp};

const MYSTERONS: &str = "Portishead/Dummy/01 Mysterons.flac";
const SOUR_TIMES: &str = "Portishead/Dummy/02 Sour Times.flac";

/// Two live tracks of one album, the second's title starting like a
/// spreadsheet formula, and a third whose file is gone.
async fn hold_album(app: &TestApp) {
    app.seed(format!(
        "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at) \
         VALUES ('a1', 'Portishead', 'portishead', 'group', 1, 1); \
         INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_id, grouping_source, created_at, updated_at) \
         VALUES ('alb', 'r1', 'alb', 'Dummy', 'dummy', 'a1', 'automatic', 1, 1); \
         INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
         path_hash, file_size_bytes, file_mtime_ns, stat_revision, disc_number, track_number, \
         title, title_folded, artist_name, artist_name_folded, album_title, album_title_folded, \
         duration_seconds, availability, file_format, ingest_source, imported_at, \
         membership_source) VALUES \
         ('t1', 'alb', 'r1', '/music/{MYSTERONS}', '{MYSTERONS}', 'h1', 1, 1, 's', 1, 1, \
         'Mysterons', 'mysterons', 'Portishead', 'portishead', 'Dummy', 'dummy', 305.0, \
         'indexed', 'flac', 'scan', 1, 'automatic'), \
         ('t2', 'alb', 'r1', '/music/{SOUR_TIMES}', '{SOUR_TIMES}', 'h2', 1, 1, 's', 1, 2, \
         '=Sour Times', '=sour times', 'Portishead', 'portishead', 'Dummy', 'dummy', 254.0, \
         'indexed', 'flac', 'scan', 1, 'automatic'), \
         ('t3', 'alb', 'r1', '/music/Portishead/Dummy/03 Gone.flac', \
         'Portishead/Dummy/03 Gone.flac', 'h3', 1, 1, 's', 1, 3, \
         'Gone', 'gone', 'Portishead', 'portishead', 'Dummy', 'dummy', 200.0, \
         'missing', 'flac', 'scan', 1, 'automatic');"
    ))
    .await;
}

async fn send(app: &TestApp, who: &Login, method: &str, uri: &str, body: Option<Value>) -> Value {
    let (status, body) = app
        .call(method, uri, &[("authorization", &who.authorization)], body)
        .await;
    assert!(status.is_success(), "{method} {uri}: {status} {body}");
    body
}

/// One export: the file's text and how many entries it left out.
async fn export(app: &TestApp, who: &Login, id: &str, query: &str) -> (String, String) {
    let (status, headers, bytes) = app
        .raw(
            "GET",
            &format!("/api/v3/playlists/{id}/export?{query}"),
            &[("authorization", &who.authorization)],
            None,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    let skipped = headers["x-droppedneedle-skipped-tracks"]
        .to_str()
        .unwrap()
        .to_owned();
    (String::from_utf8(bytes).unwrap(), skipped)
}

async fn import(app: &TestApp, who: &Login, format: &str, content: &str) -> (StatusCode, Value) {
    app.call(
        "POST",
        "/api/v3/playlists/import",
        &[("authorization", &who.authorization)],
        Some(json!({"name": "Back again", "format": format, "content": content})),
    )
    .await
}

async fn track_names(app: &TestApp, who: &Login, id: &str) -> Vec<(String, String)> {
    let detail = send(app, who, "GET", &format!("/api/v3/playlists/{id}"), None).await;
    detail["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|track| {
            (
                track["track_name"].as_str().unwrap().to_owned(),
                track["track_source_id"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

/// Export as M3U and CSV under a path prefix, leaving out an entry with no
/// file and one whose file is gone, then import each file back with an
/// entry the library lacks: the tracks come back in order while the
/// missing entry is reported. A CSV cell a spreadsheet would run is
/// guarded on the way out and read plainly on the way in. A file over 2 MB
/// is refused, a very long path is passed over quickly, and entries past
/// 10,000 are only counted.
#[tokio::test]
async fn playlists_export_and_import_back() {
    let app = TestApp::start("playlist-files").await;
    let ada = app.login_user("ada").await;
    hold_album(&app).await;
    let made = send(
        &app,
        &ada,
        "POST",
        "/api/v3/playlists",
        Some(json!({"name": "Dummy picks"})),
    )
    .await;
    let id = made["id"].as_str().unwrap().to_owned();
    let local = |title: &str, track: &str| {
        json!({"track_name": title, "artist_name": "Portishead", "album_name": "Dummy",
               "track_source_id": track, "source_type": "local"})
    };
    send(
        &app,
        &ada,
        "POST",
        &format!("/api/v3/playlists/{id}/tracks"),
        Some(json!({"tracks": [
            local("Mysterons", "t1"),
            {"track_name": "Elsewhere", "artist_name": "Nobody", "album_name": "None"},
            local("=Sour Times", "t2"),
            local("Gone", "t3"),
        ]})),
    )
    .await;
    let expected = vec![
        ("Mysterons".to_owned(), "t1".to_owned()),
        ("=Sour Times".to_owned(), "t2".to_owned()),
    ];
    let missing = json!([{"title": "Ghost", "artist": "Nobody", "reason": "no_match"}]);

    let (m3u, skipped) = export(&app, &ada, &id, "format=m3u8&prefix=Music/").await;
    assert_eq!(skipped, "2");
    assert!(m3u.contains(&format!("\nMusic/{MYSTERONS}\n")), "{m3u}");
    assert!(!m3u.contains("DROPPEDNEEDLE-PLAYLIST-ID"), "{m3u}");
    let (status, report) = import(
        &app,
        &ada,
        "m3u8",
        &format!("{m3u}#EXTINF:100,Nobody - Ghost\nElsewhere/ghost.flac\n"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{report}");
    assert_eq!(report["matched"], 2, "{report}");
    assert_eq!(report["unmatched"], missing);
    assert_eq!(report["over_limit"], 0);
    let copy = report["playlist_id"].as_str().unwrap();
    assert_eq!(track_names(&app, &ada, copy).await, expected);

    let (csv, skipped) = export(&app, &ada, &id, "format=csv").await;
    assert_eq!(skipped, "2");
    assert!(csv.contains("\"'=Sour Times\""), "{csv}");
    let (status, report) = import(
        &app,
        &ada,
        "csv",
        &format!("{csv}\"Ghost\",\"Nobody\",\"\",\"\",\"\",\"Elsewhere/ghost.flac\"\n"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{report}");
    assert_eq!(report["matched"], 2, "{report}");
    assert_eq!(report["unmatched"], missing);
    let copy = report["playlist_id"].as_str().unwrap();
    assert_eq!(track_names(&app, &ada, copy).await, expected);

    // With no path to go by, the guarded title still names its track.
    let (status, report) =
        import(&app, &ada, "csv", "title,artist\n'=Sour Times,Portishead\n").await;
    assert_eq!(status, StatusCode::CREATED, "{report}");
    assert_eq!(report["matched"], 1, "{report}");

    let too_big = "#".repeat(2 * 1024 * 1024 + 1);
    let (status, body) = import(&app, &ada, "m3u8", &too_big).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // A path longer than any in the library is passed over in one walk.
    let deep = format!("{}gone.flac\n", "a/".repeat(950_000));
    let (status, report) = import(&app, &ada, "m3u8", &deep).await;
    assert_eq!(status, StatusCode::CREATED, "{}", report["matched"]);
    assert_eq!(report["matched"], 0);
    assert_eq!(report["unmatched"][0]["reason"], "no_match");

    let crowded = format!("{MYSTERONS}\n").repeat(10_002);
    let (status, report) = import(&app, &ada, "m3u8", &crowded).await;
    assert_eq!(status, StatusCode::CREATED, "{report}");
    assert_eq!(report["matched"], 1, "{report}");
    assert_eq!(report["unmatched"].as_array().unwrap().len(), 9_999);
    assert_eq!(report["over_limit"], 2);
}
