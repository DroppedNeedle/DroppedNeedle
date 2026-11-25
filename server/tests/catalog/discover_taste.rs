//! Taste groups over a real database and the production builder: a
//! person's plays in the app and imported listens go in, the taste job
//! groups their seeds through kept similar-artist answers alone and keeps
//! the build whole, keeps the groups before it while facts are missing,
//! and is due again once they arrive. While a person's imported history is
//! current nothing of their listening statistics is read from
//! ListenBrainz; while it is not, the job reads their top artists once.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use droppedneedle_catalog::discover::due::Due;
use droppedneedle_catalog::discover::engine::history::SAME_PLAY_MS;
use droppedneedle_catalog::discover::facts::KeptFacts;
use droppedneedle_catalog::discover::facts::job::FactJobs;
use droppedneedle_catalog::discover::listening::{ArtistRow, ScoredArtist, UserMusic};
use droppedneedle_catalog::discover::services::DiscoverDeps;
use droppedneedle_kernel::clock::Clock as _;
use droppedneedle_kernel::db::Db;
use droppedneedle_kernel::jobs::queue::JobQueue;
use droppedneedle_providers::Providers;
use droppedneedle_providers::listenbrainz::ArtistPopularity;
use tower::ServiceExt as _;

use crate::common::Probe;
use crate::common::auth::AuthRig;
use crate::discover_queue::{
    Script, after_commits, discover_deps, discover_routes, insert, keep_facts_of, library_artist,
    seen_now,
};

const DAY: i64 = 86_400_000;
const MINUTE: i64 = 60_000;

fn mbid(prefix: char, n: u8) -> String {
    let p = prefix.to_string().repeat(4);
    format!("{n:08x}-{p}-4{}-8{}-{}", &p[..3], &p[..3], p.repeat(3))
}

fn neighbour(mbid: &str, score: f64) -> ScoredArtist {
    ScoredArtist {
        artist: ArtistRow {
            name: format!("Neighbour of {}", &mbid[..8]),
            mbid: Some(mbid.to_owned()),
            listen_count: None,
        },
        score,
    }
}

/// Two circles: the Alphas share listeners, the Betas share listeners, and
/// nothing links the two. Every neighbour has a listener count.
fn two_circles(user_artists: Vec<ArtistRow>) -> Script {
    let (a1, a2, b1, b2, other) = (
        mbid('a', 1),
        mbid('a', 2),
        mbid('b', 1),
        mbid('b', 2),
        mbid('f', 1),
    );
    let counted = ArtistPopularity {
        listen_count: Some(1000),
        listeners: Some(100),
    };
    Script {
        user: UserMusic {
            listenbrainz: Some("ada".to_owned()),
            ..UserMusic::default()
        },
        similar: HashMap::from([
            (
                a1.clone(),
                vec![neighbour(&a2, 100.0), neighbour(&other, 50.0)],
            ),
            (a2.clone(), vec![neighbour(&a1, 100.0)]),
            (b1.clone(), vec![neighbour(&b2, 100.0)]),
            (b2.clone(), vec![neighbour(&b1, 100.0)]),
        ]),
        listeners: [a1, a2, b1, b2, other]
            .into_iter()
            .map(|id| (id, counted))
            .collect(),
        user_artists,
        ..Script::default()
    }
}

/// `count` plays by user-1 of library artist `artist` on release group
/// `group`, the latest at `at`, a minute apart.
fn plays(artist: &str, group: &str, count: i64, at: i64) -> String {
    (0..count)
        .map(|n| {
            format!(
                "INSERT INTO library_play_history (id, user_id, local_artist_id, track_name, \
                 artist_name, release_group_mbid, played_at) VALUES ('{artist}-{group}-{n}-{at}', \
                 'user-1', '{artist}', 'Song', 'Someone', '{group}', {});",
                at - MINUTE * n
            )
        })
        .collect()
}

