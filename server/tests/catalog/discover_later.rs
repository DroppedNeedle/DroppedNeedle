//! The Later pile over a real database and the production builder: albums
//! go on and come off a person's own pile, an album the library comes to own
//! leaves it, an album on it is not dealt from the pool, and its artist
//! counts toward the person's seeds. Another person's pile is never touched.

use std::sync::Arc;

use droppedneedle_catalog::discover::engine::pool_store;
use droppedneedle_catalog::discover::facts::KeptFacts;
use droppedneedle_catalog::discover::later::LaterKey;
use droppedneedle_catalog::discover::listening::ListeningData;
use droppedneedle_catalog::discover::models::{LaterAlbum, LaterRequest};
use droppedneedle_catalog::discover::services::DiscoverDeps;
use droppedneedle_kernel::clock::Clock as _;
use droppedneedle_kernel::db::Db;
use droppedneedle_kernel::page::{Page, PageRequest};
use droppedneedle_providers::Providers;

use crate::common::Probe;
use crate::common::auth::AuthRig;
use crate::discover_pool::id;
use crate::discover_queue::{Script, discover_deps, insert, own};

const USERS: &str = "INSERT INTO auth_users (id, display_name, role, created_at) \
     VALUES ('user-1', 'Ada', 'user', 0), ('user-2', 'Bob', 'user', 0);";

fn request(title: &str, artist: &str) -> LaterRequest {
    LaterRequest {
        title: title.to_owned(),
        artist_name: format!("Artist of {title}"),
        artist_mbid: Some(artist.to_owned()),
    }
}

/// The first page of `user`'s pile, only album `only` when it names one.
async fn pile_of(deps: &DiscoverDeps, user: &str, only: Option<&str>) -> Page<LaterAlbum> {
    let first: PageRequest<LaterKey> = PageRequest {
        limit: 50,
        after: None,
    };
    deps.later
        .page(user, first, only)
        .await
        .expect("the pile reads")
}

async fn pile(deps: &DiscoverDeps, user: &str) -> Page<LaterAlbum> {
    pile_of(deps, user, None).await
}

fn groups(page: &Page<LaterAlbum>) -> Vec<String> {
    page.items
        .iter()
        .map(|row| row.album.musicbrainz_id.clone().unwrap_or_default())
        .collect()
}

fn rows(db: &Db, user: &str) -> Vec<String> {
    db.column(
        "SELECT release_group_mbid_lower FROM catalog_later_albums WHERE user_id = ?1 \
         ORDER BY release_group_mbid_lower",
        [user],
    )
}

/// Ada puts three albums on her pile, one of them twice: it holds one row
/// each, newest first, the first marking's time kept, an open request
/// marked. Bob's pile of the same album is his. When the library comes to
/// own one, it leaves Ada's list at once and its row goes at her next
/// change; removing one takes only hers.
#[tokio::test]
async fn discover_later_marks_lists_and_removes_one_persons_albums() {
    let dir = crate::common::ScratchDir::new("discover-later");
    let rig = AuthRig::new("discover-later").await;
    let db = rig.platform.db.clone();
    insert(&db, USERS.to_owned()).await;
    let deps = discover_deps(&rig, &dir, Providers::closed(), |_| {});
    let (first, second, third) = (id(5, 1), id(5, 2), id(5, 3));
    let artist = id(6, 1);

    for group in [&first, &second, &third] {
        deps.later
            .mark("user-1", group, &request(group, &artist))
            .await
            .expect("marked");
        rig.clock().advance(1_000);
    }
    let first_at = pile(&deps, "user-1").await.items[2].added_at;
    deps.later
        .mark("user-1", &first, &request("Again", &artist))
        .await
        .expect("marked again");
    deps.later
        .mark("user-2", &first, &request("Bob's", &artist))
        .await
        .expect("Bob marks");
    insert(
        &db,
        format!(
            "INSERT INTO request_history (musicbrainz_id_lower, musicbrainz_id, artist_name, \
             album_title, requested_at, status) VALUES ('{second}', '{second}', 'Someone', \
             'Wanted', 1, 'pending');"
        ),
    )
    .await;

    let page = pile(&deps, "user-1").await;
    assert_eq!(
        groups(&page),
        [third.clone(), second.clone(), first.clone()]
    );
    assert_eq!(page.total, Some(3));
    assert_eq!(page.next_cursor, None);
    assert_eq!(
        page.items[2].added_at, first_at,
        "a second mark keeps the first"
    );
    assert_eq!(page.items[2].album.title, first, "and its name");
    let requested: Vec<bool> = page.items.iter().map(|row| row.album.requested).collect();
    assert_eq!(requested, [false, true, false], "an open request is marked");
    assert!(page.items.iter().all(|row| !row.album.in_library));
    assert_eq!(
        page.items[0].album.artist_id.as_deref(),
        Some(artist.as_str())
    );

    // A page of one row at a time walks the same list.
    let mut walked = Vec::new();
    let mut after = None;
    loop {
        let page = deps
            .later
            .page("user-1", PageRequest { limit: 1, after }, None)
            .await
            .expect("a page");
        walked.extend(groups(&page));
        let Some(cursor) = page.next_cursor else {
            break;
        };
        after = Some(
            PageRequest::<LaterKey>::read(Some(1), Some(&cursor), 1, 1)
                .expect("the cursor reads")
                .after
                .expect("a key"),
        );
    }
    assert_eq!(walked, groups(&pile(&deps, "user-1").await));

    // Asked for one album, the page holds it or nothing.
    let one = pile_of(&deps, "user-1", Some(&second)).await;
    assert_eq!(groups(&one), std::slice::from_ref(&second));
    assert_eq!(one.total, Some(1));
    let none = pile_of(&deps, "user-2", Some(&second)).await;
    assert!(none.items.is_empty(), "not on Bob's pile");

    // The library comes to own the second: it leaves the list at once, and
    // its row goes with Ada's next change.
    own(&db, std::slice::from_ref(&second)).await;
    let page = pile(&deps, "user-1").await;
    assert_eq!(groups(&page), [third.clone(), first.clone()]);
    assert_eq!(page.total, Some(2));
    assert_eq!(rows(&db, "user-1").len(), 3, "the row waits for a change");
    deps.later.remove("user-1", &third).await.expect("removed");
    assert_eq!(
        rows(&db, "user-1"),
        std::slice::from_ref(&first),
        "the owned row went too"
    );
    assert_eq!(rows(&db, "user-2"), [first], "Bob's pile is his");

    // A release group that is not an id is refused.
    assert!(deps.later.remove("user-1", "not-an-id").await.is_err());
}

