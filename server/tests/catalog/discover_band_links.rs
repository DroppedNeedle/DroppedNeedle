//! Band links over a real database and the production builder. The
//! recorded MusicBrainz answers read as the five kinds and nothing else;
//! the refresh job keeps them, and a MusicBrainz that does not answer keeps
//! the rows and backs off. A seed band gives its member's album and the
//! member's other bands, each with its line, through every gate of a pick;
//! a collective's members are not followed; links not kept are wanted, the
//! build waits for them and their arrival makes it due; and a band-link
//! pick takes a weekly set slot under the same cap per seed, without moving
//! the rest of the pool.

use std::collections::BTreeSet;
use std::sync::Arc;

use droppedneedle_catalog::discover::due::Due;
use droppedneedle_catalog::discover::engine::weekly::{self, SetDay};
use droppedneedle_catalog::discover::facts::KeptFacts;
use droppedneedle_catalog::discover::facts::job::FactJobs;
use droppedneedle_catalog::discover::listening::ListeningNetwork;
use droppedneedle_catalog::discover::listening::band_links::{
    self, BandLink, BandLinkKind, LinkDirection,
};
use droppedneedle_catalog::discover::models::{EvidenceSource, FoundBy, PickReasonKind};
use droppedneedle_catalog::discover::services::DiscoverDeps;
use droppedneedle_kernel::clock::Clock as _;
use droppedneedle_kernel::db::Db;
use droppedneedle_kernel::jobs::queue::JobQueue;
use droppedneedle_providers::musicbrainz::models::MbArtist;
use droppedneedle_providers::musicbrainz::{BRAINZMASH_API_BASE, MB_API_BASE};
use droppedneedle_providers::test_support::ScriptedWire;
use droppedneedle_providers::{ProviderEndpoints, Providers};

use crate::common::Probe;
use crate::common::auth::AuthRig;
use crate::discover_pool::{counted, id, list};
use crate::discover_queue::{
    Script, discover_deps, discover_inputs, insert, library_artist, seen_now,
};
use droppedneedle_catalog::discover::wiring::live_network;

const DAY: i64 = 86_400_000;
const HOUR: i64 = 3_600_000;

const CIRCA: &str = "a2496a3c-8f42-4b98-b780-6f8d64f56441";
const GREEN: &str = "eba4c290-2ce6-42c9-affd-5b1ffab84a8f";
const SAOSIN: &str = "47b8b42a-96e8-46fa-9ea6-70c5136ae971";
const ANIMALS: &str = "9f492774-4cdb-4735-b299-5b6affafe4d4";
/// MusicBrainz's Various Artists: a special-purpose id.
const VARIOUS: &str = "89ad4ac3-39f7-470e-963a-56509c546377";

