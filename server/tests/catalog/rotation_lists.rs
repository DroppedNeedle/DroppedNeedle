//! Lists on rotation over the production builder: a refresh at a chosen
//! time makes up to five playlists from the person's own library, the two
//! always-on ones first; nobody edits them, on the web or in an app; a
//! Monday picks the rotating ones again and keeps a pinned one; a re-run
//! in the week writes nothing; turning them off deletes them; and a saved
//! copy is an ordinary playlist.

use axum::http::StatusCode;
use serde_json::{Value, json};

use droppedneedle_catalog::collections::rotation::{self, DAY_MS};

use crate::common::TestApp;

/// Wednesday 2030-01-09 12:00 UTC.
const WEDNESDAY: i64 = 1_894_190_400_000;
/// The Monday after it, 01:00 UTC.
const NEXT_MONDAY: i64 = 1_894_582_800_000;

/// A library of 30 flac songs on three albums: songs 0 to 24 from the
/// 1990s, 0 to 21 seven minutes long or more, songs 0 to 20 rated five
/// stars by `user`, and songs 0 to 11 played by them twice two days before
/// `WEDNESDAY`. So lossless (30), long ones (22), the 1990s (25) and five
/// stars (21) qualify to rotate; never played (18), forgotten favourites
/// (9) and deep cuts (8) are too thin.
fn library(user: &str) -> String {
    let mut seed = "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, \
         updated_at) VALUES ('art-1', 'Someone', 'someone', 'person', 0, 0);"
        .to_owned();
    for album in 0..3 {
        seed += &format!(
            "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
             album_artist_id, grouping_source, created_at, updated_at) \
             VALUES ('alb-{album}', 'root-1', 'g-{album}', 'Album {album}', 'album {album}', \
             'art-1', 'manual', 0, 0);"
        );
    }
    for n in 0..30 {
        let year = if n < 25 { 1990 + n % 10 } else { 2015 };
        let seconds = if n < 22 { 480 } else { 200 };
        seed += &format!(
            "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
             path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
             album_title, album_title_folded, file_format, ingest_source, imported_at, \
             membership_source, availability, duration_seconds, year) VALUES ('trk-{n:02}', \
             'alb-{album}', 'root-1', '/music/{n}.flac', '{n}.flac', 'h-{n}', 4096, 0, 's', \
             'Song {n}', 'song {n}', 'Album {album}', 'album {album}', 'flac', 'scan', {n}, \
             'automatic', 'indexed', {seconds}, {year});",
            album = n / 10
        );
    }
    for n in 0..21 {
        seed += &format!(
            "INSERT INTO catalog_user_ratings (user_id, item_kind, item_id, rating, rated_at) \
             VALUES ('{user}', 'track', 'trk-{n:02}', 5, 1);"
        );
    }
    for n in 0..12 {
        for play in 0..2 {
            seed += &format!(
                "INSERT INTO library_play_history (id, user_id, local_track_id, local_album_id, \
                 local_artist_id, track_name, artist_name, duration_ms, played_at) VALUES \
                 ('play-{n}-{play}', '{user}', 'trk-{n:02}', 'alb-{album}', 'art-1', 'Song {n}', \
                 'Someone', 480000, {at});",
                album = n / 10,
                at = WEDNESDAY - 2 * DAY_MS + play
            );
        }
    }
    seed
}

async fn call(
    app: &TestApp,
    auth: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    app.call(method, uri, &[("authorization", auth)], body)
        .await
}

/// The caller's lists on rotation, by name.
async fn rotation_lists(app: &TestApp, auth: &str) -> Vec<Value> {
    let (status, list) = call(app, auth, "GET", "/api/v3/playlists", None).await;
    assert_eq!(status, StatusCode::OK, "{list}");
    let mut lists: Vec<Value> = list["playlists"]
        .as_array()
        .expect("playlists")
        .iter()
        .filter(|playlist| playlist["kind"] == "rotation")
        .cloned()
        .collect();
    lists.sort_by_key(|list| list["name"].as_str().unwrap_or_default().to_owned());
    lists
}

