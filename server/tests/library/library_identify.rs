//! Identify briefs: proofs decide, curators overrule, names never merge.
//!
//! Every behavior reads as one identity brief: the album, what the library
//! believes about it, and why. The store is the SQLite one, on a private
//! copy of the migrated database; albums, tracks and artists are catalog
//! rows a test seeds. MusicBrainz is scripted (`FakeProviders`): nothing
//! here touches the network.

use droppedneedle_library::identify;

use std::sync::Arc;

use droppedneedle_library::matching::{CreditedArtist, Release, ReleaseTrack};
use identify::models::IdentifyJob;
use identify::models::JobState;
use identify::models::{
    AlbumIdentity, AutomaticSeal, DecisionSource, IdentificationOutcome, IdentifyKind,
    IdentityBrief, LocalAlbumFacts, LocalTrackFacts, RecallResult, ReviewState, TrackIdentity,
};
use identify::providers::FakeProviders;
use identify::queue::{
    LEASE_SECONDS, MAX_BACKOFF_SECONDS, PRIORITY_HISTORICAL_BACKLOG, PRIORITY_NEW_OR_CHANGED,
    PRIORITY_REVIEW_RETRY, PRIORITY_SUPPORTING_MAINTENANCE, backoff_secs, terminally_deferred,
};
use identify::rules::evaluate_overwrite;
use identify::service::{IdentifyDeps, IdentifyService};
use identify::sqlite::SqliteIdentifyStore;
use identify::stores::{AttemptLanding, land_job};
use rusqlite::{OptionalExtension as _, params};

use crate::common::{ScratchDir, migrated_db};

const GROUP_A: &str = "fc97b087-221c-4ea4-9dd9-5277a52eb84a";
const RELEASE_A1: &str = "aff0622e-7bd3-4fb6-9ca3-0fa19dd2340b";
const RELEASE_A2: &str = "0687c8a5-40a2-4a0c-bdc9-c1d80d94bef5";
const GROUP_B: &str = "dcff25f1-702d-3b5e-b0da-d48172e6e62a";
const RELEASE_B1: &str = "c85ad49c-6bfb-4bdc-96f8-f5f305a8799e";
const ARTIST_MBID: &str = "5441c29d-3602-4898-b1a1-b77fa23b8e50";
const RECORDING_1: &str = "beaf82cd-24f9-4163-b1a9-022339a30f77";
const RECORDING_2: &str = "5224cfc7-b3bb-4008-a41b-21b168dc631f";

/// The artist every seeded album belongs to.
const ALBUM_ARTIST: &str = "artist-1";

struct Rig {
    service: IdentifyService,
    store: Arc<SqliteIdentifyStore>,
    /// A second connection, for seeding catalog rows and reading them back.
    db: rusqlite::Connection,
    providers: Arc<FakeProviders>,
    _dir: ScratchDir,
}

fn rig_with_recall(recall: RecallResult) -> Rig {
    let dir = ScratchDir::new("lib-identify");
    let path = dir.join("app.db");
    migrated_db(&path);
    let db = droppedneedle_kernel::db::open_connection(&path).expect("side connection opens");
    let store = Arc::new(SqliteIdentifyStore::new(
        droppedneedle_kernel::db::Db::open_for_tests(&path).expect("store opens"),
    ));
    let providers = Arc::new(FakeProviders::with_recall(recall));
    let service = IdentifyService::new(IdentifyDeps {
        store: store.clone(),
        providers: providers.clone(),
    });
    let rig = Rig {
        service,
        store,
        db,
        providers,
        _dir: dir,
    };
    rig.seed_artist(ALBUM_ARTIST);
    rig
}

impl Rig {
    fn seed_artist(&self, artist: &str) {
        self.db
            .execute(
                "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, \
                 updated_at) VALUES (?1, ?1, ?1, 'person', 0, 0)",
                params![artist],
            )
            .expect("artist seeds");
    }

    /// The album and its indexed files as a scan leaves them: the store
    /// reads the album's facts back from these rows.
    fn seed_album(&self, facts: &LocalAlbumFacts) {
        let album = facts.local_album_id.as_str();
        self.db
            .execute(
                "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
                 album_artist_name, album_artist_id, grouping_source, created_at, updated_at) \
                 VALUES (?1, 'root-1', ?1, ?2, lower(?2), ?3, ?4, 'automatic', 0, 0)",
                params![album, facts.title, facts.album_artist_name, ALBUM_ARTIST],
            )
            .expect("album seeds");
        for track in &facts.tracks {
            self.db
                .execute(
                    "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, \
                     relative_path, path_hash, file_size_bytes, file_mtime_ns, stat_revision, \
                     tag_revision, title, title_folded, artist_name, album_title, \
                     album_title_folded, disc_number, track_number, duration_seconds, \
                     embedded_recording_mbid, file_format, ingest_source, imported_at, \
                     membership_source) \
                     VALUES (?1, ?2, 'root-1', '/music/' || ?1 || '.flac', ?1 || '.flac', ?1, 1, \
                     1, 'stat-1', 'tag-1', ?3, lower(?3), ?4, ?5, lower(?5), ?6, ?7, ?8, ?9, \
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
                    ],
                )
                .expect("track seeds");
        }
    }

    /// A rescan found the album's files changed on disk.
    fn rescan(&self, album: &str) {
        self.db
            .execute(
                "UPDATE local_tracks SET stat_revision = 'stat-2' WHERE local_album_id = ?1",
                params![album],
            )
            .expect("rescan lands");
    }
}

fn track_facts(id: &str, position: u32, recording: Option<&str>) -> LocalTrackFacts {
    LocalTrackFacts {
        local_track_id: id.to_owned(),
        title: format!("track {id}"),
        artist_name: "Some Artist".to_owned(),
        track_number: position,
        disc_number: 1,
        duration_secs: Some(200),
        recording_mbid: recording.map(str::to_owned),
        ..LocalTrackFacts::default()
    }
}

/// Facts for tracks `t1`, `t2`, ... in order, titled "track t1" and so on.
fn album_facts(album: &str, recordings: &[(&str, Option<&str>)]) -> LocalAlbumFacts {
    LocalAlbumFacts {
        local_album_id: album.to_owned(),
        title: "Some Album".to_owned(),
        album_artist_name: "Some Artist".to_owned(),
        tracks: recordings
            .iter()
            .enumerate()
            .map(|(index, (id, recording))| track_facts(id, index as u32 + 1, *recording))
            .collect(),
        ..LocalAlbumFacts::default()
    }
}

/// A release titled, ordered, and timed like `album_facts`, whose tracks
/// carry the given recordings: only the ids tell candidates apart.
fn release(group: &str, id: &str, recordings: &[&str]) -> Release {
    Release {
        id: id.to_owned(),
        release_group_id: group.to_owned(),
        title: "Some Album".to_owned(),
        artists: vec![CreditedArtist {
            id: ARTIST_MBID.to_owned(),
            name: "Some Artist".to_owned(),
            sort_name: None,
            join: String::new(),
        }],
        status: Some("Official".to_owned()),
        tracks: recordings
            .iter()
            .enumerate()
            .map(|(index, recording)| ReleaseTrack {
                isrcs: Vec::new(),
                genres: Vec::new(),
                id: format!("{id}-{index}"),
                recording_id: (*recording).to_owned(),
                title: format!("track t{}", index + 1),
                artists: Vec::new(),
                disc: 1,
                position: index as u32 + 1,
                absolute_position: index as u32 + 1,
                length_ms: Some(200_000),
            })
            .collect(),
        ..Release::default()
    }
}

