//! New from your artists, end to end over a real database: the install
//! reads ListenBrainz's fresh releases once a day, and each person's list
//! is cut from it by their own artists, the follow poll's releases joined
//! in.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use axum::Router;
use droppedneedle_auth::Role;
use droppedneedle_auth::session::store::SessionKind;
use droppedneedle_catalog::discover::listening::FreshRow;
use droppedneedle_catalog::discover::services::DiscoverDeps;
use droppedneedle_kernel::clock::Clock as _;
use droppedneedle_providers::test_support::ScriptedWire;
use droppedneedle_providers::{ProviderEndpoints, Providers};
use serde_json::{Value, json};

use crate::common::Probe;
use crate::common::auth::{AuthRig, test_principal, with_test_principal};
use crate::discover_queue::{
    Script, call, discover_deps, insert, library_artist, own, pool_of, seen_now,
};

const PLAYED: &str = "10000000-0000-4000-8000-000000000001";
const OWNED: &str = "10000000-0000-4000-8000-000000000002";
const FOLLOWED: &str = "10000000-0000-4000-8000-000000000003";
const NEAR: &str = "10000000-0000-4000-8000-000000000004";
const NOBODY: &str = "10000000-0000-4000-8000-000000000005";
const HERS: &str = "10000000-0000-4000-8000-000000000006";
const HOUR_MS: i64 = 3_600_000;

fn group(n: u8) -> String {
    format!("{n:08x}-eeee-4eee-8eee-eeeeeeeeeeee")
}

/// A release of the sitewide feed. The rig's clock reads 2024-01-01.
fn release(n: u8, artist: &str, date: &str, kind: &str) -> FreshRow {
    FreshRow {
        release_group_mbid: group(n),
        title: format!("Release {n}"),
        artist_name: format!("Artist {n}"),
        artist_mbids: vec![artist.to_owned()],
        release_date: date.to_owned(),
        primary_type: Some(kind.to_owned()),
        secondary_type: None,
    }
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

/// The release groups of a list's rows, in order.
fn groups_of(rows: &Value) -> Vec<String> {
    rows.as_array()
        .into_iter()
        .flatten()
        .map(|row| {
            row["album"]["musicbrainz_id"]
                .as_str()
                .unwrap_or_default()
                .to_owned()
        })
        .collect()
}

/// The section of `key` on a page and how it stands.
fn section<'a>(page: &'a Value, key: &str) -> (&'a Value, Value) {
    let content = page["sections"]
        .as_array()
        .and_then(|sections| sections.iter().find(|section| section["key"] == key))
        .map_or(&Value::Null, |section| &section["content"]);
    let mut state = page["section_states"]
        .as_array()
        .and_then(|states| states.iter().find(|state| state["key"] == key))
        .cloned()
        .unwrap_or(Value::Null);
    if let Some(state) = state.as_object_mut() {
        state.remove("zone");
    }
    (content, state)
}

