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

use droppedneedle::library::identify;
use droppedneedle::library::matching::decide::REVIEW_CEILING;
use droppedneedle::providers::Providers;
use droppedneedle::providers::musicbrainz::{
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
use identify::stores::{IdentityStore, QueueStore, ReleaseStore};
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
    let seen = Arc::new(Mutex::new(Vec::new()));
    let musicbrainz = MusicBrainzClient::official(
        FakeMb {
            routes: routes(),
            seen: seen.clone(),
            down,
        },
        MbPacing::new(Arc::new(Providers::unpaced())),
    );
    let dir = ScratchDir::new("lib-matching");
    let path = dir.join("app.db");
    migrated_db(&path);
    let db = droppedneedle::db::open_connection(&path).expect("side connection opens");
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