/// The same release marked live: identifying it needs confirmation.
fn live(mut release: Release) -> Release {
    release.secondary_types = vec!["Live".to_owned()];
    release
}

fn recall(releases: Vec<Release>) -> RecallResult {
    RecallResult {
        releases,
        ..RecallResult::default()
    }
}

fn key(group: &str, release: &str) -> String {
    format!("{group}:{release}")
}

#[tokio::test]
async fn automatic_identity_is_revisable() {
    let first = recall(vec![release(GROUP_A, RELEASE_A1, &[RECORDING_1])]);
    let rig = rig_with_recall(first);
    rig.seed_album(&album_facts("album-1", &[("t1", Some(RECORDING_1))]));
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
    let report = rig
        .service
        .run_claimed_job("job-1", 0)
        .await
        .unwrap()
        .expect("report");
    assert_eq!(report.outcome, IdentificationOutcome::Identified);
    assert_eq!(report.job.state, JobState::Succeeded);
    assert_eq!(
        rig.store
            .album_identity("album-1")
            .unwrap()
            .expect("identity")
            .release_group_mbid
            .as_deref(),
        Some(GROUP_A)
    );

    // A stronger later pass revises the automatic row.
    *rig.providers.recall.lock().expect("recall") = Some(recall(vec![release(
        GROUP_B,
        RELEASE_B1,
        &[RECORDING_1, RECORDING_2],
    )]));
    rig.service
        .enqueue_album(
            "job-2",
            "album-1",
            IdentifyKind::Automatic,
            "rev-2",
            None,
            0,
            droppedneedle_kernel::db::Lane::Background,
        )
        .unwrap();
    rig.store.claim(0, 60_000).unwrap();
    let report = rig
        .service
        .run_claimed_job("job-2", 0)
        .await
        .unwrap()
        .expect("report");
    assert_eq!(report.outcome, IdentificationOutcome::Identified);
    let identity = rig
        .store
        .album_identity("album-1")
        .unwrap()
        .expect("identity");
    assert_eq!(identity.release_group_mbid.as_deref(), Some(GROUP_B));
    assert_eq!(identity.decision_source, DecisionSource::Automatic);
    let brief = rig.service.identity_brief("album-1").unwrap();
    assert!(brief.revisable);
    assert_eq!(brief.protected_source, None);
}

#[tokio::test]
async fn automatic_retracts_on_contradiction() {
    let rig = rig_with_recall(recall(vec![release(GROUP_A, RELEASE_A1, &[RECORDING_2])]));
    rig.seed_album(&album_facts("album-1", &[("t1", Some(RECORDING_1))]));
    rig.store
        .save_album_identity(AlbumIdentity {
            local_album_id: "album-1".to_owned(),
            provider: "musicbrainz".to_owned(),
            release_group_mbid: Some(GROUP_A.to_owned()),
            release_mbid: Some(RELEASE_A1.to_owned()),
            decision_source: DecisionSource::Automatic,
            row_revision: 1,
        })
        .unwrap();
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
    let report = rig
        .service
        .run_claimed_job("job-1", 0)
        .await
        .unwrap()
        .expect("report");
    assert_eq!(report.outcome, IdentificationOutcome::Contradictory);
    assert!(rig.store.album_identity("album-1").unwrap().is_none());
    // Nothing waits in a blocking review: the album keeps its own tags.
    assert_eq!(report.review_id, None);
}

#[tokio::test]
async fn manual_survives_rescan() {
    let rig = rig_with_recall(recall(vec![release(GROUP_B, RELEASE_B1, &[RECORDING_1])]));
    rig.seed_album(&album_facts("album-1", &[("t1", Some(RECORDING_1))]));
    rig.store
        .save_album_identity(AlbumIdentity {
            local_album_id: "album-1".to_owned(),
            provider: "musicbrainz".to_owned(),
            release_group_mbid: Some(GROUP_A.to_owned()),
            release_mbid: Some(RELEASE_A1.to_owned()),
            decision_source: DecisionSource::Manual,
            row_revision: 3,
        })
        .unwrap();
    // A rescan re-reads the files, then a fresh automatic pass disagrees.
    rig.rescan("album-1");
    rig.service
        .enqueue_album(
            "job-1",
            "album-1",
            IdentifyKind::Automatic,
            "rev-2",
            None,
            0,
            droppedneedle_kernel::db::Lane::Background,
        )
        .unwrap();
    rig.store.claim(0, 60_000).unwrap();
    let report = rig
        .service
        .run_claimed_job("job-1", 0)
        .await
        .unwrap()
        .expect("report");
    assert_eq!(report.reason_code, "CHOSEN_EDITION_KEPT");
    let identity = rig
        .store
        .album_identity("album-1")
        .unwrap()
        .expect("identity");
    assert_eq!(identity.release_group_mbid.as_deref(), Some(GROUP_A));
    assert_eq!(identity.decision_source, DecisionSource::Manual);
    assert_eq!(identity.row_revision, 3);
    assert_eq!(report.review_id, None);
    let brief: IdentityBrief = rig.service.identity_brief("album-1").unwrap();
    assert!(!brief.revisable);
    assert_eq!(brief.protected_source, Some(DecisionSource::Manual));
}

/// A curator's row can land after an automatic attempt read the album and
/// found nothing there. The store refuses the automatic write by itself,
/// so the attempt's own check is not the only guard.
#[test]
fn automatic_writes_never_replace_a_curators_row() {
    let rig = rig_with_recall(RecallResult::default());
    rig.seed_album(&album_facts("album-1", &[("t1", Some(RECORDING_1))]));
    let chosen = AlbumIdentity {
        local_album_id: "album-1".to_owned(),
        provider: "musicbrainz".to_owned(),
        release_group_mbid: Some(GROUP_A.to_owned()),
        release_mbid: Some(RELEASE_A1.to_owned()),
        decision_source: DecisionSource::Manual,
        row_revision: 3,
    };
    rig.store.save_album_identity(chosen.clone()).unwrap();

    let sealed = rig
        .store
        .seal_automatic(&AutomaticSeal {
            local_album_id: "album-1".to_owned(),
            release_group_mbid: GROUP_B.to_owned(),
            release_mbid: Some(RELEASE_B1.to_owned()),
            tracks: Vec::new(),
            flag: None,
        })
        .unwrap();
    assert!(!sealed);
    rig.store
        .save_album_identity(AlbumIdentity {
            release_group_mbid: Some(GROUP_B.to_owned()),
            release_mbid: Some(RELEASE_B1.to_owned()),
            decision_source: DecisionSource::Automatic,
            row_revision: 1,
            ..chosen.clone()
        })
        .unwrap();
    rig.store.clear_album_identity("album-1").unwrap();

    assert_eq!(rig.store.album_identity("album-1").unwrap(), Some(chosen));
}

