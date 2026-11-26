//! Gaps in artists you collect and From your shelves, end to end over a
//! real database and the production builder: each is built from the
//! caller's own taste, plays and requests, and the library everyone
//! shares.

use std::sync::Arc;

use axum::Router;
use droppedneedle_auth::Role;
use droppedneedle_auth::session::store::SessionKind;
use droppedneedle_catalog::discover::services::DiscoverDeps;
use droppedneedle_kernel::clock::Clock as _;
use droppedneedle_kernel::jobs::queue::JobError;
use droppedneedle_providers::Providers;
use serde_json::Value;

use crate::common::auth::{AuthRig, test_principal, with_test_principal};
use crate::discover_pool::{album, albums};
use crate::discover_queue::{Script, call, discover_deps, insert, library_artist, seen_now};

const DAY: i64 = 86_400_000;
const COLLECTED: &str = "a0000000-0000-4000-8000-000000000001";
const OTHER: &str = "b0000000-0000-4000-8000-000000000002";

fn group(tag: char, n: u8) -> String {
    format!("{n:08x}-{tag}{tag}{tag}{tag}-4000-8000-000000000000")
}

/// Discover's routes over `deps`, signed in as `user`.
fn routes_for(deps: &DiscoverDeps, user: &str) -> Router {
    let router = droppedneedle_catalog::discover::routes()
        .with_state(deps.clone())
        .into_router();
    with_test_principal(
        router,
        test_principal(
            user,
            Role::User,
            &format!("sess-{user}"),
            SessionKind::Standard,
        ),
    )
}

/// The page once a build has settled.
async fn built_page(app: &Router) -> Value {
    tokio::time::timeout(crate::common::PATIENCE, async {
        loop {
            let (_, page) = call(app, "GET", "/api/v3/discover", None).await;
            if page["refreshing"] == false && page["generated_at"].is_number() {
                return page;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the page was built")
}

/// The content of section `key` on `page`, and its state.
fn section<'a>(page: &'a Value, key: &str) -> (&'a Value, &'a Value) {
    let find = |list: &'a Value| {
        list.as_array()
            .and_then(|rows| rows.iter().find(|row| row["key"] == key))
    };
    (
        find(&page["sections"]).map_or(&Value::Null, |section| &section["content"]),
        find(&page["section_states"]).map_or(&Value::Null, |state| &state["state"]),
    )
}

fn ids<'a>(rows: &'a Value, at: &str) -> Vec<&'a str> {
    rows.as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| row[at]["musicbrainz_id"].as_str())
        .collect()
}

/// Library albums `(id, release group, year)` by the library artist
/// `artist`, each with one track; `indexed` says whether it has a file.
fn held(artist: &str, rows: &[(&str, &str, i64)], indexed: bool) -> String {
    let availability = if indexed { "indexed" } else { "missing" };
    rows.iter()
        .map(|(id, group, year)| {
            format!(
                "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
                 album_artist_name, album_artist_id, year, grouping_source, created_at, \
                 updated_at) VALUES ('{id}', 'r1', 'k-{id}', 'Title {id}', 'title {id}', \
                 'Someone', '{artist}', {year}, 'automatic', 1, 1); \
                 INSERT INTO local_album_artists (local_album_id, position, local_artist_id, \
                 role) VALUES ('{id}', 0, '{artist}', 'main'); \
                 INSERT INTO local_album_external_identities (local_album_id, provider, \
                 release_group_mbid, decision_source, selected_at) \
                 VALUES ('{id}', 'musicbrainz', '{group}', 'manual', 1); \
                 INSERT INTO local_tracks (id, local_album_id, root_id, file_path, \
                 relative_path, path_hash, file_size_bytes, file_mtime_ns, stat_revision, \
                 title, title_folded, album_title, album_title_folded, file_format, \
                 availability, ingest_source, imported_at, membership_source) \
                 VALUES ('t-{id}', '{id}', 'r1', '/music/{id}.flac', '{id}.flac', 'h-{id}', 1, \
                 1, 's-{id}', 'Song', 'song', 'Title {id}', 'title {id}', 'flac', \
                 '{availability}', 'scan', 1, 'automatic');"
            )
        })
        .collect()
}

