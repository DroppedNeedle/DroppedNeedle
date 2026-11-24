//! A person's year in review over the production builder: it counts only
//! their own plays here, in the calendar year of the zone they name, an
//! album by where its track is now and only while it is owned, a song once
//! however many copies were played, and an artist as new only when nothing
//! before the year says it was heard.

use axum::http::StatusCode;
use serde_json::{Value, json};

use crate::common::TestApp;

/// Unix milliseconds of `y-m-d h:min` UTC.
fn at(y: i64, m: i64, d: i64, h: i64, min: i64) -> i64 {
    // Days from 1970-01-01 to the civil date (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    ((days * 24 + h) * 60 + min) * 60_000
}

fn artist(id: &str, name: &str) -> String {
    format!(
        "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at) \
         VALUES ('{id}', '{name}', lower('{name}'), 'person', 0, 0);"
    )
}

fn artist_mbid(id: &str, mbid: &str) -> String {
    format!(
        "INSERT INTO local_artist_external_identities (local_artist_id, provider, \
         provider_artist_id, decision_source, selected_at) \
         VALUES ('{id}', 'musicbrainz', '{mbid}', 'manual', 0);"
    )
}

fn album(id: &str, title: &str, artist: &str) -> String {
    format!(
        "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_id, grouping_source, created_at, updated_at) \
         VALUES ('{id}', 'root-1', 'g-{id}', '{title}', lower('{title}'), '{artist}', \
         'manual', 0, 0);"
    )
}

/// A track of `seconds` on `album`, indexed or `missing`.
fn track(id: &str, album: &str, title: &str, seconds: f64, availability: &str) -> String {
    format!(
        "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
         path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
         album_title, album_title_folded, file_format, ingest_source, imported_at, \
         membership_source, availability, duration_seconds) VALUES ('{id}', '{album}', \
         'root-1', '/music/{id}.flac', '{id}.flac', 'h-{id}', 4096, 0, 's', '{title}', \
         lower('{title}'), 'Album', 'album', 'flac', 'scan', 0, 'automatic', \
         '{availability}', {seconds});"
    )
}

/// One play of `user`'s at `at`, of `track` (else by name), filed under
/// `album` and `artist` as the play row says.
#[allow(clippy::too_many_arguments)]
fn play(
    id: &str,
    user: &str,
    track: Option<&str>,
    album: Option<&str>,
    artist: Option<&str>,
    names: (&str, &str),
    length: Option<i64>,
    at: i64,
) -> String {
    let text = |value: Option<&str>| value.map_or("NULL".to_owned(), |v| format!("'{v}'"));
    format!(
        "INSERT INTO library_play_history (id, user_id, local_track_id, local_album_id, \
         local_artist_id, track_name, artist_name, duration_ms, played_at) VALUES ('{id}', \
         '{user}', {track}, {album}, {artist}, '{song}', '{by}', {length}, {at});",
        track = text(track),
        album = text(album),
        artist = text(artist),
        song = names.0,
        by = names.1,
        length = length.map_or("NULL".to_owned(), |ms| ms.to_string()),
    )
}

async fn get(app: &TestApp, auth: &str, uri: &str) -> (StatusCode, Value) {
    app.call("GET", uri, &[("authorization", auth)], None).await
}

async fn review(app: &TestApp, auth: &str, uri: &str) -> Value {
    let (status, body) = get(app, auth, uri).await;
    assert_eq!(status, StatusCode::OK, "{uri}: {body}");
    body
}

fn ids(list: &Value) -> Vec<Value> {
    list.as_array()
        .map(|items| items.iter().map(|item| item["id"].clone()).collect())
        .unwrap_or_default()
}

