//! The candidate pool over a real database and the production builder:
//! from a person's taste rows and the kept facts the pool job stores
//! candidates with their paths and the build's counts, asks no provider,
//! holds to every gate of what a pick may be, keeps a co-credited album,
//! keeps the rows it had while lists are missing, and walks a large ranking
//! only as far as the album lists it waits for. Another person's pool is
//! never touched.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use droppedneedle_catalog::discover::due::Due;
use droppedneedle_catalog::discover::engine::candidate::PathKind;
use droppedneedle_catalog::discover::engine::pool_store;
use droppedneedle_catalog::discover::engine::select::{self, Exposure, Floors, Share};
use droppedneedle_catalog::discover::facts::KeptFacts;
use droppedneedle_catalog::discover::facts::job::FactJobs;
use droppedneedle_catalog::discover::services::DiscoverDeps;
use droppedneedle_kernel::clock::Clock as _;
use droppedneedle_kernel::db::Db;
use droppedneedle_kernel::jobs::queue::JobQueue;
use droppedneedle_providers::Providers;

use crate::common::Probe;
use crate::common::auth::AuthRig;
use crate::discover_queue::{Script, discover_deps, insert, own};

const DAY: i64 = 86_400_000;
const MINUTE: i64 = 60_000;

/// A MusicBrainz-shaped id: `n` under `tag`.
fn id(tag: u16, n: usize) -> String {
    format!("{n:08x}-{tag:04x}-4000-8000-000000000000")
}

/// One fetch row: the fact answered `outcome` at `at`.
fn fetched(artist: &str, kind: &str, source: &str, outcome: &str, at: i64) -> String {
    format!(
        "INSERT INTO catalog_fact_fetches (artist_mbid, kind, source, outcome, fetched_at, \
         failures, due_at, read_at) VALUES ('{artist}', '{kind}', '{source}', '{outcome}', {at}, \
         0, {due}, {at}) ON CONFLICT (artist_mbid, kind, source) DO UPDATE SET \
         outcome = excluded.outcome, fetched_at = excluded.fetched_at, wanted_at = NULL, \
         want_order = NULL;",
        due = at + 30 * DAY
    )
}

/// `seed`'s kept labs list, `neighbours` best first by their raw score.
fn list(seed: &str, neighbours: &[(String, f64)], at: i64) -> String {
    let mut rows: String = neighbours
        .iter()
        .enumerate()
        .map(|(rank, (neighbour, score))| {
            format!(
                "INSERT INTO catalog_artist_neighbours (artist_mbid, source, rank, \
                 neighbour_mbid, neighbour_name, score) VALUES ('{seed}', 'listenbrainz_labs', \
                 {rank}, '{neighbour}', 'Artist {}', {score});",
                &neighbour[..8]
            )
        })
        .collect();
    rows += &fetched(seed, "neighbours", "listenbrainz_labs", "found", at);
    rows
}

/// `artist`'s kept listener count.
fn counted(artist: &str, listeners: i64, at: i64) -> String {
    format!(
        "INSERT INTO catalog_artist_listeners (artist_mbid, listeners, listens) \
         VALUES ('{artist}', {listeners}, {listeners});"
    ) + &fetched(artist, "listeners", "listenbrainz", "found", at)
}

/// One kept album row: its release group, primary type, secondary types
/// and credited artists (both JSON) and listeners.
struct Album<'a> {
    group: &'a str,
    primary: &'a str,
    secondary: &'a str,
    credited: String,
    listeners: i64,
}

