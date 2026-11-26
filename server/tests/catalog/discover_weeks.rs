//! Past weeks over a real database and Discover's routes: every kept set
//! reads back as it was shown, newest first, with each pick's state as the
//! week reads it, a kept album's arrival and the person's own plays of it,
//! a page of weeks at a time; the filters narrow the picks on the server;
//! the record counts every set; "found" lists only what was kept; and
//! "Forget my history" empties both.

use axum::Router;
use axum::http::StatusCode;
use serde_json::{Value, json};

use droppedneedle_catalog::discover::engine::weekly::{self, SetDay, WEEK_MS};
use droppedneedle_kernel::clock::Clock as _;
use droppedneedle_providers::Providers;

use crate::common::auth::AuthRig;
use crate::discover_pool::id;
use crate::discover_queue::{Script, call, discover_deps, discover_routes, insert, own};

const DAY: i64 = 86_400_000;

fn album(n: usize) -> String {
    id(7, n)
}

/// A set for `user` for the week starting `week`, its picks the albums
/// `albums` in that order, each named "Album n" by "Artist n" with the
/// line "Close to Seed n.".
fn set(user: &str, week: i64, albums: &[usize]) -> String {
    let mut rows = format!(
        "INSERT INTO catalog_weekly_sets (user_id, week_start_at, built_at, plays_counted, \
         seeds, shelf_artists, shelf_albums, linked, tastes) VALUES ('{user}', {week}, {week}, \
         40, 3, 0, 0, 1, '{{\"version\":1,\"tastes\":[]}}');"
    );
    for (no, n) in albums.iter().enumerate() {
        let a = album(*n);
        let evidence = json!({
            "release_group_mbid": a, "title": format!("Album {n}"),
            "artist_name": format!("Artist {n}"),
            "reason": {"kind": "your_artists", "line": format!("Close to Seed {n}."), "artists": []},
            "seeds": [], "new_to_you": {"no_plays": true, "not_in_library": true},
            "found_by": "co_listening", "source": "listenbrainz"
        });
        rows += &format!(
            "INSERT INTO catalog_weekly_set_picks (user_id, week_start_at, pick_no, \
             release_group_mbid_lower, release_group_mbid, title, artist_mbid, artist_name, \
             primary_type, first_release_date, reason_line, evidence) VALUES ('{user}', {week}, \
             {pick_no}, '{a}', '{a}', 'Album {n}', '{artist}', 'Artist {n}', 'Album', \
             '2011-02-03', 'Close to Seed {n}.', '{evidence}');",
            pick_no = no + 1,
            artist = id(8, *n),
        );
    }
    rows
}

async fn get(app: &Router, uri: &str) -> Value {
    let (status, body) = call(app, "GET", uri, None).await;
    assert_eq!(status, StatusCode::OK, "{uri}: {body}");
    body
}

/// A listed pick: its album, state, plays and arrival.
type Listed = (String, String, Value, Value);

/// Each listed week's start and its picks, in order.
fn weeks_of(body: &Value) -> Vec<(i64, Vec<Listed>)> {
    body["items"]
        .as_array()
        .expect("weeks")
        .iter()
        .map(|week| {
            let picks = week["picks"]
                .as_array()
                .expect("picks")
                .iter()
                .map(|pick| {
                    (
                        pick["album"]["musicbrainz_id"]
                            .as_str()
                            .expect("an album id")
                            .to_owned(),
                        pick["state"].as_str().expect("a state").to_owned(),
                        pick["plays"].clone(),
                        pick["arrived_at"].clone(),
                    )
                })
                .collect();
            (week["week_start_at"].as_i64().expect("a week"), picks)
        })
        .collect()
}

/// The albums listed, week by week.
fn albums_of(body: &Value) -> Vec<Vec<String>> {
    weeks_of(body)
        .into_iter()
        .map(|(_, picks)| picks.into_iter().map(|pick| pick.0).collect())
        .collect()
}

