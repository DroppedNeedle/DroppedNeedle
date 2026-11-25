//! Kept facts: the similar-artist lists, listener counts and album lists
//! Discover keeps in the database, the job that reads them in the
//! background, and the daily ceiling on those reads.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use droppedneedle_catalog::discover::due::Due;
use droppedneedle_catalog::discover::facts::job::FactJobs;
use droppedneedle_catalog::discover::facts::{KeptFacts, plan};
use droppedneedle_catalog::discover::listening::{
    AlbumRow, ArtistRow, Ask, ListeningData, ListeningNetwork, RankedAlbum, ScoredArtist,
};
use droppedneedle_kernel::clock::Clock as _;
use droppedneedle_kernel::db::{Db, DbError, Lane};
use droppedneedle_kernel::jobs::queue::JobQueue;
use droppedneedle_kernel::runtime_config::sections::ListenBrainzSettings;
use droppedneedle_providers::executor::Method;
use droppedneedle_providers::listenbrainz::{self, ArtistPopularity};
use droppedneedle_providers::test_support::{Scripted, ScriptedWire};
use droppedneedle_providers::{ProviderEndpoints, Providers, RequestPriority};

use crate::common::Probe;
use crate::common::auth::AuthRig;
use crate::discover_queue::{Script, discover_inputs};
use droppedneedle_catalog::discover::wiring::live_network;

const DAY_MS: i64 = 86_400_000;
const USER: &str = "user-1";

fn mbid(prefix: char, n: u8) -> String {
    let p = prefix.to_string().repeat(4);
    format!("{n:08x}-{p}-4{}-8{}-{}", &p[..3], &p[..3], p.repeat(3))
}

/// `user` was last seen at `at_ms`: their one session was used then.
async fn seen(db: &Db, user: &str, at_ms: i64) {
    let user = user.to_owned();
    db.write(Lane::Foreground, "test.seen", move |tx| {
        tx.execute(
            "INSERT OR IGNORE INTO auth_users (id, display_name, role, created_at) \
             VALUES (?1, ?1, 'user', 0)",
            [&user],
        )?;
        tx.execute(
            "INSERT INTO auth_tokens (id, user_id, token_hash, issued_at, expires_at, \
             last_seen_at, revoked) VALUES (?1, ?1, ?1, 0, ?2, ?2, 0) \
             ON CONFLICT (id) DO UPDATE SET last_seen_at = excluded.last_seen_at",
            rusqlite::params![user, at_ms],
        )?;
        Ok::<_, DbError>(())
    })
    .await
    .expect("seen");
}

fn neighbour(n: u8, score: f64) -> ScoredArtist {
    ScoredArtist {
        artist: ArtistRow {
            name: format!("Neighbour {n}"),
            mbid: Some(mbid('b', n)),
            listen_count: None,
        },
        score,
    }
}

fn album(n: u8, artist: &str) -> RankedAlbum {
    RankedAlbum {
        album: AlbumRow {
            release_group_mbid: mbid('c', n),
            title: format!("Album {n}"),
            artist_name: "Someone".to_owned(),
            artist_mbid: Some(artist.to_owned()),
            facts: Default::default(),
        },
        listen_count: i64::from(n) * 10,
    }
}

/// Two seeds with two neighbours each, every neighbour counted, and two
/// albums for the first neighbour.
fn script() -> Arc<Script> {
    let (first, second) = (mbid('a', 1), mbid('a', 2));
    let counts = |listeners: i64| ArtistPopularity {
        listen_count: Some(listeners * 10),
        listeners: Some(listeners),
    };
    Arc::new(Script {
        similar: HashMap::from([
            (first, vec![neighbour(1, 100.0), neighbour(2, 90.0)]),
            (second, vec![neighbour(3, 80.0), neighbour(4, 70.0)]),
        ]),
        listeners: HashMap::from([
            (mbid('b', 1), counts(10_000)),
            (mbid('b', 2), counts(100)),
            (mbid('b', 3), counts(50)),
            (mbid('b', 4), counts(50)),
        ]),
        artist_albums: HashMap::from([(
            mbid('b', 1),
            vec![album(1, &mbid('b', 1)), album(2, &mbid('b', 1))],
        )]),
        ..Script::default()
    })
}

struct Rig {
    rig: AuthRig,
    script: Arc<Script>,
    listening: ListeningData,
    jobs: FactJobs,
}