#[tokio::test]
async fn legacy_import_survives_rescan() {
    let rig = rig_with_recall(recall(vec![release(GROUP_B, RELEASE_B1, &[RECORDING_1])]));
    rig.seed_album(&album_facts("album-1", &[("t1", Some(RECORDING_1))]));
    rig.store
        .save_album_identity(AlbumIdentity {
            local_album_id: "album-1".to_owned(),
            provider: "musicbrainz".to_owned(),
            release_group_mbid: Some(GROUP_A.to_owned()),
            release_mbid: Some(RELEASE_A1.to_owned()),
            decision_source: DecisionSource::LegacyImport,
            row_revision: 1,
        })
        .unwrap();
    rig.rescan("album-1");
    rig.service
        .enqueue_album(
            "job-1",
            "album-1",
            IdentifyKind::Automatic,
            "rev-2",
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
        .expect("report");
    let identity = rig
        .store
        .album_identity("album-1")
        .unwrap()
        .expect("identity");
    assert_eq!(identity.release_group_mbid.as_deref(), Some(GROUP_A));
    assert_eq!(identity.decision_source, DecisionSource::LegacyImport);
}

#[tokio::test]
async fn automatic_pass_skips_protected_track_rows() {
    let rig = rig_with_recall(recall(vec![release(
        GROUP_A,
        RELEASE_A1,
        &[RECORDING_1, RECORDING_2],
    )]));
    rig.seed_album(&album_facts(
        "album-1",
        &[("t1", Some(RECORDING_1)), ("t2", Some(RECORDING_2))],
    ));
    rig.store
        .save_track_identity(TrackIdentity {
            local_track_id: "t2".to_owned(),
            provider: "musicbrainz".to_owned(),
            recording_mbid: Some(RECORDING_2.to_owned()),
            release_track_mbid: None,
            decision_source: DecisionSource::Manual,
            row_revision: 7,
        })
        .unwrap();
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
        .expect("report");
    // The automatic track sealed; the manual track kept its own row.
    assert_eq!(
        rig.store
            .track_identity("t1")
            .unwrap()
            .expect("t1")
            .decision_source,
        DecisionSource::Automatic
    );
    let kept = rig.store.track_identity("t2").unwrap().expect("t2");
    assert_eq!(kept.decision_source, DecisionSource::Manual);
    assert_eq!(kept.row_revision, 7);
}

#[test]
fn overwrite_matrix_matches_v2() {
    use identify::rules::OverwriteVerdict;
    assert_eq!(
        evaluate_overwrite(None, Some(GROUP_A), None, false),
        OverwriteVerdict::MayWrite
    );
    assert_eq!(
        evaluate_overwrite(
            Some(DecisionSource::Automatic),
            Some(GROUP_B),
            Some(GROUP_A),
            false
        ),
        OverwriteVerdict::MayWrite
    );
    assert_eq!(
        evaluate_overwrite(
            Some(DecisionSource::Manual),
            Some(GROUP_B),
            Some(GROUP_A),
            false
        ),
        OverwriteVerdict::ProtectedFileReview
    );
    assert_eq!(
        evaluate_overwrite(
            Some(DecisionSource::LegacyImport),
            Some(GROUP_B),
            Some(GROUP_A),
            true
        ),
        OverwriteVerdict::ProtectedFileReview
    );
    assert_eq!(
        evaluate_overwrite(
            Some(DecisionSource::Manual),
            Some(GROUP_A),
            Some(GROUP_A),
            true
        ),
        OverwriteVerdict::QuietReconfirm
    );
    // Agreement alone decides: the contradiction flag plays no part.
    assert_eq!(
        evaluate_overwrite(
            Some(DecisionSource::Manual),
            Some(GROUP_A),
            Some(GROUP_A),
            false
        ),
        OverwriteVerdict::QuietReconfirm
    );
    assert_eq!(
        evaluate_overwrite(None, Some(GROUP_A), Some(GROUP_A), true),
        OverwriteVerdict::MayWrite
    );
}

/// File a pending review the way an edition correction does when it drops
/// editions: the dropped editions are the candidates, keyed `group:release`.
/// Identification itself never files one (it always picks a best guess).
fn file_edition_review(rig: &Rig, review_id: &str, album: &str) {
    let candidate = |group: &str, release: &str| identify::models::CandidateEvidence {
        candidate_key: key(group, release),
        release_group_mbid: group.to_owned(),
        release_mbid: Some(release.to_owned()),
        album_title: "Album".to_owned(),
        album_artist_name: String::new(),
        track_evidence: Vec::new(),
        score: 0.0,
        reason_code: "EDITION_FILES_CHANGED".to_owned(),
        distance: 0.0,
        penalties: Vec::new(),
    };
    identify::review::file_review(
        rig.store.as_ref(),
        review_id,
        album,
        "EDITION_FILES_CHANGED",
        vec![
            candidate(GROUP_A, RELEASE_A1),
            candidate(GROUP_B, RELEASE_B1),
        ],
    )
    .unwrap();
}

#[tokio::test]
async fn review_approve_seals_manual_and_holds() {
    let rig = rig_with_recall(recall(vec![release(GROUP_B, RELEASE_B1, &[RECORDING_1])]));
    rig.seed_album(&album_facts("album-1", &[("t1", Some(RECORDING_1))]));
    file_edition_review(&rig, "rev-1", "album-1");
    let review = "rev-1";
    // Unknown candidate keys never approve.
    assert_eq!(
        rig.service.approve_candidate(review, "curator-1", "nope"),
        Ok(false)
    );
    assert_eq!(
        rig.service
            .approve_candidate(review, "curator-1", &key(GROUP_A, RELEASE_A1)),
        Ok(true)
    );
    // Settling twice is a no-op.
    assert_eq!(
        rig.service
            .approve_candidate(review, "curator-1", &key(GROUP_B, RELEASE_B1)),
        Ok(false)
    );
    let stored = rig.store.get(review).unwrap().expect("review");
    assert_eq!(stored.state, ReviewState::Approved);
    let identity = rig
        .store
        .album_identity("album-1")
        .unwrap()
        .expect("identity");
    assert_eq!(identity.release_group_mbid.as_deref(), Some(GROUP_A));
    assert_eq!(identity.decision_source, DecisionSource::Manual);

    // A later automatic pass, whose evidence points at another group,
    // cannot move the curator's choice.
    rig.service
        .enqueue_album(
            "job-2",
            "album-1",
            IdentifyKind::Automatic,
            "rev-2",
            None,
            0,
            droppedneedle_kernel::db::Lane::Background,
        )
        .unwrap();
    rig.store.claim(0, 60_000).unwrap();
    rig.service
        .run_claimed_job("job-2", 0)
        .await
        .unwrap()
        .expect("report");
    let held = rig
        .store
        .album_identity("album-1")
        .unwrap()
        .expect("identity");
    assert_eq!(held.release_group_mbid.as_deref(), Some(GROUP_A));
    assert_eq!(held.decision_source, DecisionSource::Manual);
}

#[test]
fn review_reject_keeps_tagged() {
    let rig = rig_with_recall(RecallResult::default());
    rig.seed_album(&album_facts("album-1", &[("t1", Some(RECORDING_1))]));
    file_edition_review(&rig, "rev-1", "album-1");
    let review = "rev-1";
    assert!(rig.service.reject_candidates(review, "curator-1").unwrap());
    assert!(!rig.service.reject_candidates(review, "curator-1").unwrap());
    assert_eq!(
        rig.service
            .approve_candidate(review, "curator-1", &key(GROUP_A, RELEASE_A1)),
        Ok(false)
    );
    assert_eq!(
        rig.store.get(review).unwrap().expect("review").state,
        ReviewState::Rejected
    );
    assert!(rig.store.album_identity("album-1").unwrap().is_none());
    assert!(!rig.service.review_is_pending(review).unwrap());
    assert!(rig.service.pending_reviews("album-1").unwrap().is_empty());
}

/// A live release needs confirmation: without release-track ids on the
/// files the best guess is still kept (flagged unconfirmed), never left
/// undecided.
#[tokio::test]
async fn edition_uncertain_keeps_the_best_guess() {
    let rig = rig_with_recall(recall(vec![
        live(release(GROUP_A, RELEASE_A1, &[RECORDING_1, RECORDING_2])),
        live(release(GROUP_A, RELEASE_A2, &[RECORDING_1, RECORDING_2])),
    ]));
    rig.seed_album(&album_facts("album-1", &[("t1", None), ("t2", None)]));
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
    let report = rig
        .service
        .run_claimed_job("job-1", 0)
        .await
        .unwrap()
        .expect("report");
    assert_eq!(report.outcome, IdentificationOutcome::EditionUncertain);
    let identity = rig
        .store
        .album_identity("album-1")
        .unwrap()
        .expect("identity");
    assert_eq!(identity.release_group_mbid.as_deref(), Some(GROUP_A));
    assert!(identity.release_mbid.is_some());
    assert_eq!(identity.decision_source, DecisionSource::Automatic);
}

#[tokio::test]
async fn held_exact_survives_weaker_tier() {
    let rig = rig_with_recall(recall(vec![
        live(release(GROUP_A, RELEASE_A1, &[RECORDING_1, RECORDING_2])),
        live(release(GROUP_A, RELEASE_A2, &[RECORDING_1, RECORDING_2])),
    ]));
    rig.seed_album(&album_facts("album-1", &[("t1", None), ("t2", None)]));
    rig.store
        .save_album_identity(AlbumIdentity {
            local_album_id: "album-1".to_owned(),
            provider: "musicbrainz".to_owned(),
            release_group_mbid: Some(GROUP_A.to_owned()),
            release_mbid: Some(RELEASE_A1.to_owned()),
            decision_source: DecisionSource::Automatic,
            row_revision: 1,
        })
        .unwrap();
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
        .expect("report");
    // Same-group weaker evidence must not demote the sealed exact.
    let identity = rig
        .store
        .album_identity("album-1")
        .unwrap()
        .expect("identity");
    assert_eq!(identity.release_mbid.as_deref(), Some(RELEASE_A1));
}

#[test]
fn queue_claims_by_priority_then_backs_off() {
    assert_eq!(LEASE_SECONDS, 60);
    let mut ladder = [
        PRIORITY_NEW_OR_CHANGED,
        PRIORITY_REVIEW_RETRY,
        PRIORITY_HISTORICAL_BACKLOG,
        PRIORITY_SUPPORTING_MAINTENANCE,
    ];
    ladder.sort();
    assert_eq!(
        ladder,
        [
            PRIORITY_NEW_OR_CHANGED,
            PRIORITY_REVIEW_RETRY,
            PRIORITY_HISTORICAL_BACKLOG,
            PRIORITY_SUPPORTING_MAINTENANCE,
        ]
    );
    let rig = rig_with_recall(RecallResult::default());
    for album in ["album-b", "album-m", "album-a"] {
        rig.seed_album(&album_facts(album, &[]));
    }
    rig.service
        .enqueue_album(
            "job-backlog",
            "album-b",
            IdentifyKind::Historical,
            "r",
            None,
            0,
            droppedneedle_kernel::db::Lane::Background,
        )
        .unwrap();
    rig.service
        .enqueue_album(
            "job-manual",
            "album-m",
            IdentifyKind::Manual,
            "r",
            None,
            0,
            droppedneedle_kernel::db::Lane::Background,
        )
        .unwrap();
    rig.service
        .enqueue_album(
            "job-auto",
            "album-a",
            IdentifyKind::Automatic,
            "r",
            None,
            0,
            droppedneedle_kernel::db::Lane::Background,
        )
        .unwrap();
    assert_eq!(rig.store.jobs_for_album("album-a").unwrap().len(), 1);
    let first = rig
        .store
        .claim(0, LEASE_SECONDS * 1000)
        .unwrap()
        .expect("claim");
    assert_eq!(first.id, "job-auto");
    assert_eq!(first.priority, PRIORITY_NEW_OR_CHANGED);
    // Backoff ladder doubles from 30 seconds up to the cap.
    let ladder: Vec<u64> = (1..=9).map(backoff_secs).collect();
    assert_eq!(ladder, vec![30, 60, 120, 240, 480, 960, 1920, 3840, 7680]);
    assert_eq!(MAX_BACKOFF_SECONDS, 7680);
    assert_eq!(backoff_secs(0), 0);
    assert!(!terminally_deferred(9));
    assert!(terminally_deferred(10));
}

#[tokio::test]
async fn provider_deferred_job_waits_then_terminals() {
    let rig = rig_with_recall(RecallResult {
        provider_deferred: true,
        failure_code: Some("musicbrainz_unavailable".to_owned()),
        ..RecallResult::default()
    });
    rig.seed_album(&album_facts("album-1", &[("t1", Some(RECORDING_1))]));
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
    let report = rig
        .service
        .run_claimed_job("job-1", 1_000)
        .await
        .unwrap()
        .expect("report");
    assert_eq!(report.outcome, IdentificationOutcome::ProviderDeferred);
    let job = rig.store.job("job-1").unwrap().expect("job");
    assert_eq!(job.state, identify::models::JobState::Deferred);
    assert_eq!(job.not_before_ms, 1_000 + 30_000);
    // No identity was written on a deferred attempt.
    assert!(rig.store.album_identity("album-1").unwrap().is_none());
}

#[test]
fn brief_carries_the_whole_case() {
    let rig = rig_with_recall(RecallResult::default());
    let empty = rig.service.identity_brief("album-1").unwrap();
    assert!(empty.revisable);
    assert_eq!(empty.reason_code, "NO_IDENTITY");
    assert_eq!(empty.pending_review_id, None);
    let fresh = IdentityBrief::unresolved("album-9", "NO_CANDIDATE");
    assert_eq!(fresh.local_album_id, "album-9");
    assert!(fresh.revisable);
    assert!(fresh.identity.is_none());
}

#[test]
fn job_landings_cover_done_fail_attention_defer() {
    let mut job = IdentifyJob {
        id: "job-1".to_owned(),
        local_album_id: "album-1".to_owned(),
        kind: IdentifyKind::Automatic,
        priority: PRIORITY_NEW_OR_CHANGED,
        state: identify::models::JobState::Running,
        attempts: 0,
        not_before_ms: 0,
        input_revision: "r".to_owned(),
        requested_by_user_id: None,
        failure_code: None,
    };
    land_job(&mut job, AttemptLanding::Done, 0, None);
    assert_eq!(job.state, JobState::Succeeded);
    land_job(&mut job, AttemptLanding::Failed, 0, Some("boom"));
    assert_eq!(job.state, JobState::Failed);
    assert_eq!(job.failure_code.as_deref(), Some("boom"));
    land_job(&mut job, AttemptLanding::Attention, 0, Some("stuck"));
    assert_eq!(job.state, JobState::Attention);
    job.attempts = 9;
    land_job(&mut job, AttemptLanding::Deferred, 0, Some("busy"));
    assert_eq!(job.state, JobState::Attention);
    assert!(terminally_deferred(job.attempts));
}

/// Migration 0011 adds the identify tables on a fresh database and on one
/// migrated through 0010, keeping the catalog rows already there.
#[tokio::test]
async fn identify_state_migration_upgrades_in_place() {
    use droppedneedle_kernel::schema::{MIGRATOR, apply_migrations};
    use sqlx::sqlite::SqlitePoolOptions;

    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("pool opens");
    let through_0010 = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(
            MIGRATOR
                .migrations
                .iter()
                .filter(|migration| migration.version <= 10)
                .cloned()
                .collect(),
        ),
        ignore_missing: false,
        locking: true,
        no_tx: false,
    };
    through_0010.run(&pool).await.expect("0001-0010 apply");
    sqlx::query(
        "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at) \
         VALUES ('artist', 'Artist', 'artist', 'unknown', 0, 0); \
         INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_id, grouping_source, created_at, updated_at) \
         VALUES ('album', 'r', 'k', 'Album', 'album', 'artist', 'automatic', 0, 0);",
    )
    .execute(&pool)
    .await
    .expect("catalog seeds");

    apply_migrations(&pool).await.expect("0011 applies");
    let albums: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM local_albums")
        .fetch_one(&pool)
        .await
        .expect("albums count");
    assert_eq!(albums, 1);
    for table in ["library_identify_jobs", "library_identify_reviews"] {
        let found: Option<String> =
            sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?1")
                .bind(table)
                .fetch_optional(&pool)
                .await
                .expect("schema reads");
        assert_eq!(found.as_deref(), Some(table));
    }
}

