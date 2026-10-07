//! Identification end to end against MusicBrainz release documents: the
//! live recall (lookup, search, tracklist fetch, redirects) over a scripted
//! transport, scored by the matching engine, landed by the service.
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
use identify::memory::{
    MemoryAliasStore, MemoryIdentityStore, MemoryProofStore, MemoryQueueStore, MemoryReleaseStore,
    MemoryReviewStore,
};
use identify::models::{
    EvidenceClass, IdentificationOutcome, IdentifyKind, LocalAlbumFacts, LocalTrackFacts,
};
use identify::providers::LiveProviders;
use identify::service::{IdentifyDeps, IdentifyService};
use identify::sources::NoFingerprints;
use identify::stores::{IdentityStore, QueueStore, ReleaseStore, ReviewStore};

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
    identities: Arc<MemoryIdentityStore>,
    queue: Arc<MemoryQueueStore>,
    reviews: Arc<MemoryReviewStore>,
    releases: Arc<MemoryReleaseStore>,
    seen: Arc<Mutex<Vec<String>>>,
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
    let identities = Arc::new(MemoryIdentityStore::default());
    let queue = Arc::new(MemoryQueueStore::default());
    let reviews = Arc::new(MemoryReviewStore::linked(identities.clone()));
    let releases = Arc::new(MemoryReleaseStore::default());
    let service = IdentifyService::new(IdentifyDeps {
        identities: identities.clone(),
        facts: identities.clone(),
        proofs: Arc::new(MemoryProofStore::default()),
        aliases: Arc::new(MemoryAliasStore::default()),
        queue: queue.clone(),
        reviews: reviews.clone(),
        releases: releases.clone(),
        providers: Arc::new(LiveProviders::new(
            musicbrainz,
            releases.clone(),
            NoFingerprints,
        )),
    });
    Rig {
        service,
        identities,
        queue,
        reviews,
        releases,
        seen,
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
    rig.identities.save_album_facts(facts);
    rig.service.enqueue_album(
        "job-1",
        "album-1",
        IdentifyKind::Automatic,
        "rev-1",
        None,
        0,
    );
    rig.queue.claim(0, 60_000);
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
    let identity = rig.identities.album_identity("album-1").expect("identity");
    assert_eq!(identity.release_mbid.as_deref(), Some(STANDARD));
    assert_eq!(identity.release_group_mbid.as_deref(), Some(GROUP));
    let track = rig.identities.track_identity("t2").expect("track identity");
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
    let stored = rig.releases.release(STANDARD, None).expect("stored");
    assert_eq!(stored.old_ids, vec![MERGED.to_owned()]);
    assert_eq!(stored.labels, vec!["Harbour Lights Records".to_owned()]);
}

/// Untagged files, one a minute shorter than any edition's track (left
/// unmatched) and one with a placeholder title: close enough to show a
/// curator, not close enough to seal.
#[tokio::test]
async fn messy_album_goes_to_review_with_its_distances() {
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
    assert!(rig.identities.album_identity("album-1").is_none());
    let review = rig
        .reviews
        .get(report.review_id.as_deref().expect("review"))
        .expect("review stored");
    assert_eq!(review.candidates.len(), 2);
    for candidate in &review.candidates {
        assert_eq!(candidate.release_group_mbid, GROUP);
        assert!(candidate.distance > 0.0 && candidate.distance <= REVIEW_CEILING);
        assert!(!candidate.penalties.is_empty());
        let tracks = &candidate.track_evidence;
        assert_eq!(tracks[0].classification, EvidenceClass::Supported);
        // Neither the unmatched file nor the bad pair is ever sealed.
        assert_eq!(tracks[1].classification, EvidenceClass::Unknown);
        assert_eq!(tracks[1].release_track_mbid, None);
        assert_eq!(tracks[3].classification, EvidenceClass::Unknown);
        assert!(tracks[3].release_track_mbid.is_some());
    }
    // A curator approving it finds the documents still on file.
    assert!(rig.releases.release(STANDARD, None).is_some());
    assert!(rig.releases.release(REMASTER, None).is_some());
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
    let identity = rig.identities.album_identity("album-1").expect("identity");
    assert_eq!(identity.release_mbid.as_deref(), Some(REMASTER));
    let extended = rig.identities.track_identity("t2").expect("track identity");
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
    assert!(rig.identities.album_identity("album-1").is_none());
}