/// `count` listens imported for user-1 of `name`, the latest at `at`.
fn listens(artist: &str, name: &str, count: i64, at: i64) -> String {
    (0..count)
        .map(|n| {
            format!(
                "INSERT INTO media_imported_listens (user_id, listened_at, artist_mbid, \
                 artist_name, track_name, imported_at) \
                 VALUES ('user-1', {}, '{artist}', '{name}', 'Song {n}', 1);",
                at - MINUTE * n
            )
        })
        .collect()
}

/// Ada and Bob; Ada played two Alphas in the app on three albums and has
/// two Betas in her imported history. Bob has a taste of his own.
async fn history(db: &Db, at: i64) {
    let (r1, r2, r3) = (mbid('e', 1), mbid('e', 2), mbid('e', 3));
    let mut rows = String::from(
        "INSERT INTO auth_users (id, display_name, role, created_at) \
         VALUES ('user-1', 'Ada', 'user', 0), ('user-2', 'Bob', 'user', 0); \
         INSERT INTO catalog_taste_builds (user_id, built_at, plays_counted, \
         imported_listens_counted, artists_played, seeds, incomplete) \
         VALUES ('user-2', 7, 1, 0, 1, 1, 0); \
         INSERT INTO catalog_taste_artists (user_id, artist_mbid_lower, artist_mbid, name, \
         plays, imported_listens, months_active, weight, is_seed) \
         VALUES ('user-2', 'bob-artist', 'bob-artist', 'Bob''s', 1, 0, 1, 1.0, 1);",
    );
    rows += &library_artist("art-a1", "Alpha One", &mbid('a', 1), "group");
    rows += &library_artist("art-a2", "Alpha Two", &mbid('a', 2), "group");
    rows += &plays("art-a1", &r1, 3, at);
    rows += &plays("art-a1", &r3, 2, at - DAY);
    rows += &plays("art-a2", &r2, 3, at - 2 * DAY);
    rows += &listens(&mbid('b', 1), "Beta One", 6, at);
    rows += &listens(&mbid('b', 2), "Beta Two", 4, at);
    insert(db, rows).await;
}

async fn import_current(db: &Db) {
    insert(
        db,
        "INSERT INTO media_history_imports (user_id, state, source, last_synced_at, \
         updated_at) VALUES ('user-1', 'current', 'upload', 0, 0) \
         ON CONFLICT (user_id) DO UPDATE SET state = 'current'"
            .to_owned(),
    )
    .await;
}

async fn build(deps: &DiscoverDeps) {
    deps.taste
        .run_once("user-1", |_| Ok(()))
        .await
        .expect("the taste job runs");
}

type BuildCounts = (i64, i64, i64, i64, bool);

fn build_row(db: &Db) -> BuildCounts {
    db.row(
        "SELECT plays_counted, imported_listens_counted, artists_played, seeds, incomplete \
         FROM catalog_taste_builds WHERE user_id = 'user-1'",
        [],
    )
}

fn groups(db: &Db) -> Vec<(i64, String, String)> {
    db.rows(
        "SELECT group_no, anchor_name, trend FROM catalog_taste_groups \
         WHERE user_id = 'user-1' ORDER BY group_no",
        [],
    )
}

fn artists(db: &Db) -> Vec<(String, i64, i64, bool, Option<i64>)> {
    db.rows(
        "SELECT name, plays, imported_listens, is_seed, group_no FROM catalog_taste_artists \
         WHERE user_id = 'user-1' ORDER BY name",
        [],
    )
}

fn bobs(db: &Db) -> Vec<String> {
    db.column(
        "SELECT user_id || built_at FROM catalog_taste_builds WHERE user_id = 'user-2' \
         UNION ALL SELECT name FROM catalog_taste_artists WHERE user_id = 'user-2'",
        [],
    )
}

fn reads(script: &Script) -> (usize, usize, usize, usize, usize) {
    (
        script.user_artist_reads.load(Ordering::SeqCst),
        script.all_time_reads.load(Ordering::SeqCst),
        script.user_album_reads.load(Ordering::SeqCst),
        script.similar_reads.load(Ordering::SeqCst),
        script.listener_reads.load(Ordering::SeqCst),
    )
}

