//! A pick's reason is the path that found it. Over a real database and the
//! production builder, a scripted person's pool is built from their taste
//! rows, the kept lists and their weekly ListenBrainz playlist; each of
//! the first twenty picks is read through its `why` route and checked
//! against the rows it rests on. A pick whose named artist left their
//! seeds has no reason and is not offered, no field of the evidence is a
//! score, "Less from this" moves the next pool and is undone, and "Not
//! this source" leaves out what only that source found until it is undone.

use std::collections::HashMap;
use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use droppedneedle::docs::ApiDoc;
use droppedneedle_catalog::discover::engine::pool_store;
use droppedneedle_catalog::discover::listening::{PlaylistTrack, UserMusic, WeeklyPlaylist};
use droppedneedle_catalog::discover::services::DiscoverDeps;
use droppedneedle_kernel::clock::Clock as _;
use droppedneedle_kernel::db::Db;
use droppedneedle_providers::Providers;
use serde_json::{Value, json};
use utoipa::OpenApi as _;

use crate::common::Probe;
use crate::common::auth::AuthRig;
use crate::discover_pool::{album, albums, counted, id, list};
use crate::discover_queue::{Script, call, discover_deps, discover_routes, insert, seen_now};

const DAY: i64 = 86_400_000;

/// The person's seeds: id tag 1, with their plays (all imported listens)
/// and weights. The fifth has no plays: it comes from the shelves alone.
const SEEDS: [(usize, i64, f64); 5] = [
    (1, 40, 3.0),
    (2, 25, 2.0),
    (3, 12, 1.5),
    (4, 6, 1.0),
    (5, 0, 0.8),
];
/// How many albums the library holds of the fifth seed.
const SHELF_ALBUMS: usize = 4;

pub(crate) fn seed(n: usize) -> String {
    id(1, n)
}

fn seed_name(n: usize) -> String {
    format!("Seed {}", &seed(n)[..8])
}

/// A neighbour artist: id tag 2.
fn artist(n: usize) -> String {
    id(2, n)
}

/// Its `k`th album: id tag 3.
fn group(n: usize, k: usize) -> String {
    id(3, n * 10 + k)
}

/// A known artist the person plays: id tag 4.
fn known(n: usize) -> String {
    id(4, n)
}

/// The playlist's artists, albums and recordings: tags 5, 6 and 7.
fn picked(n: usize) -> (String, String, String) {
    (id(5, n), id(6, n), id(7, n))
}

/// How many albums the playlist names.
const PICKED: usize = 6;

/// `count` imported listens of `artist`, a minute apart before `at`.
fn listens(artist: &str, name: &str, count: i64, at: i64) -> String {
    (0..count)
        .map(|n| {
            format!(
                "INSERT INTO media_imported_listens (user_id, listened_at, artist_mbid, \
                 artist_name, track_name, imported_at) VALUES ('user-1', {}, '{artist}', \
                 '{name}', 'Song {n}', {at});",
                at - 60_000 * (n + 1)
            )
        })
        .collect()
}

/// Ada's taste build and history: the five seeds in one group, and three
/// artists she plays that are not seeds, each with that many listens.
fn taste(at: i64) -> String {
    let mut rows = format!(
        "INSERT INTO auth_users (id, display_name, role, created_at) \
         VALUES ('user-1', 'Ada', 'user', 0), ('user-2', 'Bob', 'user', 0); \
         INSERT INTO catalog_taste_builds (user_id, built_at, plays_counted, \
         imported_listens_counted, artists_played, seeds, incomplete) \
         VALUES ('user-1', {at}, 0, 83, 8, 5, 0); \
         INSERT INTO catalog_taste_groups (user_id, group_no, anchor_artist_mbid, \
         anchor_name, share, recent_share, trend) VALUES ('user-1', 0, '{}', 'Seed', 1.0, \
         1.0, 'steady');",
        seed(1)
    );
    for (n, plays, weight) in SEEDS {
        rows += &format!(
            "INSERT INTO catalog_taste_artists (user_id, artist_mbid_lower, artist_mbid, name, \
             plays, imported_listens, months_active, weight, is_seed, group_no) \
             VALUES ('user-1', '{id}', '{id}', '{name}', 0, {plays}, 1, {weight}, 1, 0);",
            id = seed(n),
            name = seed_name(n),
        );
        rows += &listens(&seed(n), &seed_name(n), plays, at);
    }
    for n in 1..=3 {
        rows += &format!(
            "INSERT INTO catalog_taste_artists (user_id, artist_mbid_lower, artist_mbid, name, \
             plays, imported_listens, months_active, weight, is_seed) \
             VALUES ('user-1', '{id}', '{id}', 'Known {n}', 0, 2, 1, 0.1, 0);",
            id = known(n)
        );
        rows += &listens(&known(n), &format!("Known {n}"), 2, at);
    }
    rows
}