/// One recorded artist lookup with `inc=artist-rels`.
fn recorded(name: &str) -> MbArtist {
    let path = format!(
        "{}/tests/fixtures/musicbrainz/artist_rels_{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(&path).expect("the recorded answer");
    serde_json::from_str(&text).expect("an artist document")
}

fn read(name: &str) -> Vec<BandLink> {
    let artist = recorded(name);
    band_links::links(&artist.id, &artist.relations)
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

/// `artist`'s kept band links (linked id, name, kind, direction, ended);
/// "nothing there" when none.
fn links(artist: &str, rows: &[(&str, &str, &str, &str, bool)], at: i64) -> String {
    let mut sql = format!("DELETE FROM catalog_band_links WHERE artist_mbid = '{artist}';");
    for (position, (linked, name, kind, direction, ended)) in rows.iter().enumerate() {
        sql += &format!(
            "INSERT INTO catalog_band_links (artist_mbid, position, kind, direction, \
             linked_mbid, linked_name, ended) VALUES ('{artist}', {position}, '{kind}', \
             '{direction}', '{linked}', '{name}', {});",
            i64::from(*ended)
        );
    }
    let outcome = if rows.is_empty() { "none" } else { "found" };
    sql + &fetched(artist, "band_links", "musicbrainz", outcome, at)
}

/// A member of `artist`'s band, as the band's links hold it.
fn member<'a>(mbid: &'a str, name: &'a str) -> (&'a str, &'a str, &'a str, &'a str, bool) {
    (mbid, name, "member_of_band", "backward", false)
}

/// `artist`'s kept album list, every album by `name`, counted.
fn albums(artist: &str, name: &str, groups: &[String], at: i64) -> String {
    let mut sql = String::new();
    for (position, group) in groups.iter().enumerate() {
        sql += &format!(
            "INSERT INTO catalog_artist_albums (artist_mbid, release_group_mbid, position, \
             title, artist_name, primary_type, secondary_types, first_release_date, \
             credited_artist_mbids, listeners, listens) VALUES ('{artist}', '{group}', \
             {position}, 'Title of {group}', '{name}', 'Album', '[]', '2016', '[\"{artist}\"]', \
             500, 500);"
        );
    }
    let outcome = if groups.is_empty() { "none" } else { "found" };
    sql += &fetched(artist, "albums", "musicbrainz", outcome, at);
    sql + &fetched(artist, "album_counts", "listenbrainz", "found", at)
}

/// Ada's taste build: `seeds` (id, name, weight) in one group, and the
/// other artists she knows.
fn taste(seeds: &[(&str, &str, f64)], known: &[&str], at: i64) -> String {
    let mut sql = format!(
        "INSERT INTO auth_users (id, display_name, role, created_at) \
         VALUES ('user-1', 'Ada', 'user', 0); \
         INSERT INTO catalog_taste_builds (user_id, built_at, plays_counted, \
         imported_listens_counted, artists_played, seeds, incomplete) \
         VALUES ('user-1', {at}, 10, 0, 1, {}, 0); \
         INSERT INTO catalog_taste_groups (user_id, group_no, anchor_artist_mbid, \
         anchor_name, share, recent_share, trend) VALUES ('user-1', 0, '{}', 'Anchor', 1.0, \
         1.0, 'steady');",
        seeds.len(),
        seeds[0].0
    );
    for (seed, name, weight) in seeds {
        sql += &format!(
            "INSERT INTO catalog_taste_artists (user_id, artist_mbid_lower, artist_mbid, name, \
             plays, imported_listens, months_active, weight, is_seed, group_no) \
             VALUES ('user-1', '{seed}', '{seed}', '{name}', 5, 0, 1, {weight}, 1, 0);"
        );
    }
    for artist in known {
        sql += &format!(
            "INSERT INTO catalog_taste_artists (user_id, artist_mbid_lower, artist_mbid, name, \
             plays, imported_listens, months_active, weight, is_seed) \
             VALUES ('user-1', '{artist}', '{artist}', 'Known', 1, 0, 1, 0.1, 0);"
        );
    }
    sql
}

/// A kept labs list for `seed` naming `neighbours` (id, raw score), each
/// counted at 10,000 listeners.
fn neighbours(seed: &str, rows: &[(String, f64)], at: i64) -> String {
    let mut sql = list(seed, rows, at);
    for (artist, _) in rows {
        sql += &counted(artist, 10_000, at);
    }
    sql
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

/// Ada's pool, in order: each album with its kind.
fn stored(db: &Db) -> Vec<(String, String)> {
    db.rows(
        "SELECT release_group_mbid_lower, kind FROM catalog_discover_candidates \
         WHERE user_id = 'user-1' ORDER BY rank",
        [],
    )
}

/// Whether Ada's last build waits for facts, and for whom.
fn waiting(db: &Db) -> (bool, Vec<String>) {
    let (incomplete, waiting): (bool, String) = db.row(
        "SELECT incomplete, waiting_for FROM catalog_discover_pool_builds \
         WHERE user_id = 'user-1'",
        [],
    );
    (
        incomplete,
        serde_json::from_str(&waiting).expect("a JSON list"),
    )
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
            crate::discover_queue::fresh_feed(&rig.platform),
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

/// The recorded answers read as the five kinds, each in its direction, and
/// nothing else: no marriage, supporting musician or "named after". A
/// member who left and came back is one link, ended only when every one of
/// its relations is; an artist's own instruments do not repeat it.
#[test]
fn band_links_read_the_five_kinds_and_nothing_else() {
    let kinds = |links: &[BandLink]| -> BTreeSet<(&'static str, &'static str)> {
        links
            .iter()
            .map(|link| (link.kind.as_str(), link.direction.as_str()))
            .collect()
    };

    let circa = read("circa_survive");
    assert_eq!(circa.len(), 5, "five members, each once: {circa:?}");
    assert_eq!(
        kinds(&circa),
        BTreeSet::from([("member_of_band", "backward")])
    );
    assert!(circa.iter().all(|link| !link.ended));
    assert!(
        circa
            .iter()
            .any(|link| link.mbid == GREEN && link.name == "Anthony Green")
    );

    let green = read("anthony_green");
    assert_eq!(
        kinds(&green),
        BTreeSet::from([("member_of_band", "forward")])
    );
    assert_eq!(green.len(), 7, "seven bands, no marriage: {green:?}");
    let saosin = green
        .iter()
        .find(|link| link.mbid == SAOSIN)
        .expect("Saosin");
    assert!(saosin.ended, "left Saosin, twice");
    assert!(
        green.iter().any(|link| link.mbid == CIRCA
            && !link.ended
            && link.kind == BandLinkKind::MemberOfBand)
    );

    let wu_tang = read("wu_tang_clan");
    assert_eq!(
        kinds(&wu_tang),
        BTreeSet::from([
            ("founder", "forward"),
            ("member_of_band", "backward"),
            ("subgroup", "backward"),
        ]),
        "no supporting musicians"
    );
    let doom = read("mf_doom");
    assert_eq!(
        kinds(&doom),
        BTreeSet::from([("is_person", "backward"), ("member_of_band", "forward")]),
        "not \"named after artist\""
    );
    let ffs = read("ffs");
    let collaboration: Vec<&BandLink> = ffs
        .iter()
        .filter(|link| link.kind == BandLinkKind::Collaboration)
        .collect();
    assert_eq!(collaboration.len(), 2);
    assert!(
        collaboration
            .iter()
            .all(|link| link.ended && link.direction == LinkDirection::Backward)
    );

    let every: BTreeSet<&str> = [circa, green, wu_tang, doom, ffs]
        .iter()
        .flatten()
        .map(|link| link.kind.as_str())
        .collect();
    assert_eq!(
        every,
        BandLinkKind::ALL.iter().map(|kind| kind.as_str()).collect()
    );
}

/// The refresh reads a wanted artist's links over the live client with
/// `inc=artist-rels` and keeps them. Ninety days on, a MusicBrainz that
/// answers 503 keeps every row, marks the fact failed and backs it off: it
/// is never kept as "no links".
#[tokio::test]
async fn a_dead_musicbrainz_keeps_band_links_and_backs_off() {
    let dir = crate::common::ScratchDir::new("band-links-dead");
    let rig = AuthRig::new("band-links-dead").await;
    insert(
        &rig.platform.db,
        "INSERT INTO auth_users (id, display_name, role, created_at) \
         VALUES ('user-1', 'Ada', 'user', 0);"
            .to_owned(),
    )
    .await;
    seen_now(&rig, "user-1").await;
    let wire = Arc::new(ScriptedWire::new());
    let body = include_str!("../fixtures/musicbrainz/artist_rels_circa_survive.json");
    for root in [MB_API_BASE, BRAINZMASH_API_BASE] {
        let url = format!("{root}/artist/{CIRCA}");
        wire.answer(&url, 200, &[], body);
        wire.answer(&url, 503, &[("retry-after", "60")], "");
    }
    let inputs = discover_inputs(
        &rig,
        &dir,
        Providers::unpaced_on(
            wire.clone(),
            rig.platform.clock.clone(),
            ProviderEndpoints {
                musicbrainz: MB_API_BASE.to_owned(),
                brainzmash: BRAINZMASH_API_BASE.to_owned(),
                ..ProviderEndpoints::closed()
            },
        ),
    );
    let network: Arc<dyn ListeningNetwork> = live_network(&inputs, &rig.config);
    let facts = KeptFacts::new(rig.platform.db.clone(), rig.platform.clock.clone());
    let jobs = FactJobs::new(facts, network);
    let db = &rig.platform.db;
    let now = rig.clock().now_ms();
    db.exec(
        "INSERT INTO catalog_fact_fetches (artist_mbid, kind, source, due_at, wanted_at, \
         want_order, read_at) VALUES (?1, 'band_links', 'musicbrainz', ?2, ?2, 1, ?2)",
        rusqlite::params![CIRCA, now],
    );

    jobs.run_once(&|| false).await.expect("the job runs");
    let sent = wire.sent();
    assert_eq!(sent.len(), 1, "one lookup");
    assert_eq!(sent[0].query("inc").as_deref(), Some("artist-rels"));
    let kept = |db: &Db| -> i64 {
        db.one(
            "SELECT COUNT(*) FROM catalog_band_links WHERE artist_mbid = ?1",
            [CIRCA],
        )
    };
    assert_eq!(kept(db), 5, "the five members kept");

    rig.clock().advance(91 * DAY);
    let now = rig.clock().now_ms();
    seen_now(&rig, "user-1").await;
    db.exec(
        "UPDATE catalog_fact_fetches SET read_at = ?1 WHERE kind = 'band_links'",
        [now],
    );
    jobs.run_once(&|| false).await.expect("the job runs");
    assert_eq!(wire.sent().len(), 2, "asked again once it was due");
    assert_eq!(kept(db), 5, "the rows stay");
    let (outcome, failures, due): (String, i64, i64) = db.row(
        "SELECT outcome, failures, due_at FROM catalog_fact_fetches \
         WHERE artist_mbid = ?1 AND kind = 'band_links'",
        [CIRCA],
    );
    assert_eq!((outcome.as_str(), failures), ("failed", 1));
    assert!(due > now, "backed off");
}

/// Circa Survive as a seed: its member Anthony Green's album, credited to
/// "Anthony Green & Friends", comes in as "Anthony Green of Circa
/// Survive." (the link's own name), his other bands' as "With Anthony
/// Green of Circa Survive." (the seed itself is not one of them), found by
/// a band link from MusicBrainz, read when the links were. A member the
/// person knows, one the library has, one turned down and a
/// special-purpose id give nothing. The build asks no provider and waits
/// for nothing.
#[tokio::test]
async fn a_seed_band_leads_to_its_member_and_his_other_bands() {
    let dir = crate::common::ScratchDir::new("band-links-member");
    let rig = AuthRig::new("band-links-member").await;
    let db = rig.platform.db.clone();
    let at = rig.clock().now_ms() - DAY;
    let (known, library, down, neighbour) = (id(5, 1), id(5, 2), id(5, 3), id(2, 1));
    let group = |n: usize| id(3, n);

    let mut rows = taste(&[(CIRCA, "Circa Survive", 2.0)], &[&known], at);
    rows += &neighbours(CIRCA, &[(neighbour.clone(), 100.0)], at);
    rows += &links(
        CIRCA,
        &[
            member(GREEN, "Anthony Green"),
            member(&known, "Known Member"),
            member(&library, "Library Member"),
            member(&down, "Turned Down"),
            member(VARIOUS, "Various Artists"),
        ],
        at,
    );
    rows += &links(
        GREEN,
        &[
            (CIRCA, "Circa Survive", "member_of_band", "forward", false),
            (SAOSIN, "Saosin", "member_of_band", "forward", true),
            (
                ANIMALS,
                "The Sound of Animals Fighting",
                "member_of_band",
                "forward",
                false,
            ),
        ],
        at,
    );
    rows += &links(&known, &[], at);
    rows += &links(&library, &[], at);
    rows += &counted(GREEN, 5_000, at);
    for (n, (artist, name)) in [
        (GREEN, "Anthony Green & Friends"),
        (SAOSIN, "Saosin"),
        (ANIMALS, "The Sound of Animals Fighting"),
        (known.as_str(), "Known Member"),
        (library.as_str(), "Library Member"),
        (down.as_str(), "Turned Down"),
        (VARIOUS, "Various Artists"),
        (neighbour.as_str(), "Neighbour"),
    ]
    .into_iter()
    .enumerate()
    {
        rows += &albums(artist, name, &[group(n)], at);
    }
    rows += &library_artist("lib-member", "Library Member", &library, "person");
    rows += &format!(
        "INSERT INTO catalog_discover_feedback (user_id, target, target_mbid_lower, kind, \
         created_at) VALUES ('user-1', 'artist', '{down}', 'not_artist', 1);"
    );
    insert(&db, rows).await;
    let script = Arc::new(Script::default());
    let deps = deps(&rig, &dir, Arc::clone(&script));

    build(&deps).await;
    assert!(
        script.band_link_reads.lock().expect("reads").is_empty(),
        "the build asked no provider"
    );
    assert_eq!(waiting(&db), (false, Vec::new()), "nothing to wait for");
    let pool = stored(&db);
    let thread = |n: usize| (group(n), "thread".to_owned());
    assert_eq!(
        pool,
        [
            (group(7), "new_artist".to_owned()),
            thread(0),
            thread(1),
            thread(2),
        ],
        "the member, then his two other bands, below the co-listening pick"
    );

    let why = |n: usize| {
        let deps = deps.clone();
        async move {
            deps.evidence
                .of("user-1", &group(n))
                .await
                .expect("the evidence reads")
                .expect("a reason")
        }
    };
    let member = why(0).await;
    assert_eq!(member.reason.kind, PickReasonKind::Fact);
    assert_eq!(member.reason.line, "Anthony Green of Circa Survive.");
    assert_eq!(member.reason.artists.len(), 1);
    assert_eq!(member.reason.artists[0].artist_mbid, CIRCA);
    assert_eq!(
        (member.found_by, member.source, member.source_read_at),
        (FoundBy::BandLink, EvidenceSource::Musicbrainz, Some(at))
    );
    assert!(member.seeds.is_empty(), "no co-listening seed to rank");
    for n in [1, 2] {
        assert_eq!(
            why(n).await.reason.line,
            "With Anthony Green of Circa Survive."
        );
    }
    let path: (String, String, String) = db.row(
        "SELECT fact_role, via_mbid, via_name FROM catalog_discover_candidate_paths \
         WHERE user_id = 'user-1' AND release_group_mbid_lower = ?1",
        [group(1)],
    );
    assert_eq!(
        path,
        (
            "shared_member".to_owned(),
            GREEN.to_owned(),
            "Anthony Green".to_owned()
        )
    );
}

/// A band of more than twelve members is a collective: its members' own
/// links are neither read nor wanted, so none of their other bands come
/// in. The members themselves do, three of them at most, the best known.
#[tokio::test]
async fn a_collective_gives_its_three_best_known_members_but_not_their_bands() {
    let dir = crate::common::ScratchDir::new("band-links-hub");
    let rig = AuthRig::new("band-links-hub").await;
    let db = rig.platform.db.clone();
    let at = rig.clock().now_ms() - DAY;
    let members: Vec<String> = (0..13).map(|n| id(5, n)).collect();
    let names: Vec<String> = (0..13).map(|n| format!("Member {n}")).collect();
    let mut rows = taste(&[(CIRCA, "Circa Survive", 2.0)], &[], at);
    rows += &neighbours(CIRCA, &[], at);
    let held: Vec<_> = members
        .iter()
        .zip(&names)
        .map(|(mbid, name)| member(mbid, name))
        .collect();
    rows += &links(CIRCA, &held, at);
    for (n, mbid) in members.iter().enumerate() {
        rows += &albums(mbid, &names[n], &[id(3, n)], at);
        rows += &albums(&id(6, n), "Their Band", &[id(7, n)], at);
        let listeners = if n >= 10 { 1_000 * n as i64 } else { 100 };
        rows += &counted(mbid, listeners, at);
    }
    insert(&db, rows).await;
    let deps = deps(&rig, &dir, Arc::new(Script::default()));

    build(&deps).await;
    let pool: BTreeSet<String> = stored(&db).into_iter().map(|(group, _)| group).collect();
    assert_eq!(
        pool,
        BTreeSet::from([id(3, 10), id(3, 11), id(3, 12)]),
        "the three best-known members"
    );
    assert_eq!(waiting(&db), (false, Vec::new()));
    let wanted: i64 = db.one(
        "SELECT COUNT(*) FROM catalog_fact_fetches WHERE kind = 'band_links' \
         AND artist_mbid != ?1",
        [CIRCA],
    );
    assert_eq!(wanted, 0, "no member's links are wanted");
}

/// Four seed bands of four members each: one seed gives at most three
/// linked artists, so the ten come from all four, the heaviest seeds'
/// first.
#[tokio::test]
async fn the_ten_linked_artists_spread_over_the_seeds() {
    let dir = crate::common::ScratchDir::new("band-links-spread");
    let rig = AuthRig::new("band-links-spread").await;
    let db = rig.platform.db.clone();
    let at = rig.clock().now_ms() - DAY;
    let seeds: Vec<(String, String, f64)> = (0..4)
        .map(|s| (id(1, s), format!("Band {s}"), 2.0 - 0.1 * s as f64))
        .collect();
    let taste_seeds: Vec<(&str, &str, f64)> = seeds
        .iter()
        .map(|(mbid, name, weight)| (mbid.as_str(), name.as_str(), *weight))
        .collect();
    let mut rows = taste(&taste_seeds, &[], at);
    for (s, (seed, _, _)) in seeds.iter().enumerate() {
        rows += &neighbours(seed, &[], at);
        let members: Vec<(String, String)> = (0..4)
            .map(|m| (id(5, s * 10 + m), format!("Member {s}-{m}")))
            .collect();
        let held: Vec<_> = members
            .iter()
            .map(|(mbid, name)| member(mbid, name))
            .collect();
        rows += &links(seed, &held, at);
        for (m, (mbid, name)) in members.iter().enumerate() {
            rows += &links(mbid, &[], at);
            rows += &albums(mbid, name, &[id(3, s * 10 + m)], at);
        }
    }
    insert(&db, rows).await;
    let deps = deps(&rig, &dir, Arc::new(Script::default()));

    build(&deps).await;
    assert_eq!(waiting(&db), (false, Vec::new()));
    let by_seed: Vec<(String, i64)> = db.rows(
        "SELECT p.artist_mbid, COUNT(*) FROM catalog_discover_candidate_paths p \
         WHERE p.user_id = 'user-1' AND p.kind = 'fact' GROUP BY p.artist_mbid \
         ORDER BY p.artist_mbid",
        [],
    );
    assert_eq!(
        by_seed,
        [
            (seeds[0].0.clone(), 3),
            (seeds[1].0.clone(), 3),
            (seeds[2].0.clone(), 3),
            (seeds[3].0.clone(), 1),
        ],
        "ten, from all four seeds"
    );
}

/// A band's members are followed in order of their listeners, the best
/// known first; one not counted yet comes after those counted.
#[tokio::test]
async fn a_bands_best_known_members_are_followed_first() {
    let dir = crate::common::ScratchDir::new("band-links-order");
    let rig = AuthRig::new("band-links-order").await;
    let db = rig.platform.db.clone();
    let at = rig.clock().now_ms() - DAY;
    let members: Vec<String> = (0..5).map(|n| id(5, n)).collect();
    let mut rows = taste(&[(CIRCA, "Circa Survive", 2.0)], &[], at);
    rows += &neighbours(CIRCA, &[], at);
    let names: Vec<String> = (0..5).map(|n| format!("Member {n}")).collect();
    let held: Vec<_> = members
        .iter()
        .zip(&names)
        .map(|(mbid, name)| member(mbid, name))
        .collect();
    rows += &links(CIRCA, &held, at);
    for (n, listeners) in [(0, 100), (2, 8_000), (3, 5_000)] {
        rows += &counted(&members[n], listeners, at);
    }
    insert(&db, rows).await;
    let deps = deps(&rig, &dir, Arc::new(Script::default()));

    build(&deps).await;
    let followed: BTreeSet<String> = db
        .column::<String>(
            "SELECT artist_mbid FROM catalog_fact_fetches WHERE kind = 'band_links' \
             AND wanted_at IS NOT NULL AND artist_mbid != ?1",
            [CIRCA],
        )
        .into_iter()
        .collect();
    assert_eq!(
        followed,
        BTreeSet::from([members[2].clone(), members[3].clone(), members[0].clone()]),
        "the three counted members, not the two not counted yet"
    );
}

/// A seed's links not kept yet are wanted and the build waits for them:
/// it is incomplete and names the seed. The refresh job reads them in the
/// background, and their arrival makes the pool due; the next build waits
/// for the member's album list it now reaches.
#[tokio::test]
async fn links_not_kept_are_wanted_and_their_arrival_makes_the_pool_due() {
    let dir = crate::common::ScratchDir::new("band-links-wait");
    let rig = AuthRig::new("band-links-wait").await;
    let db = rig.platform.db.clone();
    let at = rig.clock().now_ms() - DAY;
    let mut rows = taste(&[(CIRCA, "Circa Survive", 2.0)], &[], at);
    rows += &neighbours(CIRCA, &[], at);
    insert(&db, rows).await;
    seen_now(&rig, "user-1").await;
    let script = Arc::new(Script {
        band_links: std::collections::HashMap::from([(
            CIRCA.to_owned(),
            vec![BandLink {
                mbid: GREEN.to_owned(),
                name: "Anthony Green".to_owned(),
                kind: BandLinkKind::MemberOfBand,
                direction: LinkDirection::Backward,
                ended: false,
            }],
        )]),
        ..Script::default()
    });
    let deps = deps(&rig, &dir, Arc::clone(&script));

    build(&deps).await;
    assert_eq!(waiting(&db), (true, vec![CIRCA.to_owned()]));
    let wanted: i64 = db.one(
        "SELECT COUNT(*) FROM catalog_fact_fetches WHERE kind = 'band_links' \
         AND artist_mbid = ?1 AND wanted_at IS NOT NULL AND fetched_at IS NULL",
        [CIRCA],
    );
    assert_eq!(wanted, 1, "the seed's links are wanted");
    assert!(!pool_due(&rig).await, "not due while they are missing");

    rig.clock().advance(HOUR);
    deps.facts.run_once(&|| false).await.expect("the job runs");
    assert_eq!(
        *script.band_link_reads.lock().expect("reads"),
        [CIRCA.to_owned()],
        "the job read them"
    );
    assert!(pool_due(&rig).await, "their arrival makes the pool due");

    build(&deps).await;
    assert_eq!(
        waiting(&db),
        (true, vec![GREEN.to_owned()]),
        "the member's links and album list are wanted next"
    );
}

/// A band-link pick takes a weekly set slot, and a seed is the strongest
/// path of at most two picks whatever found them. Circa Survive's two
/// co-listening picks and its member's album, Seed Two's one pick: the
/// member's album joins the pool by its support without moving the rest,
/// and the set holds it, Circa Survive's best co-listening pick and Seed
/// Two's, never Circa Survive's third.
#[tokio::test]
async fn a_band_link_pick_takes_a_set_slot_under_the_cap_per_seed() {
    let dir = crate::common::ScratchDir::new("band-links-week");
    let rig = AuthRig::new("band-links-week").await;
    let db = rig.platform.db.clone();
    let now = rig.clock().now_ms();
    let friday = weekly::day_on_or_after(now + 1, SetDay::Friday);
    rig.clock().advance(friday + HOUR - now);
    let at = rig.clock().now_ms() - DAY;
    let (second, n1, n2, n3) = (id(1, 2), id(2, 1), id(2, 2), id(2, 3));
    let group = |n: usize| id(3, n);

    let mut rows = taste(
        &[(CIRCA, "Circa Survive", 2.0), (&second, "Seed Two", 1.0)],
        &[],
        at,
    );
    rows += &neighbours(CIRCA, &[(n1.clone(), 100.0), (n2.clone(), 30.0)], at);
    rows += &neighbours(&second, &[(n3.clone(), 100.0)], at);
    rows += &links(CIRCA, &[], at);
    rows += &links(&second, &[], at);
    rows += &links(GREEN, &[], at);
    rows += &albums(&n1, "One", &[group(1)], at);
    rows += &albums(&n2, "Two", &[group(2)], at);
    rows += &albums(&n3, "Three", &[group(3)], at);
    rows += &albums(GREEN, "Anthony Green", &[group(4)], at);
    insert(&db, rows).await;
    seen_now(&rig, "user-1").await;
    let deps = deps(&rig, &dir, Arc::new(Script::default()));

    build(&deps).await;
    let before: Vec<String> = stored(&db).into_iter().map(|(group, _)| group).collect();
    assert_eq!(before, [group(1), group(3), group(2)], "co-listening alone");

    insert(&db, links(CIRCA, &[member(GREEN, "Anthony Green")], at)).await;
    build(&deps).await;
    let after: Vec<String> = stored(&db).into_iter().map(|(group, _)| group).collect();
    assert_eq!(
        after,
        [group(1), group(3), group(4), group(2)],
        "the member's album by its support; the rest as they were"
    );
    assert_eq!(waiting(&db), (false, Vec::new()));

    deps.weekly
        .run_once("user-1", |_| Ok(()))
        .await
        .expect("the weekly set job runs");
    let set: Vec<(String, String)> = db.rows(
        "SELECT release_group_mbid_lower, reason_line FROM catalog_weekly_set_picks \
         WHERE user_id = 'user-1' ORDER BY pick_no",
        [],
    );
    let albums: BTreeSet<&str> = set.iter().map(|(group, _)| group.as_str()).collect();
    let wanted = [group(1), group(3), group(4)];
    assert_eq!(
        albums,
        wanted.iter().map(String::as_str).collect(),
        "never Circa Survive's third: {set:?}"
    );
    assert!(
        set.contains(&(group(4), "Anthony Green of Circa Survive.".to_owned())),
        "{set:?}"
    );
}

/// A member's album list kept but its counts not read yet: the build waits
/// for them, as it waits for the walk's, and the set day deals nothing from
/// a pool still waiting.
#[tokio::test]
async fn uncounted_band_link_albums_hold_the_set() {
    let dir = crate::common::ScratchDir::new("band-links-uncounted");
    let rig = AuthRig::new("band-links-uncounted").await;
    let db = rig.platform.db.clone();
    let now = rig.clock().now_ms();
    let friday = weekly::day_on_or_after(now + 1, SetDay::Friday);
    rig.clock().advance(friday + HOUR - now);
    let at = rig.clock().now_ms() - DAY;
    let mut rows = taste(&[(CIRCA, "Circa Survive", 2.0)], &[], at);
    rows += &neighbours(CIRCA, &[], at);
    rows += &links(CIRCA, &[member(GREEN, "Anthony Green")], at);
    rows += &links(GREEN, &[], at);
    rows += &albums(GREEN, "Anthony Green", &[id(3, 1)], at);
    rows += &format!(
        "DELETE FROM catalog_fact_fetches WHERE artist_mbid = '{GREEN}' \
         AND kind = 'album_counts';"
    );
    insert(&db, rows).await;
    seen_now(&rig, "user-1").await;
    let deps = deps(&rig, &dir, Arc::new(Script::default()));

    build(&deps).await;
    assert_eq!(waiting(&db), (true, vec![GREEN.to_owned()]));
    deps.weekly
        .run_once("user-1", |_| Ok(()))
        .await
        .expect("the weekly set job runs");
    let dealt: i64 = db.one(
        "SELECT COUNT(*) FROM catalog_weekly_set_picks WHERE user_id = 'user-1'",
        [],
    );
    assert_eq!(dealt, 0, "nothing is dealt while the pool waits");
}
