//! Identification end to end against MusicBrainz release documents: the
//! live recall (lookup, search, tracklist fetch, redirects) over a scripted
//! transport, scored by the matching engine, landed by the service in the
//! SQLite store on a private copy of the migrated database.
//!
//! The documents in `tests/fixtures/musicbrainz` follow the WS/2 JSON
//! shapes (a release lookup with artist-credits, labels, recordings and
//! release-groups; a release search page). They are written by hand,
//! since this build machine cannot reach musicbrainz.org, and their ids
//! are made up. The album is two editions of one release group: a 2019
//! CD and a 2024 digital remaster whose second track runs 45 s longer.
//! The artist lookups replay answers recorded from the BrainzMash mirror.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use droppedneedle_library::identify;
use droppedneedle_library::matching::decide::REVIEW_CEILING;
use droppedneedle_library::matching::{CreditedArtist, Release, ReleaseTrack};
use droppedneedle_providers::executor::{Prepared, Wire, WireAnswer, WireError};
use droppedneedle_providers::musicbrainz::{self, MbSource};
use droppedneedle_providers::{Providers, RequestPriority, TransportKind};
use futures_util::future::BoxFuture;
use identify::models::EvidenceClass;
use identify::models::{
    DecisionSource, IdentificationOutcome, IdentifyKind, LocalAlbumFacts, LocalTrackFacts,
};
use identify::providers::LiveProviders;
use identify::service::{IdentifyDeps, IdentifyService};
use identify::sources::NoFingerprints;
use identify::sqlite::SqliteIdentifyStore;
use rusqlite::params;

use crate::common::{ScratchDir, migrated_db};

const STANDARD: &str = "c0ffee00-0000-4000-8000-00000000c001";
const REMASTER: &str = "c0ffee00-0000-4000-8000-00000000c002";
/// A release id MusicBrainz merged into the standard edition.
const MERGED: &str = "c0ffee00-0000-4000-8000-00000000c0ff";
const GROUP: &str = "c0ffee00-0000-4000-8000-00000000b001";

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("tests/fixtures/musicbrainz/{name}")).expect("fixture reads")
}

/// One scripted answer.
#[derive(Clone)]
struct RawResponse(WireAnswer);

impl RawResponse {
    fn new(status: u16, headers: Vec<(&str, &str)>, body: Vec<u8>) -> Self {
        Self(WireAnswer {
            status,
            headers: headers
                .into_iter()
                .map(|(name, value)| (name.to_owned(), value.to_owned()))
                .collect(),
            body,
        })
    }
}

/// Scripted MusicBrainz: routes by path, 404 for anything unscripted,
/// every request recorded. No live network.
struct FakeMb {
    routes: HashMap<String, RawResponse>,
    seen: Arc<Mutex<Vec<String>>>,
    down: bool,
}

impl Wire for FakeMb {
    fn send<'a>(&'a self, prepared: Prepared<'a>) -> BoxFuture<'a, Result<WireAnswer, WireError>> {
        Box::pin(async move {
            let path = prepared
                .url
                .path()
                .split("/ws/2")
                .nth(1)
                .unwrap_or_default()
                .to_owned();
            self.seen.lock().expect("seen").push(path.clone());
            if self.down {
                return Err(WireError::Transport(TransportKind::Connect));
            }
            Ok(self
                .routes
                .get(&path)
                .cloned()
                .unwrap_or_else(|| RawResponse::new(404, Vec::new(), b"{}".to_vec()))
                .0)
        })
    }
}

/// Every route the fixtures serve.
fn routes() -> HashMap<String, RawResponse> {
    let ok = |name: &str| RawResponse::new(200, Vec::new(), fixture(name));
    HashMap::from([
        ("/release".to_owned(), ok("release_search.json")),
        (format!("/release/{STANDARD}"), ok("release_standard.json")),
        (format!("/release/{REMASTER}"), ok("release_remaster.json")),
        (
            format!("/release/{MERGED}"),
            RawResponse::new(
                301,
                vec![(
                    "location",
                    "https://musicbrainz.org/ws/2/release/c0ffee00-0000-4000-8000-00000000c001",
                )],
                Vec::new(),
            ),
        ),
    ])
}

struct Rig {
    service: IdentifyService,
    store: Arc<SqliteIdentifyStore>,
    /// A second connection, for seeding the album a scan would have found.
    db: rusqlite::Connection,
    seen: Arc<Mutex<Vec<String>>>,
    _dir: ScratchDir,
}

fn rig(down: bool) -> Rig {
    rig_with(routes(), down)
}

fn rig_with(routes: HashMap<String, RawResponse>, down: bool) -> Rig {
    rig_on(routes, down, Source::Official)
}

/// Which MusicBrainz the client is set to.
#[derive(Clone, Copy)]
enum Source {
    Official,
    BrainzMash,
    /// BrainzMash chosen, but its binding is not valid: nothing is sent.
    Unbound,
}

fn rig_on(routes: HashMap<String, RawResponse>, down: bool, source: Source) -> Rig {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let transport = FakeMb {
        routes,
        seen: seen.clone(),
        down,
    };
    let providers = Providers::unpaced(Arc::new(transport));
    let brainzmash = |binding_valid| MbSource::BrainzMash {
        base_url: musicbrainz::BRAINZMASH_API_BASE.to_owned(),
        binding_valid,
    };
    let source = match source {
        Source::Official => MbSource::official(),
        Source::BrainzMash => brainzmash(true),
        Source::Unbound => brainzmash(false),
    };
    let musicbrainz = providers
        .musicbrainz
        .fixed(source, RequestPriority::BackgroundSync);
    let dir = ScratchDir::new("lib-matching");
    let path = dir.join("app.db");
    migrated_db(&path);
    let db = droppedneedle_kernel::db::open_connection(&path).expect("side connection opens");
    let store = Arc::new(SqliteIdentifyStore::new(
        droppedneedle_kernel::db::Db::open_for_tests(&path).expect("store opens"),
        std::sync::Arc::new(droppedneedle_kernel::clock::SystemClock),
    ));
    let service = IdentifyService::new(IdentifyDeps {
        store: store.clone(),
        providers: Arc::new(LiveProviders::new(
            droppedneedle_library::identify::sources::Prioritised::new(
                musicbrainz.clone(),
                musicbrainz,
            ),
            store.clone(),
            NoFingerprints,
        )),
    });
    Rig {
        service,
        store,
        db,
        seen,
        _dir: dir,
    }
}

/// The album and its indexed files as a scan leaves them: the store reads
/// the album's facts back from these rows.
fn seed_album(rig: &Rig, facts: &LocalAlbumFacts) {
    let album = facts.local_album_id.as_str();
    rig.db
        .execute(
            "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, \
             updated_at) VALUES ('artist-1', ?1, lower(?1), 'group', 0, 0)",
            params![facts.album_artist_name],
        )
        .expect("artist seeds");
    rig.db
        .execute(
            "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
             album_artist_name, album_artist_id, grouping_source, created_at, updated_at) \
             VALUES (?1, 'root-1', ?1, ?2, lower(?2), ?3, 'artist-1', 'automatic', 0, 0)",
            params![album, facts.title, facts.album_artist_name],
        )
        .expect("album seeds");
    for track in &facts.tracks {
        rig.db
            .execute(
                "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, \
                 relative_path, path_hash, file_size_bytes, file_mtime_ns, stat_revision, \
                 tag_revision, title, title_folded, artist_name, album_title, \
                 album_title_folded, disc_number, track_number, duration_seconds, \
                 embedded_recording_mbid, embedded_release_track_mbid, embedded_release_mbid, \
                 file_format, ingest_source, imported_at, membership_source) \
                 VALUES (?1, ?2, 'root-1', '/music/' || ?1 || '.flac', ?1 || '.flac', ?1, 1, 1, \
                 'stat-1', 'tag-1', ?3, lower(?3), ?4, ?5, lower(?5), ?6, ?7, ?8, ?9, ?10, ?11, \
                 'flac', 'scan', 0, 'automatic')",
                params![
                    track.local_track_id,
                    album,
                    track.title,
                    track.artist_name,
                    facts.title,
                    track.disc_number,
                    track.track_number,
                    track.duration_secs.map(|seconds| seconds as f64),
                    track.recording_mbid,
                    track.release_track_mbid,
                    track.release_mbid,
                ],
            )
            .expect("track seeds");
    }
}