/// `artist`'s kept album list, counted; "nothing there" when `albums` is
/// empty.
fn albums(artist: &str, albums: &[Album<'_>], at: i64) -> String {
    let mut rows: String = albums
        .iter()
        .enumerate()
        .map(|(position, album)| {
            format!(
                "INSERT INTO catalog_artist_albums (artist_mbid, release_group_mbid, position, \
                 title, artist_name, primary_type, secondary_types, first_release_date, \
                 credited_artist_mbids, listeners, listens) VALUES ('{artist}', '{group}', \
                 {position}, 'Title of {group}', 'Credit of {group}', '{primary}', \
                 '{secondary}', '2016', '{credited}', {listeners}, {listeners});",
                group = album.group,
                primary = album.primary,
                secondary = album.secondary,
                credited = album.credited,
                listeners = album.listeners,
            )
        })
        .collect();
    let outcome = if albums.is_empty() { "none" } else { "found" };
    rows += &fetched(artist, "albums", "musicbrainz", outcome, at);
    rows += &fetched(artist, "album_counts", "listenbrainz", "found", at);
    rows
}

fn album<'a>(group: &'a str, primary: &'a str, secondary: &'a str, credited: &[&str]) -> Album<'a> {
    Album {
        group,
        primary,
        secondary,
        credited: serde_json::to_string(credited).expect("ids"),
        listeners: 500,
    }
}

/// user-1's taste build: `seeds` (id, weight, group), `groups` (number,
/// share) and the other artists they know.
fn taste(seeds: &[(&str, f64, i64)], groups: &[(i64, f64)], known: &[&str], at: i64) -> String {
    let mut rows = format!(
        "INSERT INTO catalog_taste_builds (user_id, built_at, plays_counted, \
         imported_listens_counted, artists_played, seeds, incomplete) \
         VALUES ('user-1', {at}, 10, 0, 1, {}, 0);",
        seeds.len()
    );
    for (no, share) in groups {
        rows += &format!(
            "INSERT INTO catalog_taste_groups (user_id, group_no, anchor_artist_mbid, \
             anchor_name, share, recent_share, trend) VALUES ('user-1', {no}, 'anchor-{no}', \
             'Anchor', {share}, {share}, 'steady');"
        );
    }
    for (seed, weight, group) in seeds {
        rows += &format!(
            "INSERT INTO catalog_taste_artists (user_id, artist_mbid_lower, artist_mbid, name, \
             plays, imported_listens, months_active, weight, is_seed, group_no) \
             VALUES ('user-1', '{seed}', '{seed}', 'Seed {}', 5, 0, 1, {weight}, 1, {group});",
            &seed[..8]
        );
    }
    for artist in known {
        rows += &format!(
            "INSERT INTO catalog_taste_artists (user_id, artist_mbid_lower, artist_mbid, name, \
             plays, imported_listens, months_active, weight, is_seed) \
             VALUES ('user-1', '{artist}', '{artist}', 'Known', 1, 0, 1, 0.1, 0);"
        );
    }
    rows
}

const USERS: &str = "INSERT INTO auth_users (id, display_name, role, created_at) \
     VALUES ('user-1', 'Ada', 'user', 0), ('user-2', 'Bob', 'user', 0); \
     INSERT INTO catalog_discover_pool_builds (user_id, built_at, incomplete, waiting_for, \
     seeds_used, neighbours_found, known_left_out, hubs_left_out, artists_ranked, \
     artists_without_album, new_artist_candidates, source_pick_candidates, gap_candidates, \
     shelf_candidates, fresh_candidates, thread_candidates) \
     VALUES ('user-2', 7, 0, '[]', 1, 1, 0, NULL, 1, 0, 1, 0, 0, 0, 0, 0); \
     INSERT INTO catalog_discover_candidates (user_id, release_group_mbid_lower, \
     rank, kind, title, artist_mbid, artist_name, credited_artist_mbids, \
     support, group_no, group_support) VALUES ('user-2', 'bobs-album', 0, \
     'new_artist', 'Bob''s', 'bobs-artist', 'Bob''s artist', '[]', 1.0, 0, 1.0);";

fn bobs(db: &Db) -> Vec<String> {
    db.column(
        "SELECT user_id || built_at FROM catalog_discover_pool_builds WHERE user_id = 'user-2' \
         UNION ALL SELECT release_group_mbid_lower FROM catalog_discover_candidates \
         WHERE user_id = 'user-2'",
        [],
    )
}

fn deps(rig: &AuthRig, dir: &std::path::Path, script: Arc<Script>) -> DiscoverDeps {
    discover_deps(rig, dir, Providers::closed(), move |slot| *slot = script)
}