/// The library holds four albums of the fifth seed.
fn shelves() -> String {
    let mut rows = format!(
        "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, \
         updated_at) VALUES ('lib-s5', '{name}', lower('{name}'), 'group', 1, 1); \
         INSERT INTO local_artist_external_identities (local_artist_id, provider, \
         provider_artist_id, decision_source, selected_at) \
         VALUES ('lib-s5', 'musicbrainz', '{mbid}', 'manual', 1);",
        name = seed_name(5),
        mbid = seed(5)
    );
    for n in 0..SHELF_ALBUMS {
        rows += &format!(
            "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
             album_artist_id, grouping_source, created_at, updated_at) \
             VALUES ('al-s5-{n}', 'r1', 'g-s5-{n}', 'Owned', 'owned', 'lib-s5', 'automatic', \
             1, 1); \
             INSERT INTO local_album_artists (local_album_id, position, local_artist_id, role) \
             VALUES ('al-s5-{n}', 0, 'lib-s5', 'main'); \
             INSERT INTO local_album_external_identities (local_album_id, provider, \
             release_group_mbid, decision_source, selected_at) \
             VALUES ('al-s5-{n}', 'musicbrainz', '{group}', 'manual', 1); \
             INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
             path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
             album_title, album_title_folded, file_format, availability, ingest_source, \
             imported_at, membership_source) VALUES ('t-s5-{n}', 'al-s5-{n}', 'r1', \
             '/music/s5-{n}.flac', 's5-{n}.flac', 'h-s5-{n}', 1, 1, 's-s5-{n}', 'Song', 'song', \
             'Owned', 'owned', 'flac', 'indexed', 'scan', 1, 'automatic');",
            group = id(8, n)
        );
    }
    rows
}

/// Each seed's kept labs list, best first by score, and who it names.
/// Every neighbour has the same listener count but the second, which has
/// four times as many, so the fame division moves it from third to last in
/// the first seed's list. Neighbours 1 to 4 have two albums, the rest one;
/// neighbour 99 has none. The first seed's list starts with an artist she
/// plays.
fn lists(at: i64) -> (String, Vec<usize>) {
    let named: [(usize, Vec<Neighbour>); 5] = [
        (
            1,
            vec![
                Neighbour::Known(1),
                Neighbour::New(1),
                Neighbour::New(2),
                Neighbour::New(3),
                Neighbour::New(4),
                Neighbour::New(5),
                Neighbour::New(6),
                Neighbour::New(7),
            ],
        ),
        (
            2,
            vec![
                Neighbour::New(3),
                Neighbour::New(4),
                Neighbour::New(11),
                Neighbour::New(12),
            ],
        ),
        (3, vec![Neighbour::New(4), Neighbour::New(13)]),
        (4, vec![Neighbour::New(4), Neighbour::New(14)]),
        (5, vec![Neighbour::New(99), Neighbour::New(15)]),
    ];
    let mut rows = String::new();
    let mut artists = Vec::new();
    let mut counts = Vec::new();
    for (n, neighbours) in &named {
        let scored: Vec<(String, f64)> = neighbours
            .iter()
            .enumerate()
            .map(|(at, neighbour)| (neighbour.mbid(), 100.0 - 5.0 * at as f64))
            .collect();
        rows += &list(&seed(*n), &scored, at);
        for neighbour in neighbours {
            counts.push(neighbour.mbid());
            if let Neighbour::New(artist) = neighbour {
                artists.push(*artist);
            }
        }
    }
    artists.sort_unstable();
    artists.dedup();
    counts.sort_unstable();
    counts.dedup();
    for neighbour in &counts {
        let listeners = if *neighbour == artist(2) {
            40_000
        } else {
            10_000
        };
        rows += &counted(neighbour, listeners, at);
    }
    for n in &artists {
        let kept: Vec<String> = match n {
            99 => Vec::new(),
            1..=4 => vec![group(*n, 1), group(*n, 2)],
            _ => vec![group(*n, 1)],
        };
        let credited = artist(*n);
        let kept: Vec<_> = kept
            .iter()
            .map(|group| album(group, "Album", "[]", &[credited.as_str()]))
            .collect();
        rows += &albums(&artist(*n), &kept, at);
    }
    (rows, artists)
}