/// One local file: title, length in seconds, and its embedded ids
/// (recording, release track, release).
type FileTags<'a> = (&'a str, u64, Option<(&'a str, &'a str, &'a str)>);

fn album(files: &[FileTags<'_>]) -> LocalAlbumFacts {
    LocalAlbumFacts {
        local_album_id: "album-1".to_owned(),
        title: "Night Shift".to_owned(),
        album_artist_name: "The Lanterns".to_owned(),
        tracks: files
            .iter()
            .enumerate()
            .map(|(index, (title, seconds, ids))| LocalTrackFacts {
                local_track_id: format!("t{}", index + 1),
                title: (*title).to_owned(),
                artist_name: "The Lanterns".to_owned(),
                track_number: index as u32 + 1,
                disc_number: 1,
                duration_secs: Some(*seconds),
                recording_mbid: ids.map(|ids| ids.0.to_owned()),
                release_track_mbid: ids.map(|ids| ids.1.to_owned()),
                release_mbid: ids.map(|ids| ids.2.to_owned()),
                ..LocalTrackFacts::default()
            })
            .collect(),
        ..LocalAlbumFacts::default()
    }
}

async fn identify(rig: &Rig, facts: LocalAlbumFacts) -> identify::service::AttemptReport {
    seed_album(rig, &facts);
    run(rig).await
}

/// Queue and run one attempt for the seeded album.
async fn run(rig: &Rig) -> identify::service::AttemptReport {
    rig.service
        .enqueue_album(
            "job-1",
            "album-1",
            IdentifyKind::Automatic,
            "rev-1",
            None,
            0,
            droppedneedle_kernel::db::Lane::Background,
        )
        .unwrap();
    rig.store.claim(0, 60_000).unwrap();
    rig.service
        .run_claimed_job("job-1", 0)
        .await
        .unwrap()
        .expect("attempt report")
}

/// Files tagged by an older tagger carry a release id MusicBrainz has
/// since merged away. The lookup follows the redirect, the old id still
/// counts as the files' release, and the album seals without a search.
#[tokio::test]
async fn tagged_album_identifies_through_a_merged_release_id() {
    let rig = rig(false);
    let id = |n: u32| format!("c0ffee00-0000-4000-8000-00000000{n:04x}");
    let (recordings, tracks): (Vec<String>, Vec<String>) = (0xd001..=0xd004)
        .map(|n| (id(n), id(n - 0xd001 + 0xe001)))
        .unzip();
    let titles = [
        ("Lamplight", 214),
        ("Blue Hour", 251),
        ("Night Shift", 198),
        ("Last Train Home", 305),
    ];
    let files: Vec<FileTags<'_>> = titles
        .iter()
        .enumerate()
        .map(|(index, (title, seconds))| {
            (
                *title,
                *seconds,
                Some((recordings[index].as_str(), tracks[index].as_str(), MERGED)),
            )
        })
        .collect();

    let report = identify(&rig, album(&files)).await;

    assert_eq!(report.outcome, IdentificationOutcome::Identified);
    let identity = rig
        .store
        .album_identity("album-1")
        .unwrap()
        .expect("identity");
    assert_eq!(identity.release_mbid.as_deref(), Some(STANDARD));
    assert_eq!(identity.release_group_mbid.as_deref(), Some(GROUP));
    let track = rig
        .store
        .track_identity("t2")
        .unwrap()
        .expect("track identity");
    assert_eq!(
        track.release_track_mbid.as_deref(),
        Some(tracks[1].as_str())
    );
    // The redirect and the release: no search was needed.
    let seen = rig.seen.lock().expect("seen").clone();
    assert_eq!(
        seen,
        vec![format!("/release/{MERGED}"), format!("/release/{STANDARD}")]
    );
    // The release document stays on file for tagging.
    let stored = rig.store.release(STANDARD, None).unwrap().expect("stored");
    assert_eq!(stored.old_ids, vec![MERGED.to_owned()]);
    assert_eq!(stored.labels, vec!["Harbour Lights Records".to_owned()]);
}

/// Untagged files, one a minute shorter than any edition's track (left
/// unmatched) and one with a placeholder title: not close enough to
/// confirm, so the best guess is kept as the album's identity (flagged
/// unconfirmed) and nothing waits in a review.
#[tokio::test]
async fn messy_album_keeps_a_best_guess_without_a_review() {
    let rig = rig(false);
    let files: [FileTags<'_>; 4] = [
        ("Lamplight", 214, None),
        ("Blu Hours", 190, None),
        ("Night Shift", 198, None),
        ("Untitled", 290, None),
    ];

    let report = identify(&rig, album(&files)).await;

    assert_eq!(report.outcome, IdentificationOutcome::Ambiguous);
    assert_eq!(report.reason_code, "WEAK_MATCH");
    assert_eq!(report.review_id, None);
    let identity = rig
        .store
        .album_identity("album-1")
        .unwrap()
        .expect("best guess");
    assert_eq!(identity.release_group_mbid.as_deref(), Some(GROUP));
    assert_eq!(identity.decision_source, DecisionSource::Automatic);
    // The unconfirmed flag carries the closest candidates with their
    // distances: close enough to show a person, not close enough to trust.
    let flag = rig
        .store
        .match_flag("album-1")
        .unwrap()
        .expect("unconfirmed flag");
    assert_eq!(flag.state, identify::models::MatchFlagState::Unconfirmed);
    assert_eq!(flag.candidates.len(), 2);
    for candidate in &flag.candidates {
        assert_eq!(candidate.release_group_mbid, GROUP);
        assert!(candidate.distance > 0.0 && candidate.distance <= REVIEW_CEILING);
        assert!(!candidate.penalties.is_empty());
        let tracks = &candidate.track_evidence;
        assert_eq!(tracks[0].classification, EvidenceClass::Supported);
        // Neither the unmatched file nor the bad pair is ever placed.
        assert_eq!(tracks[1].classification, EvidenceClass::Unknown);
        assert_eq!(tracks[1].release_track_mbid, None);
        assert_eq!(tracks[3].classification, EvidenceClass::Unknown);
        assert!(tracks[3].release_track_mbid.is_some());
    }
    // "Pick another edition" finds the documents still on file.
    assert!(rig.store.release(STANDARD, None).unwrap().is_some());
    assert!(rig.store.release(REMASTER, None).unwrap().is_some());
}

/// Two editions with the same titles: the one whose track lengths match
/// the files wins, even though search ranks the other first.
#[tokio::test]
async fn edition_with_the_closest_tracklist_wins() {
    let rig = rig(false);
    let files: [FileTags<'_>; 4] = [
        ("Lamplight", 214, None),
        ("Blue Hour", 296, None),
        ("Night Shift", 198, None),
        ("Last Train Home", 305, None),
    ];

    let report = identify(&rig, album(&files)).await;

    assert_eq!(report.outcome, IdentificationOutcome::Identified);
    let identity = rig
        .store
        .album_identity("album-1")
        .unwrap()
        .expect("identity");
    assert_eq!(identity.release_mbid.as_deref(), Some(REMASTER));
    let extended = rig
        .store
        .track_identity("t2")
        .unwrap()
        .expect("track identity");
    assert_eq!(
        extended.recording_mbid.as_deref(),
        Some("c0ffee00-0000-4000-8000-00000000d005")
    );
}

/// An outage is never "no candidate": the job defers and retries.
#[tokio::test]
async fn musicbrainz_outage_defers_the_job() {
    let rig = rig(true);
    let report = identify(&rig, album(&[("Lamplight", 214, None)])).await;
    assert_eq!(report.outcome, IdentificationOutcome::ProviderDeferred);
    assert!(rig.store.album_identity("album-1").unwrap().is_none());
}

/// A release document MusicBrainz answers in a form that does not read:
/// the recorded one without its release group.
fn unreadable(name: &str) -> RawResponse {
    let mut document: serde_json::Value =
        serde_json::from_slice(&fixture(name)).expect("fixture decodes");
    document
        .as_object_mut()
        .expect("release object")
        .remove("release-group");
    RawResponse::new(
        200,
        Vec::new(),
        serde_json::to_vec(&document).expect("encodes"),
    )
}

/// Untagged files that fit the remaster, as in
/// `edition_with_the_closest_tracklist_wins`.
fn remaster_files() -> LocalAlbumFacts {
    album(&[
        ("Lamplight", 214, None),
        ("Blue Hour", 296, None),
        ("Night Shift", 198, None),
        ("Last Train Home", 305, None),
    ])
}

/// How often release `id` was asked for.
fn asked_release(rig: &Rig, id: &str) -> usize {
    let path = format!("/release/{id}");
    rig.seen
        .lock()
        .expect("seen")
        .iter()
        .filter(|seen| **seen == path)
        .count()
}

/// One search hit MusicBrainz will not give in a readable form is
/// dropped, asked for once, and the rest identify the album. An album
/// already on that release keeps it, and the attempt lands in Attention.
#[tokio::test]
async fn an_unreadable_search_hit_does_not_hold_up_the_album() {
    let routes = || {
        with(
            routes(),
            vec![(
                format!("/release/{STANDARD}"),
                unreadable("release_standard.json"),
            )],
        )
    };
    let rig = rig_with(routes(), false);

    let report = identify(&rig, remaster_files()).await;

    assert_eq!(report.outcome, IdentificationOutcome::Identified);
    let identity = rig
        .store
        .album_identity("album-1")
        .unwrap()
        .expect("identity");
    assert_eq!(identity.release_mbid.as_deref(), Some(REMASTER));
    assert_eq!(asked_release(&rig, STANDARD), 1);

    let rig = rig_with(routes(), false);
    let facts = remaster_files();
    seed_album(&rig, &facts);
    rig.db
        .execute(
            "INSERT INTO local_album_external_identities (local_album_id, provider, \
             release_group_mbid, release_mbid, decision_source, selected_at) \
             VALUES ('album-1', 'musicbrainz', ?1, ?2, 'automatic', 1)",
            params![GROUP, STANDARD],
        )
        .expect("identity seeds");

    let report = run(&rig).await;

    assert_eq!(report.outcome, IdentificationOutcome::Failed);
    assert_eq!(report.reason_code, "HELD_RELEASE_UNREADABLE");
    assert_eq!(report.job.state, identify::models::JobState::Attention);
    assert_eq!(
        report.job.failure_code.as_deref(),
        Some("HELD_RELEASE_UNREADABLE")
    );
    let identity = rig
        .store
        .album_identity("album-1")
        .unwrap()
        .expect("identity");
    assert_eq!(identity.release_mbid.as_deref(), Some(STANDARD));
    assert_eq!(identity.decision_source, DecisionSource::Automatic);
}

/// When MusicBrainz refuses every candidate it was asked for, the job
/// waits as for an outage, under its own code: nothing is matched or
/// marked unmatched.
#[tokio::test]
async fn every_candidate_unreadable_defers_the_job() {
    let rig = rig_with(
        with(
            routes(),
            vec![
                (
                    format!("/release/{STANDARD}"),
                    unreadable("release_standard.json"),
                ),
                (
                    format!("/release/{REMASTER}"),
                    unreadable("release_remaster.json"),
                ),
            ],
        ),
        false,
    );

    let report = identify(&rig, remaster_files()).await;

    assert_eq!(report.outcome, IdentificationOutcome::ProviderDeferred);
    assert_eq!(report.job.state, identify::models::JobState::Deferred);
    assert_eq!(
        report.job.failure_code.as_deref(),
        Some("releases_unreadable")
    );
    assert!(rig.store.album_identity("album-1").unwrap().is_none());
    assert!(rig.store.match_flag("album-1").unwrap().is_none());
}

/// Search finds the release the files are, which MusicBrainz will not
/// give in a readable form, and one unrelated release that fits nothing.
/// Matching nothing is not evidence while the right release is unread:
/// the job waits as for an outage and the album is not marked unmatched.
#[tokio::test]
async fn an_unreadable_hit_beside_an_unrelated_one_defers_the_job() {
    const UNRELATED: &str = "c0ffee00-0000-4000-8000-00000000c009";
    let json = |name: &str| -> serde_json::Value {
        serde_json::from_slice(&fixture(name)).expect("fixture decodes")
    };
    let mut search = json("release_search.json");
    let hits = search["releases"].as_array_mut().expect("hits");
    hits.retain(|hit| hit["id"] == STANDARD || hit["id"] == UNRELATED);
    for hit in hits.iter_mut().filter(|hit| hit["id"] == UNRELATED) {
        hit["title"] = "Harbour Songs".into();
        hit["track-count"] = 4.into();
        hit["media"][0]["track-count"] = 4.into();
    }
    let mut unrelated = json("release_standard.json");
    unrelated["id"] = UNRELATED.into();
    unrelated["title"] = "Harbour Songs".into();
    unrelated["release-group"]["id"] = "c0ffee00-0000-4000-8000-00000000b009".into();
    unrelated["release-group"]["title"] = "Harbour Songs".into();
    for (index, track) in unrelated["media"][0]["tracks"]
        .as_array_mut()
        .expect("tracks")
        .iter_mut()
        .enumerate()
    {
        let title = format!("Harbour Song {}", index + 1);
        track["title"] = title.clone().into();
        track["recording"]["title"] = title.into();
        track["length"] = 600_000.into();
        track["recording"]["length"] = 600_000.into();
    }
    let ok = |document: serde_json::Value| {
        RawResponse::new(200, Vec::new(), document.to_string().into_bytes())
    };
    let rig = rig_with(
        with(
            routes(),
            vec![
                ("/release".to_owned(), ok(search)),
                (
                    format!("/release/{STANDARD}"),
                    unreadable("release_standard.json"),
                ),
                (format!("/release/{UNRELATED}"), ok(unrelated)),
            ],
        ),
        false,
    );
    let files: [FileTags<'_>; 4] = [
        ("Lamplight", 214, None),
        ("Blue Hour", 251, None),
        ("Night Shift", 198, None),
        ("Last Train Home", 305, None),
    ];

    let report = identify(&rig, album(&files)).await;

    assert_eq!(asked_release(&rig, UNRELATED), 1);
    assert_eq!(report.outcome, IdentificationOutcome::ProviderDeferred);
    assert_eq!(report.job.state, identify::models::JobState::Deferred);
    assert_eq!(
        report.job.failure_code.as_deref(),
        Some("releases_unreadable")
    );
    assert!(rig.store.album_identity("album-1").unwrap().is_none());
    assert!(rig.store.match_flag("album-1").unwrap().is_none());
}

/// Random Access Memories (Daft Punk), as two of its editions: the
/// release ids are MusicBrainz's, every other id is made up. Both list
/// the same 13 recordings, each with its own track ids.
const RAM: &str = "36e2aede-346d-4931-8565-78d810d167c7";
const RAM_CD: &str = "158f94bf-d77f-4055-afa4-8fc7e0b2a004";
const RAM_GROUP: &str = "0ad0b0a0-0000-4000-8000-00000000b0b0";
const RAM_TRACKS: [(&str, u64); 13] = [
    ("Give Life Back to Music", 274),
    ("The Game of Love", 321),
    ("Giorgio by Moroder", 544),
    ("Within", 228),
    ("Instant Crush", 337),
    ("Lose Yourself to Dance", 353),
    ("Touch", 498),
    ("Get Lucky", 369),
    ("Beyond", 290),
    ("Motherboard", 341),
    ("Fragments of Time", 279),
    ("Doin' It Right", 251),
    ("Contact", 381),
];

fn ram_recording(index: usize) -> String {
    format!("0ad0b0a0-0000-4000-8000-0000000d{index:04}")
}

/// The made-up track id of track `index` on `release`.
fn ram_track(release: &str, index: usize) -> String {
    format!("{}-0000-4000-8000-0000000e{index:04}", &release[..8])
}

/// A release lookup in the WS/2 shape of `release_standard.json`.
fn ram_release(release: &str, format: &str) -> serde_json::Value {
    let credit = serde_json::json!([{
        "name": "Daft Punk",
        "joinphrase": "",
        "artist": {
            "id": "0ad0b0a0-0000-4000-8000-00000000a0a0",
            "name": "Daft Punk",
            "sort-name": "Daft Punk"
        }
    }]);
    let tracks: Vec<serde_json::Value> = RAM_TRACKS
        .iter()
        .enumerate()
        .map(|(index, (title, seconds))| {
            serde_json::json!({
                "id": ram_track(release, index),
                "position": index + 1,
                "number": (index + 1).to_string(),
                "title": title,
                "length": seconds * 1000,
                "recording": {
                    "id": ram_recording(index),
                    "title": title,
                    "length": seconds * 1000,
                    "artist-credit": credit
                },
                "artist-credit": credit
            })
        })
        .collect();
    serde_json::json!({
        "id": release,
        "title": "Random Access Memories",
        "status": "Official",
        "date": "2013-05-17",
        "country": "XW",
        "artist-credit": credit,
        "label-info": [],
        "release-group": {
            "id": RAM_GROUP,
            "title": "Random Access Memories",
            "primary-type": "Album",
            "secondary-types": [],
            "first-release-date": "2013-05-17",
            "artist-credit": credit
        },
        "media": [{
            "position": 1,
            "format": format,
            "track-offset": 0,
            "track-count": 13,
            "tracks": tracks
        }]
    })
}

/// A release search page in the shape of `release_search.json`: both
/// editions, for every search and every listing of the group.
fn ram_search() -> serde_json::Value {
    let hit = |release: &str, format: &str| {
        serde_json::json!({
            "id": release,
            "score": 100,
            "title": "Random Access Memories",
            "status": "Official",
            "artist-credit": [{
                "name": "Daft Punk",
                "artist": {
                    "id": "0ad0b0a0-0000-4000-8000-00000000a0a0",
                    "name": "Daft Punk",
                    "sort-name": "Daft Punk"
                }
            }],
            "release-group": {
                "id": RAM_GROUP,
                "title": "Random Access Memories",
                "primary-type": "Album"
            },
            "track-count": 13,
            "media": [{"format": format, "disc-count": 0, "track-count": 13}]
        })
    };
    serde_json::json!({
        "created": "2026-10-14T09:00:00.000Z",
        "count": 2,
        "offset": 0,
        "releases": [hit(RAM_CD, "CD"), hit(RAM, "Digital Media")]
    })
}

/// Thirteen files name the digital release and carry its recordings and
/// track ids, except file 8, whose track id is the CD's track 8 (as a
/// downloaded album's often is). The album is the release its files name,
/// and file 8 is mapped to that release's track 8.
#[tokio::test]
async fn a_stray_track_id_does_not_move_the_album_to_another_edition() {
    let ok = |document: serde_json::Value| {
        RawResponse::new(200, Vec::new(), document.to_string().into_bytes())
    };
    let rig = rig_with(
        HashMap::from([
            ("/release".to_owned(), ok(ram_search())),
            (
                format!("/release/{RAM}"),
                ok(ram_release(RAM, "Digital Media")),
            ),
            (format!("/release/{RAM_CD}"), ok(ram_release(RAM_CD, "CD"))),
        ]),
        false,
    );
    let facts = LocalAlbumFacts {
        local_album_id: "album-1".to_owned(),
        title: "Random Access Memories".to_owned(),
        album_artist_name: "Daft Punk".to_owned(),
        tracks: RAM_TRACKS
            .iter()
            .enumerate()
            .map(|(index, (title, seconds))| LocalTrackFacts {
                local_track_id: format!("t{}", index + 1),
                title: (*title).to_owned(),
                artist_name: "Daft Punk".to_owned(),
                track_number: index as u32 + 1,
                disc_number: 1,
                duration_secs: Some(*seconds),
                recording_mbid: Some(ram_recording(index)),
                release_track_mbid: Some(ram_track(if index == 7 { RAM_CD } else { RAM }, index)),
                release_mbid: Some(RAM.to_owned()),
                ..LocalTrackFacts::default()
            })
            .collect(),
        ..LocalAlbumFacts::default()
    };

    let report = identify(&rig, facts).await;

    assert_eq!(report.outcome, IdentificationOutcome::Identified);
    let identity = rig
        .store
        .album_identity("album-1")
        .unwrap()
        .expect("identity");
    assert_eq!(identity.release_mbid.as_deref(), Some(RAM));
    let track = rig
        .store
        .track_identity("t8")
        .unwrap()
        .expect("track identity");
    let expected = ram_track(RAM, 7);
    assert_eq!(track.release_track_mbid.as_deref(), Some(expected.as_str()));
}

/// Thirteen files name the digital release, which MusicBrainz will not
/// give in a readable form, and carry its recordings. Recall goes on
/// without it and the CD fits every file, but with the files' own
/// edition unread nothing is confirmed: the CD is kept only as an
/// unconfirmed guess.
#[tokio::test]
async fn an_unreadable_tagged_release_leaves_only_a_guess() {
    let ok = |document: serde_json::Value| {
        RawResponse::new(200, Vec::new(), document.to_string().into_bytes())
    };
    let mut digital = ram_release(RAM, "Digital Media");
    digital
        .as_object_mut()
        .expect("release object")
        .remove("release-group");
    let rig = rig_with(
        HashMap::from([
            ("/release".to_owned(), ok(ram_search())),
            (format!("/release/{RAM}"), ok(digital)),
            (format!("/release/{RAM_CD}"), ok(ram_release(RAM_CD, "CD"))),
        ]),
        false,
    );
    let facts = LocalAlbumFacts {
        local_album_id: "album-1".to_owned(),
        title: "Random Access Memories".to_owned(),
        album_artist_name: "Daft Punk".to_owned(),
        tracks: RAM_TRACKS
            .iter()
            .enumerate()
            .map(|(index, (title, seconds))| LocalTrackFacts {
                local_track_id: format!("t{}", index + 1),
                title: (*title).to_owned(),
                artist_name: "Daft Punk".to_owned(),
                track_number: index as u32 + 1,
                disc_number: 1,
                duration_secs: Some(*seconds),
                recording_mbid: Some(ram_recording(index)),
                release_mbid: Some(RAM.to_owned()),
                ..LocalTrackFacts::default()
            })
            .collect(),
        ..LocalAlbumFacts::default()
    };

    let report = identify(&rig, facts).await;

    assert_eq!(report.outcome, IdentificationOutcome::Ambiguous);
    assert_eq!(report.reason_code, "TAGGED_RELEASE_UNREADABLE");
    let identity = rig
        .store
        .album_identity("album-1")
        .unwrap()
        .expect("best guess");
    assert_eq!(identity.release_mbid.as_deref(), Some(RAM_CD));
    assert_eq!(identity.decision_source, DecisionSource::Automatic);
    let flag = rig
        .store
        .match_flag("album-1")
        .unwrap()
        .expect("unconfirmed flag");
    assert_eq!(flag.state, identify::models::MatchFlagState::Unconfirmed);
    assert_eq!(flag.reason_code, "TAGGED_RELEASE_UNREADABLE");
}

/// A store of the merged release id that a lookup through the merged id
/// lands on a document already on file (with a names part grafted onto
/// it): the document learns the merged id, keeps its names, and answers
/// to it from then on, so recall stops fetching it and an album whose
/// match names the merged id has its artists worked out.
#[tokio::test]
async fn a_merged_release_id_finds_the_document_already_on_file() {
    use droppedneedle_library::matching::model::{ArtistName, ArtistNames, ReleaseExtras};

    let rig = rig(false);
    let standard_facts = album(&[("Lamplight", 214, None)]);
    let fetched = rig
        .service
        .recall(
            &standard_facts,
            Some(STANDARD),
            droppedneedle_kernel::db::Lane::Background,
            RequestPriority::BackgroundSync,
        )
        .await
        .unwrap();
    assert_eq!(fetched.releases.len(), 1);
    let names = ArtistNames {
        fetched_at_ms: 1,
        artists: [(
            "c0ffee00-0000-4000-8000-00000000a001".to_owned(),
            ArtistName {
                name: "The Lanterns".to_owned(),
                sort_name: None,
                aliases: Some(Vec::new()),
            },
        )]
        .into(),
    };
    rig.store
        .graft_extras(
            STANDARD,
            &ReleaseExtras {
                relations: None,
                names: Some(names.clone()),
            },
        )
        .unwrap()
        .expect("names grafted");
    rig.seen.lock().expect("seen").clear();

    let id = |n: u32| format!("c0ffee00-0000-4000-8000-00000000{n:04x}");
    let (recordings, tracks): (Vec<String>, Vec<String>) = (0xd001..=0xd004)
        .map(|n| (id(n), id(n - 0xd001 + 0xe001)))
        .unzip();
    let titles = [
        ("Lamplight", 214),
        ("Blue Hour", 251),
        ("Night Shift", 198),
        ("Last Train Home", 305),
    ];
    let files: Vec<FileTags<'_>> = titles
        .iter()
        .enumerate()
        .map(|(index, (title, seconds))| {
            (
                *title,
                *seconds,
                Some((recordings[index].as_str(), tracks[index].as_str(), MERGED)),
            )
        })
        .collect();
    let facts = album(&files);
    let report = identify(&rig, facts.clone()).await;
    assert_eq!(report.outcome, IdentificationOutcome::Identified);

    let stored = rig
        .store
        .release(MERGED, None)
        .unwrap()
        .expect("found by the merged id");
    assert_eq!(stored.id, STANDARD);
    assert_eq!(stored.artist_names, Some(names));
    rig.seen.lock().expect("seen").clear();
    rig.service
        .recall(
            &facts,
            None,
            droppedneedle_kernel::db::Lane::Background,
            RequestPriority::BackgroundSync,
        )
        .await
        .unwrap();
    let seen = rig.seen.lock().expect("seen").clone();
    assert!(
        !seen.contains(&format!("/release/{MERGED}")),
        "recall fetched the merged id again: {seen:?}"
    );

    rig.db
        .execute_batch(&format!(
            "INSERT INTO local_album_artists (local_album_id, position, local_artist_id, role, \
             credited_name, join_phrase) VALUES ('album-1', 0, 'artist-1', 'main', \
             'The Lanterns', ''); \
             UPDATE local_album_external_identities SET release_mbid = '{MERGED}', \
             decision_source = 'manual', row_revision = row_revision + 1 \
             WHERE local_album_id = 'album-1';"
        ))
        .expect("a person's match names the merged id");
    rig.store.link_album_artists("album-1").unwrap();
    let state: String = rig
        .db
        .query_row(
            "SELECT state FROM library_artist_reconciliation_state \
             WHERE local_album_id = 'album-1'",
            [],
            |row| row.get(0),
        )
        .expect("state");
    assert_eq!(state, "projected");
}

// ---------------------------------------------------------------------------
// Artist ids MusicBrainz merged into others.
// ---------------------------------------------------------------------------

/// Recorded from BrainzMash on 2026-10-17 (`tests/fixtures/musicbrainz`):
/// the retired id answers 301 to the current one.
const RETIRED: &str = "3ec87112-9081-467b-8327-e18286f1b020";
const CURRENT: &str = "ffbcb0c1-37ad-4df8-a78d-e31bd590d7ea";
/// A well-formed id that names nothing (404).
const NOBODY: &str = "5b0c7e7a-3f0e-4d4a-9a51-2f6c1d8e9b30";
/// MusicBrainz's [anonymous] placeholder artist.
const ANONYMOUS: &str = "f731ccc4-e22a-43af-a747-64213329e088";
/// Made-up ids for other artists.
const OTHER: &str = "c0ffee00-0000-4000-8000-0000000a0b02";
const FURTHER: &str = "c0ffee00-0000-4000-8000-0000000a0d04";
const OUTSIDE: &str = "c0ffee00-0000-4000-8000-0000000a0e05";
const NAME: &str = "Ewald Heidepriem";
const ARTIST_RELEASE: &str = "c0ffee00-0000-4000-8000-00000000f001";
const ARTIST_GROUP: &str = "c0ffee00-0000-4000-8000-00000000f002";

/// A BrainzMash redirect from `from` to `to`, as recorded: an absolute
/// same-origin `Location` with `?fmt=json`, and the kept artist as body.
fn mirror_redirect(to: &str) -> RawResponse {
    RawResponse::new(
        301,
        vec![(
            "location",
            &format!("https://api.brainzmash.cc/ws/2/artist/{to}?fmt=json"),
        )],
        fixture("artist_merged_redirect.json"),
    )
}

/// An artist lookup answered with an artist whose id is `id`.
fn artist_answer(id: &str) -> RawResponse {
    RawResponse::new(
        200,
        Vec::new(),
        serde_json::to_vec(&serde_json::json!({"id": id, "name": "Someone"})).expect("encodes"),
    )
}

/// The probe's answers: the retired id redirects, the current id and the
/// unknown one answer as recorded.
fn probe_routes() -> HashMap<String, RawResponse> {
    HashMap::from([
        (format!("/artist/{RETIRED}"), mirror_redirect(CURRENT)),
        (
            format!("/artist/{CURRENT}"),
            RawResponse::new(200, Vec::new(), fixture("artist_merged_canonical.json")),
        ),
        (
            format!("/artist/{NOBODY}"),
            RawResponse::new(404, Vec::new(), fixture("artist_merged_missing.json")),
        ),
    ])
}

fn with(
    mut routes: HashMap<String, RawResponse>,
    extra: Vec<(String, RawResponse)>,
) -> HashMap<String, RawResponse> {
    routes.extend(extra);
    routes
}

impl Rig {
    fn artist(&self, id: &str, name: &str, created: f64) {
        self.db
            .execute(
                "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, \
                 updated_at) VALUES (?1, ?2, lower(?2), 'person', ?3, ?3)",
                params![id, name, created],
            )
            .expect("artist seeds");
    }

    fn link(&self, artist: &str, mbid: &str, source: &str) {
        self.db
            .execute(
                "INSERT INTO local_artist_external_identities (local_artist_id, provider, \
                 provider_artist_id, decision_source, selected_at) \
                 VALUES (?1, 'musicbrainz', ?2, ?3, 1)",
                params![artist, mbid, source],
            )
            .expect("link seeds");
    }

    /// Album `al` by row `ar-r` with two tracks whose files hold
    /// `evidence`, matched automatically to a release on file crediting
    /// `credited` on the album and nobody on the tracks.
    fn artist_album(&self, evidence: Option<&str>, credited: &str) {
        self.artist("ar-r", NAME, 1.0);
        self.db
            .execute(
                "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
                 album_artist_name, album_artist_id, grouping_source, created_at, updated_at) \
                 VALUES ('al', 'root-1', 'al', 'Album', 'album', ?1, 'ar-r', 'automatic', 0, 0)",
                params![NAME],
            )
            .expect("album seeds");
        self.db
            .execute(
                "INSERT INTO local_album_artists (local_album_id, position, local_artist_id, \
                 role, credited_name, join_phrase) VALUES ('al', 0, 'ar-r', 'main', ?1, '')",
                params![NAME],
            )
            .expect("album credit seeds");
        self.db
            .execute(
                "INSERT INTO local_album_external_identities (local_album_id, provider, \
                 release_group_mbid, release_mbid, decision_source, selected_at) \
                 VALUES ('al', 'musicbrainz', ?1, ?2, 'automatic', 1)",
                params![ARTIST_GROUP, ARTIST_RELEASE],
            )
            .expect("match seeds");
        for number in 1..=2 {
            let track = format!("tr-{number}");
            self.db
                .execute(
                    "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, \
                     relative_path, path_hash, file_size_bytes, file_mtime_ns, stat_revision, \
                     tag_revision, title, title_folded, artist_name, album_title, \
                     album_title_folded, disc_number, track_number, file_format, \
                     ingest_source, imported_at, membership_source) \
                     VALUES (?1, 'al', 'root-1', '/music/' || ?1 || '.flac', ?1 || '.flac', ?1, \
                     1, 1, 'stat-1', 'tag-1', 'Song ' || ?2, 'song ' || ?2, ?3, 'Album', \
                     'album', 1, ?2, 'flac', 'scan', 0, 'automatic')",
                    params![track, number, NAME],
                )
                .expect("track seeds");
            self.db
                .execute(
                    "INSERT INTO local_track_artists (local_track_id, position, local_artist_id, \
                     role, credited_name, join_phrase, artist_mbid) \
                     VALUES (?1, 0, 'ar-r', 'main', ?2, '', ?3)",
                    params![track, NAME, evidence],
                )
                .expect("track credit seeds");
            self.db
                .execute(
                    "INSERT INTO local_track_external_identities (local_track_id, provider, \
                     recording_mbid, release_mbid, release_track_mbid, decision_source, \
                     selected_at) VALUES (?1, 'musicbrainz', 'rec-' || ?2, ?3, 'rt-' || ?2, \
                     'automatic', 1)",
                    params![track, number, ARTIST_RELEASE],
                )
                .expect("track mapping seeds");
        }
        self.store
            .save_release(
                &Release {
                    id: ARTIST_RELEASE.to_owned(),
                    release_group_id: ARTIST_GROUP.to_owned(),
                    title: "Album".to_owned(),
                    artists: vec![CreditedArtist {
                        id: credited.to_owned(),
                        name: NAME.to_owned(),
                        sort_name: None,
                        join: String::new(),
                    }],
                    tracks: (1..=2)
                        .map(|number| ReleaseTrack {
                            id: format!("rt-{number}"),
                            recording_id: format!("rec-{number}"),
                            title: format!("Song {number}"),
                            disc: 1,
                            position: number,
                            absolute_position: number,
                            ..ReleaseTrack::default()
                        })
                        .collect(),
                    ..Release::default()
                },
                droppedneedle_kernel::db::Lane::Background,
            )
            .unwrap();
    }

    fn texts(&self, sql: &str) -> Vec<String> {
        self.db
            .prepare(sql)
            .expect("query prepares")
            .query_map([], |row| row.get(0))
            .expect("query runs")
            .collect::<Result<_, _>>()
            .expect("rows read")
    }

    /// Every checked id as `id state canonical`.
    fn checks(&self) -> Vec<String> {
        self.texts(
            "SELECT artist_mbid || ' ' || state || ' ' || COALESCE(canonical_mbid, '-') \
             FROM library_identify_artist_id_checks ORDER BY artist_mbid",
        )
    }

    /// The ids the credits of `ar-r` hold.
    fn held(&self) -> Vec<String> {
        self.texts(
            "SELECT DISTINCT artist_mbid FROM local_track_artists \
             WHERE local_artist_id = 'ar-r' AND artist_mbid IS NOT NULL ORDER BY 1",
        )
    }

    fn link_of(&self, artist: &str) -> Vec<String> {
        self.texts(&format!(
            "SELECT provider_artist_id || ' ' || decision_source \
             FROM local_artist_external_identities WHERE local_artist_id = '{artist}'"
        ))
    }

    /// Everything an answer may write.
    fn written(&self) -> String {
        self.texts(
            "SELECT COALESCE((SELECT group_concat(id || ':' || row_revision || ':' || \
             COALESCE(converged_mbid, '')) FROM local_artists), '') || '|' || \
             COALESCE((SELECT group_concat(local_artist_id || ':' || provider_artist_id || ':' \
             || row_revision) FROM local_artist_external_identities), '') || '|' || \
             COALESCE((SELECT group_concat(local_track_id || ':' || COALESCE(artist_mbid, '')) \
             FROM local_track_artists), '') || '|' || COALESCE((SELECT group_concat(subject_id \
             || ':' || artist_mbid || ':' || row_revision) FROM library_artist_credit_proofs), '') \
             || '|' || COALESCE((SELECT group_concat(artist_mbid || ':' || state || ':' || \
             COALESCE(checked_at, '')) FROM library_identify_artist_id_checks), '') || '|' || \
             COALESCE((SELECT value FROM library_catalog_revision), 0)",
        )
        .concat()
    }

    fn seen(&self) -> Vec<String> {
        self.seen.lock().expect("seen").clone()
    }
}

fn asked(id: &str) -> String {
    format!("/artist/{id}")
}

/// Two albums carried with a match and no release document. MusicBrainz
/// refuses the first in line for good (a 400), so that album stops asking
/// and the pass goes on: the second release is fetched and proves its
/// album's artist. A later pass asks nothing again.
#[tokio::test]
async fn a_refused_release_document_does_not_hold_up_the_rest() {
    const REFUSED: &str = "c0ffee00-0000-4000-8000-0000000000bd";
    const LANTERNS: &str = "c0ffee00-0000-4000-8000-00000000a001";
    let rig = rig_with(
        with(
            routes(),
            vec![(
                format!("/release/{REFUSED}"),
                RawResponse::new(400, Vec::new(), b"{\"error\":\"Invalid mbid.\"}".to_vec()),
            )],
        ),
        false,
    );
    rig.artist("ar-l", "The Lanterns", 1.0);
    for (album, release) in [("al-refused", REFUSED), ("al-good", STANDARD)] {
        rig.db
            .execute_batch(&format!(
                "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
                 album_artist_name, album_artist_id, grouping_source, created_at, updated_at) \
                 VALUES ('{album}', 'root-1', '{album}', 'Album', 'album', 'The Lanterns', \
                 'ar-l', 'automatic', 0, 0); \
                 INSERT INTO local_album_artists (local_album_id, position, local_artist_id, \
                 role, credited_name, join_phrase) \
                 VALUES ('{album}', 0, 'ar-l', 'main', 'The Lanterns', ''); \
                 INSERT INTO local_album_external_identities (local_album_id, provider, \
                 release_group_mbid, release_mbid, decision_source, selected_at) \
                 VALUES ('{album}', 'musicbrainz', 'c0ffee00-0000-4000-8000-00000000b001', \
                 '{release}', 'automatic', 1);"
            ))
            .expect("carried album seeds");
    }
    rig.store.sweep_artist_links().unwrap();
    rig.db
        .execute(
            "UPDATE library_artist_reconciliation_state SET updated_at = 0 \
             WHERE local_album_id = 'al-refused'",
            [],
        )
        .expect("first in line");

    let pass = rig.service.fetch_waiting_documents(20).await.unwrap();
    assert_eq!(pass.fetched, 1);
    rig.store.sweep_artist_links().unwrap();
    assert_eq!(rig.link_of("ar-l"), [format!("{LANTERNS} automatic")]);
    assert_eq!(
        rig.texts(
            "SELECT local_album_id || ' ' || reason_code FROM library_artist_reconciliation_state \
             ORDER BY local_album_id"
        ),
        [
            "al-good CREDITS_LINE_UP".to_owned(),
            "al-refused RELEASE_DOCUMENT_REFUSED".to_owned(),
        ]
    );
    rig.service.fetch_waiting_documents(20).await.unwrap();
    let refused = format!("/release/{REFUSED}");
    assert_eq!(
        rig.seen().iter().filter(|path| **path == refused).count(),
        1
    );
}