fn names(lists: &[Value]) -> Vec<String> {
    lists
        .iter()
        .map(|list| list["name"].as_str().unwrap_or_default().to_owned())
        .collect()
}

fn rotating(lists: &[Value]) -> Vec<String> {
    lists
        .iter()
        .filter(|list| list["rotation"]["always_on"] == json!(false))
        .map(|list| list["name"].as_str().unwrap_or_default().to_owned())
        .collect()
}

async fn refresh(app: &TestApp, user_id: &str, at: i64) -> rotation::Written {
    let state = app.state().catalog.collections.clone();
    rotation::refresh(&state, user_id, at, |_| Ok(()))
        .await
        .expect("the refresh runs")
}

/// A person with plays, ratings and a 30-song library gets the two
/// always-on lists and three rotating ones from what qualifies, each an
/// ordinary playlist that says it is on rotation. Nobody may change one:
/// adding a song, renaming it or making it public is refused on the web
/// and through Subsonic, which lists it read-only. Another person reads
/// none of it.
#[tokio::test]
async fn a_person_gets_five_lists_nobody_can_edit() {
    let app = TestApp::start("rotation-lists-five").await;
    let ada = app.login_user("ada").await;
    let bob = app.login_user("bob").await;
    app.seed(library(&ada.user_id)).await;
    let auth = ada.authorization.as_str();

    let written = refresh(&app, &ada.user_id, WEDNESDAY).await;
    assert_eq!(written.created, 5, "{written:?}");
    let lists = rotation_lists(&app, auth).await;
    assert_eq!(lists.len(), 5, "{lists:?}");
    let names = names(&lists);
    assert!(
        names.contains(&"Added recently".to_owned()) && names.contains(&"On repeat".to_owned()),
        "{names:?}"
    );
    let rotating = rotating(&lists);
    assert_eq!(rotating.len(), 3, "{names:?}");
    for name in &rotating {
        assert!(
            ["Lossless", "Long ones", "The 1990s", "Five stars"].contains(&name.as_str()),
            "{name} does not qualify"
        );
    }
    for list in &lists {
        assert_eq!(list["can_edit"], false, "{list}");
        assert_eq!(list["can_delete"], true, "{list}");
        assert_eq!(list["is_public"], false, "{list}");
    }
    let on_repeat = lists
        .iter()
        .find(|list| list["name"] == "On repeat")
        .expect("on repeat");
    assert_eq!(on_repeat["track_count"], 12, "{on_repeat}");
    assert_eq!(on_repeat["rotation"]["idea"], "on_repeat");
    assert_eq!(on_repeat["rotation"]["always_on"], true);

    let (status, mine) = call(&app, auth, "GET", "/api/v3/me/rotation", None).await;
    assert_eq!(status, StatusCode::OK, "{mine}");
    assert_eq!(mine["enabled"], true, "{mine}");
    assert_eq!(mine["waiting"].as_array().map(Vec::len), Some(1), "{mine}");
    assert_eq!(
        mine["thin"],
        json!(["forgotten_favourites", "deep_cuts", "never_played"]),
        "{mine}"
    );
    // Nobody else's list shows them, not even as a private stub, an
    // admin's included.
    let ada_ids: Vec<Value> = lists.iter().map(|list| list["id"].clone()).collect();
    let admin = app.login_admin().await;
    for (who, auth) in [
        ("bob", bob.authorization.as_str()),
        ("admin", admin.as_str()),
    ] {
        let (status, theirs) = call(&app, auth, "GET", "/api/v3/playlists", None).await;
        assert_eq!(status, StatusCode::OK, "{theirs}");
        let shown: Vec<&Value> = theirs["playlists"]
            .as_array()
            .expect("playlists")
            .iter()
            .filter(|item| ada_ids.contains(&item["id"]))
            .collect();
        assert!(shown.is_empty(), "{who} sees ada's lists: {shown:?}");
    }

    let id = on_repeat["id"].as_str().expect("id").to_owned();
    let track = json!({"track_name": "One", "artist_name": "Someone", "album_name": "Album"});
    for (method, uri, body) in [
        (
            "POST",
            format!("/api/v3/playlists/{id}/tracks"),
            json!({"tracks": [track]}),
        ),
        (
            "PUT",
            format!("/api/v3/playlists/{id}"),
            json!({"name": "Mine now"}),
        ),
        (
            "PATCH",
            format!("/api/v3/playlists/{id}/visibility"),
            json!({"is_public": true}),
        ),
    ] {
        let (status, body) = call(&app, auth, method, &uri, Some(body)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}: {body}");
    }

    app.set_compat(true, false).await;
    let (status, password) = call(
        &app,
        auth,
        "POST",
        "/api/v3/me/app-passwords",
        Some(json!({"name": "Phone"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{password}");
    let subsonic = |call: &str| {
        let sep = if call.contains('?') { '&' } else { '?' };
        format!(
            "/subsonic/rest/{call}{sep}u=ada&p={}&v=1.16.1&c=journey&f=json",
            password["secret"].as_str().expect("secret")
        )
    };
    let (status, listed) = app.call("GET", &subsonic("getPlaylists"), &[], None).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let listed = listed["subsonic-response"]["playlists"]["playlist"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let in_app = listed
        .iter()
        .find(|playlist| playlist["name"] == "On repeat")
        .expect("the app lists it");
    assert_eq!(in_app["readonly"], true, "{in_app}");
    let app_id = in_app["id"].as_str().expect("app id").to_owned();
    let (status, opened) = app
        .call(
            "GET",
            &subsonic(&format!("getPlaylist?id={app_id}")),
            &[],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{opened}");
    let song = opened["subsonic-response"]["playlist"]["entry"][0]["id"]
        .as_str()
        .expect("a song id")
        .to_owned();
    for call_name in [
        format!("updatePlaylist?playlistId={app_id}&name=Mine"),
        format!("updatePlaylist?playlistId={app_id}&songIdToAdd={song}"),
        format!("createPlaylist?playlistId={app_id}&songId={song}"),
    ] {
        let (_, answer) = app.call("GET", &subsonic(&call_name), &[], None).await;
        assert_eq!(
            answer["subsonic-response"]["status"], "failed",
            "{call_name}: {answer}"
        );
    }
    let after = rotation_lists(&app, auth).await;
    let still = after
        .iter()
        .find(|list| list["id"] == json!(id))
        .expect("still there");
    assert_eq!(
        (still["name"].clone(), still["track_count"].clone()),
        (json!("On repeat"), json!(12)),
        "{still}"
    );
    app.stop().await;
}

/// Across a Monday the rotating lists are picked again: the one that waited
/// comes on, a pinned one stays as it was, and an unpinned one goes. A
/// second run in the same week writes nothing, and no list's time moves.
/// The always-on lists cannot be pinned.
#[tokio::test]
async fn a_monday_rotates_the_lists_and_keeps_a_pinned_one() {
    let app = TestApp::start("rotation-lists-monday").await;
    let ada = app.login_user("ada").await;
    app.seed(library(&ada.user_id)).await;
    let auth = ada.authorization.as_str();

    refresh(&app, &ada.user_id, WEDNESDAY).await;
    let before = rotation_lists(&app, auth).await;
    let again = refresh(&app, &ada.user_id, WEDNESDAY + DAY_MS).await;
    assert_eq!(
        again,
        rotation::Written::default(),
        "a re-run writes nothing"
    );
    let unchanged = rotation_lists(&app, auth).await;
    let stamps = |lists: &[Value]| {
        lists
            .iter()
            .map(|list| (list["id"].clone(), list["updated_at"].clone()))
            .collect::<Vec<_>>()
    };
    assert_eq!(stamps(&unchanged), stamps(&before));

    let always = before
        .iter()
        .find(|list| list["name"] == "Added recently")
        .expect("added recently");
    let (status, body) = call(
        &app,
        auth,
        "PUT",
        &format!(
            "/api/v3/playlists/{}/rotation",
            always["id"].as_str().expect("id")
        ),
        Some(json!({"pinned": true})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    let rotating_before = rotating(&before);
    let kept = before
        .iter()
        .find(|list| list["name"] == json!(rotating_before[0]))
        .expect("a rotating list");
    let (status, pinned) = call(
        &app,
        auth,
        "PUT",
        &format!(
            "/api/v3/playlists/{}/rotation",
            kept["id"].as_str().expect("id")
        ),
        Some(json!({"pinned": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{pinned}");
    assert_eq!(pinned["rotation"]["pinned"], true, "{pinned}");

    let monday = refresh(&app, &ada.user_id, NEXT_MONDAY).await;
    assert_eq!((monday.created, monday.dropped), (1, 1), "{monday:?}");
    let after = rotation_lists(&app, auth).await;
    let rotating_after = rotating(&after);
    assert_eq!(rotating_after.len(), 3, "{rotating_after:?}");
    assert!(
        rotating_after.contains(&rotating_before[0]),
        "the pinned one stays"
    );
    let kept_after = after
        .iter()
        .find(|list| list["id"] == kept["id"])
        .expect("the pinned list keeps its playlist");
    assert_eq!(kept_after["rotation"]["pinned"], true);
    let came_on: Vec<&String> = rotating_after
        .iter()
        .filter(|name| !rotating_before.contains(name))
        .collect();
    assert_eq!(
        came_on.len(),
        1,
        "the one that waited comes on: {rotating_after:?}"
    );
    app.stop().await;
}

/// Turning lists on rotation off deletes them and says so; a copy saved
/// before is the person's own playlist, which they may change.
#[tokio::test]
async fn off_deletes_the_lists_and_a_copy_is_ones_own() {
    let app = TestApp::start("rotation-lists-off").await;
    let ada = app.login_user("ada").await;
    app.seed(library(&ada.user_id)).await;
    let auth = ada.authorization.as_str();
    refresh(&app, &ada.user_id, WEDNESDAY).await;
    let lists = rotation_lists(&app, auth).await;
    let added = lists
        .iter()
        .find(|list| list["name"] == "Added recently")
        .expect("added recently");

    let (status, copy) = call(
        &app,
        auth,
        "POST",
        &format!(
            "/api/v3/playlists/{}/rotation/copy",
            added["id"].as_str().expect("id")
        ),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{copy}");
    assert_eq!(copy["kind"], "own", "{copy}");
    assert_eq!(copy["can_edit"], true, "{copy}");
    assert_eq!(copy["track_count"], added["track_count"], "{copy}");
    assert!(
        copy["name"]
            .as_str()
            .is_some_and(|name| name.starts_with("Added recently (")),
        "{copy}"
    );
    let copy_id = copy["id"].as_str().expect("id").to_owned();
    let (status, renamed) = call(
        &app,
        auth,
        "PUT",
        &format!("/api/v3/playlists/{copy_id}"),
        Some(json!({"name": "Keepers"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{renamed}");

    let (status, off) = call(
        &app,
        auth,
        "PUT",
        "/api/v3/me/rotation",
        Some(json!({"enabled": false})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{off}");
    assert_eq!(off["enabled"], false, "{off}");
    assert!(rotation_lists(&app, auth).await.is_empty());
    let (status, kept) = call(
        &app,
        auth,
        "GET",
        &format!("/api/v3/playlists/{copy_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the copy stays: {kept}");
    let quiet = refresh(&app, &ada.user_id, WEDNESDAY + DAY_MS).await;
    assert_eq!(quiet, rotation::Written::default(), "off makes nothing");
    assert!(rotation_lists(&app, auth).await.is_empty());
    app.stop().await;
}