/// The page once a build newer than `before` (none: any) has settled.
async fn page_after(app: &Router, before: &Value) -> Value {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let (_, page) = call(app, "GET", "/api/v3/discover", None).await;
            if page["refreshing"] == false
                && page["generated_at"].is_number()
                && page["generated_at"] != before["generated_at"]
            {
                return page;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the page was built")
}

/// One read of the feed serves everyone that day. Each person sees only
/// releases by artists they play, the library owns, they follow or that sit
/// close to theirs in their pool, newest first, each with its own date and
/// type: an owned release listed and marked, a requested one marked, a
/// single as a single, an artist's neighbour with its line. Too old, left
/// out by type or by nobody's artist is not listed. Another person with
/// another history sees another list. When the next day's read fails, the
/// list stays as it was, and once the read has missed the page says from
/// when.
#[tokio::test]
async fn each_person_sees_their_own_artists_new_releases_from_one_read() {
    let dir = crate::common::ScratchDir::new("discover-new-releases");
    let rig = AuthRig::new("discover-new-releases").await;
    let db = rig.platform.db.clone();
    insert(
        &db,
        "INSERT INTO auth_users (id, display_name, role, created_at) \
         VALUES ('user-1', 'Ada', 'user', 0), ('user-2', 'Bea', 'user', 0);"
            .to_owned(),
    )
    .await;
    seen_now(&rig, "user-1").await;
    seen_now(&rig, "user-2").await;
    insert(
        &db,
        [
            pool_of(
                "user-1",
                &[(PLAYED, "Played")],
                &[(group(90), NEAR.to_owned(), PLAYED)],
            ),
            pool_of("user-2", &[(HERS, "Hers")], &[]),
            library_artist("o", "Owned", OWNED, "person"),
            format!(
                "INSERT INTO acquire_followed_artists (user_id, artist_mbid, artist_mbid_lower, \
                 artist_name, followed_at, updated_at) \
                 VALUES ('user-1', '{FOLLOWED}', '{FOLLOWED}', 'Followed', 1, 1); \
                 INSERT INTO request_history (musicbrainz_id_lower, musicbrainz_id, \
                 artist_name, album_title, requested_at, status) \
                 VALUES ('{g3}', '{g3}', 'Followed', 'Release 3', 1, 'pending'); \
                 INSERT INTO acquire_new_release_feed (release_group_mbid_lower, \
                 release_group_mbid, artist_mbid_lower, artist_name, title, primary_type, \
                 first_release_date, discovered_at) \
                 VALUES ('{g10}', '{g10}', '{FOLLOWED}', 'Followed', 'Polled', 'EP', \
                 '2023-12-31', 1);",
                g3 = group(3),
                g10 = group(10),
            ),
        ]
        .concat(),
    )
    .await;
    own(&db, &[group(2)]).await;
    insert(
        &db,
        "INSERT INTO local_album_artists (local_album_id, position, local_artist_id, role) \
         VALUES ('al0', 0, 'o', 'main');"
            .to_owned(),
    )
    .await;

    let kind = |row: FreshRow, secondary: &str| FreshRow {
        secondary_type: Some(secondary.to_owned()),
        ..row
    };
    let soundtrack = kind(release(9, PLAYED, "2023-12-30", "Album"), "Soundtrack");
    let script = Arc::new(Script {
        fresh: vec![
            release(8, PLAYED, "2023-09-01", "Album"),
            kind(release(11, PLAYED, "2023-12-25", "EP"), "Live"),
            kind(release(12, PLAYED, "2023-12-24", "Album"), "Compilation"),
            release(7, HERS, "2023-12-24", "Album"),
            release(6, NOBODY, "2023-12-25", "Album"),
            release(5, NEAR, "2023-12-26", "Album"),
            release(4, PLAYED, "2023-12-27", "Single"),
            release(3, FOLLOWED, "2023-12-28", "EP"),
            release(2, OWNED, "2023-12-29", "Album"),
            release(1, PLAYED, "2023-12-30", "Album"),
            soundtrack,
        ],
        ..Script::default()
    });
    let network = Arc::clone(&script);
    let providers = Providers::over_wire(
        Arc::new(ScriptedWire::new()),
        rig.platform.clock.clone(),
        ProviderEndpoints::closed(),
    );
    let deps = discover_deps(&rig, &dir, providers, |slot| *slot = network);
    let (ada, bea) = (routes_for(&deps, "user-1"), routes_for(&deps, "user-2"));
    let read_at = rig.clock().now_ms() / 1000;

    assert!(deps.fresh.refresh().await, "the day's read");
    assert!(!deps.fresh.refresh().await, "a kept feed is not read again");

    let (status, list) = call(&ada, "GET", "/api/v3/discover/new-releases", None).await;
    assert_eq!(status, 200, "{list}");
    let items = &list["items"];
    assert_eq!(
        groups_of(items),
        [
            group(10),
            group(1),
            group(2),
            group(3),
            group(4),
            group(5),
            group(11),
            group(12)
        ],
        "newest first, the follow poll's release joined in: {list}"
    );
    assert_eq!(list["total"], 8);
    let owned = &items[2]["album"];
    assert_eq!(
        (owned["in_library"].clone(), owned["requested"].clone()),
        (json!(true), json!(false))
    );
    assert!(
        owned["local_id"].is_string(),
        "an owned release links to its album"
    );
    assert_eq!(items[3]["album"]["requested"], true);
    assert_eq!(items[3]["album"]["release_type"], "ep");
    assert_eq!(items[4]["album"]["release_type"], "single");
    assert_eq!(items[4]["album"]["release_date"], "2023-12-27");
    assert_eq!(items[0]["album"]["release_date"], "2023-12-31");
    assert_eq!(items[5]["reason"], "New from Artist 0, close to Played.");
    assert!(
        items[1]["reason"].is_null(),
        "their own artist needs no line"
    );
    assert_eq!(items[6]["album"]["release_type"], "ep");
    assert_eq!(items[6]["secondary_kind"], "live", "a live EP says so");
    assert_eq!(items[7]["secondary_kind"], "compilation");
    assert!(items[0]["secondary_kind"].is_null());

    let (_, first) = call(&ada, "GET", "/api/v3/discover/new-releases?limit=4", None).await;
    assert_eq!(groups_of(&first["items"]).len(), 4);
    let cursor = first["next_cursor"].as_str().expect("a second page");
    let (_, rest) = call(
        &ada,
        "GET",
        &format!("/api/v3/discover/new-releases?limit=4&cursor={cursor}"),
        None,
    )
    .await;
    assert_eq!(
        groups_of(&rest["items"]),
        [group(4), group(5), group(11), group(12)]
    );
    assert!(rest["next_cursor"].is_null());

    let (_, hers) = call(&bea, "GET", "/api/v3/discover/new-releases", None).await;
    assert_eq!(
        groups_of(&hers["items"]),
        [group(2), group(7)],
        "her own artist's and the library's, none of Ada's"
    );

    call(&ada, "GET", "/api/v3/discover", None).await;
    let page = page_after(&ada, &Value::Null).await;
    let (content, state) = section(&page, "new_from_your_artists");
    assert_eq!(
        groups_of(&content["new_releases"]["releases"]).len(),
        8,
        "{page}"
    );
    assert_eq!(content["new_releases"]["updated_at"], read_at);
    assert_eq!(state, Value::Null);
    assert_eq!(
        script.fresh_reads.load(Ordering::SeqCst),
        1,
        "two people's lists and a build cost one read"
    );

    // The pool deals the list's albums and EPs, but never a live set or a
    // compilation, as the album gate offers on every surface.
    deps.pool
        .run_once("user-1", |_| {
            Ok::<(), droppedneedle_kernel::jobs::queue::JobError>(())
        })
        .await
        .expect("the pool was built");
    let dealt = |group: String| {
        db.one::<i64>(
            "SELECT COUNT(*) FROM catalog_discover_candidates \
             WHERE user_id = 'user-1' AND kind = 'fresh' AND release_group_mbid_lower = ?1",
            [group],
        )
    };
    assert_eq!(dealt(group(1)), 1, "a new album by their artist is dealt");
    assert_eq!(dealt(group(11)), 0, "a live EP is not");
    assert_eq!(dealt(group(12)), 0, "a compilation is not");

    // The next day ListenBrainz answers nothing: the kept list stays, and
    // the try counts as failed, so the hourly retry still runs.
    rig.clock().advance(25 * HOUR_MS);
    script.fresh_empty.store(true, Ordering::SeqCst);
    assert!(
        !deps.fresh.refresh().await,
        "an empty answer replaces nothing"
    );
    assert_eq!(script.fresh_reads.load(Ordering::SeqCst), 2);
    let (_, kept) = call(&ada, "GET", "/api/v3/discover/new-releases", None).await;
    assert_eq!(
        groups_of(&kept["items"]),
        groups_of(items),
        "yesterday's list stays"
    );
    assert!(!deps.fresh.refresh().await, "no second try within the hour");
    assert_eq!(script.fresh_reads.load(Ordering::SeqCst), 2);

    rig.clock().advance(HOUR_MS + 60_000);
    script.fresh_empty.store(false, Ordering::SeqCst);
    script.fresh_down.store(true, Ordering::SeqCst);
    assert!(
        !deps.fresh.refresh().await,
        "the retry an hour later fails too"
    );
    assert_eq!(
        script.fresh_reads.load(Ordering::SeqCst),
        3,
        "a second try was made"
    );
    assert!(!deps.fresh.refresh().await, "two tries a day at most");
    assert_eq!(script.fresh_reads.load(Ordering::SeqCst), 3);
    let (_, kept) = call(&ada, "GET", "/api/v3/discover/new-releases", None).await;
    assert_eq!(groups_of(&kept["items"]), groups_of(items));

    rig.clock().advance(11 * HOUR_MS);
    let page = page_after(&ada, &page).await;
    let (content, state) = section(&page, "new_from_your_artists");
    assert_eq!(groups_of(&content["new_releases"]["releases"]).len(), 8);
    assert_eq!(
        state,
        json!({
            "key": "new_from_your_artists",
            "state": "kept",
            "source": "listenbrainz",
            "since": read_at,
        }),
        "the page says when the list is from"
    );
}