enum Neighbour {
    Known(usize),
    New(usize),
}

impl Neighbour {
    fn mbid(&self) -> String {
        match self {
            Self::Known(n) => known(*n),
            Self::New(n) => artist(*n),
        }
    }
}

/// Ada's weekly playlist, made `made_ms`: one track from each of six
/// albums by artists she does not know.
fn playlist(made_ms: i64) -> (WeeklyPlaylist, HashMap<String, String>) {
    use time::format_description::well_known::Rfc3339;
    let date = time::OffsetDateTime::from_unix_timestamp(made_ms / 1000)
        .expect("a date")
        .format(&Rfc3339)
        .expect("formats");
    let mut groups = HashMap::new();
    let tracks = (1..=PICKED)
        .map(|n| {
            let (artist, group, recording) = picked(n);
            groups.insert(recording.clone(), group);
            PlaylistTrack {
                title: format!("Track {n}"),
                creator: format!("Picked {n}"),
                album: format!("Picked album {n}"),
                recording_mbid: Some(recording),
                artist_mbid: Some(artist),
                caa_release_mbid: None,
                duration_ms: None,
            }
        })
        .collect();
    let weekly = WeeklyPlaylist {
        title: "Weekly Exploration".to_owned(),
        date,
        source_url: "https://listenbrainz.org/playlist/weekly".to_owned(),
        tracks,
    };
    (weekly, groups)
}

/// What the test built and against which rows it checks.
pub(crate) struct Rig {
    rig: AuthRig,
    _dir: crate::common::ScratchDir,
    deps: DiscoverDeps,
    pub(crate) app: Router,
    weekly: WeeklyPlaylist,
    recordings: HashMap<String, String>,
}

impl Rig {
    pub(crate) fn db(&self) -> &Db {
        &self.rig.platform.db
    }

    fn now(&self) -> i64 {
        self.rig.clock().now_ms()
    }

    pub(crate) async fn build(&self) {
        self.deps
            .pool
            .run_once("user-1", |_| Ok(()))
            .await
            .expect("the pool job runs");
    }

    async fn ranked(&self) -> Vec<String> {
        pool_store::ranked(self.db(), "user-1", self.now())
            .await
            .expect("the pool reads")
            .into_iter()
            .map(|candidate| candidate.release_group_mbid)
            .collect()
    }

    async fn why(&self, group: &str) -> (StatusCode, Value) {
        call(
            &self.app,
            "GET",
            &format!("/api/v3/discover/picks/{group}/why"),
            None,
        )
        .await
    }

    /// Whether Ada's pool build is queued, clearing it.
    pub(crate) fn pool_queued(&self) -> bool {
        let queued: i64 = self.db().one(
            "SELECT COUNT(*) FROM durable_jobs WHERE kind = 'discover.pool' \
             AND idempotency_key = 'user-1' AND state = 'queued'",
            [],
        );
        self.db()
            .exec("DELETE FROM durable_jobs WHERE kind = 'discover.pool'", []);
        queued > 0
    }
}

pub(crate) async fn rig(name: &str) -> Rig {
    rig_with(name, "", false).await
}

