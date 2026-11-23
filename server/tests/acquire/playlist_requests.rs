//! Playlist songs the library lacks: a file import keeps them, the
//! identify job finds their album by proof, and the link pass points them
//! at the library once that album lands.

use axum::http::StatusCode;
use droppedneedle_testkit::musicbrainz::MockMusicBrainz;
use serde_json::{Value, json};

use crate::common::{Login, TestApp, closed_outbound};

/// The release group the MusicBrainz mock proves _Harbour Lights_ is.
const HARBOUR_LIGHTS: &str = "5907a1b0-0000-4000-8000-000000001001";
/// The recording of _Paper Lanterns_ on it.
const PAPER_LANTERNS: &str = "5907a1b0-0000-4000-8000-000000001101";

/// The app with the MusicBrainz mock as its MusicBrainz source.
async fn app_with_musicbrainz(tag: &str) -> (TestApp, MockMusicBrainz) {
    let musicbrainz = MockMusicBrainz::start().await.expect("the mock serves");
    let app = TestApp::start_with(
        tag,
        droppedneedle::bootstrap::Outbound {
            brainzmash: musicbrainz.base_url(),
            ..closed_outbound()
        },
    )
    .await;
    (app, musicbrainz)
}

/// Import `content` as a CSV playlist named `name` as `who`.
async fn import_csv(app: &TestApp, who: &Login, name: &str, content: &str) -> Value {
    let (status, report) = app
        .call(
            "POST",
            "/api/v3/playlists/import",
            &[("authorization", who.authorization.as_str())],
            Some(json!({"name": name, "format": "csv", "content": content})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{report}");
    report
}

/// The playlist's entries as `(title, album, cover, recording, file)`.
fn entries(app: &TestApp, playlist_id: &str) -> Vec<[Option<String>; 5]> {
    app.db()
        .prepare(
            "SELECT track_name, album_id, cover_url, recording_mbid, library_file_id \
             FROM catalog_playlist_tracks WHERE playlist_id = ?1 ORDER BY position",
        )
        .unwrap()
        .query_map([playlist_id], |row| {
            Ok([
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ])
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// A library copy of _Harbour Lights_ with its two songs, as a scan of a
/// matched album leaves it.
async fn land_harbour_lights(app: &TestApp) {
    app.seed(format!(
        "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, \
         updated_at) VALUES ('a-wf', 'Willow Fen', 'willow fen', 'group', 1, 1); \
         INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_id, grouping_source, created_at, updated_at) \
         VALUES ('alb-hl', 'r1', 'alb-hl', 'Harbour Lights', 'harbour lights', 'a-wf', \
         'automatic', 1, 1); \
         INSERT INTO local_album_external_identities (local_album_id, release_group_mbid, \
         decision_source, selected_at) VALUES ('alb-hl', '{HARBOUR_LIGHTS}', 'manual', 1); \
         INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
         path_hash, file_size_bytes, file_mtime_ns, stat_revision, disc_number, track_number, \
         title, title_folded, artist_name, artist_name_folded, album_title, album_title_folded, \
         duration_seconds, availability, file_format, ingest_source, imported_at, \
         membership_source) VALUES \
         ('t-pl', 'alb-hl', 'r1', '/m/hl/01.flac', 'hl/01.flac', 'h-pl', 1, 1, 's', 1, 1, \
         'Paper Lanterns', 'paper lanterns', 'Willow Fen', 'willow fen', 'Harbour Lights', \
         'harbour lights', 200.0, 'indexed', 'flac', 'scan', 1, 'automatic'), \
         ('t-sr', 'alb-hl', 'r1', '/m/hl/02.flac', 'hl/02.flac', 'h-sr', 1, 1, 's', 1, 2, \
         'Salt Road', 'salt road', 'Willow Fen', 'willow fen', 'Harbour Lights', \
         'harbour lights', 210.0, 'indexed', 'flac', 'scan', 1, 'automatic'); \
         UPDATE library_catalog_revision SET value = value + 1 WHERE singleton = 1;"
    ))
    .await;
}

/// A CSV import keeps the two songs the library lacks. The identify job
/// proves their album by its listing and writes it, with its cover and the
/// one recording of each title, onto both songs; once a copy of that album
/// lands, the link pass points each song at its track.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kept_songs_link_once_their_album_lands() {
    let (app, _musicbrainz) = app_with_musicbrainz("playlist-kept-link").await;
    let listener = app.login_user("listener").await;
    let report = import_csv(
        &app,
        &listener,
        "Road Trip",
        "title,artist,album\n\
         Paper Lanterns,Willow Fen,Harbour Lights\n\
         Salt Road,Willow Fen,Harbour Lights\n",
    )
    .await;
    assert_eq!(report["kept"], 2, "{report}");
    let playlist_id = report["playlist_id"].as_str().unwrap().to_owned();

    let found = app
        .until("the identify job finds the album", || {
            let rows = entries(&app, &playlist_id);
            async move { rows.iter().all(|row| row[1].is_some()).then_some(rows) }
        })
        .await;
    let cover = format!("/api/v3/covers/release-group/{HARBOUR_LIGHTS}?size=250");
    let recording = |title: &str| match title {
        "Paper Lanterns" => Some(PAPER_LANTERNS.to_owned()),
        _ => Some("5907a1b0-0000-4000-8000-000000001110".to_owned()),
    };
    for row in &found {
        let title = row[0].as_deref().unwrap();
        assert_eq!(row[1].as_deref(), Some(HARBOUR_LIGHTS), "{title}");
        assert_eq!(row[2].as_deref(), Some(cover.as_str()), "{title}");
        assert_eq!(row[3], recording(title), "{title}");
        assert_eq!(row[4], None, "{title} waits for its album");
    }

    land_harbour_lights(&app).await;
    let linked = app
        .until("the link pass links both songs", || {
            let rows = entries(&app, &playlist_id);
            async move { rows.iter().all(|row| row[4].is_some()).then_some(rows) }
        })
        .await;
    let files: Vec<Option<&str>> = linked.iter().map(|row| row[4].as_deref()).collect();
    assert_eq!(files, [Some("t-pl"), Some("t-sr")]);
    app.stop().await;
}