async fn build(deps: &DiscoverDeps) {
    deps.pool
        .run_once("user-1", |_| Ok(()))
        .await
        .expect("the pool job runs");
}

fn stored(db: &Db) -> Vec<String> {
    db.column(
        "SELECT release_group_mbid_lower FROM catalog_discover_candidates \
         WHERE user_id = 'user-1' ORDER BY rank",
        [],
    )
}

fn calls(script: &Script) -> usize {
    script.similar_reads.load(Ordering::SeqCst)
        + script.listener_reads.load(Ordering::SeqCst)
        + script.discography_reads.lock().expect("reads").len()
        + script.count_reads.lock().expect("reads").len()
        + script.user_artist_reads.load(Ordering::SeqCst)
        + script.all_time_reads.load(Ordering::SeqCst)
        + script.user_album_reads.load(Ordering::SeqCst)
}

type BuildRow = (bool, i64, i64, i64, Option<i64>, i64, i64, i64);

fn build_row(db: &Db) -> BuildRow {
    db.row(
        "SELECT incomplete, seeds_used, neighbours_found, known_left_out, hubs_left_out, \
         artists_ranked, artists_without_album, new_artist_candidates \
         FROM catalog_discover_pool_builds WHERE user_id = 'user-1'",
        [],
    )
}

fn wanted_lists(db: &Db) -> i64 {
    db.one(
        "SELECT COUNT(*) FROM catalog_fact_fetches WHERE kind = 'albums' \
         AND source = 'musicbrainz' AND wanted_at IS NOT NULL",
        [],
    )
}