/// The rig, with `extra` rows in the database before the first build,
/// and with `lastfm` her Last.fm key working.
async fn rig_with(name: &str, extra: &str, lastfm: bool) -> Rig {
    let dir = crate::common::ScratchDir::new(name);
    let rig = AuthRig::new(name).await;
    let db = rig.platform.db.clone();
    let at = rig.clock().now_ms() - DAY;
    let (rows, _) = lists(at);
    insert(&db, taste(at) + &shelves() + &rows + extra).await;
    // Seen today, so the day's background reads have room for her playlist.
    seen_now(&rig, "user-1").await;
    let (weekly, recordings) = playlist(at);
    let script = Arc::new(Script {
        user: UserMusic {
            listenbrainz: Some("ada".to_owned()),
            lastfm,
            ..UserMusic::default()
        },
        weekly: Some(weekly.clone()),
        recording_groups: recordings.clone(),
        ..Script::default()
    });
    let network = Arc::clone(&script);
    let deps = discover_deps(&rig, &dir, Providers::closed(), move |slot| {
        *slot = network;
    });
    let app = discover_routes(deps.clone());
    let built = Rig {
        rig,
        _dir: dir,
        deps,
        app,
        weekly,
        recordings,
    };
    built.build().await;
    built
}

/// How many plays and imported listens of `artist` Ada's history holds.
fn history(db: &Db, artist: &str) -> i64 {
    db.one::<i64>(
        "SELECT COUNT(*) FROM media_imported_listens WHERE user_id = 'user-1' \
         AND lower(artist_mbid) = ?1",
        [artist],
    ) + db.one::<i64>(
        "SELECT COUNT(*) FROM library_play_history h \
         JOIN local_artist_external_identities ai ON ai.local_artist_id = h.local_artist_id \
           AND ai.provider = 'musicbrainz' \
         WHERE h.user_id = 'user-1' AND lower(ai.provider_artist_id) = ?1",
        [artist],
    )
}

/// How many live albums with files the library holds of `artist`.
fn owned_albums(db: &Db, artist: &str) -> i64 {
    db.one(
        "SELECT COUNT(DISTINCT ob.id) FROM local_artist_external_identities ai \
         JOIN local_album_artists oc ON oc.local_artist_id = ai.local_artist_id \
         JOIN local_albums ob ON ob.id = oc.local_album_id \
         WHERE ai.provider = 'musicbrainz' AND lower(ai.provider_artist_id) = ?1 \
           AND ob.retired_into_album_id IS NULL AND ob.has_indexed_track = 1",
        [artist],
    )
}

/// The fame exponent co-listening is divided by, as the one similar answer
/// reads it.
const FAME_EXPONENT: f64 = 0.5;

/// `seed`'s kept labs list in the order the app reads it: each neighbour's
/// score over its kept listener count to [`FAME_EXPONENT`], best first.
fn kept_list(db: &Db, seed: &str) -> Vec<String> {
    let mut rows: Vec<(String, f64, i64)> = db.rows(
        "SELECT n.neighbour_mbid, n.score, l.listeners FROM catalog_artist_neighbours n \
         JOIN catalog_artist_listeners l ON l.artist_mbid = n.neighbour_mbid \
         WHERE n.artist_mbid = ?1 AND n.source = 'listenbrainz_labs'",
        [seed],
    );
    let value = |row: &(String, f64, i64)| row.1 / (row.2.max(1) as f64).powf(FAME_EXPONENT);
    rows.sort_by(|a, b| value(b).total_cmp(&value(a)).then_with(|| a.0.cmp(&b.0)));
    rows.into_iter().map(|row| row.0).collect()
}

/// Ada's seeds whose kept labs list names `artist`.
fn seeds_naming(db: &Db, artist: &str) -> Vec<String> {
    db.column(
        "SELECT DISTINCT n.artist_mbid FROM catalog_artist_neighbours n \
         JOIN catalog_taste_artists t ON t.artist_mbid_lower = n.artist_mbid \
           AND t.user_id = 'user-1' AND t.is_seed = 1 \
         WHERE n.source = 'listenbrainz_labs' AND n.neighbour_mbid = ?1 \
         ORDER BY n.artist_mbid",
        [artist],
    )
}