/// Only the caller's own plays here count: not an imported listen, not a
/// playback event, not another person's plays. A play counts for the album
/// its track is on now, a removed album's plays count in the totals but
/// it tops no list, a length falls back to the track's, and under 30 plays
/// the lists are empty.
#[tokio::test]
async fn a_year_counts_the_callers_own_plays_here() {
    const GROUP: &str = "1f9e3a5c-7b2d-4c8e-9a6f-0d1e2b3c4a5d";
    let app = TestApp::start("year-review-counts").await;
    let ada = app.login_user("ada").await;
    let bob = app.login_user("bob").await;
    let mut seed = [
        artist("art-1", "Ada Lovelace Band"),
        artist("art-2", "Second"),
        artist("art-3", "Third"),
        artist("art-4", "Fourth"),
        artist("art-9", "Bob Only"),
        album("alb-now", "Where It Is", "art-1"),
        album("alb-then", "Where It Was", "art-1"),
        album("alb-gone", "Gone Away", "art-1"),
        track("trk-1", "alb-now", "Moved Song", 180.0, "indexed"),
        track("trk-then", "alb-then", "Stayed", 180.0, "indexed"),
        track("trk-gone", "alb-gone", "Lost Song", 100.0, "missing"),
        format!(
            "INSERT INTO local_album_external_identities (local_album_id, \
             release_group_mbid, decision_source, selected_at) \
             VALUES ('alb-now', '{GROUP}', 'manual', 0);"
        ),
    ]
    .concat();
    let march = at(2025, 3, 10, 12, 0);
    for n in 0..20 {
        seed += &play(
            &format!("ada-now-{n}"),
            &ada.user_id,
            Some("trk-1"),
            Some("alb-then"),
            Some("art-1"),
            ("Moved Song", "Ada Lovelace Band"),
            Some(200_000),
            march + n * 60_000,
        );
    }
    for n in 0..6 {
        seed += &play(
            &format!("ada-gone-{n}"),
            &ada.user_id,
            Some("trk-gone"),
            Some("alb-gone"),
            Some("art-1"),
            ("Lost Song", "Ada Lovelace Band"),
            None,
            march + (30 + n) * 60_000,
        );
    }
    for (n, (id, name)) in [
        (Some("art-2"), "Second"),
        (Some("art-3"), "Third"),
        (Some("art-4"), "Fourth"),
        (None, "The Busker"),
        (None, "Unknown Artist"),
    ]
    .into_iter()
    .enumerate()
    {
        seed += &play(
            &format!("ada-named-{n}"),
            &ada.user_id,
            None,
            None,
            id,
            ("A Song", name),
            Some(0),
            at(2025, 4, 1 + n as i64, 9, 0),
        );
    }
    for n in 0..40 {
        seed += &play(
            &format!("bob-{n}"),
            &bob.user_id,
            None,
            None,
            Some("art-9"),
            ("Bob Song", "Bob Only"),
            Some(1000),
            march + n * 60_000,
        );
    }
    seed += &format!(
        "INSERT INTO media_imported_listens (user_id, listened_at, artist_mbid, artist_name, \
         track_name, imported_at) VALUES ('{ada}', {listen}, NULL, 'Elsewhere', 'Song', 0); \
         INSERT INTO media_playback_events (id, user_id, track_name, artist_name, source, \
         client, started_at, ended_at, listened_ms, ended_by, came_from, counted) VALUES \
         ('ev-1', '{ada}', 'Moved Song', 'Ada Lovelace Band', 'local', 'web', {event}, \
         {event}, 1000, 'skipped', 'album', 0);",
        ada = ada.user_id,
        listen = at(2025, 5, 1, 0, 0),
        event = at(2025, 5, 2, 0, 0),
    );
    app.seed(seed).await;

    let year = review(
        &app,
        &ada.authorization,
        "/api/v3/me/year-in-review/2025?zone=UTC",
    )
    .await;
    assert_eq!(year["state"], "complete", "{year}");
    assert_eq!(year["zone"], "UTC");
    assert_eq!(
        (year["plays"].clone(), year["artists"].clone()),
        (json!(31), json!(5)),
        "{year}"
    );
    assert_eq!(
        (
            year["listened_ms"].clone(),
            year["plays_without_length"].clone()
        ),
        (json!(20 * 200_000 + 6 * 100_000), json!(5)),
        "a length falls back to the track's: {year}"
    );
    assert_eq!(year["months"][2], 26, "{year}");
    assert_eq!(year["months"][3], 5, "{year}");
    assert_eq!(year["months"][4], 0, "a listen and an event add nothing");
    assert_eq!(ids(&year["top_albums"]), vec![json!("alb-now")], "{year}");
    assert_eq!(year["top_albums"][0]["plays"], 20, "{year}");
    assert_eq!(
        ids(&year["top_artists"]),
        vec![
            json!("art-1"),
            json!("art-2"),
            json!("art-3"),
            json!("art-4"),
            Value::Null
        ],
        "{year}"
    );
    assert_eq!(year["top_artists"][4]["name"], "The Busker", "{year}");
    assert_eq!(
        ids(&year["top_songs"])[..2],
        [json!("trk-1"), json!("trk-gone")],
        "{year}"
    );
    assert_eq!(year["top_songs"][0]["album_id"], "alb-now", "{year}");
    assert_eq!(year["top_songs"][0]["album_name"], "Where It Is", "{year}");
    assert_eq!(
        (
            year["top_albums"][0]["release_group_mbid"].clone(),
            year["top_songs"][0]["release_group_mbid"].clone()
        ),
        (json!(GROUP), json!(GROUP)),
        "a matched album carries its release group: {year}"
    );
    assert_eq!(year["busiest_month"]["month"], 3, "{year}");
    assert_eq!(year["busiest_month"]["album"]["id"], "alb-now", "{year}");

    let years = review(
        &app,
        &bob.authorization,
        "/api/v3/me/year-in-review?zone=UTC",
    )
    .await;
    let bobs_2025 = years["years"]
        .as_array()
        .and_then(|years| years.iter().find(|row| row["year"] == 2025))
        .cloned();
    assert_eq!(
        bobs_2025.map(|row| row["plays"].clone()),
        Some(json!(40)),
        "{years}"
    );

    app.seed("DELETE FROM library_play_history WHERE id IN ('ada-now-0', 'ada-now-1')")
        .await;
    let year = review(
        &app,
        &ada.authorization,
        "/api/v3/me/year-in-review/2025?zone=UTC",
    )
    .await;
    assert_eq!(year["state"], "too_few_plays", "{year}");
    assert_eq!(year["plays"], 29, "{year}");
    for list in ["top_albums", "top_artists", "top_songs"] {
        assert_eq!(year[list], json!([]), "{list}: {year}");
    }
    assert_eq!(year["new_artists"], Value::Null, "{year}");
    app.stop().await;
}