impl Rig {
    async fn new(tag: &str) -> Self {
        let rig = AuthRig::new(tag).await;
        let script = script();
        let network: Arc<dyn ListeningNetwork> = script.clone();
        let facts = KeptFacts::new(rig.platform.db.clone(), rig.platform.clock.clone());
        let listening = ListeningData::new(
            Arc::clone(&network),
            Providers::closed().cache,
            facts.clone(),
        );
        let jobs = FactJobs::new(facts, network);
        Self {
            rig,
            script,
            listening,
            jobs,
        }
    }

    fn db(&self) -> &Db {
        &self.rig.platform.db
    }

    fn now(&self) -> i64 {
        self.rig.clock().now_ms()
    }

    /// The similar artists of `artists` as a build reads them, with
    /// whether the build met a failure.
    async fn build_similar(&self, artists: &[String]) -> (Vec<Result<Vec<String>, ()>>, bool) {
        let ask = Ask::build(RequestPriority::BackgroundSync);
        let found = self
            .listening
            .similar_artists_each(USER, artists, 10, &ask)
            .await
            .into_iter()
            .map(|rows| {
                rows.map(|rows| rows.into_iter().map(|row| row.artist.name).collect())
                    .map_err(|_| ())
            })
            .collect();
        (found, ask.met_failure())
    }

    async fn build_albums(&self, artist: &str) -> (Vec<String>, bool) {
        let ask = Ask::build(RequestPriority::BackgroundSync);
        let rows = self
            .listening
            .artist_albums(USER, artist, 10, &ask)
            .await
            .expect("an album read answers");
        let titles = rows.into_iter().map(|row| row.album.title).collect();
        (titles, ask.met_failure())
    }

    async fn refresh(&self) -> u64 {
        self.jobs.run_once(&|| false).await.expect("the job runs")
    }

    fn reads(&self) -> (usize, usize, usize, usize) {
        (
            self.script.similar_reads.load(Ordering::SeqCst),
            self.script.listener_reads.load(Ordering::SeqCst),
            self.script.discography_reads.lock().unwrap().len(),
            self.script.count_reads.lock().unwrap().len(),
        )
    }
}

/// A build reads kept rows only: a fact not kept answers "not yet", fails
/// the build's read so a shelf that had rows keeps them, and is wanted.
/// The job reads what was wanted (a list, then its neighbours' counts in
/// one read; an album list, then its counts) and the next build has it in
/// R8's order. With every provider read failing, a build still answers the
/// kept rows and "not yet" for the rest, and sends nothing.
#[tokio::test]
async fn a_build_reads_kept_rows_and_the_job_reads_what_it_wanted() {
    let rig = Rig::new("kept-facts-build").await;
    seen(rig.db(), USER, rig.now()).await;
    let (first, second) = (mbid('a', 1), mbid('a', 2));

    let (lists, failed) = rig.build_similar(std::slice::from_ref(&first)).await;
    assert_eq!(lists, [Err(())], "not yet");
    assert!(failed);
    let (albums, failed) = rig.build_albums(&mbid('b', 1)).await;
    assert!(albums.is_empty() && failed, "not yet");
    assert_eq!(rig.reads(), (0, 0, 0, 0), "a build sends nothing");

    assert_eq!(rig.refresh().await, 4);
    assert_eq!(rig.reads(), (1, 1, 1, 1), "one read of each kind");

    let (lists, failed) = rig.build_similar(std::slice::from_ref(&first)).await;
    assert_eq!(
        lists,
        [Ok(vec!["Neighbour 2".to_owned(), "Neighbour 1".to_owned()])],
        "fame divided out"
    );
    assert!(!failed);
    let (albums, failed) = rig.build_albums(&mbid('b', 1)).await;
    assert_eq!(albums, ["Album 2", "Album 1"], "most listeners first");
    assert!(!failed);

    rig.script.similar_down.store(true, Ordering::SeqCst);
    rig.script.albums_down.store(true, Ordering::SeqCst);
    let (lists, failed) = rig.build_similar(&[first, second]).await;
    assert_eq!(
        lists,
        [
            Ok(vec!["Neighbour 2".to_owned(), "Neighbour 1".to_owned()]),
            Err(())
        ]
    );
    assert!(failed);
    assert_eq!(rig.reads(), (1, 1, 1, 1), "still nothing sent");
}