/// Check one pick's evidence against the rows it rests on; answers its
/// line.
fn audit(rig: &Rig, group: &str, why: &Value) -> String {
    let db = rig.db();
    let line = why["reason"]["line"].as_str().expect("a line").to_owned();
    assert!(!line.contains('%'), "{line}");
    let artist: String = db.one(
        "SELECT artist_mbid FROM catalog_discover_candidates WHERE user_id = 'user-1' \
         AND release_group_mbid_lower = ?1",
        [group],
    );
    let kept_listeners = db
        .maybe::<Option<i64>>(
            "SELECT listeners FROM catalog_artist_albums WHERE release_group_mbid = ?1",
            [group],
        )
        .flatten();
    assert_eq!(
        why["listeners"].as_i64(),
        kept_listeners,
        "{group}: listeners"
    );
    assert_eq!(
        why["new_to_you"]["no_plays"],
        json!(history(db, &artist) == 0)
    );
    assert_eq!(
        why["new_to_you"]["not_in_library"],
        json!(owned_albums(db, &artist) == 0)
    );
    match why["reason"]["kind"].as_str() {
        Some("your_artists") => {
            assert_eq!(why["found_by"], "co_listening", "{group}");
            assert_eq!(why["source"], "listenbrainz", "{group}");
            let seeds = why["seeds"].as_array().expect("seeds");
            let named = why["reason"]["artists"].as_array().expect("artists");
            assert!(!seeds.is_empty() && named.len() == seeds.len().min(3));
            let mut leading: Vec<String> = seeds
                .iter()
                .map(|seed| seed["artist_mbid"].as_str().expect("an id").to_owned())
                .collect();
            leading.sort();
            assert_eq!(
                leading,
                seeds_naming(db, &artist),
                "{group}: every seed that leads to it, and no other"
            );
            for (at, seed) in seeds.iter().enumerate() {
                let mbid = seed["artist_mbid"].as_str().expect("an id");
                let plays: i64 = db.one(
                    "SELECT plays + imported_listens FROM catalog_taste_artists \
                     WHERE user_id = 'user-1' AND artist_mbid_lower = ?1 AND is_seed = 1",
                    [mbid],
                );
                assert_eq!(seed["your_plays"], json!(plays), "{group}: {mbid}");
                assert_eq!(seed["your_plays"], json!(history(db, mbid)), "{group}");
                assert_eq!(
                    seed["albums_owned"],
                    json!(owned_albums(db, mbid)),
                    "{group}"
                );
                let list = kept_list(db, mbid);
                let place = list.iter().position(|row| *row == artist).expect("listed");
                assert_eq!(seed["rank"], json!(place + 1), "{group}: rank in {mbid}");
                assert_eq!(seed["of"], json!(list.len()), "{group}: length of {mbid}");
                if let Some(named) = named.get(at) {
                    assert_eq!(named["artist_mbid"], seed["artist_mbid"]);
                    assert!(line.contains(named["name"].as_str().expect("a name")));
                }
                if line.contains("isn't in your history") {
                    assert_eq!(seeds.len(), 1, "{line}");
                    for above in &list[..place] {
                        assert!(history(db, above) > 0, "{line}: {above} is not played");
                    }
                    assert_eq!(history(db, &artist), 0, "{line}");
                }
            }
            if let Some(owned) = line.split("You own ").nth(1) {
                let count: i64 = owned
                    .split(' ')
                    .next()
                    .and_then(|n| n.parse().ok())
                    .expect("a count");
                let named = &seeds[..named.len()];
                assert!(named.iter().all(|seed| seed["your_plays"] == 0), "{line}");
                let total: i64 = named
                    .iter()
                    .map(|seed| owned_albums(db, seed["artist_mbid"].as_str().expect("id")))
                    .sum();
                assert_eq!(count, total, "{line}");
            }
        }
        Some("source") => {
            assert_eq!(why["found_by"], "source_pick", "{group}");
            assert_eq!(why["source"], "listenbrainz", "{group}");
            assert!(why["seeds"].as_array().is_some_and(Vec::is_empty));
            let listed = rig.weekly.tracks.iter().any(|track| {
                track
                    .recording_mbid
                    .as_ref()
                    .and_then(|recording| rig.recordings.get(recording))
                    .is_some_and(|listed| listed == group)
            });
            assert!(listed, "{group} is not in the playlist");
            let made = time::OffsetDateTime::parse(
                &rig.weekly.date,
                &time::format_description::well_known::Rfc3339,
            )
            .expect("the playlist's date");
            assert_eq!(
                why["source_read_at"],
                json!(made.unix_timestamp() * 1000),
                "{group}: the playlist's date"
            );
        }
        other => panic!("{group}: no reason kind {other:?}"),
    }
    line
}

