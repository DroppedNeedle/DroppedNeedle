//! Playlist songs the library lacks: a file import keeps them, the
//! identify job finds their album by proof, and the link pass points them
//! at the library once that album lands. Request missing asks for their
//! albums in one press, and a plain user's press waits as one approval
//! card.

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

/// A release group for the journeys, numbered.
fn group(n: u32) -> String {
    format!("a0000000-0000-4000-8000-{n:012}")
}

/// `who` calls `uri` and expects `status`, answering the body.
async fn expect(
    app: &TestApp,
    who: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
    status: StatusCode,
) -> Value {
    let (got, answer) = app.call(method, uri, &[("authorization", who)], body).await;
    assert_eq!(got, status, "{method} {uri}: {answer}");
    answer
}

/// A plain user's ask for album `group`, waiting for approval.
async fn ask_album(app: &TestApp, who: &Login, group: &str) {
    let asked = expect(
        app,
        &who.authorization,
        "POST",
        "/api/v3/requests/albums",
        Some(json!({"musicbrainz_id": group, "artist": "Someone", "album": "Asked"})),
        StatusCode::ACCEPTED,
    )
    .await;
    assert_eq!(asked["status"], "awaiting_approval", "{asked}");
}

/// Playlist `id` of `owner`, its songs all without a source: each a title
/// and the album found for it, if any.
async fn seed_playlist(app: &TestApp, id: &str, owner: &str, songs: &[(&str, Option<String>)]) {
    let mut sql = format!(
        "INSERT INTO catalog_playlists (id, name, created_at, updated_at, user_id, is_public) \
         VALUES ('{id}', 'Road Trip', 1, 1, '{owner}', 0);"
    );
    for (position, (title, album)) in songs.iter().enumerate() {
        let album = album
            .as_ref()
            .map_or("NULL".to_owned(), |group| format!("'{group}'"));
        sql.push_str(&format!(
            "INSERT INTO catalog_playlist_tracks (id, playlist_id, position, track_name, \
             artist_name, album_name, album_id, source_type, created_at) \
             VALUES ('{id}-{position}', '{id}', {position}, '{title}', 'Someone', \
             'Album {position}', {album}, '', 1);"
        ));
    }
    app.seed(sql).await;
}

/// A matched library copy of album `group`.
async fn hold_album(app: &TestApp, group: &str) {
    app.seed(format!(
        "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, \
         updated_at) VALUES ('a-held', 'Someone', 'someone', 'group', 1, 1); \
         INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_id, grouping_source, created_at, updated_at) \
         VALUES ('alb-held', 'r1', 'alb-held', 'Held', 'held', 'a-held', 'automatic', 1, 1); \
         INSERT INTO local_album_external_identities (local_album_id, release_group_mbid, \
         decision_source, selected_at) VALUES ('alb-held', '{group}', 'manual', 1); \
         INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
         path_hash, file_size_bytes, file_mtime_ns, stat_revision, disc_number, track_number, \
         title, title_folded, artist_name, artist_name_folded, album_title, album_title_folded, \
         duration_seconds, availability, file_format, ingest_source, imported_at, \
         membership_source) VALUES ('t-held', 'alb-held', 'r1', '/m/held/01.flac', \
         'held/01.flac', 'h-held', 1, 1, 's', 1, 1, 'Held Song', 'held song', 'Someone', \
         'someone', 'Held', 'held', 200.0, 'indexed', 'flac', 'scan', 1, 'automatic');"
    ))
    .await;
}

/// A press of playlist `id` as `who`, under a fresh idempotency key.
async fn press(app: &TestApp, who: &str, id: &str, status: StatusCode) -> Value {
    expect(
        app,
        who,
        "POST",
        &format!("/api/v3/playlists/{id}/request-missing"),
        Some(json!({"idempotency_key": uuid::Uuid::new_v4().to_string()})),
        status,
    )
    .await
}

/// The status of album `group`'s request.
fn request_status(app: &TestApp, group: &str) -> String {
    app.db()
        .query_row(
            "SELECT status FROM request_history WHERE musicbrainz_id_lower = ?1",
            [group],
            |row| row.get(0),
        )
        .unwrap()
}