/// A refresh that fails keeps the rows it had, records when the fact first
/// failed and keeps that time through the next failure; the rows still
/// answer.
#[tokio::test]
async fn a_failed_refresh_keeps_the_rows_and_says_since_when() {
    let rig = Rig::new("kept-facts-failed").await;
    let first = mbid('a', 1);
    seen(rig.db(), USER, rig.now()).await;
    rig.build_similar(std::slice::from_ref(&first)).await;
    rig.refresh().await;

    rig.rig.clock().advance(15 * DAY_MS);
    seen(rig.db(), USER, rig.now()).await;
    rig.build_similar(std::slice::from_ref(&first)).await;
    rig.script.similar_down.store(true, Ordering::SeqCst);
    let failed_at = rig.now();
    rig.refresh().await;
    let state = |db: &Db| -> (String, i64, i64) {
        let row = |column: &str| {
            format!(
                "SELECT {column} FROM catalog_fact_fetches \
                 WHERE kind = 'neighbours' AND artist_mbid = '{first}'"
            )
        };
        (
            db.one(&row("outcome"), []),
            db.one(&row("failing_since_at"), []),
            db.one(&row("failures"), []),
        )
    };
    assert_eq!(state(rig.db()), ("failed".to_owned(), failed_at, 1));

    rig.rig.clock().advance(3_600_000);
    rig.refresh().await;
    assert_eq!(
        state(rig.db()),
        ("failed".to_owned(), failed_at, 2),
        "the first failure's time stands"
    );
    let (lists, _) = rig.build_similar(std::slice::from_ref(&first)).await;
    assert_eq!(
        lists,
        [Ok(vec!["Neighbour 2".to_owned(), "Neighbour 1".to_owned()])],
        "the kept list answers"
    );
    assert_eq!(rig.reads().0, 3);
}

/// The day's background reads stop at the ceiling: 60 plus 30 for each
/// person seen in seven days, never over 300, none with nobody seen. The
/// count is in the database, so a restart does not reset it, and the next
/// day the job reads again.
#[tokio::test]
async fn the_job_stops_at_the_ceiling_and_its_count_survives_a_restart() {
    assert_eq!(plan::ceiling(0), 0);
    assert_eq!(plan::ceiling(1), 90);
    assert_eq!(plan::ceiling(4), 180);
    assert_eq!(plan::ceiling(8), 300);
    assert_eq!(plan::ceiling(500), 300);

    let rig = Rig::new("kept-facts-ceiling").await;
    seen(rig.db(), USER, rig.now()).await;
    let today = rig.now().div_euclid(DAY_MS);
    rig.db()
        .write(Lane::Foreground, "test.counted", move |tx| {
            tx.execute(
                "INSERT INTO catalog_background_read_days (day, source, reads) \
                 VALUES (?1, 'listenbrainz', 89)",
                [today],
            )?;
            Ok::<_, DbError>(())
        })
        .await
        .expect("counted");
    rig.build_similar(&[mbid('a', 1), mbid('a', 2)]).await;

    assert_eq!(rig.refresh().await, 1, "one read left today");
    let counted = |db: &Db| -> i64 {
        db.one(
            "SELECT SUM(reads) FROM catalog_background_read_days WHERE day = ?1",
            [today],
        )
    };
    assert_eq!(counted(rig.db()), 90);

    let restarted = FactJobs::new(
        KeptFacts::new(rig.db().clone(), rig.rig.platform.clock.clone()),
        rig.script.clone(),
    );
    assert_eq!(restarted.run_once(&|| false).await.unwrap(), 0);
    assert_eq!(rig.reads().0, 1);

    rig.rig.clock().advance(DAY_MS);
    assert!(
        restarted.run_once(&|| false).await.unwrap() > 0,
        "a new day"
    );
}

/// No background read is made while nobody has been seen for seven days:
/// the tick queues no refresh and a run reads nothing. The person's next
/// request records them seen, and the next tick queues it.
#[tokio::test]
async fn nothing_runs_for_a_person_unseen_for_a_week_and_a_request_wakes_it() {
    let rig = Rig::new("kept-facts-quiet").await;
    seen(rig.db(), USER, rig.now() - 8 * DAY_MS).await;
    rig.build_similar(&[mbid('a', 1)]).await;
    let due = Due::new(
        rig.db().clone(),
        JobQueue::new(rig.db().clone(), rig.rig.platform.clock.clone()),
        rig.rig.platform.clock.clone(),
        true,
        (
            FactJobs::new(
                KeptFacts::new(rig.db().clone(), rig.rig.platform.clock.clone()),
                rig.script.clone(),
            )
            .due(),
            crate::common::history_store(&rig.rig.platform),
            crate::discover_queue::fresh_feed(&rig.rig.platform),
        ),
    );
    let queued = |db: &Db| -> i64 {
        db.one(
            "SELECT COUNT(*) FROM durable_jobs WHERE kind = 'discover.facts'",
            [],
        )
    };

    due.clock_pass().await;
    assert_eq!(queued(rig.db()), 0);
    assert_eq!(rig.refresh().await, 0);
    assert_eq!(rig.reads(), (0, 0, 0, 0));

    seen(rig.db(), USER, rig.now()).await;
    due.clock_pass().await;
    assert_eq!(queued(rig.db()), 1, "the next tick queues the refresh");
}