/// Twenty picks, twenty true reasons: every line names the seeds the pick
/// was found through, with their plays, owned albums and the pick's place
/// in each one's kept list; "isn't in your history" holds in the history,
/// "You own" in the library, a service's pick is in its playlist of that
/// date, and the listener count is the kept row's. Each form of the line
/// is among them.
#[tokio::test]
async fn audit_twenty_picks_twenty_true_reasons() {
    let rig = rig("discover-reasons-audit").await;
    let ranked = rig.ranked().await;
    assert_eq!(ranked.len(), 22, "{ranked:?}");
    let mut lines = HashMap::new();
    for group in &ranked[..20] {
        let (status, why) = rig.why(group).await;
        assert_eq!(status, StatusCode::OK, "{group}: {why}");
        assert_eq!(why["release_group_mbid"], json!(group));
        lines.insert(group.clone(), audit(&rig, group, &why));
    }
    let line = |group: String| lines.get(&group).cloned().unwrap_or_default();
    assert_eq!(
        line(group(1, 1)),
        format!(
            "Closest artist to {} that isn't in your history.",
            seed_name(1)
        )
    );
    assert_eq!(line(group(2, 1)), format!("Close to {}.", seed_name(1)));
    assert_eq!(
        line(group(3, 1)),
        format!("Close to both {} and {}.", seed_name(1), seed_name(2))
    );
    assert_eq!(
        line(group(4, 1)),
        format!(
            "Near {}, {} and {}.",
            seed_name(1),
            seed_name(2),
            seed_name(3)
        )
    );
    assert_eq!(
        line(group(15, 1)),
        format!("Close to {}. You own 4 of their albums.", seed_name(5))
    );
    let sourced = lines
        .values()
        .filter(|line| *line == "From ListenBrainz's weekly picks for you.")
        .count();
    assert_eq!(sourced, 4, "the four service picks in the first twenty");
}