/// The calendar is the zone asked for: one play on New Year's morning in
/// London is New Year's Eve in Los Angeles. A zone the server does not
/// know falls back to the server's and says so, one that cannot be a name
/// is refused, and a year after this one has no review.
#[tokio::test]
async fn the_year_follows_the_zone_asked_for() {
    let app = TestApp::start_configured("year-review-zones", |mut config| {
        config.timezone = Some("Asia/Tokyo".to_owned());
        config
    })
    .await;
    let ada = app.login_user("ada").await;
    let auth = ada.authorization.as_str();
    app.seed(play(
        "new-year",
        &ada.user_id,
        None,
        None,
        None,
        ("Auld Lang Syne", "Choir"),
        None,
        at(2026, 1, 1, 7, 30),
    ))
    .await;
    let base = "/api/v3/me/year-in-review";

    let la = review(&app, auth, &format!("{base}/2025?zone=America/Los_Angeles")).await;
    assert_eq!(
        (la["plays"].clone(), la["zone"].clone()),
        (json!(1), json!("America/Los_Angeles"))
    );
    let london = review(&app, auth, &format!("{base}/2026?zone=Europe/London")).await;
    assert_eq!(london["plays"], 1, "{london}");
    let (status, body) = get(&app, auth, &format!("{base}/2025?zone=Europe/London")).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "no play in London's 2025: {body}"
    );
    assert_eq!(body["error"]["code"], "YEAR_NOT_FOUND", "{body}");

    for asked in ["?zone=Mars/Olympus_Mons", "?zone=Etc/Unknown", ""] {
        let fallen = review(&app, auth, &format!("{base}/2026{asked}")).await;
        assert_eq!(
            (fallen["zone"].clone(), fallen["plays"].clone()),
            (json!("Asia/Tokyo"), json!(1)),
            "{asked}: {fallen}"
        );
    }
    let long = "A".repeat(65);
    for malformed in [long.as_str(), "Europe%2FLon%01don"] {
        let (status, body) = get(&app, auth, &format!("{base}?zone={malformed}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{malformed}: {body}");
    }

    let years = review(&app, auth, &format!("{base}?zone=Europe/London")).await;
    let current = years["years"][0]["year"]
        .as_i64()
        .expect("the current year");
    assert_eq!(years["zone"], "Europe/London", "{years}");
    for year in [current + 1, 99_999, 0] {
        let (status, body) = get(&app, auth, &format!("{base}/{year}?zone=Europe/London")).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{year}: {body}");
    }
    app.stop().await;
}