// ---------------------------------------------------------------------------
// Artists linked to MusicBrainz from their album's accepted match.
// ---------------------------------------------------------------------------

const LINK_X: &str = "aaaaaaaa-0000-4000-8000-00000000000a";
const LINK_Y: &str = "aaaaaaaa-0000-4000-8000-00000000000b";
const LINK_Z: &str = "aaaaaaaa-0000-4000-8000-00000000000c";
const LINK_RELEASE: &str = "bbbbbbbb-0000-4000-8000-000000000001";
const LINK_GROUP: &str = "bbbbbbbb-0000-4000-8000-000000000002";

/// One credit as a test seeds it: the artist row, the name it is credited
/// as, and the MusicBrainz id its file gave.
type Seeded<'a> = (&'a str, &'a str, Option<&'a str>);

/// A catalog whose albums' matches link their artists: rows the test
/// seeds, a release document on file, and the identify store that works
/// the links out.
struct Links {
    store: SqliteIdentifyStore,
    db: rusqlite::Connection,
    _dir: ScratchDir,
}

impl Links {
    fn new() -> Self {
        let dir = ScratchDir::new("lib-artist-links");
        let path = dir.join("app.db");
        migrated_db(&path);
        let db = droppedneedle_kernel::db::open_connection(&path).expect("side connection opens");
        db.execute(
            "INSERT INTO auth_users (id, display_name, role, created_at) \
             VALUES ('admin', 'Admin', 'admin', '2026-01-01T00:00:00Z')",
            [],
        )
        .expect("admin seeds");
        Self {
            store: SqliteIdentifyStore::new(
                droppedneedle_kernel::db::Db::open_for_tests(&path).expect("store opens"),
            ),
            db,
            _dir: dir,
        }
    }

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