/// An album on the pile is not dealt from the pool while it is there, and
/// comes back once taken off; Bob's pile changes nothing of Ada's.
#[tokio::test]
async fn discover_later_album_is_not_dealt_from_the_pool() {
    let dir = crate::common::ScratchDir::new("discover-later-pool");
    let rig = AuthRig::new("discover-later-pool").await;
    let db = rig.platform.db.clone();
    let (kept, later) = (id(5, 1), id(5, 2));
    let mut seed = String::from(USERS);
    for (rank, group) in [&kept, &later].into_iter().enumerate() {
        seed += &format!(
            "INSERT INTO catalog_discover_candidates (user_id, release_group_mbid_lower, rank, \
             kind, title, artist_mbid, artist_name, credited_artist_mbids, support, \
             group_support) VALUES ('user-1', '{group}', {rank}, 'source_pick', 'Album', \
             '{artist}', 'Artist', '[]', 1.0, 1.0); \
             INSERT INTO catalog_discover_candidate_paths (user_id, release_group_mbid_lower, \
             path_no, kind, source, named_by_labs, named_by_lastfm) \
             VALUES ('user-1', '{group}', 0, 'source', 'listenbrainz', 0, 0);",
            artist = id(6, rank),
        );
    }
    insert(&db, seed).await;
    let deps = discover_deps(&rig, &dir, Providers::closed(), |_| {});
    let dealt = || async {
        pool_store::ranked(&db, "user-1", rig.clock().now_ms())
            .await
            .expect("the pool reads")
            .into_iter()
            .map(|candidate| candidate.release_group_mbid)
            .collect::<Vec<_>>()
    };
    assert_eq!(dealt().await, [kept.clone(), later.clone()]);

    deps.later
        .mark("user-2", &kept, &request("Bob's", &id(6, 0)))
        .await
        .expect("Bob marks");
    deps.later
        .mark("user-1", &later, &request("Later", &id(6, 1)))
        .await
        .expect("marked");
    assert_eq!(
        dealt().await,
        std::slice::from_ref(&kept),
        "on the pile, not dealt"
    );

    deps.later.remove("user-1", &later).await.expect("removed");
    assert_eq!(dealt().await, [kept, later], "off the pile, dealt again");
}

/// A person with nothing else has no seeds; an album on their Later pile
/// makes its artist one, as a follow would, and an album the library owns
/// does not.
#[tokio::test]
async fn discover_later_album_gives_its_artist_weight() {
    let dir = crate::common::ScratchDir::new("discover-later-seeds");
    let rig = AuthRig::new("discover-later-seeds").await;
    let db = rig.platform.db.clone();
    insert(&db, USERS.to_owned()).await;
    let deps = discover_deps(&rig, &dir, Providers::closed(), |_| {});
    let seeds = || async {
        let listening = ListeningData::new(
            Arc::new(Script::default()),
            Providers::closed().cache,
            KeptFacts::new(db.clone(), rig.platform.clock.clone()),
        );
        droppedneedle_catalog::test_support::seed_artists(
            &listening,
            &db,
            "user-1",
            rig.clock().now_ms(),
        )
        .await
    };
    let (wanted, owned) = (id(6, 1), id(6, 2));
    assert!(seeds().await.is_empty(), "no seeds yet");

    deps.later
        .mark("user-1", &id(5, 1), &request("Wanted", &wanted))
        .await
        .expect("marked");
    deps.later
        .mark("user-2", &id(5, 3), &request("Bob's", &id(6, 3)))
        .await
        .expect("Bob marks");
    own(&db, &[id(5, 2)]).await;
    insert(
        &db,
        format!(
            "INSERT INTO catalog_later_albums (user_id, release_group_mbid_lower, \
             release_group_mbid, title, artist_mbid, artist_name, added_at) \
             VALUES ('user-1', '{group}', '{group}', 'Owned', '{owned}', 'Owned', 1);",
            group = id(5, 2)
        ),
    )
    .await;
    assert_eq!(seeds().await, [wanted], "the pile's artist is a seed");
}
