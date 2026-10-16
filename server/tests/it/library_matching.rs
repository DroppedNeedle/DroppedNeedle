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

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use droppedneedle_library::identify;
use droppedneedle_library::matching::decide::REVIEW_CEILING;
use droppedneedle_providers::Providers;
use droppedneedle_providers::musicbrainz::{
    MbPacing, MbRequest, MbTransport, MusicBrainzClient, RawResponse, TransportError,
};
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

/// Scripted MusicBrainz: routes by path, 404 for anything unscripted,
/// every request recorded. No live network.
struct FakeMb {
    routes: HashMap<String, RawResponse>,
    seen: Arc<Mutex<Vec<String>>>,
    down: bool,
}

impl MbTransport for FakeMb {
    async fn get(&self, request: &MbRequest) -> Result<RawResponse, TransportError> {
        let path = request
            .url
            .split("/ws/2")
            .nth(1)
            .unwrap_or_default()
            .to_owned();
        self.seen.lock().expect("seen").push(path.clone());
        if self.down {
            return Err(TransportError("wire is down".to_owned()));
        }
        Ok(self
            .routes
            .get(&path)
            .cloned()
            .unwrap_or_else(|| RawResponse::new(404, Vec::new(), b"{}".to_vec())))
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
    let seen = Arc::new(Mutex::new(Vec::new()));
    let musicbrainz = MusicBrainzClient::official(
        FakeMb {
            routes,
            seen: seen.clone(),
            down,
        },
        MbPacing::new(Arc::new(Providers::unpaced())),
    );
    let dir = ScratchDir::new("lib-matching");
    let path = dir.join("app.db");
    migrated_db(&path);
    let db = droppedneedle_kernel::db::open_connection(&path).expect("side connection opens");
    let store = Arc::new(SqliteIdentifyStore::open(&path).expect("store opens"));
    let service = IdentifyService::new(IdentifyDeps {
        identities: store.clone(),
        facts: store.clone(),
        proofs: store.clone(),
        aliases: store.clone(),
        queue: store.clone(),
        reviews: store.clone(),
        releases: store.clone(),
        providers: Arc::new(LiveProviders::new(
            musicbrainz,
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
    rig.service.enqueue_album(
        "job-1",
        "album-1",
        IdentifyKind::Automatic,
        "rev-1",
        None,
        0,
    );
    rig.store.claim(0, 60_000);
    rig.service
        .run_claimed_job("job-1", 0)
        .await
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
    let identity = rig.store.album_identity("album-1").expect("identity");
    assert_eq!(identity.release_mbid.as_deref(), Some(STANDARD));
    assert_eq!(identity.release_group_mbid.as_deref(), Some(GROUP));
    let track = rig.store.track_identity("t2").expect("track identity");
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
    let stored = rig.store.release(STANDARD, None).expect("stored");
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
    let identity = rig.store.album_identity("album-1").expect("best guess");
    assert_eq!(identity.release_group_mbid.as_deref(), Some(GROUP));
    assert_eq!(identity.decision_source, DecisionSource::Automatic);
    // The unconfirmed flag carries the closest candidates with their
    // distances: close enough to show a person, not close enough to trust.
    let flag = rig.store.match_flag("album-1").expect("unconfirmed flag");
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
    assert!(rig.store.release(STANDARD, None).is_some());
    assert!(rig.store.release(REMASTER, None).is_some());
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
    let identity = rig.store.album_identity("album-1").expect("identity");
    assert_eq!(identity.release_mbid.as_deref(), Some(REMASTER));
    let extended = rig.store.track_identity("t2").expect("track identity");
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
    assert!(rig.store.album_identity("album-1").is_none());
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
    let identity = rig.store.album_identity("album-1").expect("identity");
    assert_eq!(identity.release_mbid.as_deref(), Some(RAM));
    let track = rig.store.track_identity("t8").expect("track identity");
    let expected = ram_track(RAM, 7);
    assert_eq!(track.release_track_mbid.as_deref(), Some(expected.as_str()));
}