    fn credits(&self, table: &str, key: &str, subject: &str, credits: &[Seeded<'_>]) {
        for (position, (artist, name, mbid)) in credits.iter().enumerate() {
            self.db
                .execute(
                    &format!(
                        "INSERT INTO {table} ({key}, position, local_artist_id, role, \
                         credited_name, join_phrase, artist_mbid) \
                         VALUES (?1, ?2, ?3, 'main', ?4, '', ?5)"
                    ),
                    params![subject, position as i64, artist, name, mbid],
                )
                .expect("credit seeds");
        }
    }

    /// An album credited as `credits` with its tracks, each credited as
    /// given, matched by `source` to the release with each track mapped to
    /// the release track of its number.
    fn album(
        &self,
        album: &str,
        credits: &[Seeded<'_>],
        tracks: &[(&str, &[Seeded<'_>])],
        source: &str,
    ) {
        self.db
            .execute(
                "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
                 album_artist_name, album_artist_id, grouping_source, created_at, updated_at) \
                 VALUES (?1, 'root-1', ?1, 'Album', 'album', ?2, ?3, 'automatic', 0, 0)",
                params![album, credits[0].1, credits[0].0],
            )
            .expect("album seeds");
        self.credits("local_album_artists", "local_album_id", album, credits);
        self.db
            .execute(
                "INSERT INTO local_album_external_identities (local_album_id, provider, \
                 release_group_mbid, release_mbid, decision_source, selected_at) \
                 VALUES (?1, 'musicbrainz', ?2, ?3, ?4, 1)",
                params![album, LINK_GROUP, LINK_RELEASE, source],
            )
            .expect("match seeds");
        for (index, (track, track_credits)) in tracks.iter().enumerate() {
            let number = index as i64 + 1;
            self.db
                .execute(
                    "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, \
                     relative_path, path_hash, file_size_bytes, file_mtime_ns, stat_revision, \
                     tag_revision, title, title_folded, artist_name, album_title, \
                     album_title_folded, disc_number, track_number, file_format, \
                     ingest_source, imported_at, membership_source) \
                     VALUES (?1, ?2, 'root-1', '/music/' || ?1 || '.flac', ?1 || '.flac', ?1, \
                     1, 1, 'stat-1', 'tag-1', 'Song ' || ?3, 'song ' || ?3, ?4, 'Album', \
                     'album', 1, ?3, 'flac', 'scan', 0, 'automatic')",
                    params![track, album, number, track_credits[0].1],
                )
                .expect("track seeds");
            self.credits(
                "local_track_artists",
                "local_track_id",
                track,
                track_credits,
            );
            self.db
                .execute(
                    "INSERT INTO local_track_external_identities (local_track_id, provider, \
                     recording_mbid, release_mbid, release_track_mbid, decision_source, \
                     selected_at) VALUES (?1, 'musicbrainz', 'rec-' || ?2, ?3, 'rt-' || ?2, ?4, 1)",
                    params![track, number, LINK_RELEASE, source],
                )
                .expect("track mapping seeds");
        }
    }

    /// The release on file: credited to `artists`, with `tracks` tracks
    /// each credited to `track_artists`.
    fn release(&self, artists: &[(&str, &str)], tracks: u32, track_artists: &[(&str, &str)]) {
        let credited = |artists: &[(&str, &str)]| -> Vec<CreditedArtist> {
            artists
                .iter()
                .map(|(id, name)| CreditedArtist {
                    id: (*id).to_owned(),
                    name: (*name).to_owned(),
                    sort_name: None,
                    join: String::new(),
                })
                .collect()
        };
        self.store
            .save_release(
                &Release {
                    id: LINK_RELEASE.to_owned(),
                    release_group_id: LINK_GROUP.to_owned(),
                    title: "Album".to_owned(),
                    artists: credited(artists),
                    tracks: (1..=tracks)
                        .map(|number| ReleaseTrack {
                            id: format!("rt-{number}"),
                            recording_id: format!("rec-{number}"),
                            title: format!("Song {number}"),
                            artists: credited(track_artists),
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

    fn link_of(&self, artist: &str) -> Option<(String, String)> {
        self.db
            .query_row(
                "SELECT provider_artist_id, decision_source FROM local_artist_external_identities \
                 WHERE local_artist_id = ?1",
                params![artist],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .expect("link reads")
    }

    /// Every proof as (kind, subject, position, artist, id).
    fn proofs(&self) -> Vec<(String, String, i64, String, String)> {
        self.db
            .prepare(
                "SELECT subject_kind, subject_id, credit_position, local_artist_id, artist_mbid \
                 FROM library_artist_credit_proofs \
                 ORDER BY subject_kind, subject_id, credit_position",
            )
            .expect("proofs prepare")
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })
            .expect("proofs read")
            .collect::<rusqlite::Result<_>>()
            .expect("proof rows")
    }

    fn state(&self, album: &str) -> Option<String> {
        self.db
            .query_row(
                "SELECT state FROM library_artist_reconciliation_state WHERE local_album_id = ?1",
                params![album],
                |row| row.get(0),
            )
            .optional()
            .expect("state reads")
    }

    /// Everything a pass may write, with its revisions and times.
    fn written(&self) -> String {
        self.db
            .query_row(
                "SELECT COALESCE((SELECT group_concat(id || ':' || row_revision || ':' || updated_at) \
                 FROM local_artists), '') || '|' || COALESCE((SELECT group_concat(local_artist_id \
                 || ':' || provider_artist_id || ':' || row_revision || ':' || selected_at) \
                 FROM local_artist_external_identities), '') || '|' || COALESCE((SELECT \
                 group_concat(subject_id || ':' || row_revision || ':' || updated_at) \
                 FROM library_artist_credit_proofs), '') || '|' || COALESCE((SELECT \
                 group_concat(local_album_id || ':' || input_revision || ':' || updated_at) \
                 FROM library_artist_reconciliation_state), '') || '|' || COALESCE((SELECT \
                 value FROM library_catalog_revision), 0)",
                [],
                |row| row.get(0),
            )
            .expect("written reads")
    }

    /// An album with no match, credited as `credits`.
    fn unmatched_album(&self, album: &str, credits: &[Seeded<'_>]) {
        self.db
            .execute(
                "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
                 album_artist_name, album_artist_id, grouping_source, created_at, updated_at) \
                 VALUES (?1, 'root-1', ?1, 'Other', 'other', ?2, ?3, 'automatic', 0, 0)",
                params![album, credits[0].1, credits[0].0],
            )
            .expect("album seeds");
        self.credits("local_album_artists", "local_album_id", album, credits);
    }

    /// One favorite and one play of the artist.
    fn listen(&self, artist: &str) {
        self.db
            .execute_batch(&format!(
                "INSERT INTO library_user_favorites (user_id, item_kind, item_id, created_at) \
                 VALUES ('admin', 'artist', '{artist}', 1); \
                 INSERT INTO library_play_history (id, user_id, local_artist_id, track_name, \
                 artist_name, played_at) VALUES ('play-{artist}', 'admin', '{artist}', 'Song', \
                 'Artist', '2026-01-01T00:00:00Z');"
            ))
            .expect("listening seeds");
    }

    fn count(&self, sql: &str) -> i64 {
        self.db
            .query_row(sql, [], |row| row.get(0))
            .unwrap_or_else(|error| panic!("{sql}: {error}"))
    }

    fn retired_into(&self, artist: &str) -> Option<String> {
        self.db
            .query_row(
                "SELECT retired_into_artist_id FROM local_artists WHERE id = ?1",
                params![artist],
                |row| row.get(0),
            )
            .expect("artist reads")
    }

    /// The audit rows of automatic merges.
    fn merges(&self) -> i64 {
        self.count(
            "SELECT COUNT(*) FROM library_catalog_actions \
             WHERE reason_code = 'AUTOMATIC_PROVIDER_PROVEN_ARTIST_CONVERGENCE'",
        )
    }

    fn proof(
        kind: &str,
        subject: &str,
        position: i64,
        artist: &str,
        mbid: &str,
    ) -> (String, String, i64, String, String) {
        (
            kind.to_owned(),
            subject.to_owned(),
            position,
            artist.to_owned(),
            mbid.to_owned(),
        )
    }
}

fn automatic(mbid: &str) -> Option<(String, String)> {
    Some((mbid.to_owned(), "automatic".to_owned()))
}

/// An accepted match whose album and tracks credit their artists as the
/// release does proves every position and links each artist by itself;
/// the album's state is projected. A second pass on nothing new writes
/// nothing.
#[test]
fn an_accepted_match_links_every_credited_artist() {
    let links = Links::new();
    links.artist("ar-a", "Alpha", 1.0);
    links.artist("ar-b", "Beta", 2.0);
    let alpha: &[Seeded<'_>] = &[("ar-a", "Alpha", None)];
    links.album(
        "al",
        &[("ar-a", "Alpha", None), ("ar-b", "Beta", None)],
        &[("t1", alpha), ("t2", alpha)],
        "automatic",
    );
    links.release(
        &[(LINK_X, "Alpha"), (LINK_Y, "Beta")],
        2,
        &[(LINK_X, "Alpha")],
    );
    links.store.sweep_artist_links().unwrap();
    assert_eq!(
        links.proofs(),
        vec![
            Links::proof("album", "al", 0, "ar-a", LINK_X),
            Links::proof("album", "al", 1, "ar-b", LINK_Y),
            Links::proof("track", "t1", 0, "ar-a", LINK_X),
            Links::proof("track", "t2", 0, "ar-a", LINK_X),
        ]
    );
    assert_eq!(links.link_of("ar-a"), automatic(LINK_X));
    assert_eq!(links.link_of("ar-b"), automatic(LINK_Y));
    assert_eq!(links.state("al").as_deref(), Some("projected"));

    let before = links.written();
    links.store.sweep_artist_links().unwrap();
    links.store.link_and_merge_album_artists("al").unwrap();
    links.store.link_album_artists("al").unwrap();
    assert_eq!(links.written(), before, "nothing new, nothing written");
}

/// An album credit "A & B" against a release crediting A and B does not
/// line up: no album proof, and the album's credits are ambiguous. Tracks
/// credited to A alone still link A.
#[test]
fn a_joined_album_credit_proves_nothing() {
    let links = Links::new();
    links.artist("ar-ab", "Alpha & Beta", 1.0);
    links.artist("ar-a", "Alpha", 2.0);
    links.album(
        "al",
        &[("ar-ab", "Alpha & Beta", None)],
        &[("t1", &[("ar-a", "Alpha", None)])],
        "automatic",
    );
    links.release(
        &[(LINK_X, "Alpha"), (LINK_Y, "Beta")],
        1,
        &[(LINK_X, "Alpha")],
    );
    links.store.sweep_artist_links().unwrap();
    assert_eq!(
        links.proofs(),
        vec![Links::proof("track", "t1", 0, "ar-a", LINK_X)]
    );
    assert_eq!(
        links.state("al").as_deref(),
        Some("ambiguous_credit_structure")
    );
    assert_eq!(links.link_of("ar-a"), automatic(LINK_X));
    assert_eq!(links.link_of("ar-ab"), None);
}

/// A credit spelled otherwise than the release credits it links nothing,
/// until the artist's MusicBrainz names are on file with that spelling
/// among them.
#[test]
fn another_spelling_links_only_through_the_artists_names() {
    use droppedneedle_library::matching::model::{
        ArtistAlias, ArtistName, ArtistNames, ReleaseExtras,
    };

    let links = Links::new();
    links.artist("ar-s", "Ryuichi Sakamoto", 1.0);
    links.album("al", &[("ar-s", "Ryuichi Sakamoto", None)], &[], "manual");
    links.release(&[(LINK_X, "坂本龍一")], 0, &[]);
    links.store.sweep_artist_links().unwrap();
    assert_eq!(links.link_of("ar-s"), None);
    assert!(links.proofs().is_empty());
    assert_eq!(links.state("al").as_deref(), Some("projected"));

    let names = ArtistNames {
        fetched_at_ms: 1,
        artists: std::collections::BTreeMap::from([(
            LINK_X.to_owned(),
            ArtistName {
                name: "坂本龍一".to_owned(),
                sort_name: None,
                aliases: Some(vec![ArtistAlias {
                    name: "Ryūichi Sakamoto".to_owned(),
                    ..ArtistAlias::default()
                }]),
            },
        )]),
    };
    links
        .store
        .graft_extras(
            LINK_RELEASE,
            &ReleaseExtras {
                relations: None,
                names: Some(names),
            },
        )
        .unwrap()
        .expect("names grafted");
    links.store.link_album_artists("al").unwrap();
    assert_eq!(links.link_of("ar-s"), automatic(LINK_X));
}

/// A best guess links nothing; once a person says it looks right, the
/// next sweep links its artist.
#[test]
fn a_best_guess_links_once_it_looks_right() {
    use droppedneedle_library::operations::choice;

    let links = Links::new();
    links.artist("ar-a", "Alpha", 1.0);
    links.album("al", &[("ar-a", "Alpha", None)], &[], "automatic");
    links
        .db
        .execute(
            "INSERT INTO library_album_match_state (local_album_id, state, reason_code, \
             release_mbid, updated_at) VALUES ('al', 'unconfirmed', 'BEST_GUESS', ?1, 1)",
            params![LINK_RELEASE],
        )
        .expect("best guess seeds");
    links.release(&[(LINK_X, "Alpha")], 0, &[]);
    links.store.sweep_artist_links().unwrap();
    assert_eq!(links.link_of("ar-a"), None);
    assert_eq!(links.state("al").as_deref(), Some("waiting_for_identity"));

    let tx = links.db.unchecked_transaction().expect("transaction");
    choice::confirm(&tx, "al", "admin", 2.0).expect("looks right");
    tx.commit().expect("commit");
    links.store.sweep_artist_links().unwrap();
    assert_eq!(links.link_of("ar-a"), automatic(LINK_X));
    assert_eq!(links.state("al").as_deref(), Some("projected"));
}

/// A row whose files carry another id is proven two artists, so it is
/// not linked and loses the app's link; a row a person linked to another
/// id keeps it, whatever its album proves.
#[test]
fn files_naming_another_artist_keep_the_row_unlinked() {
    let links = Links::new();
    links.artist("ar-a", "Alpha", 1.0);
    links.artist("ar-p", "Pe", 2.0);
    links.link("ar-a", LINK_X, "automatic");
    links.link("ar-p", LINK_Y, "manual");
    links.album(
        "al",
        &[("ar-a", "Alpha", Some(LINK_Y)), ("ar-p", "Pe", None)],
        &[],
        "automatic",
    );
    links.release(&[(LINK_X, "Alpha"), (LINK_Z, "Pe")], 0, &[]);
    links.store.sweep_artist_links().unwrap();
    assert_eq!(
        links.proofs(),
        vec![
            Links::proof("album", "al", 0, "ar-a", LINK_X),
            Links::proof("album", "al", 1, "ar-p", LINK_Z),
        ]
    );
    assert_eq!(links.link_of("ar-a"), None);
    assert_eq!(
        links.link_of("ar-p"),
        Some((LINK_Y.to_owned(), "manual".to_owned()))
    );
}

/// Taking the match off an album takes away, at the next sweep, its
/// proofs, its state and the links they made.
#[test]
fn detaching_the_match_takes_its_links_away() {
    use droppedneedle_library::operations::choice;

    let links = Links::new();
    links.artist("ar-a", "Alpha", 1.0);
    links.album(
        "al",
        &[("ar-a", "Alpha", None)],
        &[("t1", &[("ar-a", "Alpha", None)])],
        "automatic",
    );
    links.release(&[(LINK_X, "Alpha")], 1, &[(LINK_X, "Alpha")]);
    links.store.sweep_artist_links().unwrap();
    assert_eq!(links.link_of("ar-a"), automatic(LINK_X));

    let tx = links.db.unchecked_transaction().expect("transaction");
    choice::detach_by_person(&tx, "al", "WRONG_MATCH", "admin", 2.0)
        .expect("detached")
        .expect("a match to detach");
    tx.commit().expect("commit");
    links.store.sweep_artist_links().unwrap();
    assert!(links.proofs().is_empty());
    assert_eq!(links.state("al"), None);
    assert_eq!(links.link_of("ar-a"), None);
}

/// A row the app linked whose files are read again with another id loses
/// the link at the next sweep, with no change of match.
#[test]
fn files_that_turn_to_another_artist_lose_the_link() {
    let links = Links::new();
    links.artist("ar-a", "Alpha", 1.0);
    links.album("al", &[("ar-a", "Alpha", None)], &[], "automatic");
    links.release(&[(LINK_X, "Alpha")], 0, &[]);
    links.store.sweep_artist_links().unwrap();
    assert_eq!(links.link_of("ar-a"), automatic(LINK_X));

    links
        .db
        .execute(
            "UPDATE local_album_artists SET artist_mbid = ?1 WHERE local_artist_id = 'ar-a'",
            params![LINK_Y],
        )
        .expect("files read again");
    links.store.sweep_artist_links().unwrap();
    assert_eq!(links.link_of("ar-a"), None);
    assert_eq!(links.state("al").as_deref(), Some("projected"));
}

/// A row whose every credit a matched album proves to be x is merged,
/// right after the pass, into the row a person linked to x: its credits,
/// favorite and play move, its id stays as an alias, and the duplicate
/// artists list shows the merge as resolved. A second pass merges nothing.
#[test]
fn a_row_its_matched_album_proves_merges_into_the_linked_row() {
    use droppedneedle_library::reconcile::groups::build;
    use droppedneedle_library::reconcile::models::GroupState;
    use droppedneedle_library::reconcile::store::inputs;

    let links = Links::new();
    links.artist("ar-s", "Alpha", 1.0);
    links.artist("ar-r", "ALPHA", 2.0);
    links.link("ar-s", LINK_X, "manual");
    let alpha: &[Seeded<'_>] = &[("ar-r", "Alpha", None)];
    links.album("al", alpha, &[("t1", alpha)], "automatic");
    links.release(&[(LINK_X, "Alpha")], 1, &[(LINK_X, "Alpha")]);
    links.listen("ar-r");

    links.store.link_and_merge_album_artists("al").unwrap();
    assert_eq!(links.retired_into("ar-r").as_deref(), Some("ar-s"));
    assert_eq!(
        links.count(
            "SELECT COUNT(*) FROM local_artist_aliases WHERE alias = 'ar-r' \
             AND local_artist_id = 'ar-s' AND kind = 'merged_artist'"
        ),
        1
    );
    assert_eq!(
        links.count(
            "SELECT (SELECT COUNT(*) FROM local_album_artists WHERE local_artist_id = 'ar-s') \
             + (SELECT COUNT(*) FROM local_track_artists WHERE local_artist_id = 'ar-s')"
        ),
        2
    );
    assert_eq!(
        links.count(
            "SELECT (SELECT COUNT(*) FROM library_user_favorites WHERE item_id = 'ar-s') \
             + (SELECT COUNT(*) FROM library_play_history WHERE local_artist_id = 'ar-s')"
        ),
        2
    );
    assert_eq!(links.merges(), 1);
    let resolved: Vec<_> = build(&inputs(&links.db).expect("groups read"))
        .into_iter()
        .filter(|group| group.state == GroupState::ResolvedAutomatically)
        .collect();
    assert_eq!(resolved.len(), 1);
    let mut members: Vec<&str> = resolved[0].members.iter().map(|m| m.id.as_str()).collect();
    members.sort_unstable();
    assert_eq!(members, vec!["ar-r", "ar-s"]);

    links.store.link_and_merge_album_artists("al").unwrap();
    links.store.sweep_artist_links().unwrap();
    assert_eq!(links.merges(), 1, "nothing new to merge");
}

/// No merge while the row has a credit no proof covers, while the two
/// rows are marked as different people, or while the row the app linked
/// has a credit of its own no proof covers.
#[test]
fn a_row_not_proven_on_every_credit_is_not_merged() {
    let case = |survivor_link: &str| {
        let links = Links::new();
        links.artist("ar-s", "Alpha", 1.0);
        links.artist("ar-r", "ALPHA", 2.0);
        links.link("ar-s", LINK_X, survivor_link);
        let alpha: &[Seeded<'_>] = &[("ar-r", "Alpha", None)];
        links.album("al", alpha, &[("t1", alpha)], "automatic");
        links.release(&[(LINK_X, "Alpha")], 1, &[(LINK_X, "Alpha")]);
        links
    };
    let unmerged = |links: &Links| {
        links.store.link_and_merge_album_artists("al").unwrap();
        links.store.sweep_artist_links().unwrap();
        assert_eq!(links.retired_into("ar-r"), None);
        assert_eq!(links.merges(), 0);
    };

    let links = case("manual");
    links.unmatched_album("an", &[("ar-r", "Alpha", None)]);
    unmerged(&links);

    let links = case("manual");
    links
        .db
        .execute(
            "INSERT INTO library_artist_reconciliation_dismissals (left_artist_id, \
             right_artist_id, left_artist_revision, right_artist_revision, \
             dismissed_by_user_id, created_at, updated_at) \
             VALUES ('ar-r', 'ar-s', 1, 1, 'admin', 1, 1)",
            [],
        )
        .expect("dismissal seeds");
    unmerged(&links);

    let links = case("automatic");
    links.album("as", &[("ar-s", "Alpha", None)], &[], "automatic");
    links.unmatched_album("an", &[("ar-s", "Alpha", None)]);
    unmerged(&links);
    assert_eq!(links.link_of("ar-s"), automatic(LINK_X));
}

/// After a sweep finds no album to work through, writes by other
/// connections alone (plays, sessions, scans) do not walk the albums again
/// until a floor has passed; a write through the store itself does, at
/// the next sweep.
#[test]
fn the_album_walk_waits_out_other_writers() {
    let links = Links::new();
    links.release(&[(LINK_X, "Alpha")], 0, &[]);
    links.store.sweep_artist_links().unwrap();

    links.artist("ar-a", "Alpha", 1.0);
    links.album("al", &[("ar-a", "Alpha", None)], &[], "automatic");
    links.store.sweep_artist_links().unwrap();
    assert_eq!(
        links.link_of("ar-a"),
        None,
        "another connection's write waits"
    );
    assert_eq!(links.state("al"), None);

    links.release(&[(LINK_X, "Alpha")], 0, &[]);
    links.store.sweep_artist_links().unwrap();
    assert_eq!(links.link_of("ar-a"), automatic(LINK_X));
}

/// A write on one album moves only a bounded share of the library's
/// waiting continuations, so a backlog never pushes it past its time
/// budget. The identify sweep then works the whole backlog off: no play
/// is left on a track that continued into another.
#[test]
fn a_continuation_backlog_moves_in_bounded_turns() {
    let rig = rig_with_recall(RecallResult::default());
    rig.seed_album(&album_facts("album-1", &[("t1", Some(RECORDING_1))]));
    rig.db
        .execute(
            "INSERT INTO auth_users (id, display_name, role, created_at) \
             VALUES ('listener', 'Listener', 'user', '2026-01-01T00:00:00Z')",
            [],
        )
        .expect("listener seeds");
    let backlog = 450;
    for n in 0..backlog {
        rig.db
            .execute_batch(&format!(
                "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, \
                 relative_path, path_hash, file_size_bytes, file_mtime_ns, stat_revision, \
                 title, title_folded, album_title, album_title_folded, file_format, \
                 ingest_source, imported_at, membership_source, availability) \
                 VALUES ('gone-{n}', 'album-1', 'root-1', '/music/gone-{n}.flac', \
                 'gone-{n}.flac', 'gone-{n}', 1, 1, 'stat-1', 'Gone', 'gone', 'Some Album', \
                 'some album', 'flac', 'scan', 0, 'automatic', 'missing'); \
                 INSERT INTO local_track_aliases (alias, local_track_id, created_at) \
                 VALUES ('gone-{n}', 't1', 0); \
                 INSERT INTO library_play_history (id, user_id, local_track_id, track_name, \
                 artist_name, played_at) VALUES ('play-{n}', 'listener', 'gone-{n}', 'T', 'A', \
                 '2026-01-01T00:00:00Z');"
            ))
            .expect("continued track seeds");
    }
    let left = |rig: &Rig| -> i64 {
        rig.db
            .query_row(
                "SELECT COUNT(*) FROM library_play_history WHERE local_track_id LIKE 'gone-%'",
                [],
                |row| row.get(0),
            )
            .expect("plays count")
    };

    rig.store
        .save_track_identity(TrackIdentity {
            local_track_id: "t1".to_owned(),
            provider: "musicbrainz".to_owned(),
            recording_mbid: Some(RECORDING_1.to_owned()),
            release_track_mbid: Some("rt-1".to_owned()),
            decision_source: DecisionSource::Automatic,
            row_revision: 1,
        })
        .unwrap();
    let after_write = left(&rig);
    assert!(after_write > 0, "the write moved the whole backlog");
    assert!(after_write < backlog, "the write moved none of the backlog");

    rig.store.sweep_artist_links().unwrap();
    assert_eq!(left(&rig), 0, "the sweep works the backlog off");
    let on_live: i64 = rig
        .db
        .query_row(
            "SELECT COUNT(*) FROM library_play_history WHERE local_track_id = 't1'",
            [],
            |row| row.get(0),
        )
        .expect("plays count");
    assert_eq!(on_live, backlog);
}