/// The refresh over the live clients sends no token: the facts are public.
/// With ListenBrainz reads switched off the job sends nothing, and a build
/// still reads the kept list.
#[tokio::test]
async fn no_token_goes_out_and_with_listenbrainz_off_nothing_does() {
    const RADIOHEAD: &str = "a74b1b7f-71a5-4011-9441-d0b5e4122711";
    let dir = crate::common::ScratchDir::new("kept-facts-live");
    let rig = AuthRig::new("kept-facts-live").await;
    seen(&rig.platform.db, USER, rig.clock().now_ms()).await;
    crate::common::listening_store(&rig.platform)
        .save_listenbrainz(USER, "listener", "secret-token")
        .await
        .expect("a linked person");
    let wire = Arc::new(ScriptedWire::new());
    wire.answer(
        &format!("{}/similar-artists/json", listenbrainz::LABS_BASE_URL),
        200,
        &[],
        include_str!("../fixtures/listenbrainz/labs_similar_artists_radiohead.json"),
    );
    wire.on(
        Method::Post,
        &format!("{}/1/popularity/artist", listenbrainz::DEFAULT_BASE_URL),
        Scripted::Answer {
            status: 200,
            headers: Vec::new(),
            body: include_bytes!("../fixtures/listenbrainz/popularity_artist_post.json").to_vec(),
        },
    );
    let inputs = discover_inputs(
        &rig,
        &dir,
        Providers::unpaced_on(
            wire.clone(),
            rig.platform.clock.clone(),
            ProviderEndpoints {
                listenbrainz: listenbrainz::DEFAULT_BASE_URL.to_owned(),
                listenbrainz_labs: listenbrainz::LABS_BASE_URL.to_owned(),
                ..ProviderEndpoints::closed()
            },
        ),
    );
    let network = live_network(&inputs, &rig.config);
    let facts = KeptFacts::new(rig.platform.db.clone(), rig.platform.clock.clone());
    let listening = ListeningData::new(
        Arc::clone(&network),
        inputs.providers.cache.clone(),
        facts.clone(),
    );
    let jobs = FactJobs::new(facts, network);
    let build = |artist: &'static str| {
        let listening = listening.clone();
        async move {
            let ask = Ask::build(RequestPriority::BackgroundSync);
            listening
                .similar_artists("someone-else", artist, 100, &ask)
                .await
                .map(|rows| rows.len())
                .ok()
        }
    };

    assert_eq!(build(RADIOHEAD).await, None, "not yet");
    jobs.run_once(&|| false).await.expect("the job runs");
    let sent = wire.sent();
    assert_eq!(sent.len(), 2, "the list, then its neighbours' counts");
    for request in &sent {
        assert!(
            request
                .headers
                .iter()
                .all(|(name, _)| !name.eq_ignore_ascii_case("authorization")),
            "{} carried a token",
            request.url
        );
    }
    assert_eq!(build(RADIOHEAD).await, Some(100));

    rig.config
        .save_blocking(ListenBrainzSettings {
            reads_enabled: false,
        })
        .expect("off");
    assert_eq!(
        build("b10bbbfc-cf9e-42e0-be17-e2c3e1d2600d").await,
        Some(0),
        "nothing to wait for"
    );
    rig.clock().advance(15 * DAY_MS);
    seen(&rig.platform.db, USER, rig.clock().now_ms()).await;
    jobs.run_once(&|| false).await.expect("the job runs");
    assert_eq!(wire.sent().len(), 2, "nothing is sent while off");
    assert_eq!(build(RADIOHEAD).await, Some(100), "the kept list answers");
}