/// Files holding the retired id, and a matched album proving the current
/// one: the pass records both ids and links nothing. MusicBrainz's answer
/// rewrites the files' evidence to the current id and links the row; a
/// second batch asks nothing and writes nothing.
#[tokio::test]
async fn an_artist_id_musicbrainz_merged_is_followed() {
    let rig = rig_on(probe_routes(), false, Source::BrainzMash);
    rig.artist_album(Some(RETIRED), CURRENT);
    rig.store.link_and_merge_album_artists("al").unwrap();
    assert_eq!(
        rig.checks(),
        [
            format!("{RETIRED} pending -"),
            format!("{CURRENT} pending -")
        ]
    );
    assert!(rig.link_of("ar-r").is_empty());

    assert_eq!(rig.service.resolve_merged_artist_ids(20).await.unwrap(), 2);
    assert_eq!(
        rig.checks(),
        [
            format!("{RETIRED} merged {CURRENT}"),
            format!("{CURRENT} kept -"),
        ]
    );
    assert_eq!(rig.held(), [CURRENT]);
    assert_eq!(rig.link_of("ar-r"), [format!("{CURRENT} automatic")]);
    let seen = rig.seen();
    let current = seen.iter().filter(|path| **path == asked(CURRENT)).count();
    assert_eq!(
        seen.iter().filter(|path| **path == asked(RETIRED)).count(),
        1
    );
    assert!((1..=2).contains(&current), "{seen:?}");
    assert_eq!(seen.len(), 1 + current, "{seen:?}");

    let before = rig.written();
    assert_eq!(rig.service.resolve_merged_artist_ids(20).await.unwrap(), 0);
    assert_eq!(
        rig.seen().len(),
        seen.len(),
        "nothing pending, nothing asked"
    );
    assert_eq!(rig.written(), before);
}