/// A plain user's press files, as one batch, the albums of the playlist's
/// missing songs that are new or whose last request was cancelled; owned,
/// live and rejected ones are counted and left alone. It waits as one
/// approval card, counted once, whose albums leave the single approvals
/// list and never show in Discover's batches. One decide approves the
/// ticked albums and rejects the rest in one write, and an album it
/// rejected that someone asks for again is theirs alone. A second press
/// files nothing new; an editor may press and a reader may not; a trusted
/// user's press makes no card; a user near their quota files what fits.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_playlist_press_waits_as_one_approval_card() {
    let app = TestApp::start("playlist-press-card").await;
    let admin = app.login_admin().await;
    let ada = app.login_user("ada").await;
    let bob = app.login_user("bob").await;
    let (owned, live, rejected, cancelled, first, second) =
        (group(1), group(2), group(3), group(4), group(5), group(6));
    ask_album(&app, &bob, &live).await;
    ask_album(&app, &bob, &rejected).await;
    expect(
        &app,
        &admin,
        "POST",
        &format!("/api/v3/requests/approvals/{rejected}/reject"),
        None,
        StatusCode::OK,
    )
    .await;
    ask_album(&app, &bob, &cancelled).await;
    expect(
        &app,
        &bob.authorization,
        "DELETE",
        &format!("/api/v3/requests/active/{cancelled}"),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(request_status(&app, &cancelled), "cancelled");
    hold_album(&app, &owned).await;
    seed_playlist(
        &app,
        "pl-ada",
        &ada.user_id,
        &[
            ("Owned", Some(owned.clone())),
            ("Live", Some(live.clone())),
            ("Rejected", Some(rejected.clone())),
            ("Cancelled", Some(cancelled.clone())),
            ("First A", Some(first.clone())),
            ("First B", Some(first.clone())),
            ("Second", Some(second.clone())),
            ("Unknown", None),
        ],
    )
    .await;

    let pressed = press(&app, &ada.authorization, "pl-ada", StatusCode::ACCEPTED).await;
    let batch_id = pressed["batch_id"].as_str().expect("a batch").to_owned();
    assert_eq!(
        pressed,
        json!({
            "batch_id": batch_id, "requested": 3, "in_library": 1, "already_requested": 1,
            "rejected_before": 1, "over_cap": 0, "over_quota": 0, "needs_approval": true,
        })
    );
    let cards = expect(
        &app,
        &admin,
        "GET",
        "/api/v3/requests/approvals/batches",
        None,
        StatusCode::OK,
    )
    .await;
    let card = &cards["cards"][0];
    assert_eq!(cards["cards"].as_array().unwrap().len(), 1, "{cards}");
    assert_eq!(card["id"], batch_id.as_str());
    assert_eq!(card["source"], "playlist");
    assert_eq!(card["name"], "Road Trip");
    assert_eq!(card["waiting"], 3);
    let items: Vec<(&str, u64)> = card["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| {
            (
                item["musicbrainz_id"].as_str().unwrap(),
                item["song_count"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        items,
        [
            (cancelled.as_str(), 1),
            (first.as_str(), 2),
            (second.as_str(), 1)
        ]
    );
    let singles = expect(
        &app,
        &admin,
        "GET",
        "/api/v3/requests/approvals",
        None,
        StatusCode::OK,
    )
    .await;
    let single_ids: Vec<&str> = singles["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["musicbrainz_id"].as_str().unwrap())
        .collect();
    assert_eq!(
        single_ids,
        [live.as_str()],
        "a card's albums are not listed alone"
    );
    let count = expect(
        &app,
        &admin,
        "GET",
        "/api/v3/requests/approvals/count",
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(count["count"], 2, "one single ask and one card");
    let discover = expect(
        &app,
        &ada.authorization,
        "GET",
        "/api/v3/discover/batches",
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        discover["batches"],
        json!([]),
        "a press is not a Discover batch"
    );
    expect(
        &app,
        &ada.authorization,
        "GET",
        &format!("/api/v3/discover/batches/{batch_id}"),
        None,
        StatusCode::NOT_FOUND,
    )
    .await;
    let state = expect(
        &app,
        &ada.authorization,
        "GET",
        "/api/v3/playlists/pl-ada/request-state",
        None,
        StatusCode::OK,
    )
    .await;
    let states: Vec<&str> = state["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["state"].as_str().unwrap())
        .collect();
    assert_eq!(
        states,
        [
            "album_in_library",
            "awaiting_approval",
            "rejected",
            "awaiting_approval",
            "awaiting_approval",
            "awaiting_approval",
            "awaiting_approval",
            "no_album_yet",
        ]
    );

    let again = press(&app, &ada.authorization, "pl-ada", StatusCode::ACCEPTED).await;
    assert_eq!(again["requested"], 0, "{again}");
    assert_eq!(again["already_requested"], 4, "{again}");
    assert_eq!(again["batch_id"], Value::Null);

    let decided = expect(
        &app,
        &admin,
        "POST",
        &format!("/api/v3/requests/approvals/batches/{batch_id}/decide"),
        Some(json!({"approve": [first, group(99)]})),
        StatusCode::OK,
    )
    .await;
    assert_eq!(decided, json!({"approved": 1, "rejected": 2, "skipped": 1}));
    let first_status = request_status(&app, &first);
    assert!(
        !["awaiting_approval", "rejected"].contains(&first_status.as_str()),
        "the ticked album is on its way: {first_status}"
    );
    assert_eq!(request_status(&app, &second), "rejected");
    assert_eq!(request_status(&app, &cancelled), "rejected");
    let cards = expect(
        &app,
        &admin,
        "GET",
        "/api/v3/requests/approvals/batches",
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(cards["cards"], json!([]), "a decided card goes");

    // An album the card rejected, asked for again by someone else, is their
    // own ask: listed alone, and the old card stays gone.
    ask_album(&app, &bob, &second).await;
    let cards = expect(
        &app,
        &admin,
        "GET",
        "/api/v3/requests/approvals/batches",
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        cards["cards"],
        json!([]),
        "a later ask never revives a card"
    );
    let singles = expect(
        &app,
        &admin,
        "GET",
        "/api/v3/requests/approvals",
        None,
        StatusCode::OK,
    )
    .await;
    let single_ids: Vec<&str> = singles["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["musicbrainz_id"].as_str().unwrap())
        .collect();
    assert_eq!(single_ids, [live.as_str(), second.as_str()], "{singles}");

    // Bob may edit Ada's playlist and Cyra may only read it.
    let cy = app.login_user("cyra").await;
    app.seed(format!(
        "INSERT INTO catalog_playlist_members (playlist_id, user_id, can_edit, added_at) \
         VALUES ('pl-ada', '{}', 1, 1), ('pl-ada', '{}', 0, 1);",
        bob.user_id, cy.user_id
    ))
    .await;
    let edited = press(&app, &bob.authorization, "pl-ada", StatusCode::ACCEPTED).await;
    assert_eq!(edited["requested"], 0, "{edited}");
    press(&app, &cy.authorization, "pl-ada", StatusCode::NOT_FOUND).await;
    expect(
        &app,
        &cy.authorization,
        "GET",
        "/api/v3/playlists/pl-ada/request-state",
        None,
        StatusCode::NOT_FOUND,
    )
    .await;

    // A trusted user's press needs no approval.
    let tess = app.login_user("tess").await;
    expect(
        &app,
        &admin,
        "PUT",
        &format!("/api/v3/admin/users/{}/role", tess.user_id),
        Some(json!({"role": "trusted"})),
        StatusCode::OK,
    )
    .await;
    seed_playlist(
        &app,
        "pl-tess",
        &tess.user_id,
        &[("Trusted", Some(group(7)))],
    )
    .await;
    let trusted = press(&app, &tess.authorization, "pl-tess", StatusCode::ACCEPTED).await;
    assert_eq!(trusted["requested"], 1, "{trusted}");
    assert_eq!(trusted["needs_approval"], false, "{trusted}");
    let cards = expect(
        &app,
        &admin,
        "GET",
        "/api/v3/requests/approvals/batches",
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(cards["cards"], json!([]), "a trusted press makes no card");

    // A user with room for one more ask files one and counts the other.
    let quin = app.login_user("quin").await;
    expect(
        &app,
        &admin,
        "PUT",
        &format!("/api/v3/admin/users/{}/quota", quin.user_id),
        Some(json!({"request_quota_count": 1, "request_quota_days": 7})),
        StatusCode::OK,
    )
    .await;
    seed_playlist(
        &app,
        "pl-quin",
        &quin.user_id,
        &[("One", Some(group(8))), ("Two", Some(group(9)))],
    )
    .await;
    let near = press(&app, &quin.authorization, "pl-quin", StatusCode::ACCEPTED).await;
    assert_eq!(near["requested"], 1, "{near}");
    assert_eq!(near["over_quota"], 1, "{near}");
    app.stop().await;
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