/// With the second seed gone from Ada's taste rows (as after a rebuild),
/// an album its line would name has no reason, is left out of the pool as
/// it is read and has no evidence; one named by the first seed alone keeps
/// its own.
#[tokio::test]
async fn audit_a_pick_whose_seed_left_has_no_reason() {
    let rig = rig("discover-reasons-gone").await;
    let both = group(3, 1);
    assert!(rig.ranked().await.contains(&both));
    rig.db().exec(
        "DELETE FROM catalog_taste_artists WHERE user_id = 'user-1' AND artist_mbid_lower = ?1",
        [seed(2)],
    );
    let ranked = rig.ranked().await;
    assert!(!ranked.contains(&both), "{ranked:?}");
    assert!(
        !ranked.contains(&group(4, 1)),
        "named through the second seed too"
    );
    assert!(ranked.contains(&group(2, 1)));
    let (status, _) = rig.why(&both).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = rig.why(&group(2, 1)).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = rig.why(&id(9, 1)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "not in her pool");
    let (status, _) = rig.why("not-an-mbid").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// A library artist `local_id` with the MusicBrainz id `mbid` and no album
/// of their own: a guest on the first song of the fifth seed's first album,
/// at credit `position`.
fn guest(local_id: &str, mbid: &str, position: usize) -> String {
    format!(
        "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, \
         updated_at) VALUES ('{local_id}', 'Guest', 'guest', 'person', 1, 1); \
         INSERT INTO local_artist_external_identities (local_artist_id, provider, \
         provider_artist_id, decision_source, selected_at) \
         VALUES ('{local_id}', 'musicbrainz', '{mbid}', 'manual', 1); \
         INSERT INTO local_track_artists (local_track_id, position, local_artist_id, role) \
         VALUES ('t-s5-0', {position}, '{local_id}', 'featured');"
    )
}

/// An artist the library has with a MusicBrainz id but holds no album of,
/// only a guest credit on someone else's song, is known: nothing filed
/// under them is a candidate, whether the walk found it or the weekly
/// playlist did, while both still find other artists' albums.
#[tokio::test]
async fn a_library_artist_without_an_album_is_never_a_candidate() {
    let (walked, (listed, ..)) = (artist(1), picked(1));
    let guests = guest("lib-walked", &walked, 1) + &guest("lib-listed", &listed, 2);
    let rig = rig_with("discover-reasons-library-artist", &guests, false).await;
    let filed: Vec<(String, String)> = rig.db().rows(
        "SELECT artist_mbid, kind FROM catalog_discover_candidates WHERE user_id = 'user-1'",
        [],
    );
    for (artist, _) in &filed {
        assert!(*artist != walked && *artist != listed, "{artist} is filed");
    }
    for kind in ["new_artist", "source_pick"] {
        assert!(
            filed.iter().any(|(_, filed)| filed == kind),
            "the {kind} path still finds albums"
        );
    }
}

/// No field of a reason or of the evidence behind it, at any depth, is a
/// score, a share or a percentage.
#[test]
fn audit_no_reason_field_is_a_score() {
    let doc = serde_json::to_value(ApiDoc::openapi()).expect("document serializes");
    let schemas = &doc["components"]["schemas"];
    let mut stack = vec!["PickEvidence".to_owned()];
    let mut seen = Vec::new();
    while let Some(name) = stack.pop() {
        if seen.contains(&name) {
            continue;
        }
        let schema = &schemas[&name];
        assert!(schema.is_object(), "{name} is in the document");
        for (field, spec) in schema["properties"].as_object().into_iter().flatten() {
            for word in [
                "pct", "percent", "score", "match", "support", "strength", "weight", "share",
            ] {
                assert!(!field.contains(word), "{name}.{field}");
            }
            let text = spec.to_string();
            for reference in text.split("#/components/schemas/").skip(1) {
                stack.push(reference.split('"').next().unwrap_or_default().to_owned());
            }
        }
        seen.push(name);
    }
    for name in ["PickReason", "EvidenceSeed", "NewToYou", "PickReasonArtist"] {
        assert!(seen.iter().any(|seen| seen == name), "{name} was walked");
    }
}

/// "Less from this" halves the first seed's weight in the next pool: an
/// album found through it alone falls below one found through the second
/// seed. It is listed under the name her taste rows give the artist, the
/// evidence marks the seed, its pool build is queued, and undoing it puts
/// the order back. An artist that is not one of hers is refused, and so
/// are the kinds later work gives meaning to.
#[tokio::test]
async fn audit_less_from_this_moves_the_pool_and_undoes() {
    let rig = rig("discover-reasons-less").await;
    let before = rig.ranked().await;
    let place = |order: &[String], group: String| order.iter().position(|row| *row == group);
    let (first_only, second_only) = (group(1, 1), group(11, 1));
    assert!(place(&before, first_only.clone()) < place(&before, second_only.clone()));
    rig.pool_queued();

    let tunings = "/api/v3/discover/tunings";
    let (status, body) = call(
        &rig.app,
        "POST",
        tunings,
        Some(json!({"kind": "less_from_seed", "target": seed(1)})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(rig.pool_queued(), "the write queued her pool");
    rig.build().await;
    let after = rig.ranked().await;
    assert!(
        place(&after, first_only.clone()) > place(&after, second_only.clone()),
        "{after:?}"
    );
    let (_, listed) = call(&rig.app, "GET", tunings, None).await;
    assert_eq!(
        listed["items"],
        json!([{"kind": "less_from_seed", "target": seed(1), "name": seed_name(1),
            "group_anchor_mbid": null, "group_label": null, "created_at": rig.now() / 1000}]),
        "{listed}"
    );
    let (_, why) = rig.why(&group(4, 1)).await;
    let marks: Vec<(Value, Value)> = why["seeds"]
        .as_array()
        .expect("seeds")
        .iter()
        .map(|seed| (seed["artist_mbid"].clone(), seed["turned_down"].clone()))
        .collect();
    assert!(marks.contains(&(json!(seed(1)), json!(true))), "{why}");
    assert!(marks.contains(&(json!(seed(2)), json!(false))), "{why}");

    let (status, body) = call(
        &rig.app,
        "POST",
        tunings,
        Some(json!({"kind": "less_from_seed", "target": artist(1)})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "not one of her artists: {body}"
    );
    for (kind, target) in [
        ("less_of_kind", seed(1)),
        ("not_this_source", seed(1)),
        ("not_this_source", "listenbrainz_labs".to_owned()),
        ("not_this_source", "library".to_owned()),
    ] {
        let (status, body) = call(
            &rig.app,
            "POST",
            tunings,
            Some(json!({"kind": kind, "target": target})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{kind}: {body}");
        let (status, _) = call(
            &rig.app,
            "DELETE",
            &format!("{tunings}/{kind}/{target}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{kind}");
    }
    assert!(!rig.pool_queued(), "a refused kind queues nothing");

    let (status, _) = call(
        &rig.app,
        "DELETE",
        &format!("{tunings}/less_from_seed/{}", seed(1)),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(rig.pool_queued(), "the undo queued her pool");
    let (_, listed) = call(&rig.app, "GET", tunings, None).await;
    assert_eq!(listed["items"], json!([]));
    rig.build().await;
    assert_eq!(rig.ranked().await, before, "the order is back");
}

/// "Not this source" for Last.fm leaves out an album only Last.fm's list
/// found and keeps one that labs' list also names; it is listed under
/// Last.fm's name, its pool build is queued, and undoing it brings the
/// album back. A group cannot be named for it.
#[tokio::test]
async fn audit_not_this_source_leaves_out_what_only_it_found() {
    let rig = rig_with("discover-reasons-source", "", true).await;
    // Last.fm's list for her first seed: a neighbour labs also names, and
    // one only Last.fm names.
    let at = rig.now() - DAY;
    let lastfm_only = artist(21);
    let mut rows = String::new();
    for (rank, neighbour) in [artist(3), lastfm_only.clone()].iter().enumerate() {
        rows += &format!(
            "INSERT INTO catalog_artist_neighbours (artist_mbid, source, rank,              neighbour_mbid, neighbour_name, score) VALUES ('{seed}', 'lastfm', {rank},              '{neighbour}', 'Artist {short}', {score});",
            seed = seed(1),
            short = &neighbour[..8],
            score = 1.0 - rank as f64 * 0.1,
        );
    }
    rows += &counted(&lastfm_only, 10_000, at);
    let credited = lastfm_only.clone();
    rows += &albums(
        &lastfm_only,
        &[album(&group(21, 1), "Album", "[]", &[credited.as_str()])],
        at,
    );
    insert(rig.db(), rows).await;
    rig.build().await;
    let before = rig.ranked().await;
    let (only_lastfm, both) = (group(21, 1), group(3, 1));
    assert!(before.contains(&only_lastfm), "{before:?}");
    assert!(before.contains(&both), "{before:?}");
    rig.pool_queued();

    let tunings = "/api/v3/discover/tunings";
    let (status, body) = call(
        &rig.app,
        "POST",
        tunings,
        Some(json!({"kind": "not_this_source", "group_no": 1})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "a group: {body}");
    let (status, body) = call(
        &rig.app,
        "POST",
        tunings,
        Some(json!({"kind": "not_this_source", "target": "lastfm"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(rig.pool_queued(), "the write queued her pool");
    let (_, listed) = call(&rig.app, "GET", tunings, None).await;
    assert_eq!(
        listed["items"],
        json!([{"kind": "not_this_source", "target": "lastfm", "name": "Last.fm",
            "group_anchor_mbid": null, "group_label": null, "created_at": rig.now() / 1000}]),
        "{listed}"
    );
    rig.build().await;
    let after = rig.ranked().await;
    assert!(!after.contains(&only_lastfm), "{after:?}");
    assert!(after.contains(&both), "labs names it too: {after:?}");

    let (status, _) = call(
        &rig.app,
        "DELETE",
        &format!("{tunings}/not_this_source/lastfm"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(rig.pool_queued(), "the undo queued her pool");
    rig.build().await;
    assert!(rig.ranked().await.contains(&only_lastfm), "it is back");
}