/// Two ids MusicBrainz says are both current stay apart: both kept,
/// nothing rewritten, the row unlinked. Another pass records and asks
/// nothing until a month has passed.
#[tokio::test]
async fn ids_musicbrainz_keeps_apart_are_asked_again_after_a_month() {
    let routes = with(probe_routes(), vec![(asked(OTHER), artist_answer(OTHER))]);
    let rig = rig_on(routes, false, Source::BrainzMash);
    rig.artist_album(Some(OTHER), CURRENT);
    rig.store.link_and_merge_album_artists("al").unwrap();
    assert_eq!(rig.service.resolve_merged_artist_ids(20).await.unwrap(), 2);
    assert_eq!(
        rig.checks(),
        [format!("{OTHER} kept -"), format!("{CURRENT} kept -")]
    );
    assert_eq!(rig.held(), [OTHER]);
    assert!(rig.link_of("ar-r").is_empty());

    let before = rig.written();
    let asks = rig.seen().len();
    rig.store.link_and_merge_album_artists("al").unwrap();
    assert_eq!(rig.service.resolve_merged_artist_ids(20).await.unwrap(), 0);
    assert_eq!(rig.written(), before);
    assert_eq!(rig.seen().len(), asks);

    rig.db
        .execute(
            "UPDATE library_identify_artist_id_checks \
             SET checked_at = checked_at - 31 * 24 * 3600 * 1000 WHERE artist_mbid = ?1",
            params![OTHER],
        )
        .expect("checked a month ago");
    rig.store.link_and_merge_album_artists("al").unwrap();
    assert_eq!(rig.checks()[0], format!("{OTHER} pending -"));
    assert_eq!(rig.service.resolve_merged_artist_ids(20).await.unwrap(), 1);
    assert_eq!(rig.seen()[asks..], [asked(OTHER)]);
}