async fn get(app: &axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let response = app
        .clone()
        .oneshot(Request::get(uri).body(Body::empty()).expect("request"))
        .await
        .expect("router answers");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 4 << 20)
        .await
        .expect("body");
    (status, serde_json::from_slice(&bytes).unwrap_or_default())
}

/// Whether a visit by Ada now queues her taste build.
async fn due_on_visit(rig: &AuthRig) -> bool {
    let db = rig.platform.db.clone();
    let clear = "DELETE FROM durable_jobs WHERE kind = 'discover.taste'";
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
            crate::discover_queue::fresh_feed(&rig.platform),
        ),
    );
    due.visit("user-1").await;
    let queued: i64 = db.one(
        "SELECT COUNT(*) FROM durable_jobs WHERE kind = 'discover.taste' \
         AND idempotency_key = 'user-1' AND state = 'queued'",
        [],
    );
    insert(&db, clear.to_owned()).await;
    queued > 0
}

/// Ada's plays and imported listens make two groups, the heavier first,
/// each named after its heaviest artist; every artist she played is known
/// and the albums she played here are known. Her import is current, so the
/// job, her page and her top albums read nothing from ListenBrainz's
/// statistics, and the job reads no public fact at all. A play removed is
/// gone from the next build. With two seeds' lists missing the groups stay
/// as they were while her plays are counted again, and once the lists
/// arrive a visit queues a build that groups them again. Bob's rows are
/// never touched.
#[tokio::test]
async fn discover_taste_groups_history_and_keeps_the_groups_through_missing_facts() {
    let dir = crate::common::ScratchDir::new("discover-taste");
    let rig = AuthRig::new("discover-taste").await;
    let db = rig.platform.db.clone();
    let at = rig.clock().now_ms() - 10 * DAY;
    history(&db, at).await;
    import_current(&db).await;
    keep_facts_of(&rig, Arc::new(two_circles(Vec::new()))).await;
    let bob = bobs(&db);
    let script = Arc::new(two_circles(Vec::new()));
    let network = Arc::clone(&script);
    let deps = discover_deps(&rig, &dir, Providers::closed(), move |slot| {
        *slot = network;
    });

    build(&deps).await;
    assert_eq!(build_row(&db), (8, 10, 4, 4, false));
    assert_eq!(
        groups(&db),
        [
            (0, "Beta One".to_owned(), "steady".to_owned()),
            (1, "Alpha One".to_owned(), "steady".to_owned()),
        ]
    );
    assert_eq!(
        artists(&db),
        [
            ("Alpha One".to_owned(), 5, 0, true, Some(1)),
            ("Alpha Two".to_owned(), 3, 0, true, Some(1)),
            ("Beta One".to_owned(), 0, 6, true, Some(0)),
            ("Beta Two".to_owned(), 0, 4, true, Some(0)),
        ]
    );
    let known: Vec<(String, i64)> = db.rows(
        "SELECT release_group_mbid_lower, plays FROM catalog_known_release_groups \
         WHERE user_id = 'user-1' ORDER BY release_group_mbid_lower",
        [],
    );
    assert_eq!(
        known,
        [(mbid('e', 1), 3), (mbid('e', 2), 3), (mbid('e', 3), 2)]
    );
    assert_eq!(reads(&script), (0, 0, 0, 0, 0), "the job read nothing");

    // Her page seeds from the build and her top albums come from her plays.
    let app = discover_routes(deps);
    assert_eq!(get(&app, "/api/v3/discover").await.0, StatusCode::OK);
    after_commits(&db, "the page was saved", || async {
        (db.one::<i64>(
            "SELECT COUNT(*) FROM catalog_cached_pages WHERE user_id = 'user-1' \
             AND page = 'discover'",
            [],
        ) == 1)
            .then_some(())
    })
    .await;
    let (status, top) = get(&app, "/api/v3/home/your-top/albums?range=all_time&limit=10").await;
    assert_eq!(status, StatusCode::OK);
    let counts: Vec<i64> = top["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|item| item["listen_count"].as_i64().unwrap_or(0))
        .collect();
    assert_eq!(counts, [3, 3, 2], "her three albums, most played first");
    let (user_artists, all_time, user_albums, _, _) = reads(&script);
    assert_eq!(
        (user_artists, all_time, user_albums),
        (0, 0, 0),
        "no listening statistics read from ListenBrainz"
    );

    // Alpha Two's plays go: the next build has no row of it.
    let deps = discover_deps(&rig, &dir, Providers::closed(), {
        let network = Arc::clone(&script);
        move |slot| *slot = network
    });
    insert(
        &db,
        "DELETE FROM library_play_history WHERE local_artist_id = 'art-a2'".to_owned(),
    )
    .await;
    build(&deps).await;
    assert_eq!(build_row(&db), (5, 10, 3, 3, false));
    assert_eq!(
        artists(&db),
        [
            ("Alpha One".to_owned(), 5, 0, true, Some(1)),
            ("Beta One".to_owned(), 0, 6, true, Some(0)),
            ("Beta Two".to_owned(), 0, 4, true, Some(0)),
        ]
    );
    let before = groups(&db);

    // The Betas' lists are gone and Alpha One was played once more: the
    // play counts, the groups stay, and the lists are wanted.
    let (b1, b2) = (mbid('b', 1), mbid('b', 2));
    insert(
        &db,
        format!(
            "DELETE FROM catalog_artist_neighbours WHERE artist_mbid IN ('{b1}', '{b2}'); \
             DELETE FROM catalog_fact_fetches WHERE kind = 'neighbours' \
               AND artist_mbid IN ('{b1}', '{b2}');"
        ) + &plays("art-a1", &mbid('e', 1), 1, at + MINUTE),
    )
    .await;
    let asked = reads(&script).3;
    build(&deps).await;
    assert_eq!(build_row(&db), (6, 10, 3, 3, true));
    assert_eq!(groups(&db), before, "the groups before it are kept");
    assert_eq!(
        artists(&db),
        [
            ("Alpha One".to_owned(), 6, 0, true, Some(1)),
            ("Beta One".to_owned(), 0, 6, true, Some(0)),
            ("Beta Two".to_owned(), 0, 4, true, Some(0)),
        ]
    );
    let wanted: i64 = db.one(
        &format!(
            "SELECT COUNT(*) FROM catalog_fact_fetches WHERE kind = 'neighbours' \
             AND artist_mbid IN ('{b1}', '{b2}') AND wanted_at IS NOT NULL"
        ),
        [],
    );
    assert_eq!(wanted, 2);
    assert_eq!(reads(&script).3, asked, "nothing was asked of labs");
    assert!(
        !due_on_visit(&rig).await,
        "not due while the lists are missing"
    );

    // The lists arrive: a visit queues her build, which groups them again.
    rig.clock().advance(MINUTE);
    keep_facts_of(&rig, Arc::new(two_circles(Vec::new()))).await;
    assert!(due_on_visit(&rig).await, "due once they arrive");
    build(&deps).await;
    assert_eq!(build_row(&db), (6, 10, 3, 3, false));
    assert_eq!(groups(&db), before);
    assert!(!due_on_visit(&rig).await, "built, so not due again");

    assert_eq!(bobs(&db), bob, "Bob's rows are untouched");
}

/// While Ada's imported history is not current the job reads her
/// ListenBrainz top artists once, and the artist it names is known; once
/// it is current that input is left out.
#[tokio::test]
async fn discover_taste_reads_top_artists_only_while_the_import_is_behind() {
    let dir = crate::common::ScratchDir::new("discover-taste-linked");
    let rig = AuthRig::new("discover-taste-linked").await;
    let db = rig.platform.db.clone();
    history(&db, rig.clock().now_ms() - 10 * DAY).await;
    seen_now(&rig, "user-1").await;
    let named = ArtistRow {
        name: "Gamma One".to_owned(),
        mbid: Some(mbid('c', 1)),
        listen_count: Some(100),
    };
    keep_facts_of(&rig, Arc::new(two_circles(Vec::new()))).await;
    let script = Arc::new(two_circles(vec![named]));
    let network = Arc::clone(&script);
    let deps = discover_deps(&rig, &dir, Providers::closed(), move |slot| {
        *slot = network;
    });
    let gamma = |db: &Db| -> i64 {
        db.one(
            "SELECT COUNT(*) FROM catalog_taste_artists WHERE user_id = 'user-1' \
             AND name = 'Gamma One'",
            [],
        )
    };

    build(&deps).await;
    assert_eq!(reads(&script).0, 1, "one read of her top artists this year");
    assert_eq!(reads(&script).1, 0, "enough seeds: no all-time read");
    assert_eq!(gamma(&db), 1, "the artist it names is known");

    import_current(&db).await;
    build(&deps).await;
    assert_eq!(reads(&script).0, 1, "nothing more read");
    assert_eq!(gamma(&db), 0, "the history holds her listening now");
}

/// An input that fails, here Ada's own Jellyfin, marks the build as
/// missing something but still groups her seeds: only missing similar
/// artists keep the groups before it.
#[tokio::test]
async fn discover_taste_regroups_when_an_input_fails() {
    let dir = crate::common::ScratchDir::new("discover-taste-input");
    let rig = AuthRig::new("discover-taste-input").await;
    let db = rig.platform.db.clone();
    history(&db, rig.clock().now_ms() - 10 * DAY).await;
    import_current(&db).await;
    keep_facts_of(&rig, Arc::new(two_circles(Vec::new()))).await;
    let mut script = two_circles(Vec::new());
    script.user.jellyfin = true;
    script.jellyfin_down.store(true, Ordering::SeqCst);
    let network = Arc::new(script);
    let deps = discover_deps(&rig, &dir, Providers::closed(), move |slot| {
        *slot = network;
    });

    build(&deps).await;
    assert_eq!(build_row(&db), (8, 10, 4, 4, true), "the build says so");
    assert_eq!(groups(&db).len(), 2, "her seeds are grouped all the same");
}

/// One play in the app by `user` of `artist`'s `track`, at `at`.
fn play(user: &str, artist: &str, track: &str, recording: Option<&str>, at: i64) -> String {
    let recording = recording.map_or("NULL".to_owned(), |id| format!("'{id}'"));
    format!(
        "INSERT INTO library_play_history (id, user_id, local_artist_id, track_name, \
         artist_name, recording_mbid, played_at) VALUES ('{user}-{artist}-{at}', '{user}', \
         '{artist}', '{track}', 'Someone', {recording}, {at});"
    )
}

/// One listen imported for Ada of `artist`'s `track`, at `at`.
fn listen(artist: &str, track: &str, recording: &str, at: i64) -> String {
    format!(
        "INSERT INTO media_imported_listens (user_id, listened_at, recording_mbid, artist_mbid, \
         artist_name, track_name, imported_at) \
         VALUES ('user-1', {at}, '{recording}', '{artist}', 'Someone', '{track}', 1);"
    )
}

/// A play Ada scrobbled to ListenBrainz comes back as a listen and counts
/// once, as the play: matched by artist and recording, or by artist and
/// title when the play has no recording id or ListenBrainz mapped the
/// scrobble to another recording, minutes apart. A listen of another of the
/// artist's tracks, one further off than the window, and one matching
/// only Bob's play all count.
#[tokio::test]
async fn discover_taste_counts_a_scrobbled_play_once() {
    let dir = crate::common::ScratchDir::new("discover-taste-scrobbled");
    let rig = AuthRig::new("discover-taste-scrobbled").await;
    let db = rig.platform.db.clone();
    let at = rig.clock().now_ms() - 10 * DAY;
    let (a1, a2) = (mbid('a', 1), mbid('a', 2));
    let (song, other, titled, remapped) = (mbid('d', 1), mbid('d', 2), mbid('d', 3), mbid('d', 4));
    let mut rows = String::from(
        "INSERT INTO auth_users (id, display_name, role, created_at) \
         VALUES ('user-1', 'Ada', 'user', 0), ('user-2', 'Bob', 'user', 0);",
    );
    rows += &library_artist("art-a1", "Alpha One", &a1, "group");
    rows += &library_artist("art-a2", "Alpha Two", &a2, "group");
    rows += &play("user-1", "art-a1", "Song", Some(&song), at);
    rows += &play("user-1", "art-a2", "Title Case", None, at);
    rows += &play("user-2", "art-a1", "Song", Some(&song), at - 3 * DAY);
    rows += &listen(&a1, "Song", &song, at + 5 * MINUTE);
    rows += &listen(&a1, "song", &remapped, at + 3 * MINUTE);
    rows += &listen(&a2, "title case", &titled, at + MINUTE);
    rows += &listen(&a1, "Other Song", &other, at + 2 * MINUTE);
    rows += &listen(&a1, "Song", &song, at + SAME_PLAY_MS + MINUTE);
    rows += &listen(&a1, "Song", &song, at - 3 * DAY);
    insert(&db, rows).await;
    import_current(&db).await;
    let deps = discover_deps(&rig, &dir, Providers::closed(), |slot| {
        *slot = Arc::new(Script::default());
    });

    build(&deps).await;
    let counted: Vec<(String, i64, i64)> = db.rows(
        "SELECT name, plays, imported_listens FROM catalog_taste_artists \
         WHERE user_id = 'user-1' ORDER BY name",
        [],
    );
    assert_eq!(
        counted,
        [
            ("Alpha One".to_owned(), 1, 3),
            ("Alpha Two".to_owned(), 1, 0),
        ]
    );
    let (plays, listens, ..) = build_row(&db);
    assert_eq!((plays, listens), (2, 3), "the build counts what was kept");
}

/// Two plays make an artist a seed even long after them, and so do a play
/// and a listen of another of its tracks. One play is not enough, nor a
/// play and its own scrobble, and a one-star rating keeps two plays out.
#[tokio::test]
async fn discover_taste_two_plays_make_a_seed() {
    let dir = crate::common::ScratchDir::new("discover-taste-two-plays");
    let rig = AuthRig::new("discover-taste-two-plays").await;
    let db = rig.platform.db.clone();
    let at = rig.clock().now_ms() - 200 * DAY;
    let mut rows = String::from(
        "INSERT INTO auth_users (id, display_name, role, created_at) \
         VALUES ('user-1', 'Ada', 'user', 0);",
    );
    let names = ["Twice", "Once", "Scrobbled", "Mixed", "Low"];
    for (n, name) in (1..).zip(names) {
        let artist = mbid('a', n);
        let id = format!("art-{}", name.to_lowercase());
        let song = mbid('d', n);
        rows += &library_artist(&id, name, &artist, "group");
        rows += &play("user-1", &id, "Song", Some(&song), at);
        match name {
            "Twice" | "Low" => rows += &play("user-1", &id, "Song", Some(&song), at + 4 * MINUTE),
            "Scrobbled" => rows += &listen(&artist, "Song", &song, at + MINUTE),
            "Mixed" => rows += &listen(&artist, "Other Song", &mbid('e', 9), at + 4 * MINUTE),
            _ => {}
        }
    }
    rows += "INSERT INTO catalog_user_ratings (user_id, item_kind, item_id, rating, rated_at) \
             VALUES ('user-1', 'artist', 'art-low', 1, 1);";
    insert(&db, rows).await;
    import_current(&db).await;
    let deps = discover_deps(&rig, &dir, Providers::closed(), |slot| {
        *slot = Arc::new(Script::default());
    });

    build(&deps).await;
    let seeds: Vec<String> = db.column(
        "SELECT name FROM catalog_taste_artists WHERE user_id = 'user-1' AND is_seed = 1 \
         ORDER BY name",
        [],
    );
    assert_eq!(seeds, ["Mixed", "Twice"]);
}