/// Two taste groups. Every artist the seeds point at has one album, and
/// each but two is kept out by one gate of what a pick may be: owned,
/// being fetched, hidden, the artist known, the artist turned down, a
/// single, a live set, a demo, and an artist with nothing left. Two kept
/// in: an album two seeds agree on, and a Cult of Luna album with Julie
/// Christmas on it, found through Cult of Luna, whom the person does not
/// know; the same co-credit owned stays out. The job asks no provider. With
/// one seed's list gone the rows stay as they were, the list is wanted and
/// the build says it is incomplete. Gates that move apply when the pool is
/// read. Bob's pool is never touched.
#[tokio::test]
async fn discover_pool_keeps_what_a_pick_may_be_from_kept_facts_alone() {
    let dir = crate::common::ScratchDir::new("discover-pool");
    let rig = AuthRig::new("discover-pool").await;
    let db = rig.platform.db.clone();
    let at = rig.clock().now_ms() - DAY;
    let (s1, s2, s3, julie) = (id(1, 1), id(1, 2), id(1, 3), id(1, 4));
    let names = [
        "agreed",
        "owned",
        "fetching",
        "hidden",
        "known",
        "turned-down",
        "single",
        "live",
        "demo",
        "nothing",
        "cult",
    ];
    let artist = |name: &str| id(2, names.iter().position(|n| *n == name).expect("named"));
    let group = |name: &str| id(3, names.iter().position(|n| *n == name).expect("named"));
    let collab = id(3, 99);

    let mut rows = String::from(USERS);
    rows += &taste(
        &[
            (&s1, 2.0, 0),
            (&s2, 1.0, 0),
            (&s3, 1.5, 1),
            (&julie, 1.0, 1),
        ],
        &[(0, 0.6), (1, 0.4)],
        &[&artist("known")],
        at,
    );
    let pointed = |names: &[&str]| -> Vec<(String, f64)> {
        names
            .iter()
            .enumerate()
            .map(|(n, name)| (artist(name), 100.0 - n as f64))
            .collect()
    };
    rows += &list(
        &s1,
        &pointed(&["agreed", "owned", "fetching", "hidden", "known"]),
        at,
    );
    rows += &list(&s2, &pointed(&["agreed", "turned-down", "single"]), at);
    rows += &list(&s3, &pointed(&["live", "demo", "nothing"]), at);
    rows += &list(&julie, &pointed(&["cult"]), at);
    for name in names {
        rows += &counted(&artist(name), 10_000, at);
        let (primary, secondary) = match name {
            "single" => ("Single", "[]"),
            "live" => ("Album", "[\"Live\"]"),
            "demo" => ("EP", "[\"Demo\"]"),
            _ => ("Album", "[]"),
        };
        let own_group = group(name);
        let kept = match name {
            "nothing" => Vec::new(),
            "cult" => vec![
                album(&own_group, "Album", "[]", &[&artist("cult"), &julie]),
                album(&collab, "Album", "[]", &[&artist("cult"), &julie]),
            ],
            _ => vec![album(&own_group, primary, secondary, &[&artist(name)])],
        };
        rows += &albums(&artist(name), &kept, at);
    }
    rows += &format!(
        "INSERT INTO request_history (musicbrainz_id_lower, musicbrainz_id, artist_name, \
         album_title, requested_at, status) VALUES ('{fetching}', '{fetching}', 'Someone', \
         'Wanted', 1, 'pending'); \
         INSERT INTO catalog_discover_feedback (user_id, target, target_mbid_lower, kind, \
         created_at) VALUES ('user-1', 'album', '{hidden}', 'no', 1), \
         ('user-1', 'artist', '{turned_down}', 'not_artist', 1);",
        fetching = group("fetching"),
        hidden = group("hidden"),
        turned_down = artist("turned-down"),
    );
    insert(&db, rows).await;
    own(&db, &[group("owned"), collab.clone()]).await;
    let bob = bobs(&db);
    let script = Arc::new(Script::default());
    let deps = deps(&rig, &dir, Arc::clone(&script));

    build(&deps).await;
    assert_eq!(calls(&script), 0, "the job asked no provider");
    let (agreed, mariner) = (group("agreed"), group("cult"));
    assert_eq!(stored(&db), [agreed.clone(), mariner.clone()]);
    assert_eq!(
        build_row(&db),
        (false, 4, 11, 1, Some(0), 9, 7, 2),
        "seeds, neighbours, known, hubs, ranked, without an album, candidates"
    );
    let paths: Vec<(String, String, String, Option<String>)> = db.rows(
        "SELECT release_group_mbid_lower, kind, artist_mbid, known_credit_mbid \
         FROM catalog_discover_candidate_paths WHERE user_id = 'user-1' \
         ORDER BY release_group_mbid_lower, path_no",
        [],
    );
    assert_eq!(
        paths,
        [
            (agreed.clone(), "seed".to_owned(), s1.clone(), None),
            (agreed.clone(), "seed".to_owned(), s2.clone(), None),
            (
                mariner.clone(),
                "seed".to_owned(),
                julie.clone(),
                Some(julie.clone())
            ),
        ],
        "one path a pointing seed, strongest first; the known credit noted"
    );
    let groups: Vec<(String, Option<i64>)> = db.rows(
        "SELECT release_group_mbid_lower, group_no FROM catalog_discover_candidates \
         WHERE user_id = 'user-1' ORDER BY rank",
        [],
    );
    assert_eq!(
        groups,
        [(agreed.clone(), Some(0)), (mariner.clone(), Some(1))]
    );

    // s1's list is gone: the rows stay as they were, and the list is wanted.
    insert(
        &db,
        format!(
            "DELETE FROM catalog_artist_neighbours WHERE artist_mbid = '{s1}'; \
             DELETE FROM catalog_fact_fetches WHERE artist_mbid = '{s1}' AND kind = 'neighbours';"
        ),
    )
    .await;
    build(&deps).await;
    assert_eq!(
        stored(&db),
        [agreed.clone(), mariner.clone()],
        "the rows stay"
    );
    assert!(build_row(&db).0, "the build says it is incomplete");
    let wanted: i64 = db.one(
        "SELECT COUNT(*) FROM catalog_fact_fetches WHERE artist_mbid = ?1 \
         AND kind = 'neighbours' AND wanted_at IS NOT NULL",
        [&s1],
    );
    assert_eq!(wanted, 1, "the missing list is wanted");
    assert_eq!(calls(&script), 0, "still nothing asked");

    // What moves is applied as the pool is read.
    let now = rig.clock().now_ms();
    let read = pool_store::ranked(&db, "user-1", now).await.expect("pool");
    assert_eq!(read.len(), 2);
    assert!(read[0].paths.iter().all(|path| path.kind == PathKind::Seed));
    insert(
        &db,
        format!(
            "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
             album_artist_id, grouping_source, created_at, updated_at) \
             VALUES ('al-late', 'r1', 'g-late', 'Owned', 'owned', 'a1', 'automatic', 1, 1); \
             INSERT INTO local_album_external_identities (local_album_id, provider, \
             release_group_mbid, decision_source, selected_at) \
             VALUES ('al-late', 'musicbrainz', '{agreed}', 'manual', 1); \
             INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
             path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
             album_title, album_title_folded, file_format, availability, ingest_source, \
             imported_at, membership_source) VALUES ('t-late', 'al-late', 'r1', \
             '/music/t-late.flac', 't-late.flac', 'h-late', 1, 1, 's-late', 'Song', 'song', \
             'Owned', 'owned', 'flac', 'indexed', 'scan', 1, 'automatic'); \
             INSERT INTO catalog_discover_feedback (user_id, target, target_mbid_lower, kind, \
             created_at) VALUES ('user-1', 'artist', '{cult}', 'not_artist', 2);",
            cult = artist("cult")
        ),
    )
    .await;
    let read = pool_store::ranked(&db, "user-1", now).await.expect("pool");
    assert!(read.is_empty(), "owned and turned down since: {read:?}");
    assert_eq!(stored(&db).len(), 2, "the rows wait for the next build");

    assert_eq!(bobs(&db), bob, "Bob's pool is untouched");
}