/// MusicBrainz down, refusing this install (403), or not set up: the batch
/// stops at the first id, which stays pending, and nothing is written. A
/// dead wire is tried three times, as all background reads are.
#[tokio::test]
async fn no_answer_leaves_the_ids_pending() {
    let refused = HashMap::from([
        (
            asked(RETIRED),
            RawResponse::new(403, Vec::new(), Vec::new()),
        ),
        (
            asked(CURRENT),
            RawResponse::new(403, Vec::new(), Vec::new()),
        ),
    ]);
    for (rig, requests) in [
        (rig_on(probe_routes(), true, Source::BrainzMash), 3),
        (rig_on(refused, false, Source::BrainzMash), 1),
        (rig_on(probe_routes(), false, Source::Unbound), 0),
    ] {
        rig.artist_album(Some(RETIRED), CURRENT);
        rig.store.link_and_merge_album_artists("al").unwrap();
        let before = rig.written();
        assert_eq!(rig.service.resolve_merged_artist_ids(20).await.unwrap(), 0);
        assert_eq!(rig.seen().len(), requests, "{:?}", rig.seen());
        assert_eq!(rig.written(), before);
        assert_eq!(
            rig.checks(),
            [
                format!("{RETIRED} pending -"),
                format!("{CURRENT} pending -")
            ]
        );
    }
}