/// With three kept weeks, the list answers newest first with each pick's
/// state and its reason as stored, a kept album's arrival and the person's
/// own plays of it; the cursor pages and ends; the answer and the search
/// narrow the picks on the server and leave each week's tally whole; the
/// record counts every set and dates "still played" only once a kept pick
/// has arrived; "found" lists the kept picks only; and Forget empties both.
#[tokio::test]
async fn discover_weeks_reads_every_set_as_it_was_shown() {
    let dir = crate::common::ScratchDir::new("discover-weeks");
    let rig = AuthRig::new("discover-weeks").await;
    let db = rig.platform.db.clone();
    let now = rig.clock().now_ms();
    let w3 = weekly::day_on_or_before(now, SetDay::Friday);
    let (w2, w1) = (w3 - WEEK_MS, w3 - 2 * WEEK_MS);
    let at = now - DAY;

    let mut rows = String::from(
        "INSERT INTO auth_users (id, display_name, role, created_at) \
         VALUES ('user-1', 'Ada', 'user', 0), ('user-2', 'Bob', 'user', 0);",
    );
    rows += &set("user-1", w1, &[1, 2, 3]);
    rows += &set("user-1", w2, &[4, 5, 6]);
    rows += &set("user-1", w3, &[7, 8]);
    // Bob's set holds the album Ada kept; it is never Ada's.
    rows += &set("user-2", w3, &[1]);
    // Album 1 kept and in the library, album 6 asked for and on its way.
    for n in [1, 6] {
        rows += &format!(
            "INSERT INTO request_history (musicbrainz_id_lower, musicbrainz_id, artist_name, \
             album_title, requested_at, status, user_id) VALUES ('{a}', '{a}', 'Artist {n}', \
             'Album {n}', {at}, 'pending', 'user-1'); \
             INSERT INTO request_history_requesters (user_id, musicbrainz_id_lower, \
             requested_at) VALUES ('user-1', '{a}', {at});",
            a = album(n)
        );
    }
    rows += &format!(
        "INSERT INTO catalog_discover_feedback (user_id, target, target_mbid_lower, kind, \
         created_at) VALUES ('user-1', 'album', '{no}', 'no', {at}), \
         ('user-1', 'album', '{known}', 'know_it', {at}); \
         INSERT INTO catalog_later_albums (user_id, release_group_mbid_lower, \
         release_group_mbid, title, artist_name, added_at) VALUES ('user-1', '{later}', \
         '{later}', 'Album 4', 'Artist 4', {at});",
        no = album(2),
        known = album(5),
        later = album(4),
    );
    insert(&db, rows).await;
    own(&db, &[album(1)]).await;
    insert(
        &db,
        format!(
            "INSERT INTO library_user_album_plays (user_id, local_album_id, plays, \
             last_played_at, played) VALUES ('user-1', 'al0', 3, {at}, 1), \
             ('user-2', 'al0', 9, {at}, 1);"
        ),
    )
    .await;

    let deps = discover_deps(&rig, &dir, Providers::closed(), |slot| {
        *slot = std::sync::Arc::new(Script::default());
    });
    let app = discover_routes(deps);
    let weeks = "/api/v3/discover/weeks";

    // Before the kept album's arrival is in the pick record, nothing dates
    // "still played a month on".
    let body = get(&app, weeks).await;
    assert_eq!(
        body["record"],
        json!({
            "picked": 8,
            "answers": {"kept": 2, "later": 1, "known": 1, "passed": 1, "open": 3},
            "counted_from_at": null
        }),
        "the record counts every set: {body}"
    );
    let arrived = at - DAY;
    insert(
        &db,
        format!(
            "INSERT INTO catalog_discover_picks (user_id, release_group_mbid_lower, \
             release_group_mbid, first_surface, first_offered_at, last_offered_at, evidence, \
             arrived_at) VALUES ('user-1', '{a}', '{a}', 'weekly_set', {w1}, {w1}, '{{}}', \
             {arrived})",
            a = album(1)
        ),
    )
    .await;

    let body = get(&app, weeks).await;
    assert_eq!(
        body["record"]["counted_from_at"],
        json!(arrived + 30 * DAY),
        "counted from the first arrival a month on: {body}"
    );
    let s = |text: &str| text.to_owned();
    assert_eq!(
        weeks_of(&body),
        [
            (
                w3,
                vec![
                    (album(7), s("open"), Value::Null, Value::Null),
                    (album(8), s("open"), Value::Null, Value::Null),
                ]
            ),
            (
                w2,
                vec![
                    (album(4), s("later"), Value::Null, Value::Null),
                    (album(5), s("known"), Value::Null, Value::Null),
                    (album(6), s("asked"), Value::Null, Value::Null),
                ]
            ),
            (
                w1,
                vec![
                    (album(1), s("arrived"), json!(3), json!(arrived)),
                    (album(2), s("no"), Value::Null, Value::Null),
                    (album(3), s("open"), Value::Null, Value::Null),
                ]
            ),
        ],
        "newest first, each pick's state, and Ada's own plays of what she kept: {body}"
    );
    let oldest = &body["items"][2];
    assert_eq!(oldest["week_number"], weekly::week_number(w1));
    assert_eq!(
        oldest["tally"],
        json!({"kept": 1, "later": 0, "known": 0, "passed": 1, "open": 1})
    );
    assert_eq!(oldest["picks"][1]["reason"]["line"], "Close to Seed 2.");
    assert_eq!(oldest["picks"][0]["album"]["year"], 2011);
    assert_eq!(body["next_cursor"], Value::Null);

    // Two weeks a page: the cursor reaches the third and ends there.
    let first = get(&app, &format!("{weeks}?limit=2")).await;
    assert_eq!(albums_of(&first).len(), 2);
    assert!(
        first["record"].is_object(),
        "the first page carries the record"
    );
    let cursor = first["next_cursor"].as_str().expect("a next page");
    let next = get(&app, &format!("{weeks}?limit=2&cursor={cursor}")).await;
    assert_eq!(weeks_of(&next)[0].0, w1);
    assert_eq!(next["next_cursor"], Value::Null, "the last page");
    assert_eq!(next["record"], Value::Null, "only the first page has it");
    let (status, _) = call(&app, "GET", &format!("{weeks}?cursor=nonsense"), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // The answer and the search narrow the picks; a week with none left
    // goes, and each week's tally stays whole.
    let kept = get(&app, &format!("{weeks}?state=kept")).await;
    assert_eq!(albums_of(&kept), [vec![album(6)], vec![album(1)]]);
    assert_eq!(kept["items"][1]["tally"]["passed"], 1);
    let open = get(&app, &format!("{weeks}?state=open")).await;
    assert_eq!(albums_of(&open), [vec![album(7), album(8)], vec![album(3)]]);
    let passed = get(&app, &format!("{weeks}?state=passed")).await;
    assert_eq!(albums_of(&passed), [vec![album(2)]]);
    let by_title = get(&app, &format!("{weeks}?q=%20ALBUM%205%20")).await;
    assert_eq!(albums_of(&by_title), [vec![album(5)]]);
    let by_artist = get(&app, &format!("{weeks}?q=artist%207&state=open")).await;
    assert_eq!(albums_of(&by_artist), [vec![album(7)]]);
    let none = get(&app, &format!("{weeks}?q=nothing%20like%20it")).await;
    assert_eq!(none["items"], json!([]));

    // Found lists the kept picks only, with the week each was picked.
    let found = get(&app, "/api/v3/discover/found").await;
    assert_eq!(
        found["items"],
        json!([
            {"release_group_mbid": album(6), "week_start_at": w2,
             "week_number": weekly::week_number(w2)},
            {"release_group_mbid": album(1), "week_start_at": w1,
             "week_number": weekly::week_number(w1)},
        ])
    );

    // Forget my history takes every set, so both are empty.
    let (status, body) = call(&app, "DELETE", "/api/v3/discover/history", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let body = get(&app, weeks).await;
    assert_eq!(body["items"], json!([]));
    assert_eq!(body["record"]["picked"], 0);
    let found = get(&app, "/api/v3/discover/found").await;
    assert_eq!(found["items"], json!([]));
}