/// A cold build over a ranking of 1,200 artists, none of whose album lists
/// is kept, wants exactly 80 lists and keeps nothing yet. Once they arrive
/// (five artists with nothing to offer) a visit queues the pool again, and
/// that build wants one more list for each of the five.
#[tokio::test]
async fn discover_pool_cold_build_wants_80_lists_then_one_more_for_each_empty() {
    let dir = crate::common::ScratchDir::new("discover-pool-cold");
    let rig = AuthRig::new("discover-pool-cold").await;
    let db = rig.platform.db.clone();
    let at = rig.clock().now_ms() - DAY;
    let seed = id(1, 1);
    let ranked: Vec<(String, f64)> = (0..1_200)
        .map(|n| (id(2, n), 10_000.0 - n as f64))
        .collect();
    let mut rows = String::from(USERS);
    rows += &taste(&[(&seed, 1.0, 0)], &[(0, 1.0)], &[], at);
    rows += &list(&seed, &ranked, at);
    for (artist, _) in &ranked {
        rows += &counted(artist, 1_000, at);
    }
    insert(&db, rows).await;
    let bob = bobs(&db);
    let script = Arc::new(Script::default());
    let deps = deps(&rig, &dir, Arc::clone(&script));

    build(&deps).await;
    assert_eq!(
        wanted_lists(&db),
        80,
        "a cold build wants 80 lists and no more"
    );
    assert!(stored(&db).is_empty());
    let (incomplete, waiting): (bool, i64) = db.row(
        "SELECT incomplete, json_array_length(waiting_for) FROM catalog_discover_pool_builds \
         WHERE user_id = 'user-1'",
        [],
    );
    assert_eq!((incomplete, waiting), (true, 80));
    assert!(!pool_due(&rig).await, "not due while the lists are missing");

    // The 80 lists arrive: the first five have nothing, the rest one album.
    rig.clock().advance(MINUTE);
    let now = rig.clock().now_ms();
    let mut rows = String::new();
    for (n, (artist, _)) in ranked.iter().take(80).enumerate() {
        let groups = id(3, n);
        let kept = if n < 5 {
            Vec::new()
        } else {
            vec![album(&groups, "Album", "[]", &[artist])]
        };
        rows += &albums(artist, &kept, now);
    }
    insert(&db, rows).await;
    assert!(pool_due(&rig).await, "due once they arrive");
    build(&deps).await;
    assert_eq!(wanted_lists(&db), 5, "one more list for each with nothing");
    assert_eq!(stored(&db).len(), 75);
    let wanted: Vec<String> = db.column(
        "SELECT artist_mbid FROM catalog_fact_fetches WHERE kind = 'albums' \
         AND wanted_at IS NOT NULL ORDER BY artist_mbid",
        [],
    );
    let next: Vec<String> = ranked[80..85]
        .iter()
        .map(|(artist, _)| artist.clone())
        .collect();
    assert_eq!(wanted, next, "the next five in the ranking");
    assert_eq!(calls(&script), 0, "the job asked no provider");
    assert_eq!(bobs(&db), bob, "Bob's pool is untouched");
}