/// `user`'s taste build at `at`, its history from `from`, naming
/// `artists` (id, weight).
fn taste(user: &str, artists: &[(&str, f64)], at: i64, from: i64) -> String {
    let mut rows = format!(
        "INSERT INTO catalog_taste_builds (user_id, built_at, plays_counted, \
         imported_listens_counted, artists_played, seeds, incomplete, history_from_at) \
         VALUES ('{user}', {at}, 10, 0, 1, 1, 0, {from}); \
         INSERT INTO catalog_taste_groups (user_id, group_no, anchor_artist_mbid, anchor_name, \
         share, recent_share, trend) VALUES ('{user}', 0, 'anchor', 'Anchor', 1.0, 1.0, \
         'steady');"
    );
    for (artist, weight) in artists {
        rows += &format!(
            "INSERT INTO catalog_taste_artists (user_id, artist_mbid_lower, artist_mbid, name, \
             plays, imported_listens, months_active, weight, is_seed, group_no) \
             VALUES ('{user}', '{artist}', '{artist}', 'Name {}', 5, 0, 1, {weight}, 1, 0);",
            &artist[..2]
        );
    }
    rows
}

/// `count` plays by `user` of the track of album `album`, the latest at
/// `at` (unix ms), a minute apart.
fn plays_of(user: &str, album: &str, count: i64, at: i64) -> String {
    (0..count)
        .map(|n| {
            format!(
                "INSERT INTO library_play_history (id, user_id, local_track_id, track_name, \
                 artist_name, played_at) VALUES ('{user}-{album}-{n}', '{user}', 't-{album}', \
                 'Song', 'Someone', {});",
                at - 60_000 * n
            )
        })
        .collect()
}

fn deps(rig: &AuthRig, dir: &std::path::Path) -> DiscoverDeps {
    discover_deps(rig, dir, Providers::closed(), |slot| {
        *slot = Arc::new(Script::default())
    })
}