/// An artist first played here this year is new unless an imported listen
/// from before the year names it; an artist played here before is never
/// new, nor a play filed by name alone; in a first year (no history before
/// it) there is no list at all.
#[tokio::test]
async fn new_to_you_skips_artists_heard_before_the_year() {
    let app = TestApp::start("year-review-new").await;
    let cy = app.login_user("cyril").await;
    let dee = app.login_user("deirdre").await;
    let heard = "6c5a1c8e-2f0b-4d8a-9b7e-3f1d2c4a5b60";
    let fresh = "7d6b2d9f-3a1c-4e9b-8c8f-4a2e3d5b6c71";
    let mut seed = [
        artist("art-old", "Old Friend"),
        artist("art-heard", "Heard Elsewhere"),
        artist("art-fresh", "Fresh Find"),
        artist("art-plain", "Plain Find"),
        artist_mbid("art-heard", heard),
        artist_mbid("art-fresh", fresh),
    ]
    .concat();
    seed += &play(
        "cy-2024",
        &cy.user_id,
        None,
        None,
        Some("art-old"),
        ("Old Song", "Old Friend"),
        None,
        at(2024, 6, 1, 12, 0),
    );
    for (n, (who, id, name)) in [
        (&cy.user_id, Some("art-fresh"), "Fresh Find"),
        (&cy.user_id, Some("art-plain"), "Plain Find"),
        (&cy.user_id, Some("art-old"), "Old Friend"),
        (&cy.user_id, Some("art-heard"), "Heard Elsewhere"),
        (&cy.user_id, None, "The Busker"),
        (&dee.user_id, Some("art-fresh"), "Fresh Find"),
        (&dee.user_id, Some("art-plain"), "Plain Find"),
        (&dee.user_id, Some("art-old"), "Old Friend"),
        (&dee.user_id, Some("art-heard"), "Heard Elsewhere"),
        (&dee.user_id, None, "The Busker"),
    ]
    .into_iter()
    .enumerate()
    {
        for k in 0..6 {
            seed += &play(
                &format!("p-{n}-{k}"),
                who,
                None,
                None,
                id,
                ("Song", name),
                None,
                at(2025, 2, 1 + (n % 5) as i64, 10, k),
            );
        }
    }
    seed += &format!(
        "INSERT INTO media_imported_listens (user_id, listened_at, artist_mbid, artist_name, \
         track_name, imported_at) VALUES ('{cy}', {before}, '{heard}', 'Heard Elsewhere', \
         'Song', 0), ('{cy}', {inside}, '{fresh}', 'Fresh Find', 'Song', 0);",
        cy = cy.user_id,
        before = at(2024, 9, 1, 0, 0),
        inside = at(2025, 6, 1, 0, 0),
    );
    app.seed(seed).await;
    let uri = "/api/v3/me/year-in-review/2025?zone=UTC";

    let year = review(&app, &cy.authorization, uri).await;
    let new = &year["new_artists"];
    assert_eq!(new["count"], 2, "{year}");
    let mut found = ids(&new["artists"]);
    found.sort_by_key(Value::to_string);
    assert_eq!(
        found,
        vec![json!("art-fresh"), json!("art-plain")],
        "{year}"
    );
    assert_eq!(new["first"]["artist"]["id"], "art-fresh", "{year}");
    assert_eq!(new["first"]["date"], "2025-02-01", "{year}");

    let first_year = review(&app, &dee.authorization, uri).await;
    assert_eq!(first_year["state"], "complete", "{first_year}");
    assert_eq!(first_year["new_artists"], Value::Null, "{first_year}");
    app.stop().await;
}