/// A rate-limited answer's `Retry-After` holds every lookup back that long
/// (with up to a fifth more), then they run again.
#[tokio::test(start_paused = true)]
async fn a_retry_after_holds_artist_lookups_back() {
    let limited = || RawResponse::new(429, vec![("retry-after", "30")], Vec::new());
    let routes = HashMap::from([(asked(RETIRED), limited()), (asked(CURRENT), limited())]);
    let rig = rig_on(routes, false, Source::Official);
    rig.artist_album(Some(RETIRED), CURRENT);
    rig.store.link_and_merge_album_artists("al").unwrap();
    assert_eq!(rig.service.resolve_merged_artist_ids(20).await.unwrap(), 0);
    assert_eq!(rig.seen().len(), 1);
    tokio::time::advance(std::time::Duration::from_secs(29)).await;
    assert_eq!(rig.service.resolve_merged_artist_ids(20).await.unwrap(), 0);
    assert_eq!(rig.seen().len(), 1, "held back");
    tokio::time::advance(std::time::Duration::from_secs(8)).await;
    assert_eq!(rig.service.resolve_merged_artist_ids(20).await.unwrap(), 0);
    assert_eq!(rig.seen().len(), 2, "asked again");
}

/// Merges that are not followed: into a placeholder artist, into an id no
/// record held beside another, and back into the id asked about. The id is
/// kept and nothing is rewritten.
#[tokio::test]
async fn merges_the_app_does_not_follow() {
    let placeholder = with(
        probe_routes(),
        vec![
            (asked(RETIRED), mirror_redirect(ANONYMOUS)),
            (asked(ANONYMOUS), artist_answer(ANONYMOUS)),
        ],
    );
    let outside = with(
        probe_routes(),
        vec![
            (asked(RETIRED), mirror_redirect(OUTSIDE)),
            (asked(OUTSIDE), artist_answer(OUTSIDE)),
        ],
    );
    for routes in [placeholder, outside] {
        let rig = rig_on(routes, false, Source::BrainzMash);
        rig.artist_album(Some(RETIRED), CURRENT);
        rig.store.link_and_merge_album_artists("al").unwrap();
        assert_eq!(rig.service.resolve_merged_artist_ids(20).await.unwrap(), 2);
        assert_eq!(
            rig.checks(),
            [format!("{RETIRED} kept -"), format!("{CURRENT} kept -")]
        );
        assert_eq!(rig.held(), [RETIRED]);
        assert!(rig.link_of("ar-r").is_empty());
    }

    let rig = rig_on(probe_routes(), false, Source::BrainzMash);
    rig.artist_album(Some(RETIRED), CURRENT);
    rig.db
        .execute_batch(&format!(
            "INSERT INTO library_identify_artist_id_checks (artist_mbid, state, \
             canonical_mbid, nominated_at, checked_at) VALUES \
             ('{CURRENT}', 'merged', '{RETIRED}', 0, 0); \
             INSERT INTO library_identify_artist_id_checks (artist_mbid, state, nominated_at) \
             VALUES ('{RETIRED}', 'pending', 1);"
        ))
        .expect("the current id on file as merged into the retired one");
    assert_eq!(rig.service.resolve_merged_artist_ids(20).await.unwrap(), 1);
    assert_eq!(
        rig.checks(),
        [
            format!("{RETIRED} kept -"),
            format!("{CURRENT} merged {RETIRED}"),
        ]
    );
}