/// Whether a visit by Ada now queues her pool build.
async fn pool_due(rig: &AuthRig) -> bool {
    let db = rig.platform.db.clone();
    let clear = "DELETE FROM durable_jobs WHERE kind = 'discover.pool'";
    insert(&db, clear.to_owned()).await;
    let due = Due::new(
        db.clone(),
        JobQueue::new(db.clone(), rig.platform.clock.clone()),
        rig.platform.clock.clone(),
        false,
        (
            FactJobs::new(
                KeptFacts::new(db.clone(), rig.platform.clock.clone()),
                Arc::new(Script::default()),
            )
            .due(),
            crate::common::history_store(&rig.platform),
        ),
    );
    due.visit("user-1").await;
    let queued: i64 = db.one(
        "SELECT COUNT(*) FROM durable_jobs WHERE kind = 'discover.pool' \
         AND idempotency_key = 'user-1' AND state = 'queued'",
        [],
    );
    insert(&db, clear.to_owned()).await;
    queued > 0
}

/// An artist ListenBrainz counted and found no listeners for has none, and
/// stays under the floor; one not counted yet is not known to be under it,
/// and is selected.
#[tokio::test]
async fn discover_pool_an_artist_counted_with_no_listeners_is_under_the_floor() {
    let dir = crate::common::ScratchDir::new("discover-pool-floor");
    let rig = AuthRig::new("discover-pool-floor").await;
    let db = rig.platform.db.clone();
    let at = rig.clock().now_ms() - DAY;
    let seed = id(1, 1);
    let (none, unread) = (id(2, 1), id(2, 2));
    let (none_album, unread_album) = (id(3, 1), id(3, 2));
    let mut rows = String::from(USERS);
    rows += &taste(&[(&seed, 1.0, 0)], &[(0, 1.0)], &[], at);
    rows += &list(&seed, &[(none.clone(), 100.0), (unread.clone(), 90.0)], at);
    rows += &fetched(&none, "listeners", "listenbrainz", "none", at);
    rows += &albums(&none, &[album(&none_album, "Album", "[]", &[&none])], at);
    rows += &albums(
        &unread,
        &[album(&unread_album, "Album", "[]", &[&unread])],
        at,
    );
    insert(&db, rows).await;
    let deps = deps(&rig, &dir, Arc::new(Script::default()));

    build(&deps).await;
    let listeners: Vec<(String, Option<i64>)> = db.rows(
        "SELECT artist_mbid, artist_listeners FROM catalog_discover_candidates \
         WHERE user_id = 'user-1' ORDER BY artist_mbid",
        [],
    );
    assert_eq!(listeners, [(none.clone(), Some(0)), (unread.clone(), None)]);
    let pool = pool_store::ranked(&db, "user-1", rig.clock().now_ms())
        .await
        .expect("pool");
    let shares = [Share {
        group: 0,
        share: 1.0,
    }];
    let picks = select::select(&pool, 8, &shares, &Exposure::default(), Floors::PROPOSED);
    let chosen: Vec<&str> = picks
        .picks
        .iter()
        .map(|pick| pick.release_group_mbid.as_str())
        .collect();
    assert_eq!(chosen, [unread_album.as_str()]);
}