/// A labs outage is a read that went out: a 503 with a Retry-After is
/// counted and marks its list failed, with the time it first failed. The
/// rest it starts sends nothing more and counts nothing more.
#[tokio::test]
async fn a_labs_outage_is_counted_and_marked_failed() {
    let dir = crate::common::ScratchDir::new("kept-facts-outage");
    let rig = AuthRig::new("kept-facts-outage").await;
    seen(&rig.platform.db, USER, rig.clock().now_ms()).await;
    let wire = Arc::new(ScriptedWire::new());
    wire.answer(
        &format!("{}/similar-artists/json", listenbrainz::LABS_BASE_URL),
        503,
        &[("retry-after", "60")],
        "",
    );
    let inputs = discover_inputs(
        &rig,
        &dir,
        Providers::unpaced_on(
            wire.clone(),
            rig.platform.clock.clone(),
            ProviderEndpoints {
                listenbrainz: listenbrainz::DEFAULT_BASE_URL.to_owned(),
                listenbrainz_labs: listenbrainz::LABS_BASE_URL.to_owned(),
                ..ProviderEndpoints::closed()
            },
        ),
    );
    let network = live_network(&inputs, &rig.config);
    let facts = KeptFacts::new(rig.platform.db.clone(), rig.platform.clock.clone());
    let listening = ListeningData::new(
        Arc::clone(&network),
        inputs.providers.cache.clone(),
        facts.clone(),
    );
    let jobs = FactJobs::new(facts, network);
    let artist = mbid('a', 1);
    let ask = Ask::build(RequestPriority::BackgroundSync);
    listening
        .similar_artists_each(USER, std::slice::from_ref(&artist), 10, &ask)
        .await;
    let failed_at = rig.clock().now_ms();

    for _ in 0..3 {
        jobs.run_once(&|| false).await.expect("the job runs");
        rig.clock().advance(90_000);
    }
    assert_eq!(wire.sent().len(), 1, "one labs read went out");
    let today = failed_at.div_euclid(DAY_MS);
    let counted: i64 = rig.platform.db.one(
        "SELECT COALESCE(SUM(reads), 0) FROM catalog_background_read_days WHERE day = ?1",
        [today],
    );
    assert_eq!(counted, 1, "the read that went out is counted");
    let state = |column: &str| -> i64 {
        rig.platform.db.one(
            &format!(
                "SELECT {column} FROM catalog_fact_fetches \
                 WHERE kind = 'neighbours' AND artist_mbid = '{artist}' AND outcome = 'failed'"
            ),
            [],
        )
    };
    assert_eq!(state("failing_since_at"), failed_at);
    assert_eq!(state("failures"), 1);
}

/// Only reads that go out are counted. A labs read that fails rests labs;
/// while it rests the client sends nothing, so the runs after it count
/// nothing, mark no other list failed and plan no list.
#[tokio::test]
async fn reads_turned_down_while_labs_rests_are_not_counted() {
    let dir = crate::common::ScratchDir::new("kept-facts-rest");
    let rig = AuthRig::new("kept-facts-rest").await;
    seen(&rig.platform.db, USER, rig.clock().now_ms()).await;
    let wire = Arc::new(ScriptedWire::new());
    wire.answer(
        &format!("{}/similar-artists/json", listenbrainz::LABS_BASE_URL),
        400,
        &[("content-type", "text/html")],
        include_str!("../fixtures/listenbrainz/labs_unknown_algorithm.html"),
    );
    let inputs = discover_inputs(
        &rig,
        &dir,
        Providers::unpaced_on(
            wire.clone(),
            rig.platform.clock.clone(),
            ProviderEndpoints {
                listenbrainz: listenbrainz::DEFAULT_BASE_URL.to_owned(),
                listenbrainz_labs: listenbrainz::LABS_BASE_URL.to_owned(),
                ..ProviderEndpoints::closed()
            },
        ),
    );
    let network = live_network(&inputs, &rig.config);
    let facts = KeptFacts::new(rig.platform.db.clone(), rig.platform.clock.clone());
    let listening = ListeningData::new(
        Arc::clone(&network),
        inputs.providers.cache.clone(),
        facts.clone(),
    );
    let jobs = FactJobs::new(facts, network);
    let artists: Vec<String> = (1..=3).map(|n| mbid('a', n)).collect();
    let ask = Ask::build(RequestPriority::BackgroundSync);
    listening
        .similar_artists_each(USER, &artists, 10, &ask)
        .await;

    for _ in 0..6 {
        jobs.run_once(&|| false).await.expect("the job runs");
        rig.clock().advance(90_000);
    }
    assert_eq!(wire.sent().len(), 1, "one labs read went out");
    let today = rig.clock().now_ms().div_euclid(DAY_MS);
    let counted: i64 = rig.platform.db.one(
        "SELECT COALESCE(SUM(reads), 0) FROM catalog_background_read_days WHERE day = ?1",
        [today],
    );
    assert_eq!(counted, 1, "the day counts what was sent");
    let failed: i64 = rig.platform.db.one(
        "SELECT COUNT(*) FROM catalog_fact_fetches WHERE outcome = 'failed'",
        [],
    );
    assert_eq!(failed, 1, "only the list that was asked for failed");
}
