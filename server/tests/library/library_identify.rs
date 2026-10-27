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
        std::sync::Arc::new(droppedneedle_kernel::clock::SystemClock),
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
                     embedded_recording_mbid, embedded_release_group_mbid, file_format, \
                     ingest_source, imported_at, membership_source) \
                     VALUES (?1, ?2, 'root-1', '/music/' || ?1 || '.flac', ?1 || '.flac', ?1, 1, \
                     1, 'stat-1', 'tag-1', ?3, lower(?3), ?4, ?5, lower(?5), ?6, ?7, ?8, ?9, \
                     ?10, 'flac', 'scan', 0, 'automatic')",
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
                        track.release_group_mbid,
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

    assert_eq!(rig.store.album_identity("album-1").unwrap(), Some(chosen));
}

/// A retraction that lands after a person's match did leaves that match
/// and both its undo rows alone, whether the person chose it now or in v2.
#[test]
fn retract_never_takes_a_persons_match() {
    use droppedneedle_library::identify::models::{MatchFlag, MatchFlagState};
    use droppedneedle_library::identity::{Inputs, retract};

    let rig = rig_with_recall(RecallResult::default());
    let albums = [("album-1", "manual"), ("album-2", "legacy_import")];
    for (album, source) in albums {
        rig.seed_album(&album_facts(album, &[]));
        rig.carried(album, RELEASE_A1, source, 2);
    }
    let tables = || albums.map(|(album, _)| rig.identity_tables(album));
    let before = tables();
    let flag = MatchFlag {
        state: MatchFlagState::Unmatched,
        reason_code: "CONFLICTING_TRACK_EVIDENCE".to_owned(),
        release_mbid: None,
        candidates: Vec::new(),
    };
    let db = droppedneedle_kernel::db::Db::open_for_tests(&rig.path()).expect("db opens");
    db.write_blocking(
        droppedneedle_kernel::db::Lane::Background,
        "test.retract",
        |tx| {
            for (album, _) in albums {
                let revision = identify::sqlite::album_input_revision(tx, album)?;
                let inputs = Inputs {
                    input_revision: &revision,
                    now: 1.0,
                };
                retract(tx, album, true, &flag, inputs)?;
            }
            Ok::<_, droppedneedle_kernel::db::DbError>(())
        },
    )
    .unwrap();

    assert_eq!(tables(), before);
    assert_eq!(before[1].len(), 3, "{before:?}");
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

/// A live release needs confirmation: when the files carry no
/// release-track ids and fit it less than very closely (one length 15 s
/// off), the best guess is still kept (flagged unconfirmed), never left
/// undecided.
#[tokio::test]
async fn edition_uncertain_keeps_the_best_guess() {
    let rig = rig_with_recall(recall(vec![
        live(release(GROUP_A, RELEASE_A1, &[RECORDING_1, RECORDING_2])),
        live(release(GROUP_A, RELEASE_A2, &[RECORDING_1, RECORDING_2])),
    ]));
    let mut facts = album_facts("album-1", &[("t1", None), ("t2", None)]);
    facts.tracks[1].duration_secs = Some(215);
    rig.seed_album(&facts);
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
    let mut facts = album_facts("album-1", &[("t1", None), ("t2", None)]);
    facts.tracks[1].duration_secs = Some(215);
    rig.seed_album(&facts);
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
    // Same-group weaker evidence must not demote the sealed exact.
    assert_eq!(report.reason_code, "QUIET_RECONFIRM");
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
    let rig = rig_with_recall(RecallResult::default());
    rig.seed_album(&album_facts("album-1", &[]));
    let mut job: IdentifyJob = rig
        .service
        .enqueue_album(
            "job-1",
            "album-1",
            IdentifyKind::Automatic,
            "r",
            None,
            0,
            droppedneedle_kernel::db::Lane::Background,
        )
        .unwrap();
    assert_eq!(job.priority, PRIORITY_NEW_OR_CHANGED);
    job.state = JobState::Running;
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
                std::sync::Arc::new(droppedneedle_kernel::clock::SystemClock),
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
        let each = vec![track_artists; tracks as usize];
        self.release_tracks(artists, &each);
    }

    /// The release on file: credited to `artists`, with one track per
    /// entry of `tracks`, credited as it says.
    fn release_tracks(&self, artists: &[(&str, &str)], tracks: &[&[(&str, &str)]]) {
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
                    tracks: (1..)
                        .zip(tracks)
                        .map(|(number, track_artists)| ReleaseTrack {
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

    /// The album's state and reason, as `state REASON`.
    fn reason(&self, album: &str) -> Option<String> {
        self.db
            .query_row(
                "SELECT state || ' ' || reason_code FROM library_artist_reconciliation_state \
                 WHERE local_album_id = ?1",
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

/// A person approves a review whose release is not on file: the approval
/// commits, and when it answers the album's credits already wait for the
/// document. Once the document lands, the sweep proves and links them.
#[test]
fn an_approval_without_its_document_waits_and_the_sweep_links_later() {
    let links = Links::new();
    links.artist("ar-a", "Alpha", 1.0);
    let alpha: &[Seeded<'_>] = &[("ar-a", "Alpha", None)];
    links.album("al", alpha, &[("t1", alpha)], "automatic");
    links
        .db
        .execute(
            "INSERT INTO library_identify_reviews (id, local_album_id, reason_code, \
             candidates_json, state, created_ms, updated_ms) \
             VALUES ('rv', 'al', 'R', '[]', 'pending', 0, 0)",
            [],
        )
        .expect("review seeds");
    let approved = links
        .store
        .approve(&identify::stores::Approval {
            review_id: "rv".to_owned(),
            by_user_id: "admin".to_owned(),
            candidate_key: "c1".to_owned(),
            album: AlbumIdentity {
                local_album_id: "al".to_owned(),
                provider: "musicbrainz".to_owned(),
                release_group_mbid: Some(LINK_GROUP.to_owned()),
                release_mbid: Some(LINK_RELEASE.to_owned()),
                decision_source: DecisionSource::Manual,
                row_revision: 1,
            },
        })
        .expect("approval commits");
    assert!(approved);
    assert_eq!(
        links.reason("al").as_deref(),
        Some("waiting_for_identity RELEASE_DOCUMENT_MISSING")
    );
    assert!(links.proofs().is_empty());
    assert_eq!(links.link_of("ar-a"), None);

    links.release(&[(LINK_X, "Alpha")], 1, &[(LINK_X, "Alpha")]);
    links.store.sweep_artist_links().unwrap();
    assert!(!links.proofs().is_empty());
    assert_eq!(links.link_of("ar-a"), automatic(LINK_X));
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

/// A link carried from v2 on an album whose release document is not on
/// file yet stays: a missing document contradicts nothing. Files that do
/// contradict a link still take it away while the album waits.
#[test]
fn a_carried_link_waits_for_its_release_document() {
    let links = Links::new();
    links.artist("ar-a", "Alpha", 1.0);
    links.artist("ar-b", "Beta", 2.0);
    links.link("ar-a", LINK_X, "automatic");
    links.link("ar-b", LINK_Y, "automatic");
    links.album(
        "al",
        &[("ar-a", "Alpha", None), ("ar-b", "Beta", Some(LINK_Z))],
        &[
            ("t1", &[("ar-a", "Alpha", None)]),
            ("t2", &[("ar-b", "Beta", Some(LINK_Y))]),
        ],
        "automatic",
    );
    links.store.sweep_artist_links().unwrap();
    assert_eq!(links.link_of("ar-a"), automatic(LINK_X));
    assert_eq!(links.link_of("ar-b"), None, "files naming two ids");
    assert_eq!(
        links.reason("al").as_deref(),
        Some("waiting_for_identity RELEASE_DOCUMENT_MISSING")
    );
}

/// A release document that lands after the album's pass wakes the album:
/// the next sweep proves its artists and links them.
#[test]
fn a_release_document_that_lands_links_the_waiting_album() {
    let links = Links::new();
    links.artist("ar-a", "Alpha", 1.0);
    links.album(
        "al",
        &[("ar-a", "Alpha", None)],
        &[("t1", &[("ar-a", "Alpha", None)])],
        "automatic",
    );
    links.store.sweep_artist_links().unwrap();
    assert_eq!(
        links.reason("al").as_deref(),
        Some("waiting_for_identity RELEASE_DOCUMENT_MISSING")
    );
    assert_eq!(links.link_of("ar-a"), None);

    links.release(&[(LINK_X, "Alpha")], 1, &[(LINK_X, "Alpha")]);
    links.store.sweep_artist_links().unwrap();
    assert_eq!(
        links.proofs(),
        vec![
            Links::proof("album", "al", 0, "ar-a", LINK_X),
            Links::proof("track", "t1", 0, "ar-a", LINK_X),
        ]
    );
    assert_eq!(links.link_of("ar-a"), automatic(LINK_X));
    assert_eq!(links.state("al").as_deref(), Some("projected"));
}

/// A document learned through an id MusicBrainz merged wakes the album
/// matched to that id, also when the release's own document was already
/// on file.
#[test]
fn a_document_learned_through_a_merged_id_links_its_album() {
    const OLD: &str = "aaaaaaaa-0000-4000-8000-0000000000f0";
    let links = Links::new();
    links.artist("ar-a", "Alpha", 1.0);
    links.album("al", &[("ar-a", "Alpha", None)], &[], "automatic");
    links
        .db
        .execute(
            "UPDATE local_album_external_identities SET release_mbid = ?1",
            params![OLD],
        )
        .expect("matched to the merged id");
    links.release(&[(LINK_X, "Alpha")], 0, &[]);
    links.store.sweep_artist_links().unwrap();
    assert_eq!(
        links.reason("al").as_deref(),
        Some("waiting_for_identity RELEASE_DOCUMENT_MISSING")
    );

    let merged = Release {
        id: LINK_RELEASE.to_owned(),
        old_ids: vec![OLD.to_owned()],
        ..Release::default()
    };
    assert!(
        links
            .store
            .keep_old_ids(&merged, droppedneedle_kernel::db::Lane::Background)
            .unwrap()
    );
    links.store.sweep_artist_links().unwrap();
    assert_eq!(links.link_of("ar-a"), automatic(LINK_X));
    assert_eq!(links.state("al").as_deref(), Some("projected"));
}

/// An album whose release MusicBrainz no longer has keeps its artist's
/// link and is offered for identification again, so a new match can end
/// the wait.
#[test]
fn a_release_musicbrainz_no_longer_has_is_matched_again() {
    let links = Links::new();
    links.artist("ar-a", "Alpha", 1.0);
    links.link("ar-a", LINK_X, "automatic");
    links.album(
        "al",
        &[("ar-a", "Alpha", None)],
        &[("t1", &[("ar-a", "Alpha", None)])],
        "automatic",
    );
    links.store.sweep_artist_links().unwrap();
    assert_eq!(links.store.release_not_found(LINK_RELEASE).unwrap(), 1);
    links.store.sweep_artist_links().unwrap();
    assert_eq!(
        links.reason("al").as_deref(),
        Some("waiting_for_identity RELEASE_NOT_FOUND")
    );
    assert_eq!(links.link_of("ar-a"), automatic(LINK_X));
    assert_eq!(
        links.count(
            "SELECT COUNT(*) FROM library_identify_jobs \
             WHERE local_album_id = 'al' AND state = 'queued'"
        ),
        1
    );
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
    use droppedneedle_library::matching::model::{
        ArtistAlias, ArtistName, ArtistNames, ReleaseExtras,
    };

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

    // The file credits only a guest of the release's track: the album's
    // proof of its lead does not stand in for it.
    let links = Links::new();
    links.artist("ar-s", "Alpha", 1.0);
    links.artist("ar-r", "Gamma", 2.0);
    links.link("ar-s", LINK_X, "manual");
    links.album(
        "al",
        &[("ar-r", "Alpha", None)],
        &[("t1", &[("ar-r", "Gamma", None)])],
        "automatic",
    );
    links.release(
        &[(LINK_X, "Alpha")],
        1,
        &[(LINK_X, "Alpha"), (LINK_Z, "Gamma")],
    );
    unmerged(&links);

    // The row the app linked to x has a file holding x, but another
    // matched album proves it to be y, and nothing has settled its link
    // since.
    let links = Links::new();
    links.artist("ar-s", "Alpha", 1.0);
    links.artist("ar-r", "ALPHA", 2.0);
    links.artist("ar-q", "Qu", 3.0);
    links.unmatched_album("af", &[("ar-s", "Alpha", Some(LINK_X))]);
    links.album(
        "ay",
        &[("ar-q", "Qu", None)],
        &[
            ("ty1", &[("ar-q", "Qu", None)]),
            ("ty2", &[("ar-s", "Alpha", None)]),
        ],
        "automatic",
    );
    let alpha: &[Seeded<'_>] = &[("ar-r", "Alpha", None)];
    links.album("al", alpha, &[("t1", alpha)], "automatic");
    links.release_tracks(
        &[(LINK_X, "Alpha")],
        &[&[(LINK_X, "Alpha")], &[(LINK_Y, "Alpha")]],
    );
    links.store.link_album_artists("ay").unwrap();
    assert!(
        links
            .proofs()
            .contains(&Links::proof("track", "ty2", 0, "ar-s", LINK_Y))
    );
    links.link("ar-s", LINK_X, "automatic");
    unmerged(&links);

    // The file credits "Alpha & Gamma" as one artist. MusicBrainz lists
    // that as one of x's names, but it holds the guest's name too: the
    // album's proof does not stand in for it.
    let links = Links::new();
    links.artist("ar-s", "Alpha", 1.0);
    links.artist("ar-r", "ALPHA", 2.0);
    links.link("ar-s", LINK_X, "manual");
    links.album(
        "al",
        &[("ar-r", "Alpha", None)],
        &[("t1", &[("ar-r", "Alpha & Gamma", None)])],
        "automatic",
    );
    links.release(
        &[(LINK_X, "Alpha")],
        1,
        &[(LINK_X, "Alpha"), (LINK_Z, "Gamma")],
    );
    links
        .store
        .graft_extras(
            LINK_RELEASE,
            &ReleaseExtras {
                relations: None,
                names: Some(ArtistNames {
                    fetched_at_ms: 1,
                    artists: std::collections::BTreeMap::from([(
                        LINK_X.to_owned(),
                        ArtistName {
                            name: "Alpha".to_owned(),
                            sort_name: None,
                            aliases: Some(vec![ArtistAlias {
                                name: "Alpha & Gamma".to_owned(),
                                ..ArtistAlias::default()
                            }]),
                        },
                    )]),
                }),
            },
        )
        .unwrap()
        .expect("names grafted");
    unmerged(&links);
    assert!(
        !links
            .proofs()
            .iter()
            .any(|(kind, subject, ..)| kind == "track" && subject == "t1")
    );
}

/// Rows a matched album proves to be x merge into the row the app linked
/// to x when its files hold x, though a credit of its own on an album
/// with no accepted match is not proven. A track the file credits to the
/// lead alone, where the release credits the lead and a guest, is proven
/// by the album's proof of its one artist, so the row merges too.
#[test]
fn rows_a_matched_album_proves_merge_into_the_row_holding_the_id() {
    let links = Links::new();
    links.artist("ar-s", "Alpha", 1.0);
    links.artist("ar-r", "ALPHA", 2.0);
    links.link("ar-s", LINK_X, "automatic");
    links.unmatched_album("af", &[("ar-s", "Alpha", Some(LINK_X))]);
    links.unmatched_album("an", &[("ar-s", "Alpha", None)]);
    let alpha: &[Seeded<'_>] = &[("ar-r", "Alpha", None)];
    links.album("al", alpha, &[("t1", alpha)], "automatic");
    links.release(&[(LINK_X, "Alpha")], 1, &[(LINK_X, "Alpha")]);
    links.store.link_and_merge_album_artists("al").unwrap();
    assert_eq!(links.retired_into("ar-r").as_deref(), Some("ar-s"));
    assert_eq!(links.link_of("ar-s"), automatic(LINK_X));

    let links = Links::new();
    links.artist("ar-s", "Alpha", 1.0);
    links.artist("ar-r", "ALPHA", 2.0);
    links.link("ar-s", LINK_X, "manual");
    let alpha: &[Seeded<'_>] = &[("ar-r", "Alpha", None)];
    links.album("al", alpha, &[("t1", alpha), ("t2", alpha)], "automatic");
    links.release_tracks(
        &[(LINK_X, "Alpha")],
        &[
            &[(LINK_X, "Alpha")],
            &[(LINK_X, "Alpha"), (LINK_Z, "Gamma")],
        ],
    );
    links.store.link_and_merge_album_artists("al").unwrap();
    assert_eq!(links.retired_into("ar-r").as_deref(), Some("ar-s"));
    assert!(
        links
            .proofs()
            .contains(&Links::proof("track", "t2", 0, "ar-s", LINK_X)),
        "the lead's proof stands in for the track"
    );
}

/// A row whose proven credits all moved to another row (as a changed
/// sort name moves them) and that has nothing left of its own merges
/// into that row, its id kept as an alias, even on a pass that merges
/// nothing else. It stays when one of its plays is of a track now
/// credited to another artist (the play would be filed under the wrong
/// one), and when a link or a file proves the other row another artist.
#[test]
fn a_row_left_empty_follows_its_credits() {
    let case = |play_track: &str, before: &dyn Fn(&Links)| {
        let links = Links::new();
        links.artist("ar-o", "Alpha", 1.0);
        links.artist("ar-n", "Alpha", 2.0);
        links.artist("ar-z", "Zeta", 3.0);
        links.album(
            "al",
            &[("ar-o", "Alpha", None)],
            &[
                ("t1", &[("ar-o", "Alpha", None)]),
                ("t2", &[("ar-z", "Zeta", None)]),
            ],
            "automatic",
        );
        links.release(&[(LINK_X, "Alpha")], 2, &[(LINK_X, "Alpha")]);
        links.store.link_album_artists("al").unwrap();
        assert_eq!(links.link_of("ar-o"), automatic(LINK_X));
        links
            .db
            .execute_batch(&format!(
                "UPDATE local_album_artists SET local_artist_id = 'ar-n' \
                 WHERE local_artist_id = 'ar-o'; \
                 UPDATE local_track_artists SET local_artist_id = 'ar-n' \
                 WHERE local_artist_id = 'ar-o'; \
                 UPDATE local_albums SET album_artist_id = 'ar-n' WHERE id = 'al'; \
                 INSERT INTO library_play_history (id, user_id, local_artist_id, \
                 local_track_id, track_name, artist_name, played_at) VALUES ('play', 'admin', \
                 'ar-o', '{play_track}', 'Song', 'Alpha', '2026-01-01T00:00:00Z');"
            ))
            .expect("credits move");
        before(&links);
        links.store.link_album_artists("al").unwrap();
        links
    };
    let stays = |links: &Links| {
        assert_eq!(links.retired_into("ar-o"), None);
        assert_eq!(links.merges(), 0);
    };

    let links = case("t1", &|_| {});
    assert_eq!(links.retired_into("ar-o").as_deref(), Some("ar-n"));
    assert_eq!(links.link_of("ar-n"), automatic(LINK_X));
    assert_eq!(
        links.count(
            "SELECT COUNT(*) FROM local_artist_aliases WHERE alias = 'ar-o' \
             AND local_artist_id = 'ar-n' AND kind = 'merged_artist'"
        ),
        1
    );
    assert_eq!(
        links.count("SELECT COUNT(*) FROM library_play_history WHERE local_artist_id = 'ar-n'"),
        1
    );
    assert_eq!(links.merges(), 1);

    stays(&case("t2", &|_| {}));
    stays(&case("t1", &|links| links.link("ar-n", LINK_Y, "manual")));
    stays(&case("t1", &|links| {
        links.unmatched_album("ay", &[("ar-n", "Alpha", Some(LINK_Y))]);
    }));
}

/// A row whose proven credits moved while other credits of its own have
/// yet to (a scan commits a large retag in several windows) waits as an
/// open merge candidate, and follows them at the next sweep once the rest
/// have moved too.
#[test]
fn a_row_follows_its_credits_once_the_rest_have_moved() {
    let links = Links::new();
    links.artist("ar-o", "Alpha", 1.0);
    links.artist("ar-n", "Alpha", 2.0);
    let alpha: &[Seeded<'_>] = &[("ar-o", "Alpha", None)];
    links.album("al", alpha, &[("t1", alpha)], "automatic");
    links.unmatched_album("ac", alpha);
    links.release(&[(LINK_X, "Alpha")], 1, &[(LINK_X, "Alpha")]);
    links.store.link_album_artists("al").unwrap();
    let waiting = "SELECT COUNT(*) FROM local_artist_merge_candidates \
                   WHERE left_artist_id = 'ar-o' AND right_artist_id = 'ar-n' \
                   AND reason_code = 'CREDITS_MOVED_TO_PROVEN_ROW' AND state = ";

    links
        .db
        .execute_batch(
            "UPDATE local_album_artists SET local_artist_id = 'ar-n' WHERE local_album_id = 'al'; \
             UPDATE local_track_artists SET local_artist_id = 'ar-n' WHERE local_track_id = 't1'; \
             UPDATE local_albums SET album_artist_id = 'ar-n' WHERE id = 'al';",
        )
        .expect("first window moves");
    links.store.link_album_artists("al").unwrap();
    assert_eq!(links.retired_into("ar-o"), None);
    assert_eq!(links.count(&format!("{waiting}'open'")), 1);

    links
        .db
        .execute_batch(
            "UPDATE local_album_artists SET local_artist_id = 'ar-n' WHERE local_album_id = 'ac'; \
             UPDATE local_albums SET album_artist_id = 'ar-n' WHERE id = 'ac';",
        )
        .expect("second window moves");
    links.store.sweep_artist_links().unwrap();
    assert_eq!(links.retired_into("ar-o").as_deref(), Some("ar-n"));
    assert_eq!(links.count(&format!("{waiting}'resolved'")), 1);
    assert_eq!(links.merges(), 1);
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

// ---------------------------------------------------------------------------
// The identity commands, through the real store: what each leaves in the
// album's match, its files' matches, its match flag and its two undo rows,
// and that the catalog revision moved.
// ---------------------------------------------------------------------------

impl Rig {
    fn path(&self) -> std::path::PathBuf {
        self._dir.join("app.db")
    }

    fn run(&self, sql: &str) {
        self.db.execute_batch(sql).expect("seed lands");
    }

    fn catalog_revision(&self) -> i64 {
        self.db
            .query_row(
                "SELECT COALESCE((SELECT value FROM library_catalog_revision), 0)",
                [],
                |row| row.get(0),
            )
            .expect("revision reads")
    }

    fn texts(&self, sql: &str, album: &str) -> Vec<String> {
        self.db
            .prepare(sql)
            .expect("query prepares")
            .query_map(params![album], |row| row.get(0))
            .expect("query runs")
            .collect::<rusqlite::Result<_>>()
            .expect("rows read")
    }

    /// The album's rows in the five identity tables, one line each.
    fn identity_tables(&self, album: &str) -> Vec<String> {
        let mut lines = self.texts(
            "SELECT 'album ' || release_group_mbid || ' ' || COALESCE(release_mbid, '-') \
             || ' ' || decision_source || ' ' || row_revision \
             FROM local_album_external_identities WHERE local_album_id = ?1",
            album,
        );
        lines.extend(self.texts(
            "SELECT 'track ' || i.local_track_id || ' ' || i.recording_mbid || ' ' \
             || COALESCE(i.release_mbid, '-') || ' ' || COALESCE(i.release_track_mbid, '-') \
             || ' ' || COALESCE(i.medium_position, '-') || '/' \
             || COALESCE(i.release_track_position, '-') || ' ' || i.decision_source || ' ' \
             || i.row_revision FROM local_track_external_identities i \
             JOIN local_tracks t ON t.id = i.local_track_id \
             WHERE t.local_album_id = ?1 ORDER BY i.local_track_id",
            album,
        ));
        lines.extend(self.texts(
            "SELECT 'flag ' || state || ' ' || reason_code FROM library_album_match_state \
             WHERE local_album_id = ?1",
            album,
        ));
        lines.extend(self.texts(
            "SELECT 'automatic undo ' || reason_code || ' ' || expected_post_identity_revision \
             || ' ' || COALESCE(json_extract(prior_identity_json, '$.decision_source'), '-') \
             || ' ' || COALESCE(consumed_at, 'live') \
             FROM library_automatic_edition_undo WHERE local_album_id = ?1",
            album,
        ));
        lines.extend(self.texts(
            "SELECT 'choice undo ' || expected_identity_revision \
             || CASE WHEN json_extract(prior_hold_json, '$.carried') THEN ' carried' ELSE '' END \
             FROM library_edition_choice_undo WHERE local_album_id = ?1",
            album,
        ));
        lines.extend(self.texts(
            "SELECT 'remap ' || origin || ' ' || release_mbid || ' ' \
             || COALESCE(chosen_by_user_id, '-') \
             FROM library_edition_remap_queue WHERE local_album_id = ?1",
            album,
        ));
        lines.extend(self.texts(
            "SELECT 'custom ' || manifest_id FROM library_custom_edition_active \
             WHERE local_album_id = ?1",
            album,
        ));
        lines.extend(self.texts(
            "SELECT 'exclusion ' || reason FROM library_management_exclusions \
             WHERE local_album_id = ?1",
            album,
        ));
        lines.extend(self.texts(
            "SELECT 'hand match ' || h.local_track_id FROM library_track_hand_matches h \
             JOIN local_tracks t ON t.id = h.local_track_id WHERE t.local_album_id = ?1",
            album,
        ));
        lines.extend(self.texts(
            "SELECT 'review ' || id || ' ' || state FROM library_identify_reviews \
             WHERE local_album_id = ?1 ORDER BY id",
            album,
        ));
        lines
    }

    /// The album's identification jobs still waiting, as `job <kind>`.
    fn waiting_jobs(&self, album: &str) -> Vec<String> {
        self.texts(
            "SELECT 'job ' || kind FROM library_identify_jobs \
             WHERE local_album_id = ?1 AND state = 'queued' ORDER BY kind",
            album,
        )
    }

    /// A person able to choose, and a custom edition manifest `manifest`
    /// of `album` they sealed (not active).
    fn manifest(&self, album: &str, manifest: &str) {
        self.run(&format!(
            "INSERT OR IGNORE INTO auth_users (id, display_name, role, created_at) \
             VALUES ('curator-1', 'Curator', 'admin', '2026-01-01T00:00:00Z'); \
             INSERT INTO library_custom_edition_manifests (id, local_album_id, version, \
             release_group_mbid, album_title, album_artist_name, source_album_revision, \
             input_revision, content_hash, sealed_by_user_id, sealed_at) \
             VALUES ('{manifest}', '{album}', 1, '{GROUP_A}', 'Some Album', 'Some Artist', 1, \
             'rev', '{manifest}', 'curator-1', 0);"
        ));
    }

    /// Run `change` in one transaction on the album's database.
    fn change<T>(
        &self,
        change: impl FnOnce(
            &rusqlite::Transaction<'_>,
        )
            -> Result<T, droppedneedle_library::operations::models::OperationError>,
    ) -> Result<T, String> {
        droppedneedle_kernel::db::Db::open_for_tests(&self.path())
            .expect("db opens")
            .write_blocking(
                droppedneedle_kernel::db::Lane::Foreground,
                "test.change",
                change,
            )
            .map_err(|error| error.to_string())
    }

    /// The album's identity row as v2 carried it, with both undo rows a
    /// change before it left, naming revision `revision`.
    fn carried(&self, album: &str, release: &str, source: &str, revision: i64) {
        self.run(&format!(
            "INSERT INTO local_album_external_identities (local_album_id, provider, \
             release_group_mbid, release_mbid, decision_source, selected_at, row_revision) \
             VALUES ('{album}', 'musicbrainz', '{GROUP_A}', '{release}', '{source}', 0, \
             {revision}); \
             INSERT INTO library_automatic_edition_undo (id, local_album_id, \
             prior_track_identities_json, expected_post_album_revision, \
             expected_post_identity_revision, reason_code, created_at) \
             VALUES ('auto-{album}', '{album}', '[]', 1, {revision}, 'OLD', 0); \
             INSERT INTO library_edition_choice_undo (id, local_album_id, action_id, \
             prior_tracks_json, expected_identity_revision, created_at) \
             VALUES ('choice-{album}', '{album}', 'action-old', '[]', {revision}, 0);"
        ));
    }

    /// Enqueue `job` for `album`, claim it, and run it through `service`.
    async fn attempt(
        &self,
        service: &IdentifyService,
        job: &str,
        album: &str,
    ) -> Option<identify::service::AttemptReport> {
        service
            .enqueue_album(
                job,
                album,
                IdentifyKind::Automatic,
                "rev-1",
                None,
                0,
                droppedneedle_kernel::db::Lane::Background,
            )
            .unwrap();
        self.store.claim(0, 60_000).unwrap();
        service.run_claimed_job(job, 0).await.unwrap()
    }
}

/// Recalls a scripted answer, and while it does, runs `sql` on the
/// database the way another writer would mid-attempt.
struct DuringRecall {
    recall: RecallResult,
    path: std::path::PathBuf,
    sql: String,
}

impl identify::providers::IdentifyProviders for DuringRecall {
    fn recall_candidates(
        &self,
        _facts: &LocalAlbumFacts,
        _named_group: Option<&str>,
        _priority: droppedneedle_providers::RequestPriority,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = identify::providers::RecallOutcome> + Send + '_>,
    > {
        droppedneedle_kernel::db::open_connection(&self.path)
            .expect("writer opens")
            .execute_batch(&self.sql)
            .expect("mid-attempt write lands");
        let result = self.recall.clone();
        Box::pin(async move {
            identify::providers::RecallOutcome {
                result,
                recall_criticality: None,
            }
        })
    }
}

fn service_during_recall(rig: &Rig, recall: RecallResult, sql: &str) -> IdentifyService {
    IdentifyService::new(IdentifyDeps {
        store: rig.store.clone(),
        providers: Arc::new(DuringRecall {
            recall,
            path: rig.path(),
            sql: sql.to_owned(),
        }),
    })
}

/// Identification seals over a match v2 took from the files' tags
/// (`embedded`): the album row turns automatic on the new release, the
/// supported files are placed on it, the unsure flag clears, the replaced
/// row is kept as the automatic-edition undo, the edition-choice undo is
/// left as it was, and the catalog revision moves.
#[tokio::test]
async fn seal_automatic_replaces_an_embedded_match() {
    let rig = rig_with_recall(recall(vec![release(
        GROUP_A,
        RELEASE_A1,
        &[RECORDING_1, RECORDING_2],
    )]));
    rig.seed_album(&album_facts(
        "album-1",
        &[("t1", Some(RECORDING_1)), ("t2", Some(RECORDING_2))],
    ));
    rig.carried("album-1", RELEASE_A2, "embedded", 4);
    rig.run(
        "INSERT INTO library_album_match_state (local_album_id, state, reason_code, \
         candidates_json, updated_at) VALUES ('album-1', 'unconfirmed', 'OLD_GUESS', '[]', 0);",
    );
    let before = rig.catalog_revision();

    let report = rig
        .attempt(&rig.service, "job-1", "album-1")
        .await
        .expect("report");

    assert_eq!(report.outcome, IdentificationOutcome::Identified);
    assert_eq!(report.job.state, JobState::Succeeded);
    assert_eq!(
        rig.identity_tables("album-1"),
        vec![
            format!("album {GROUP_A} {RELEASE_A1} automatic 5"),
            format!("track t1 {RECORDING_1} {RELEASE_A1} {RELEASE_A1}-0 -/- automatic 1"),
            format!("track t2 {RECORDING_2} {RELEASE_A1} {RELEASE_A1}-1 -/- automatic 1"),
            "automatic undo AUTOMATIC_EXACT_EDITION 5 embedded live".to_owned(),
            "choice undo 4".to_owned(),
        ]
    );
    assert!(
        rig.catalog_revision() > before,
        "the seal moves the catalog revision"
    );
}

/// New evidence contradicts a match v2 took from the files' tags: the
/// match goes like an automatic one, the album is flagged unmatched, both
/// undo rows kept for it go (one could otherwise match the album's next
/// match and put an older one back), the files' rows stay, and the
/// catalog revision moves. The withdrawn match is kept as the album's
/// edition undo, keyed to the album having no match, and undo puts it
/// back.
#[tokio::test]
async fn a_withdrawn_match_keeps_one_undo() {
    let rig = rig_with_recall(recall(vec![release(GROUP_A, RELEASE_A1, &[RECORDING_2])]));
    rig.seed_album(&album_facts("album-1", &[("t1", Some(RECORDING_1))]));
    rig.carried("album-1", RELEASE_A1, "embedded", 2);
    rig.run(&format!(
        "INSERT INTO local_track_external_identities (local_track_id, provider, \
         recording_mbid, release_mbid, decision_source, selected_at) \
         VALUES ('t1', 'musicbrainz', '{RECORDING_1}', '{RELEASE_A1}', 'embedded', 0);"
    ));
    let before = rig.catalog_revision();

    let report = rig
        .attempt(&rig.service, "job-1", "album-1")
        .await
        .expect("report");

    assert_eq!(report.outcome, IdentificationOutcome::Contradictory);
    assert_eq!(
        rig.identity_tables("album-1"),
        vec![
            format!("track t1 {RECORDING_1} {RELEASE_A1} - -/- embedded 1"),
            "flag unmatched CONFLICTING_TRACK_EVIDENCE".to_owned(),
            "choice undo 0".to_owned(),
        ]
    );
    assert_eq!(
        rig.texts(
            "SELECT action_kind || ' ' || reason_code FROM library_catalog_actions \
             WHERE local_album_id = ?1",
            "album-1",
        ),
        vec!["automatic_match_withdrawn CONFLICTING_TRACK_EVIDENCE".to_owned()]
    );
    assert!(
        rig.catalog_revision() > before,
        "the retraction moves the catalog revision"
    );

    rig.run(
        "INSERT INTO auth_users (id, display_name, role, created_at) \
         VALUES ('curator-1', 'Curator', 'admin', '2026-01-01T00:00:00Z');",
    );
    let undone = rig.change(|tx| {
        droppedneedle_library::operations::choice::undo(tx, "album-1", "curator-1", 2.0)
    });
    assert_eq!(undone, Ok(()));
    assert_eq!(
        rig.identity_tables("album-1")[..2],
        [
            format!("album {GROUP_A} {RELEASE_A1} embedded 1"),
            format!("track t1 {RECORDING_1} {RELEASE_A1} - -/- embedded 2"),
        ]
    );
}

/// Album A's ten-track release, its first track `first_ms` long, and B's
/// one-track single of that song: one file of the album fits both.
fn album_and_single(first_ms: u64) -> Vec<Release> {
    let recordings: Vec<String> = (0..10).map(|index| format!("rec-{index}")).collect();
    let recordings: Vec<&str> = recordings.iter().map(String::as_str).collect();
    let mut album = release(GROUP_A, RELEASE_A1, &recordings);
    album.tracks[0].length_ms = Some(first_ms);
    vec![album, release(GROUP_B, RELEASE_B1, &recordings[..1])]
}

fn group_only(rig: &Rig, album: &str, group: &str) {
    rig.store
        .save_album_identity(AlbumIdentity {
            local_album_id: album.to_owned(),
            provider: "musicbrainz".to_owned(),
            release_group_mbid: Some(group.to_owned()),
            release_mbid: None,
            decision_source: DecisionSource::Automatic,
            row_revision: 1,
        })
        .unwrap();
}

/// One file of album A, matched to A's group without an edition, fits A
/// and B's single equally: the tie goes to A, the group its match names.
#[tokio::test]
async fn a_group_only_match_wins_its_tie() {
    let rig = rig_with_recall(recall(album_and_single(200_000)));
    rig.seed_album(&album_facts("album-1", &[("t1", None)]));
    group_only(&rig, "album-1", GROUP_A);

    let report = rig.attempt(&rig.service, "job-1", "album-1").await;

    assert_eq!(
        report.map(|report| report.outcome),
        Some(IdentificationOutcome::Identified)
    );
    assert_eq!(
        rig.identity_tables("album-1")[0],
        format!("album {GROUP_A} {RELEASE_A1} automatic 2")
    );
}

/// When B's single fits clearly better than the album the match named, it
/// is sealed as a guess, and stays one on later passes, when no group is
/// named any more: through a pass that finds it only a loose fit, and one
/// that finds it exact again.
#[tokio::test]
async fn a_guess_outside_the_named_group_stays_a_guess() {
    let rig = rig_with_recall(recall(album_and_single(290_000)));
    rig.seed_album(&album_facts("album-1", &[("t1", None)]));
    group_only(&rig, "album-1", GROUP_A);
    let mut loose = album_and_single(290_000);
    loose[1] = live(loose[1].clone());
    loose[1].tracks[0].length_ms = Some(205_000);

    for (job, recalled) in [
        ("job-1", None),
        ("job-2", Some(loose)),
        ("job-3", Some(album_and_single(290_000))),
    ] {
        if let Some(recalled) = recalled {
            rig.providers.set_recall(recall(recalled));
        }
        let report = rig.attempt(&rig.service, job, "album-1").await;
        assert_eq!(
            report.map(|report| report.reason_code),
            Some("NAMED_GROUP_DIFFERS".to_owned()),
            "{job}"
        );
        let tables = rig.identity_tables("album-1");
        assert!(
            tables[0].starts_with(&format!("album {GROUP_B} {RELEASE_B1} automatic")),
            "{job}: {tables:?}"
        );
        assert!(
            tables.contains(&"flag unconfirmed NAMED_GROUP_DIFFERS".to_owned()),
            "{job}: {tables:?}"
        );
    }
}

/// An album confirmed on B's single whose files are all tagged with A's
/// group, which fits clearly worse: the confirmation is not kept quietly,
/// the single stays as a guess.
#[tokio::test]
async fn a_confirmed_match_outside_the_tagged_group_becomes_a_guess() {
    let rig = rig_with_recall(recall(album_and_single(290_000)));
    let mut facts = album_facts("album-1", &[("t1", None)]);
    facts.tracks[0].release_group_mbid = Some(GROUP_A.to_owned());
    rig.seed_album(&facts);
    rig.store
        .save_album_identity(AlbumIdentity {
            local_album_id: "album-1".to_owned(),
            provider: "musicbrainz".to_owned(),
            release_group_mbid: Some(GROUP_B.to_owned()),
            release_mbid: Some(RELEASE_B1.to_owned()),
            decision_source: DecisionSource::Automatic,
            row_revision: 1,
        })
        .unwrap();

    let report = rig.attempt(&rig.service, "job-1", "album-1").await;

    assert_eq!(
        report.map(|report| report.reason_code),
        Some("NAMED_GROUP_DIFFERS".to_owned())
    );
    let tables = rig.identity_tables("album-1");
    assert!(
        tables[0].starts_with(&format!("album {GROUP_B} {RELEASE_B1} automatic")),
        "{tables:?}"
    );
    assert!(
        tables.contains(&"flag unconfirmed NAMED_GROUP_DIFFERS".to_owned()),
        "{tables:?}"
    );
}

/// MusicBrainz as recall sees it for one group-tagged album: the old group
/// id lists nothing and was merged into `GROUP_B`, which lists `RELEASE_B1`;
/// a group it no longer has is not found.
struct MergedGroups;

const MERGED_GROUP: &str = "0e1d2c3b-4a59-4687-9a1b-2c3d4e5f6a7b";
const GONE_GROUP: &str = "9f8e7d6c-5b4a-4392-8a1b-0c9d8e7f6a5b";

impl identify::sources::ReleaseSource for MergedGroups {
    fn search<'a>(
        &'a self,
        _title: &'a str,
        _artist: &'a str,
    ) -> futures_util::future::BoxFuture<
        'a,
        Result<Vec<identify::sources::ReleaseHit>, identify::sources::SourceError>,
    > {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn release<'a>(
        &'a self,
        mbid: &'a str,
    ) -> futures_util::future::BoxFuture<'a, Result<Option<Release>, identify::sources::SourceError>>
    {
        Box::pin(async move {
            Ok((mbid == RELEASE_B1).then(|| release(GROUP_B, RELEASE_B1, &[RECORDING_1])))
        })
    }

    fn canonical_recording<'a>(
        &'a self,
        mbid: &'a str,
    ) -> futures_util::future::BoxFuture<'a, Result<Option<String>, identify::sources::SourceError>>
    {
        Box::pin(async move { Ok(Some(mbid.to_owned())) })
    }

    fn search_editions<'a>(
        &'a self,
        query: &'a identify::sources::EditionQuery,
    ) -> futures_util::future::BoxFuture<
        'a,
        Result<identify::sources::EditionPage, identify::sources::SourceError>,
    > {
        Box::pin(async move {
            let items = if query.release_group_mbid.as_deref() == Some(GROUP_B) {
                vec![identify::sources::Edition {
                    release_mbid: RELEASE_B1.to_owned(),
                    release_group_mbid: GROUP_B.to_owned(),
                    track_count: 1,
                    ..identify::sources::Edition::default()
                }]
            } else {
                Vec::new()
            };
            Ok(identify::sources::EditionPage {
                total: items.len() as u64,
                offset: 0,
                items,
            })
        })
    }

    fn canonical_release_group<'a>(
        &'a self,
        mbid: &'a str,
    ) -> futures_util::future::BoxFuture<'a, Result<Option<String>, identify::sources::SourceError>>
    {
        Box::pin(async move { Ok((mbid == MERGED_GROUP).then(|| GROUP_B.to_owned())) })
    }
}

/// A group id MusicBrainz merged is followed to the group it was kept
/// under, whose editions are recalled; one it no longer has names nothing.
#[tokio::test]
async fn a_merged_group_is_followed() {
    use identify::providers::IdentifyProviders as _;

    let rig = rig_with_recall(RecallResult::default());
    let providers = identify::providers::LiveProviders::new(
        identify::sources::Prioritised::new(MergedGroups, MergedGroups),
        rig.store.clone(),
        identify::sources::NoFingerprints,
    );
    let facts = album_facts("album-1", &[("t1", None)]);

    let merged = providers
        .recall_candidates(
            &facts,
            Some(MERGED_GROUP),
            droppedneedle_providers::RequestPriority::BackgroundSync,
        )
        .await
        .result;
    assert_eq!(merged.named_group.as_deref(), Some(GROUP_B));
    assert!(merged.releases.iter().any(|found| found.id == RELEASE_B1));

    let gone = providers
        .recall_candidates(
            &facts,
            Some(GONE_GROUP),
            droppedneedle_providers::RequestPriority::BackgroundSync,
        )
        .await
        .result;
    assert_eq!(gone.named_group, None);
    assert!(!gone.provider_deferred);
}

/// Files whose release id names a release MusicBrainz now files under
/// group B, while their group tag still says A: the release's group is
/// the one they name, and the release confirms.
#[tokio::test]
async fn a_moved_release_names_its_own_group() {
    use identify::providers::IdentifyProviders as _;

    let rig = rig_with_recall(RecallResult::default());
    let providers = Arc::new(identify::providers::LiveProviders::new(
        identify::sources::Prioritised::new(MergedGroups, MergedGroups),
        rig.store.clone(),
        identify::sources::NoFingerprints,
    ));
    let mut facts = album_facts("album-1", &[("t1", Some(RECORDING_1))]);
    facts.tracks[0].release_mbid = Some(RELEASE_B1.to_owned());
    facts.tracks[0].release_group_mbid = Some(GROUP_A.to_owned());

    let named = identify::evidence::named_group(&facts, None);
    let recall = providers
        .recall_candidates(
            &facts,
            named.as_deref(),
            droppedneedle_providers::RequestPriority::BackgroundSync,
        )
        .await
        .result;
    assert_eq!(recall.named_group.as_deref(), Some(GROUP_B));
    let service = IdentifyService::new(IdentifyDeps {
        store: rig.store.clone(),
        providers,
    });
    let ranking = service.rank(&facts, &recall).expect("default preferences");
    assert!(ranking.lead_identified, "{}", ranking.reason_code);
}

/// A recheck's automatic acceptance never replaces an edition carried over
/// from v2. On an automatic match it places the files on the accepted
/// release, unplaces the rest, clears the flag and the waiting placement,
/// keeps both undo rows, and moves the catalog revision.
#[test]
fn accept_automatic_keeps_a_v2_choice_and_seals_an_automatic_match() {
    use droppedneedle_library::identify::models::{EvidenceClass, TrackEvidence};
    use droppedneedle_library::operations::choice::accept_automatic;
    use droppedneedle_library::operations::models::{CandidateTrack, ReidentificationCandidate};

    let rig = rig_with_recall(RecallResult::default());
    for album in ["album-1", "album-2"] {
        let tracks = if album == "album-1" {
            [("t1", None), ("t2", None)]
        } else {
            [("t3", None), ("t4", None)]
        };
        rig.seed_album(&album_facts(album, &tracks));
    }
    rig.carried("album-1", RELEASE_A2, "legacy_import", 3);
    rig.carried("album-2", RELEASE_A2, "automatic", 3);
    rig.run(&format!(
        "INSERT INTO local_track_external_identities (local_track_id, provider, \
         recording_mbid, release_mbid, release_track_mbid, decision_source, selected_at) \
         VALUES ('t4', 'musicbrainz', '{RECORDING_2}', '{RELEASE_A2}', 'old-rt', \
         'automatic', 0); \
         INSERT INTO library_album_match_state (local_album_id, state, reason_code, \
         candidates_json, updated_at) VALUES ('album-2', 'unconfirmed', 'OLD_GUESS', '[]', 0); \
         INSERT INTO library_edition_remap_queue (local_album_id, release_mbid, origin, \
         queued_at) VALUES ('album-2', '{RELEASE_A2}', 'place_only', 0);"
    ));
    let candidate = |track: &str, other: &str| ReidentificationCandidate {
        candidate_key: RELEASE_A1.to_owned(),
        automatic_safe: true,
        evidence: identify::models::CandidateEvidence {
            candidate_key: RELEASE_A1.to_owned(),
            release_group_mbid: GROUP_A.to_owned(),
            release_mbid: Some(RELEASE_A1.to_owned()),
            album_title: "Some Album".to_owned(),
            album_artist_name: "Some Artist".to_owned(),
            track_evidence: vec![TrackEvidence {
                local_track_id: track.to_owned(),
                classification: EvidenceClass::Supported,
                evidence_kinds: Vec::new(),
                recording_mbid: Some(RECORDING_1.to_owned()),
                release_track_mbid: Some("rt-1".to_owned()),
            }],
            score: 1.0,
            reason_code: "SUPPORTED".to_owned(),
            distance: 0.0,
            penalties: Vec::new(),
        },
        artist_mbid: None,
        release_type: None,
        release_date: None,
        local_album_title: "Some Album".to_owned(),
        local_album_artist_name: "Some Artist".to_owned(),
        album_title_classification: EvidenceClass::Supported,
        album_artist_classification: EvidenceClass::Supported,
        tracks: [track, other]
            .iter()
            .enumerate()
            .map(|(index, id)| CandidateTrack {
                local_track_id: (*id).to_owned(),
                title: None,
                disc_number: Some(1),
                position: Some(index as u32 + 1),
            })
            .collect(),
        unmatched_expected_tracks: Vec::new(),
    };
    let before = rig.catalog_revision();
    let kept = rig.identity_tables("album-1");
    let db = droppedneedle_kernel::db::Db::open_for_tests(&rig.path()).expect("db opens");

    let accepted = db
        .write_blocking(
            droppedneedle_kernel::db::Lane::Background,
            "test.accept",
            |tx| {
                Ok::<_, droppedneedle_library::operations::models::OperationError>((
                    accept_automatic(tx, "album-1", &candidate("t1", "t2"), "R", None, None, 1.0)?,
                    accept_automatic(tx, "album-2", &candidate("t3", "t4"), "R", None, None, 1.0)?,
                ))
            },
        )
        .unwrap();

    assert!(accepted.0.is_none(), "a v2 choice is a person's");
    assert!(accepted.1.is_some());
    assert_eq!(rig.identity_tables("album-1"), kept);
    assert_eq!(
        rig.identity_tables("album-2"),
        vec![
            format!("album {GROUP_A} {RELEASE_A1} automatic 4"),
            format!("track t3 {RECORDING_1} {RELEASE_A1} rt-1 1/1 automatic 1"),
            format!("track t4 {RECORDING_2} - - -/- automatic 2"),
            "automatic undo R 4 automatic live".to_owned(),
            "choice undo 4".to_owned(),
        ]
    );
    assert_eq!(
        rig.texts(
            "SELECT local_album_id FROM library_edition_remap_queue WHERE local_album_id = ?1",
            "album-2"
        ),
        Vec::<String>::new()
    );
    assert!(
        rig.catalog_revision() > before,
        "the acceptance moves the catalog revision"
    );
}

/// The existing-matches repair never rechecks an edition carried over from
/// v2 for removal, and reports one whose files contradict its release as a
/// person's identity, never as safe to detach.
#[test]
fn the_existing_matches_repair_reports_a_v2_choice_as_a_persons() {
    use droppedneedle_library::identify::models::{EvidenceClass, TrackEvidence};
    use droppedneedle_library::operations::models::{CandidateTrack, ReidentificationCandidate};
    use droppedneedle_library::repairs::{classify, store};

    let rig = rig_with_recall(RecallResult::default());
    rig.seed_album(&album_facts("album-1", &[("t1", Some(RECORDING_1))]));
    rig.carried("album-1", RELEASE_A1, "legacy_import", 1);

    let estimate = store::estimate_repair(&rig.db, &[]).expect("estimate reads");
    assert_eq!(
        estimate.identity_count, 0,
        "a v2 choice is not rechecked for removal"
    );

    let subject = store::subject(&rig.db, "album-1")
        .expect("subject reads")
        .expect("album checked");
    let identity = subject.identity.expect("identity");
    let contradicted = ReidentificationCandidate {
        candidate_key: RELEASE_A1.to_owned(),
        automatic_safe: false,
        evidence: identify::models::CandidateEvidence {
            candidate_key: RELEASE_A1.to_owned(),
            release_group_mbid: GROUP_A.to_owned(),
            release_mbid: Some(RELEASE_A1.to_owned()),
            album_title: "Some Album".to_owned(),
            album_artist_name: "Some Artist".to_owned(),
            track_evidence: vec![TrackEvidence {
                local_track_id: "t1".to_owned(),
                classification: EvidenceClass::Contradictory,
                evidence_kinds: Vec::new(),
                recording_mbid: None,
                release_track_mbid: None,
            }],
            score: 0.0,
            reason_code: "CONFLICTING_TRACK_EVIDENCE".to_owned(),
            distance: 1.0,
            penalties: Vec::new(),
        },
        artist_mbid: None,
        release_type: None,
        release_date: None,
        local_album_title: "Some Album".to_owned(),
        local_album_artist_name: "Some Artist".to_owned(),
        album_title_classification: EvidenceClass::Supported,
        album_artist_classification: EvidenceClass::Supported,
        tracks: vec![CandidateTrack {
            local_track_id: "t1".to_owned(),
            title: None,
            disc_number: Some(1),
            position: Some(1),
        }],
        unmatched_expected_tracks: Vec::new(),
    };
    let verdict = classify::existing_match(&identity, Some(&contradicted), 1, 1);
    assert_eq!(
        verdict.finding_code,
        droppedneedle_library::repairs::models::FindingCode::ManualIdentity
    );
}

/// A rescan changes the album's files while identification is recalling
/// candidates: the attempt writes nothing for files it never read, lands
/// as `INPUT_MOVED`, and the album is queued again on its new files.
#[tokio::test]
async fn an_attempt_on_files_that_moved_writes_nothing_and_queues_them() {
    let rig = rig_with_recall(RecallResult::default());
    rig.seed_album(&album_facts("album-1", &[("t1", Some(RECORDING_1))]));
    let service = service_during_recall(
        &rig,
        recall(vec![release(GROUP_A, RELEASE_A1, &[RECORDING_1])]),
        "UPDATE local_tracks SET stat_revision = 'stat-2' WHERE local_album_id = 'album-1'",
    );

    let report = rig
        .attempt(&service, "job-1", "album-1")
        .await
        .expect("report");

    assert_eq!(report.reason_code, "INPUT_MOVED");
    assert_eq!(report.job.state, JobState::Failed);
    assert_eq!(rig.identity_tables("album-1"), Vec::<String>::new());
    let queued = rig.texts(
        "SELECT id FROM library_identify_jobs WHERE local_album_id = ?1 AND state = 'queued'",
        "album-1",
    );
    assert_eq!(queued.len(), 1, "the new files are queued: {queued:?}");
    assert_ne!(queued[0], "job-1");
}

/// The attempt's lease lapsed and another worker claimed the job while it
/// was recalling: the attempt lands nothing, its seal included, and the
/// job stays with the worker that holds it now.
#[tokio::test]
async fn an_attempt_whose_lease_was_taken_over_lands_nothing() {
    let rig = rig_with_recall(RecallResult::default());
    rig.seed_album(&album_facts("album-1", &[("t1", Some(RECORDING_1))]));
    let service = service_during_recall(
        &rig,
        recall(vec![release(GROUP_A, RELEASE_A1, &[RECORDING_1])]),
        "UPDATE library_identify_jobs SET lease_expires_ms = 120000 WHERE id = 'job-1'",
    );

    let report = rig.attempt(&service, "job-1", "album-1").await;

    assert!(report.is_none(), "{report:?}");
    assert_eq!(rig.identity_tables("album-1"), Vec::<String>::new());
    assert_eq!(
        rig.texts(
            "SELECT state || ' ' || lease_expires_ms FROM library_identify_jobs WHERE id = ?1",
            "job-1"
        ),
        vec!["running 120000".to_owned()]
    );
}

// ---------------------------------------------------------------------------
// A person's identity commands: what each leaves behind, and so what an
// undo can still bring back.
// ---------------------------------------------------------------------------

/// A candidate offering `release` of [`GROUP_A`], with `supported` placed
/// on disc 1 in order (release track `rt-<file>`) and `other` weighed but
/// not supported.
fn offering(
    release: &str,
    supported: &[&str],
    other: &[&str],
) -> droppedneedle_library::operations::models::ReidentificationCandidate {
    use droppedneedle_library::identify::models::{EvidenceClass, TrackEvidence};
    use droppedneedle_library::operations::models::{CandidateTrack, ReidentificationCandidate};
    let evidence = |file: &str, class: EvidenceClass| TrackEvidence {
        local_track_id: file.to_owned(),
        classification: class,
        evidence_kinds: Vec::new(),
        recording_mbid: Some(RECORDING_1.to_owned()),
        release_track_mbid: Some(format!("rt-{file}")),
    };
    ReidentificationCandidate {
        candidate_key: key(GROUP_A, release),
        automatic_safe: true,
        evidence: identify::models::CandidateEvidence {
            candidate_key: key(GROUP_A, release),
            release_group_mbid: GROUP_A.to_owned(),
            release_mbid: Some(release.to_owned()),
            album_title: "Some Album".to_owned(),
            album_artist_name: "Some Artist".to_owned(),
            track_evidence: supported
                .iter()
                .map(|file| evidence(file, EvidenceClass::Supported))
                .chain(
                    other
                        .iter()
                        .map(|file| evidence(file, EvidenceClass::Contradictory)),
                )
                .collect(),
            score: 1.0,
            reason_code: "SUPPORTED".to_owned(),
            distance: 0.0,
            penalties: Vec::new(),
        },
        artist_mbid: None,
        release_type: None,
        release_date: None,
        local_album_title: "Some Album".to_owned(),
        local_album_artist_name: "Some Artist".to_owned(),
        album_title_classification: EvidenceClass::Supported,
        album_artist_classification: EvidenceClass::Supported,
        tracks: supported
            .iter()
            .chain(other)
            .enumerate()
            .map(|(index, file)| CandidateTrack {
                local_track_id: (*file).to_owned(),
                title: None,
                disc_number: Some(1),
                position: Some(index as u32 + 1),
            })
            .collect(),
        unmatched_expected_tracks: Vec::new(),
    }
}

/// Each person's command leaves exactly what its rule says in the album's
/// match, its files' matches, its flag, both undos, the queued placement,
/// the custom edition, "leave unmanaged" and its reviews:
///
/// - choosing places the files as the person's and keeps "leave
///   unmanaged"; confirming makes a best guess the person's but leaves a
///   v2 choice as it is;
/// - handing back ends the custom edition, keeps tags-carried rows, queues
///   identification, and its undo still brings the edition back after that
///   identification sealed;
/// - a repair's or a person's removal takes the match, its files' rows,
///   the custom edition and the queued placement, leaves no automatic undo
///   behind, and undo brings all of it back;
/// - undoing an automatic edition with nothing before it leaves no undo
///   for a later match to meet;
/// - approving a review settles only that review, leaves the track rows,
///   and queues the files to be placed as the curator's choice.
///
/// Every row is checked and every difference reported together.
#[tokio::test]
async fn person_commands_leave_what_their_rule_says() {
    use droppedneedle_library::operations::{choice, decisions};

    let rig = rig_with_recall(recall(vec![release(
        GROUP_A,
        RELEASE_A1,
        &[RECORDING_1, RECORDING_2],
    )]));
    let mut differ: Vec<String> = Vec::new();
    let mut check = |row: &str, actual: Vec<String>, expected: Vec<String>| {
        if actual != expected {
            differ.push(format!("{row}:\n  got  {actual:?}\n  want {expected:?}"));
        }
    };
    let lines = |items: &[&str]| -> Vec<String> { items.iter().map(|s| (*s).to_owned()).collect() };
    let without_reviews = |album: &str| -> Vec<String> {
        rig.identity_tables(album)
            .into_iter()
            .filter(|line| !line.starts_with("review "))
            .collect()
    };
    let seal = |album: &str, release: Option<&str>, track: Option<&str>| {
        rig.store
            .seal_automatic(&AutomaticSeal {
                local_album_id: album.to_owned(),
                release_group_mbid: GROUP_A.to_owned(),
                release_mbid: release.map(str::to_owned),
                tracks: track
                    .map(|track| TrackIdentity {
                        local_track_id: track.to_owned(),
                        provider: "musicbrainz".to_owned(),
                        recording_mbid: Some(RECORDING_1.to_owned()),
                        release_track_mbid: Some(format!("rt-{track}")),
                        decision_source: DecisionSource::Automatic,
                        row_revision: 1,
                    })
                    .into_iter()
                    .collect(),
                flag: None,
            })
            .expect("seal lands")
    };

    // A repair takes off an exact automatic match: nothing of it is left
    // to match the album's next seal, and its carried undo follows that
    // seal and brings the exact match back.
    rig.seed_album(&album_facts("a", &[("a1", Some(RECORDING_1))]));
    assert!(seal("a", Some(RELEASE_A1), Some("a1")));
    let removed =
        rig.change(|tx| choice::detach(tx, "a", "IDENTITY_FAILED_RECHECK", None, None, 1.0));
    check(
        "repair removes",
        rig.identity_tables("a"),
        lines(&[
            "flag unmatched IDENTITY_FAILED_RECHECK",
            "choice undo 0 carried",
        ]),
    );
    assert!(removed.is_ok_and(|action| action.is_some()));
    assert!(seal("a", None, None));
    check(
        "a group-only seal after the repair meets no automatic undo",
        vec![format!(
            "{:?}",
            decisions::live_automatic_edition_undo(&rig.db, "a").expect("reads")
        )],
        lines(&["None"]),
    );
    let undone = rig.change(|tx| choice::undo(tx, "a", "curator-1", 2.0));
    check(
        "the repair's carried undo brings the exact match back",
        vec![format!("{undone:?}")]
            .into_iter()
            .chain(rig.identity_tables("a"))
            .collect(),
        vec![
            "Ok(())".to_owned(),
            format!("album {GROUP_A} {RELEASE_A1} automatic 2"),
            format!("track a1 {RECORDING_1} {RELEASE_A1} rt-a1 -/- automatic 1"),
        ],
    );

    // A person takes off a group-only match with a custom edition and a
    // queued placement: all of it goes, and undo brings all of it back.
    rig.seed_album(&album_facts("b", &[("b1", Some(RECORDING_1))]));
    rig.manifest("b", "m-b");
    rig.run(&format!(
        "INSERT INTO local_album_external_identities (local_album_id, provider, \
         release_group_mbid, decision_source, selected_at, row_revision) \
         VALUES ('b', 'musicbrainz', '{GROUP_A}', 'manual', 0, 3); \
         INSERT INTO local_track_external_identities (local_track_id, provider, \
         recording_mbid, decision_source, selected_at) \
         VALUES ('b1', 'musicbrainz', '{RECORDING_1}', 'manual', 0); \
         INSERT INTO library_custom_edition_active (local_album_id, manifest_id, \
         activated_at) VALUES ('b', 'm-b', 0); \
         INSERT INTO library_edition_remap_queue (local_album_id, release_mbid, origin, \
         queued_at) VALUES ('b', '{RELEASE_A1}', 'place_only', 0);"
    ));
    let removed = rig.change(|tx| choice::detach_by_person(tx, "b", "DETACHED", "curator-1", 1.0));
    assert!(removed.is_ok_and(|action| action.is_some()));
    check(
        "a person removes",
        rig.identity_tables("b"),
        lines(&["flag unmatched DETACHED", "choice undo 0"]),
    );
    let undone = rig.change(|tx| choice::undo(tx, "b", "curator-1", 2.0));
    check(
        "undo after a person removes",
        vec![format!("{undone:?}")]
            .into_iter()
            .chain(rig.identity_tables("b"))
            .collect(),
        vec![
            "Ok(())".to_owned(),
            format!("album {GROUP_A} - manual 1"),
            format!("track b1 {RECORDING_1} - - -/- manual 1"),
            format!("remap place_only {RELEASE_A1} -"),
            "custom m-b".to_owned(),
        ],
    );

    // An automatic edition with nothing before it is undone: no undo row
    // is left, an older choice undo included, and an acceptance's own undo
    // gives its match state back.
    for (album, choice_undo, flag) in [
        ("c", 5, Vec::new()),
        ("d", 2, vec!["flag unconfirmed OLD".to_owned()]),
    ] {
        rig.seed_album(&album_facts(album, &[(format!("{album}1").as_str(), None)]));
        rig.run(&format!(
            "INSERT INTO local_album_external_identities (local_album_id, provider, \
             release_group_mbid, release_mbid, decision_source, selected_at, row_revision) \
             VALUES ('{album}', 'musicbrainz', '{GROUP_A}', '{RELEASE_A1}', 'automatic', 0, 2); \
             INSERT INTO library_automatic_edition_undo (id, local_album_id, \
             prior_track_identities_json, expected_post_album_revision, \
             expected_post_identity_revision, reason_code, created_at) \
             VALUES ('auto-{album}', '{album}', '[]', 1, 2, 'R', 0); \
             INSERT INTO library_edition_choice_undo (id, local_album_id, action_id, \
             prior_tracks_json, prior_match_state_json, expected_identity_revision, created_at) \
             VALUES ('choice-{album}', '{album}', 'action-old', '[]', \
             '{{\"state\":\"unconfirmed\",\"reason_code\":\"OLD\",\"candidates_json\":\"[]\"}}', \
             {choice_undo}, 0);"
        ));
        let undone = rig.change(|tx| {
            decisions::undo_automatic_edition(tx, album, 1, 2, "curator-1", 1.0)
                .map(|outcome| outcome.outcome)
        });
        check(
            &format!("undo automatic to review, choice undo at {choice_undo}"),
            vec![format!("{undone:?}")]
                .into_iter()
                .chain(without_reviews(album))
                .collect(),
            vec![format!(
                "{:?}",
                Ok::<_, String>(droppedneedle_library::identity::AutomaticUndone::ClearedToReview)
            )]
            .into_iter()
            .chain(flag)
            .collect(),
        );
    }

    // Handing back a custom edition: it ends, the person's rows go back to
    // automatic choice, the tags' rows keep their source, identification
    // is queued, and after it sealed, undo brings the edition back.
    rig.seed_album(&album_facts(
        "f",
        &[("t1", Some(RECORDING_1)), ("t2", Some(RECORDING_2))],
    ));
    rig.manifest("f", "m-f");
    rig.run(&format!(
        "INSERT INTO local_album_external_identities (local_album_id, provider, \
         release_group_mbid, decision_source, selected_at, row_revision) \
         VALUES ('f', 'musicbrainz', '{GROUP_A}', 'manual', 0, 3); \
         INSERT INTO local_track_external_identities (local_track_id, provider, \
         recording_mbid, decision_source, selected_at) \
         VALUES ('t1', 'musicbrainz', '{RECORDING_1}', 'manual', 0), \
         ('t2', 'musicbrainz', '{RECORDING_2}', 'embedded', 0); \
         INSERT INTO library_custom_edition_active (local_album_id, manifest_id, \
         activated_at) VALUES ('f', 'm-f', 0);"
    ));
    let handed = rig.change(|tx| choice::hand_back(tx, "f", "curator-1", 1.0).map(|_| ()));
    check(
        "hand back",
        vec![format!("{handed:?}")]
            .into_iter()
            .chain(rig.identity_tables("f"))
            .chain(rig.waiting_jobs("f"))
            .collect(),
        vec![
            "Ok(())".to_owned(),
            format!("album {GROUP_A} - automatic 4"),
            format!("track t1 {RECORDING_1} - - -/- automatic 2"),
            format!("track t2 {RECORDING_2} - - -/- embedded 1"),
            "choice undo 4 carried".to_owned(),
            "job manual".to_owned(),
        ],
    );
    let report = rig
        .attempt(&rig.service, "job-f", "f")
        .await
        .expect("report");
    assert_eq!(report.outcome, IdentificationOutcome::Identified);
    let undone = rig.change(|tx| choice::undo(tx, "f", "curator-1", 2.0));
    check(
        "undo after the identification a hand back queued",
        vec![format!("{undone:?}")]
            .into_iter()
            .chain(
                rig.identity_tables("f")
                    .into_iter()
                    .filter(|line| !line.starts_with("track ")),
            )
            .collect(),
        vec![
            "Ok(())".to_owned(),
            format!("album {GROUP_A} - manual 6"),
            "custom m-f".to_owned(),
        ],
    );

    // A curator approves a release with evidence: the album is theirs, its
    // files wait to be placed as their choice, the track rows are left,
    // and only the approved review settles.
    rig.seed_album(&album_facts("g", &[("g1", Some(RECORDING_1))]));
    rig.manifest("g", "m-g");
    rig.run(&format!(
        "INSERT INTO local_album_external_identities (local_album_id, provider, \
         release_group_mbid, release_mbid, decision_source, selected_at, row_revision) \
         VALUES ('g', 'musicbrainz', '{GROUP_A}', '{RELEASE_A2}', 'automatic', 0, 1); \
         INSERT INTO library_album_match_state (local_album_id, state, reason_code, \
         candidates_json, updated_at) VALUES ('g', 'unconfirmed', 'OLD', '[]', 0); \
         INSERT INTO library_custom_edition_active (local_album_id, manifest_id, \
         activated_at) VALUES ('g', 'm-g', 0); \
         INSERT INTO library_edition_remap_queue (local_album_id, release_mbid, origin, \
         queued_at) VALUES ('g', '{RELEASE_A2}', 'place_only', 0);"
    ));
    for review in ["rev-g", "rev-h"] {
        identify::review::file_review(
            rig.store.as_ref(),
            review,
            "g",
            "EDITION_UNCERTAIN",
            vec![offering(RELEASE_A1, &["g1"], &[]).evidence],
        )
        .unwrap();
    }
    assert_eq!(
        rig.service
            .approve_candidate("rev-g", "curator-1", &key(GROUP_A, RELEASE_A1)),
        Ok(true)
    );
    check(
        "approve a release",
        rig.identity_tables("g"),
        vec![
            format!("album {GROUP_A} {RELEASE_A1} manual 2"),
            "choice undo 2".to_owned(),
            format!("remap album_pin {RELEASE_A1} curator-1"),
            "review rev-g approved".to_owned(),
            "review rev-h pending".to_owned(),
        ],
    );

    // "Looks right" makes a best guess the person's; a v2 choice already
    // is one and keeps its source.
    for (album, source, after) in [
        ("h", "automatic", "manual 3"),
        ("i", "legacy_import", "legacy_import 2"),
    ] {
        rig.seed_album(&album_facts(
            album,
            &[(format!("{album}1").as_str(), Some(RECORDING_1))],
        ));
        rig.run(&format!(
            "INSERT INTO local_album_external_identities (local_album_id, provider, \
             release_group_mbid, release_mbid, decision_source, selected_at, row_revision) \
             VALUES ('{album}', 'musicbrainz', '{GROUP_A}', '{RELEASE_A1}', '{source}', 0, 2); \
             INSERT INTO library_album_match_state (local_album_id, state, reason_code, \
             candidates_json, updated_at) VALUES ('{album}', 'unconfirmed', 'GUESS', '[]', 0);"
        ));
        let confirmed = rig.change(|tx| choice::confirm(tx, album, "curator-1", 1.0));
        let undo = after.split(' ').next_back().unwrap_or_default();
        check(
            &format!("confirm a {source} match"),
            vec![format!("{confirmed:?}")]
                .into_iter()
                .chain(rig.identity_tables(album))
                .collect(),
            vec![
                "Ok(())".to_owned(),
                format!("album {GROUP_A} {RELEASE_A1} {after}"),
                format!("choice undo {undo}"),
            ],
        );
    }

    // Choosing places the supported file as the person's, leaves the other
    // unplaced as theirs, keeps "leave unmanaged", and settles the reviews.
    rig.seed_album(&album_facts(
        "j",
        &[("j1", Some(RECORDING_1)), ("j2", Some(RECORDING_2))],
    ));
    rig.run(&format!(
        "INSERT INTO local_album_external_identities (local_album_id, provider, \
         release_group_mbid, release_mbid, decision_source, selected_at, row_revision) \
         VALUES ('j', 'musicbrainz', '{GROUP_A}', '{RELEASE_A2}', 'automatic', 0, 1); \
         INSERT INTO local_track_external_identities (local_track_id, provider, \
         recording_mbid, release_mbid, release_track_mbid, decision_source, selected_at) \
         VALUES ('j2', 'musicbrainz', '{RECORDING_2}', '{RELEASE_A2}', 'old', 'automatic', 0); \
         INSERT INTO library_album_match_state (local_album_id, state, reason_code, \
         candidates_json, updated_at) VALUES ('j', 'unconfirmed', 'GUESS', '[]', 0); \
         INSERT INTO library_edition_remap_queue (local_album_id, release_mbid, origin, \
         queued_at) VALUES ('j', '{RELEASE_A2}', 'place_only', 0); \
         INSERT INTO library_management_exclusions (local_album_id, reason, \
         excluded_by_user_id, excluded_at) VALUES ('j', 'administrator_choice', 'curator-1', 0);"
    ));
    identify::review::file_review(
        rig.store.as_ref(),
        "rev-j",
        "j",
        "EDITION_UNCERTAIN",
        Vec::new(),
    )
    .unwrap();
    let chosen = rig.change(|tx| {
        choice::choose(
            tx,
            "j",
            &offering(RELEASE_A1, &["j1"], &["j2"]),
            Some("curator-1"),
            1.0,
        )
        .map(|choice| choice.extra_track_ids)
    });
    check(
        "choose",
        vec![format!("{chosen:?}")]
            .into_iter()
            .chain(rig.identity_tables("j"))
            .collect(),
        vec![
            format!("{:?}", Ok::<_, String>(vec!["j2".to_owned()])),
            format!("album {GROUP_A} {RELEASE_A1} manual 2"),
            format!("track j1 {RECORDING_1} {RELEASE_A1} rt-j1 1/1 manual 1"),
            format!("track j2 {RECORDING_2} - - -/- manual 2"),
            "choice undo 2".to_owned(),
            "exclusion administrator_choice".to_owned(),
            "review rev-j approved".to_owned(),
        ],
    );

    assert!(differ.is_empty(), "{}", differ.join("\n"));
}

const RECORDING_3: &str = "0c3f7a52-6f0b-4a58-9e39-4d5f0f6c1a23";
const RELEASE_A3: &str = "6b2d1c8e-3f4a-4b5c-8d9e-0a1b2c3d4e5f";
const UNFETCHED: &str = "1f2e3d4c-5b6a-4798-8a7b-6c5d4e3f2a1b";
const MERGED_A1: &str = "7e6d5c4b-3a29-4180-9f8e-7d6c5b4a3928";

/// One file's release id tag, media, barcode, catalog number and country.
type FileTags = (
    Option<&'static str>,
    &'static str,
    Option<&'static str>,
    Option<&'static str>,
    &'static str,
);

/// One shape of a confirmed album held on `held`, identified again with
/// the editions `recall` offers.
struct EditionIdCase {
    name: &'static str,
    held: Release,
    recall: Vec<Release>,
    files: [FileTags; 3],
    source: DecisionSource,
    live_loose: bool,
    expected: &'static str,
}

/// An edition of `GROUP_A` on `media` from `country`, with a barcode and
/// catalog number when given.
fn pressing(
    id: &str,
    media: &str,
    country: &str,
    barcode: Option<&str>,
    catalog: Option<&str>,
) -> Release {
    let mut release = release(GROUP_A, id, &[RECORDING_1, RECORDING_2, RECORDING_3]);
    release.media = vec![droppedneedle_library::matching::ReleaseMedium {
        position: 1,
        format: Some(media.to_owned()),
        title: None,
        track_count: 3,
    }];
    release.country = Some(country.to_owned());
    release.barcode = barcode.map(str::to_owned);
    release.catalog_numbers = catalog.map(str::to_owned).into_iter().collect();
    release
}

/// A confirmed album moves to another edition of its group when no file
/// carries a release ID for the group, the barcode or catalog number most
/// files carry names that edition while the held one states another, and
/// the new one fits as any confirmation must and contradicts no more of
/// the files' media and country. Every other shape keeps its edition.
#[tokio::test]
async fn a_confirmed_album_moves_to_the_edition_its_barcode_names() {
    let vinyl = || {
        pressing(
            RELEASE_A1,
            "12\" Vinyl",
            "GB",
            Some("5012345678900"),
            Some("WS 1768"),
        )
    };
    let cd = || {
        pressing(
            RELEASE_A2,
            "CD",
            "US",
            Some("075992717625"),
            Some("W2 1768"),
        )
    };
    let merged_vinyl = || {
        let mut held = vinyl();
        held.old_ids = vec![MERGED_A1.to_owned()];
        held
    };
    let cd_tags: FileTags = (None, "CD", Some("075992717625"), Some("W2 1768"), "US");
    let plain_cd: FileTags = (None, "CD", None, None, "US");
    let tagged = |id| -> FileTags { (Some(id), "CD", Some("075992717625"), Some("W2 1768"), "US") };
    let base = |name| EditionIdCase {
        name,
        held: vinyl(),
        recall: vec![vinyl(), cd()],
        files: [cd_tags; 3],
        source: DecisionSource::Automatic,
        live_loose: false,
        expected: RELEASE_A1,
    };
    let mut in_group_b = release(
        GROUP_B,
        RELEASE_B1,
        &[RECORDING_2, RECORDING_1, RECORDING_3],
    );
    in_group_b.title = "Another Album".to_owned();
    let mut short_cd = cd();
    short_cd.tracks.truncate(1);
    let cases = vec![
        EditionIdCase {
            expected: RELEASE_A2,
            ..base("untagged files move to the edition their barcode names")
        },
        EditionIdCase {
            files: [tagged(RELEASE_A1); 3],
            ..base("a release-ID tag naming the held edition wins")
        },
        EditionIdCase {
            files: [tagged(RELEASE_A1), cd_tags, cd_tags],
            ..base("so does one on a minority of files")
        },
        EditionIdCase {
            recall: vec![vinyl(), cd(), pressing(RELEASE_A3, "CD", "JP", None, None)],
            files: [tagged(RELEASE_A3), cd_tags, cd_tags],
            ..base("a file naming another edition of the group")
        },
        EditionIdCase {
            files: [tagged(UNFETCHED), cd_tags, cd_tags],
            ..base("a file naming a release recall did not fetch")
        },
        EditionIdCase {
            held: merged_vinyl(),
            recall: vec![merged_vinyl(), cd()],
            files: [tagged(MERGED_A1), cd_tags, cd_tags],
            ..base("a file naming the held edition by a merged id")
        },
        EditionIdCase {
            recall: vec![vinyl(), cd(), in_group_b],
            files: [tagged(RELEASE_B1), cd_tags, cd_tags],
            expected: RELEASE_A2,
            ..base("a file naming a release of another group does not hold it")
        },
        EditionIdCase {
            files: [plain_cd; 3],
            ..base("media and country alone never move it")
        },
        EditionIdCase {
            files: [(
                None,
                "12\" Vinyl",
                Some("075992717625"),
                Some("W2 1768"),
                "GB",
            ); 3],
            ..base("the named edition contradicts more of the files' tags")
        },
        EditionIdCase {
            recall: vec![vinyl(), short_cd],
            ..base("the named edition does not fit the files")
        },
        EditionIdCase {
            held: pressing(RELEASE_A1, "12\" Vinyl", "GB", Some("075992717625"), None),
            recall: vec![
                pressing(RELEASE_A1, "12\" Vinyl", "GB", Some("075992717625"), None),
                cd(),
            ],
            ..base("the held edition carries the barcode too")
        },
        EditionIdCase {
            held: pressing(RELEASE_A1, "12\" Vinyl", "GB", None, None),
            recall: vec![pressing(RELEASE_A1, "12\" Vinyl", "GB", None, None), cd()],
            ..base("the held edition is silent on both")
        },
        EditionIdCase {
            recall: vec![
                vinyl(),
                pressing(RELEASE_A2, "CD", "US", Some("075992717625"), Some("X 9")),
            ],
            ..base("the named edition states another catalog number")
        },
        EditionIdCase {
            files: [cd_tags, plain_cd, plain_cd],
            ..base("a barcode on fewer than half the files")
        },
        EditionIdCase {
            live_loose: true,
            ..base("a live edition that fits too loosely to confirm")
        },
        EditionIdCase {
            source: DecisionSource::Manual,
            ..base("a person's choice")
        },
        EditionIdCase {
            source: DecisionSource::LegacyImport,
            ..base("a choice carried from v2")
        },
    ];
    let mut wrong = Vec::new();
    for case in cases {
        let live_loose = case.live_loose;
        let releases = case
            .recall
            .into_iter()
            .map(|release| if live_loose { live(release) } else { release })
            .collect();
        let rig = rig_with_recall(recall(releases));
        let mut facts = album_facts(
            "album-1",
            &[
                ("t1", Some(RECORDING_1)),
                ("t2", Some(RECORDING_2)),
                ("t3", Some(RECORDING_3)),
            ],
        );
        if live_loose {
            facts.tracks[1].duration_secs = Some(215);
        }
        rig.seed_album(&facts);
        for (track, (release_id, media, barcode, catalog, country)) in
            facts.tracks.iter().zip(case.files)
        {
            rig.db
                .execute(
                    "UPDATE local_tracks SET embedded_release_mbid = ?2 WHERE id = ?1",
                    params![track.local_track_id, release_id],
                )
                .expect("release id tagged");
            rig.db
                .execute(
                    "INSERT INTO local_track_edition_tags (local_track_id, media, barcode, \
                     catalog_number, release_country) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![track.local_track_id, media, barcode, catalog, country],
                )
                .expect("edition tags read");
        }
        rig.store
            .save_album_identity(AlbumIdentity {
                local_album_id: "album-1".to_owned(),
                provider: "musicbrainz".to_owned(),
                release_group_mbid: Some(GROUP_A.to_owned()),
                release_mbid: Some(case.held.id.clone()),
                decision_source: case.source,
                row_revision: 1,
            })
            .unwrap();
        rig.attempt(&rig.service, "job-1", "album-1").await;
        let got = rig
            .store
            .album_identity("album-1")
            .unwrap()
            .and_then(|identity| identity.release_mbid);
        if got.as_deref() != Some(case.expected) {
            wrong.push(format!(
                "{}: expected {}, got {got:?}",
                case.name, case.expected
            ));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// Recalls a scripted answer that misses the held edition, and answers
/// the held edition's own recall with a second script.
struct HeldRecall {
    recall: RecallResult,
    held: RecallResult,
}

impl identify::providers::IdentifyProviders for HeldRecall {
    fn recall_candidates(
        &self,
        _facts: &LocalAlbumFacts,
        _named_group: Option<&str>,
        _priority: droppedneedle_providers::RequestPriority,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = identify::providers::RecallOutcome> + Send + '_>,
    > {
        let result = self.recall.clone();
        Box::pin(async move {
            identify::providers::RecallOutcome {
                result,
                recall_criticality: None,
            }
        })
    }

    fn recall_release(
        &self,
        _facts: &LocalAlbumFacts,
        _release_mbid: &str,
        _priority: droppedneedle_providers::RequestPriority,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = identify::providers::RecallOutcome> + Send + '_>,
    > {
        let result = self.held.clone();
        Box::pin(async move {
            identify::providers::RecallOutcome {
                result,
                recall_criticality: None,
            }
        })
    }
}

/// An album whose held edition MusicBrainz cannot answer for stays on it:
/// the attempt waits as a busy recall does, even when recall offers
/// another edition of the group that fits.
#[tokio::test]
async fn a_held_edition_that_cannot_be_fetched_keeps_the_album() {
    let rig = rig_with_recall(RecallResult::default());
    rig.seed_album(&album_facts(
        "album-1",
        &[("t1", Some(RECORDING_1)), ("t2", Some(RECORDING_2))],
    ));
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
    let service = IdentifyService::new(IdentifyDeps {
        store: rig.store.clone(),
        providers: Arc::new(HeldRecall {
            recall: recall(vec![release(
                GROUP_A,
                RELEASE_A2,
                &[RECORDING_1, RECORDING_2],
            )]),
            held: RecallResult {
                provider_deferred: true,
                failure_code: Some("musicbrainz_unavailable".to_owned()),
                ..RecallResult::default()
            },
        }),
    });

    let report = rig.attempt(&service, "job-1", "album-1").await;

    let held = rig.store.album_identity("album-1").unwrap();
    assert_eq!(
        held.and_then(|row| row.release_mbid).as_deref(),
        Some(RELEASE_A1)
    );
    assert_eq!(
        report.map(|report| report.outcome),
        Some(IdentificationOutcome::ProviderDeferred)
    );
    let job = rig.store.job("job-1").unwrap().expect("job");
    assert_eq!(job.state, JobState::Deferred);
}

/// A file carrying a recording id MusicBrainz merged, which only the held
/// edition's own recall resolved, does not count against the held edition:
/// recall offered another edition of the group (a stale document still
/// listing the retired id), and the album stays where it is.
#[tokio::test]
async fn a_merged_recording_the_held_edition_resolves_keeps_the_album() {
    const RETIRED: &str = "3c2b1a09-8f7e-4d6c-9b5a-4f3e2d1c0b9a";
    let rig = rig_with_recall(RecallResult::default());
    rig.seed_album(&album_facts(
        "album-1",
        &[("t1", Some(RECORDING_1)), ("t2", Some(RETIRED))],
    ));
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
    let mut held = recall(vec![release(
        GROUP_A,
        RELEASE_A1,
        &[RECORDING_1, RECORDING_2],
    )]);
    held.recording_aliases
        .insert(RETIRED.to_owned(), RECORDING_2.to_owned());
    let service = IdentifyService::new(IdentifyDeps {
        store: rig.store.clone(),
        providers: Arc::new(HeldRecall {
            recall: recall(vec![release(GROUP_A, RELEASE_A2, &[RECORDING_1, RETIRED])]),
            held,
        }),
    });

    rig.attempt(&service, "job-1", "album-1").await;

    let kept = rig.store.album_identity("album-1").unwrap();
    assert_eq!(
        kept.and_then(|row| row.release_mbid).as_deref(),
        Some(RELEASE_A1)
    );
}