/// The reverse: a release document stored before the merge credits the
/// retired id, while the files hold the current one. The answer rewrites
/// the proof, marks the album to be worked through again and links the
/// row.
#[tokio::test]
async fn a_release_crediting_the_retired_id_proves_the_current_one() {
    use droppedneedle_library::identify::artist_links::stale_albums;

    let rig = rig_on(probe_routes(), false, Source::BrainzMash);
    rig.artist_album(Some(CURRENT), RETIRED);
    rig.store.link_and_merge_album_artists("al").unwrap();
    assert!(stale_albums(&rig.db, 50).expect("stale").is_empty());
    rig.service.resolve_merged_artist_ids(20).await.unwrap();
    assert_eq!(
        rig.texts("SELECT DISTINCT artist_mbid FROM library_artist_credit_proofs"),
        [CURRENT]
    );
    assert_eq!(stale_albums(&rig.db, 50).expect("stale"), ["al"]);
    assert_eq!(rig.link_of("ar-r"), [format!("{CURRENT} automatic")]);
}

/// An id already recorded as merged into one that MusicBrainz now says
/// was merged further names the final id, as do the credits.
#[tokio::test]
async fn a_recorded_merge_follows_a_later_one() {
    let routes = HashMap::from([
        (asked(CURRENT), mirror_redirect(FURTHER)),
        (asked(FURTHER), artist_answer(FURTHER)),
    ]);
    let rig = rig_on(routes, false, Source::BrainzMash);
    rig.artist_album(Some(CURRENT), FURTHER);
    rig.db
        .execute(
            "INSERT INTO library_identify_artist_id_checks (artist_mbid, state, \
             canonical_mbid, nominated_at, checked_at) VALUES (?1, 'merged', ?2, 0, 0)",
            params![RETIRED, CURRENT],
        )
        .expect("an earlier merge on file");
    rig.store.link_and_merge_album_artists("al").unwrap();
    rig.service.resolve_merged_artist_ids(20).await.unwrap();
    assert_eq!(
        rig.checks(),
        [
            format!("{RETIRED} merged {FURTHER}"),
            format!("{FURTHER} kept -"),
            format!("{CURRENT} merged {FURTHER}"),
        ]
    );
    assert_eq!(rig.held(), [FURTHER]);
}