/// Two people on one library get the gaps of their own artists, heaviest
/// first: albums and EPs the library does not hold of artists it holds three
/// or more albums of, never a single, a demo, an owned album or one already
/// asked for, each saying how many of the artist's albums the library holds
/// and showing them, oldest first.
#[tokio::test]
async fn each_person_gets_the_gaps_in_their_own_artists() {
    let dir = crate::common::ScratchDir::new("discover-gaps");
    let rig = AuthRig::new("discover-gaps").await;
    let db = rig.platform.db.clone();
    let now = rig.clock().now_ms();
    let (single, demo, asked) = (group('c', 10), group('c', 11), group('c', 12));
    let (first, second) = (group('c', 20), group('c', 21));
    let theirs = group('d', 20);
    let owned_a = [group('a', 1), group('a', 2), group('a', 3)];
    let owned_b = [group('b', 1), group('b', 2), group('b', 3), group('b', 4)];
    insert(
        &db,
        [
            "INSERT INTO auth_users (id, display_name, role, created_at) \
             VALUES ('user-1', 'Ada', 'user', 0), ('user-2', 'Bea', 'user', 0);"
                .to_owned(),
            library_artist("ar-a", "Collected", COLLECTED, "person"),
            library_artist("ar-b", "Other", OTHER, "person"),
            held(
                "ar-a",
                &[
                    ("al-a3", &owned_a[2], 2010),
                    ("al-a1", &owned_a[0], 2005),
                    ("al-a2", &owned_a[1], 2007),
                ],
                true,
            ),
            held(
                "ar-b",
                &[
                    ("al-b1", &owned_b[0], 1972),
                    ("al-b2", &owned_b[1], 1973),
                    ("al-b3", &owned_b[2], 1974),
                    ("al-b4", &owned_b[3], 1976),
                ],
                true,
            ),
            albums(
                COLLECTED,
                &[
                    album(&owned_a[0], "Album", "[]", &[COLLECTED]),
                    album(&single, "Single", "[]", &[COLLECTED]),
                    album(&demo, "Album", "[\"Demo\"]", &[COLLECTED]),
                    album(&asked, "Album", "[]", &[COLLECTED]),
                    album(&first, "Album", "[]", &[COLLECTED]),
                    album(&second, "EP", "[]", &[COLLECTED]),
                ],
                now,
            ),
            albums(
                OTHER,
                &[
                    album(&owned_b[3], "Album", "[]", &[OTHER]),
                    album(&theirs, "Album", "[]", &[OTHER]),
                ],
                now,
            ),
            format!(
                "INSERT INTO request_history (musicbrainz_id_lower, musicbrainz_id, \
                 artist_name, album_title, requested_at, status) \
                 VALUES ('{asked}', '{asked}', 'Collected', 'Asked', 1, 'pending');"
            ),
            // Ada plays both artists, the collected one most; Bea only the
            // other.
            taste("user-1", &[(COLLECTED, 2.0), (OTHER, 1.0)], now, now - DAY),
            taste("user-2", &[(OTHER, 1.0)], now, now - DAY),
        ]
        .concat(),
    )
    .await;
    seen_now(&rig, "user-1").await;
    seen_now(&rig, "user-2").await;
    let deps = deps(&rig, &dir);
    for user in ["user-1", "user-2"] {
        deps.pool
            .run_once(user, |_| Ok::<(), JobError>(()))
            .await
            .expect("the pool was built");
    }

    let page = built_page(&routes_for(&deps, "user-1")).await;
    let (gaps, _) = section(&page, "gaps_in_your_artists");
    let gaps = &gaps["gaps"];
    assert_eq!(
        ids(gaps, "album"),
        [first.as_str(), second.as_str(), theirs.as_str()],
        "her heaviest artist's gaps first, and nothing owned, asked for, single or demo: {page}"
    );
    assert_eq!(
        gaps[0]["reason"],
        "You have 3 of their albums, not this one."
    );
    assert_eq!(
        gaps[2]["reason"],
        "You have 4 of their albums, not this one."
    );
    let shelf: Vec<(&str, i64)> = gaps[0]["held"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|row| {
            (
                row["local_id"].as_str().unwrap_or_default(),
                row["year"].as_i64().unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(
        shelf,
        [("al-a1", 2005), ("al-a2", 2007), ("al-a3", 2010)],
        "the library's albums of the artist, oldest first"
    );
    assert_eq!(gaps[0]["album"]["year"], 2016, "the missing album's year");

    let page = built_page(&routes_for(&deps, "user-2")).await;
    let (gaps, _) = section(&page, "gaps_in_your_artists");
    assert_eq!(
        ids(&gaps["gaps"], "album"),
        [theirs.as_str()],
        "Bea's own artist only: {page}"
    );
}

/// From your shelves lists an album played nine times and last played a
/// hundred days ago, not one played last week or one with no files, and an
/// album its requester asked for and never played, for that person only.
#[tokio::test]
async fn own_shelves_hold_only_the_callers_plays_and_requests() {
    let dir = crate::common::ScratchDir::new("discover-own-shelves");
    let rig = AuthRig::new("discover-own-shelves").await;
    let db = rig.platform.db.clone();
    let now = rig.clock().now_ms();
    let arrived = group('e', 4);
    insert(
        &db,
        [
            "INSERT INTO auth_users (id, display_name, role, created_at) \
             VALUES ('user-1', 'Ada', 'user', 0), ('user-2', 'Bea', 'user', 0);"
                .to_owned(),
            library_artist("ar-s", "Shelved", COLLECTED, "person"),
            held(
                "ar-s",
                &[
                    ("al-quiet", &group('e', 1), 2001),
                    ("al-lately", &group('e', 2), 2002),
                    ("al-asked", &arrived, 2003),
                ],
                true,
            ),
            held("ar-s", &[("al-gone", &group('e', 3), 2004)], false),
            plays_of("user-1", "al-quiet", 9, now - 100 * DAY),
            plays_of("user-1", "al-lately", 9, now - 7 * DAY),
            plays_of("user-1", "al-gone", 9, now - 100 * DAY),
            format!(
                "INSERT INTO request_history (musicbrainz_id_lower, musicbrainz_id, \
                 artist_name, album_title, requested_at, completed_at, status) \
                 VALUES ('{arrived}', '{arrived}', 'Shelved', 'Asked', {asked}, {done}, \
                 'imported'); \
                 INSERT INTO request_history_requesters (user_id, musicbrainz_id_lower, \
                 requested_at) VALUES ('user-1', '{arrived}', {asked});",
                asked = now - 25 * DAY,
                done = now - 20 * DAY,
            ),
        ]
        .concat(),
    )
    .await;
    seen_now(&rig, "user-1").await;
    seen_now(&rig, "user-2").await;
    let deps = deps(&rig, &dir);

    let page = built_page(&routes_for(&deps, "user-1")).await;
    let (content, _) = section(&page, "from_your_shelves");
    let shelves = &content["own_shelves"];
    let local = |list: &str| -> Vec<&str> {
        shelves[list]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|row| row["album"]["local_id"].as_str())
            .collect()
    };
    assert_eq!(local("played_a_lot"), ["al-quiet"], "{page}");
    assert_eq!(
        shelves["played_a_lot"][0]["line"],
        "9 plays in September 2023, none since."
    );
    assert_eq!(shelves["played_a_lot"][0]["last_played"], "2023-09");
    assert_eq!(local("never_played"), ["al-asked"]);
    assert_eq!(
        shelves["never_played"][0]["line"],
        "Asked for in December 2023, not played yet."
    );
    assert_eq!(shelves["history_from"], "2023-09");
    assert_eq!(shelves["month"], "2024-01");

    let page = built_page(&routes_for(&deps, "user-2")).await;
    let (content, state) = section(&page, "from_your_shelves");
    assert!(
        content.is_null(),
        "Ada's plays and requests are not Bea's: {page}"
    );
    assert_eq!(state, "empty");
}