/// A person's link to the retired id moves to the current one, still the
/// person's, with one catalog action. While another row holds the current
/// id by a person's link, the merge is not followed at all.
#[tokio::test]
async fn a_persons_link_follows_the_merge_unless_another_holds_the_id() {
    let rig = rig_on(probe_routes(), false, Source::BrainzMash);
    rig.artist_album(None, CURRENT);
    rig.link("ar-r", RETIRED, "manual");
    rig.store.link_and_merge_album_artists("al").unwrap();
    rig.service.resolve_merged_artist_ids(20).await.unwrap();
    assert_eq!(rig.link_of("ar-r"), [format!("{CURRENT} manual")]);
    assert_eq!(
        rig.texts(
            "SELECT action_kind || ' ' || before_json || ' ' || after_json \
             FROM library_catalog_actions WHERE reason_code = 'MUSICBRAINZ_MERGED_ARTIST_ID'"
        ),
        [format!(
            "follow_merged_artist_id {{\"provider_artist_id\":\"{RETIRED}\"}} \
             {{\"provider_artist_id\":\"{CURRENT}\"}}"
        )]
    );

    let rig = rig_on(probe_routes(), false, Source::BrainzMash);
    rig.artist_album(Some(RETIRED), CURRENT);
    rig.link("ar-r", RETIRED, "manual");
    rig.artist("ar-s", "Someone Else", 2.0);
    rig.link("ar-s", CURRENT, "manual");
    rig.store.link_and_merge_album_artists("al").unwrap();
    rig.service.resolve_merged_artist_ids(20).await.unwrap();
    assert_eq!(rig.checks()[0], format!("{RETIRED} kept -"));
    assert_eq!(rig.link_of("ar-r"), [format!("{RETIRED} manual")]);
    assert_eq!(rig.held(), [RETIRED]);
    assert!(
        rig.texts("SELECT id FROM library_catalog_actions")
            .is_empty(),
        "nothing moved"
    );
}
