//! Identify briefs: proofs decide, curators overrule, names never merge.
//!
//! Every behavior reads as one identity brief: the album, what the library
//! believes about it, and why. The store is the SQLite one, on a private
//! copy of the migrated database; albums, tracks and artists are catalog
//! rows a test seeds. MusicBrainz is scripted (`FakeProviders`): nothing
//! here touches the network.

use droppedneedle::library::identify;

use std::sync::Arc;

use droppedneedle::library::matching::{CreditedArtist, Release, ReleaseTrack};
use identify::models::IdentifyJob;
use identify::models::JobState;
use identify::models::{
    AlbumIdentity, AliasKind, ArtistCredit, ArtistIdentity, AutomaticSeal, CreditProof,
    DecisionSource, IdentificationOutcome, IdentifyKind, IdentityBrief, LocalAlbumFacts,
    LocalTrackFacts, RecallResult, ReviewState, TrackIdentity,
};
use identify::providers::FakeProviders;
use identify::queue::{
    LEASE_SECONDS, MAX_BACKOFF_SECONDS, PRIORITY_HISTORICAL_BACKLOG, PRIORITY_NEW_OR_CHANGED,
    PRIORITY_REVIEW_RETRY, PRIORITY_SUPPORTING_MAINTENANCE, backoff_secs, terminally_deferred,
};
use identify::rules::{
    ReconciliationProof, ReconciliationVerdict, SubstitutionCase, SubstitutionRefusal,
    SubstitutionVerdict, evaluate_overwrite, evaluate_reconciliation, evaluate_substitution,
};
use identify::service::{IdentifyDeps, IdentifyService};
use identify::sqlite::SqliteIdentifyStore;
use identify::stores::{AttemptLanding, land_job};
use rusqlite::params;

use crate::common::{ScratchDir, migrated_db};

const GROUP_A: &str = "fc97b087-221c-4ea4-9dd9-5277a52eb84a";
const RELEASE_A1: &str = "aff0622e-7bd3-4fb6-9ca3-0fa19dd2340b";
const RELEASE_A2: &str = "0687c8a5-40a2-4a0c-bdc9-c1d80d94bef5";
const GROUP_B: &str = "dcff25f1-702d-3b5e-b0da-d48172e6e62a";
const RELEASE_B1: &str = "c85ad49c-6bfb-4bdc-96f8-f5f305a8799e";
const ARTIST_MBID: &str = "5441c29d-3602-4898-b1a1-b77fa23b8e50";
const OTHER_ARTIST_MBID: &str = "9cb4af06-1c2d-4e5f-8a7b-6c5d4e3f2a10";
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
    let store = Arc::new(SqliteIdentifyStore::open(&path).expect("store opens"));
    let providers = Arc::new(FakeProviders::with_recall(recall));
    let service = IdentifyService::new(IdentifyDeps {
        identities: store.clone(),
        facts: store.clone(),
        proofs: store.clone(),
        aliases: store.clone(),
        queue: store.clone(),
        reviews: store.clone(),
        releases: store.clone(),
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

    /// `album-1` with its one track `t1`, both automatically identified,
    /// the album row at `album_revision` and the track row at 1: what a
    /// credit proof row is checked against.
    fn seed_identified_album(&self, album_revision: u64) {
        self.seed_album(&album_facts("album-1", &[("t1", Some(RECORDING_1))]));
        self.store.save_album_identity(AlbumIdentity {
            local_album_id: "album-1".to_owned(),
            provider: "musicbrainz".to_owned(),
            release_group_mbid: Some(GROUP_A.to_owned()),
            release_mbid: Some(RELEASE_A1.to_owned()),
            decision_source: DecisionSource::Automatic,
            row_revision: album_revision,
        });
        self.store.save_track_identity(TrackIdentity {
            local_track_id: "t1".to_owned(),
            provider: "musicbrainz".to_owned(),
            recording_mbid: Some(RECORDING_1.to_owned()),
            release_track_mbid: None,
            decision_source: DecisionSource::Automatic,
            row_revision: 1,
        });
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

fn proof_row(album: &str, track: &str, source: &str, mbid: &str, release: &str) -> CreditProof {
    CreditProof {
        local_album_id: album.to_owned(),
        local_track_id: track.to_owned(),
        source_local_artist_id: source.to_owned(),
        artist_mbid: mbid.to_owned(),
        release_mbid: release.to_owned(),
        album_identity_revision: 1,
        track_identity_revision: 1,
    }
}

#[tokio::test]
async fn automatic_identity_is_revisable() {
    let first = recall(vec![release(GROUP_A, RELEASE_A1, &[RECORDING_1])]);
    let rig = rig_with_recall(first);
    rig.seed_album(&album_facts("album-1", &[("t1", Some(RECORDING_1))]));
    rig.service.enqueue_album(
        "job-1",
        "album-1",
        IdentifyKind::Automatic,
        "rev-1",
        None,
        0,
    );
    rig.store.claim(0, 60_000);
    let report = rig
        .service
        .run_claimed_job("job-1", 0)
        .await
        .expect("report");
    assert_eq!(report.outcome, IdentificationOutcome::Identified);
    assert_eq!(report.job.state, JobState::Succeeded);
    assert_eq!(
        rig.store
            .album_identity("album-1")
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
    rig.service.enqueue_album(
        "job-2",
        "album-1",
        IdentifyKind::Automatic,
        "rev-2",
        None,
        0,
    );
    rig.store.claim(0, 60_000);
    let report = rig
        .service
        .run_claimed_job("job-2", 0)
        .await
        .expect("report");
    assert_eq!(report.outcome, IdentificationOutcome::Identified);
    let identity = rig.store.album_identity("album-1").expect("identity");
    assert_eq!(identity.release_group_mbid.as_deref(), Some(GROUP_B));
    assert_eq!(identity.decision_source, DecisionSource::Automatic);
    let brief = rig.service.identity_brief("album-1");
    assert!(brief.revisable);
    assert_eq!(brief.protected_source, None);
}

#[tokio::test]
async fn automatic_retracts_on_contradiction() {
    let rig = rig_with_recall(recall(vec![release(GROUP_A, RELEASE_A1, &[RECORDING_2])]));
    rig.seed_album(&album_facts("album-1", &[("t1", Some(RECORDING_1))]));
    rig.store.save_album_identity(AlbumIdentity {
        local_album_id: "album-1".to_owned(),
        provider: "musicbrainz".to_owned(),
        release_group_mbid: Some(GROUP_A.to_owned()),
        release_mbid: Some(RELEASE_A1.to_owned()),
        decision_source: DecisionSource::Automatic,
        row_revision: 1,
    });
    rig.service.enqueue_album(
        "job-1",
        "album-1",
        IdentifyKind::Automatic,
        "rev-1",
        None,
        0,
    );
    rig.store.claim(0, 60_000);
    let report = rig
        .service
        .run_claimed_job("job-1", 0)
        .await
        .expect("report");
    assert_eq!(report.outcome, IdentificationOutcome::Contradictory);
    assert!(rig.store.album_identity("album-1").is_none());
    // Nothing waits in a blocking review: the album keeps its own tags.
    assert_eq!(report.review_id, None);
}

#[tokio::test]
async fn manual_survives_rescan() {
    let rig = rig_with_recall(recall(vec![release(GROUP_B, RELEASE_B1, &[RECORDING_1])]));
    rig.seed_album(&album_facts("album-1", &[("t1", Some(RECORDING_1))]));
    rig.store.save_album_identity(AlbumIdentity {
        local_album_id: "album-1".to_owned(),
        provider: "musicbrainz".to_owned(),
        release_group_mbid: Some(GROUP_A.to_owned()),
        release_mbid: Some(RELEASE_A1.to_owned()),
        decision_source: DecisionSource::Manual,
        row_revision: 3,
    });
    // A rescan re-reads the files, then a fresh automatic pass disagrees.
    rig.rescan("album-1");
    rig.service.enqueue_album(
        "job-1",
        "album-1",
        IdentifyKind::Automatic,
        "rev-2",
        None,
        0,
    );
    rig.store.claim(0, 60_000);
    let report = rig
        .service
        .run_claimed_job("job-1", 0)
        .await
        .expect("report");
    assert_eq!(report.reason_code, "CHOSEN_EDITION_KEPT");
    let identity = rig.store.album_identity("album-1").expect("identity");
    assert_eq!(identity.release_group_mbid.as_deref(), Some(GROUP_A));
    assert_eq!(identity.decision_source, DecisionSource::Manual);
    assert_eq!(identity.row_revision, 3);
    assert_eq!(report.review_id, None);
    let brief: IdentityBrief = rig.service.identity_brief("album-1");
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
    rig.store.save_album_identity(chosen.clone());

    let sealed = rig.store.seal_automatic(&AutomaticSeal {
        local_album_id: "album-1".to_owned(),
        release_group_mbid: GROUP_B.to_owned(),
        release_mbid: Some(RELEASE_B1.to_owned()),
        tracks: Vec::new(),
        flag: None,
    });
    assert!(!sealed);
    rig.store.save_album_identity(AlbumIdentity {
        release_group_mbid: Some(GROUP_B.to_owned()),
        release_mbid: Some(RELEASE_B1.to_owned()),
        decision_source: DecisionSource::Automatic,
        row_revision: 1,
        ..chosen.clone()
    });
    rig.store.clear_album_identity("album-1");

    assert_eq!(rig.store.album_identity("album-1"), Some(chosen));
}

#[tokio::test]
async fn legacy_import_survives_rescan() {
    let rig = rig_with_recall(recall(vec![release(GROUP_B, RELEASE_B1, &[RECORDING_1])]));
    rig.seed_album(&album_facts("album-1", &[("t1", Some(RECORDING_1))]));
    rig.store.save_album_identity(AlbumIdentity {
        local_album_id: "album-1".to_owned(),
        provider: "musicbrainz".to_owned(),
        release_group_mbid: Some(GROUP_A.to_owned()),
        release_mbid: Some(RELEASE_A1.to_owned()),
        decision_source: DecisionSource::LegacyImport,
        row_revision: 1,
    });
    rig.rescan("album-1");
    rig.service.enqueue_album(
        "job-1",
        "album-1",
        IdentifyKind::Automatic,
        "rev-2",
        None,
        0,
    );
    rig.store.claim(0, 60_000);
    rig.service
        .run_claimed_job("job-1", 0)
        .await
        .expect("report");
    let identity = rig.store.album_identity("album-1").expect("identity");
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
    rig.store.save_track_identity(TrackIdentity {
        local_track_id: "t2".to_owned(),
        provider: "musicbrainz".to_owned(),
        recording_mbid: Some(RECORDING_2.to_owned()),
        release_track_mbid: None,
        decision_source: DecisionSource::Manual,
        row_revision: 7,
    });
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
        .expect("report");
    // The automatic track sealed; the manual track kept its own row.
    assert_eq!(
        rig.store.track_identity("t1").expect("t1").decision_source,
        DecisionSource::Automatic
    );
    let kept = rig.store.track_identity("t2").expect("t2");
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

#[test]
fn substitution_accepted_release_allows() {
    let verdict = evaluate_substitution(&SubstitutionCase {
        accepted_release_mbid: Some(RELEASE_A1.to_owned()),
        durable_proof_mbids: Vec::new(),
        expected_artist_mbid: ARTIST_MBID.to_owned(),
        credits_unambiguous: true,
        direct_identity_mbid: None,
        name_evidence_present: true,
    });
    assert_eq!(verdict, SubstitutionVerdict::Allowed);
}

#[test]
fn substitution_durable_proof_allows() {
    let verdict = evaluate_substitution(&SubstitutionCase {
        accepted_release_mbid: None,
        durable_proof_mbids: vec![ARTIST_MBID.to_owned(), ARTIST_MBID.to_owned()],
        expected_artist_mbid: ARTIST_MBID.to_owned(),
        credits_unambiguous: true,
        direct_identity_mbid: None,
        name_evidence_present: false,
    });
    assert_eq!(verdict, SubstitutionVerdict::Allowed);
}

#[test]
fn substitution_missing_proof_waits() {
    let verdict = evaluate_substitution(&SubstitutionCase {
        accepted_release_mbid: None,
        durable_proof_mbids: Vec::new(),
        expected_artist_mbid: ARTIST_MBID.to_owned(),
        credits_unambiguous: true,
        direct_identity_mbid: None,
        name_evidence_present: false,
    });
    assert_eq!(verdict, SubstitutionVerdict::Waiting);
}

#[test]
fn substitution_name_only_refuses() {
    let verdict = evaluate_substitution(&SubstitutionCase {
        accepted_release_mbid: None,
        durable_proof_mbids: Vec::new(),
        expected_artist_mbid: ARTIST_MBID.to_owned(),
        credits_unambiguous: true,
        direct_identity_mbid: None,
        name_evidence_present: true,
    });
    assert_eq!(
        verdict,
        SubstitutionVerdict::Refused(SubstitutionRefusal::NameOnly)
    );
}

#[test]
fn substitution_conflicting_proof_needs_review() {
    let verdict = evaluate_substitution(&SubstitutionCase {
        accepted_release_mbid: Some(RELEASE_A1.to_owned()),
        durable_proof_mbids: vec![ARTIST_MBID.to_owned(), OTHER_ARTIST_MBID.to_owned()],
        expected_artist_mbid: ARTIST_MBID.to_owned(),
        credits_unambiguous: true,
        direct_identity_mbid: None,
        name_evidence_present: false,
    });
    assert_eq!(verdict, SubstitutionVerdict::NeedsReview);
}

#[test]
fn substitution_composite_credits_refuse() {
    let verdict = evaluate_substitution(&SubstitutionCase {
        accepted_release_mbid: Some(RELEASE_A1.to_owned()),
        durable_proof_mbids: Vec::new(),
        expected_artist_mbid: ARTIST_MBID.to_owned(),
        credits_unambiguous: false,
        direct_identity_mbid: None,
        name_evidence_present: false,
    });
    assert_eq!(
        verdict,
        SubstitutionVerdict::Refused(SubstitutionRefusal::CompositeOrAmbiguousCredits)
    );
}

#[test]
fn substitution_conflicting_direct_identity_refuses() {
    let verdict = evaluate_substitution(&SubstitutionCase {
        accepted_release_mbid: Some(RELEASE_A1.to_owned()),
        durable_proof_mbids: Vec::new(),
        expected_artist_mbid: ARTIST_MBID.to_owned(),
        credits_unambiguous: true,
        direct_identity_mbid: Some(OTHER_ARTIST_MBID.to_owned()),
        name_evidence_present: false,
    });
    assert_eq!(
        verdict,
        SubstitutionVerdict::Refused(SubstitutionRefusal::ConflictingDirectIdentity)
    );
}

/// Two local artists a folded name nominated for merging, and `album-1`
/// identified with its album row at `album_revision`.
fn retirement_rig(album_revision: u64) -> Rig {
    let rig = rig_with_recall(RecallResult::default());
    rig.seed_artist("artist-old");
    rig.seed_artist("artist-new");
    rig.seed_identified_album(album_revision);
    rig
}

#[test]
fn stale_proof_rows_do_not_count() {
    // The album's identity row moved on to revision 2 after the proof was
    // banked at revision 1, so the stored row is stale.
    let rig = retirement_rig(2);
    rig.store.save_proof(proof_row(
        "album-1",
        "t1",
        "artist-old",
        ARTIST_MBID,
        RELEASE_A1,
    ));
    let verdict = rig
        .service
        .retire_artist("artist-old", "artist-new", ARTIST_MBID, true, None);
    assert_eq!(
        verdict,
        SubstitutionVerdict::Refused(SubstitutionRefusal::NameOnly)
    );
    assert_eq!(rig.store.resolve("artist-old"), "artist-old");
}

#[test]
fn retire_artist_through_each_gate() {
    // Accepted release: allowed.
    let rig = retirement_rig(1);
    rig.db
        .execute(
            "INSERT INTO local_album_artists (local_album_id, position, local_artist_id, role) \
             VALUES ('album-1', 0, 'artist-old', 'primary')",
            [],
        )
        .expect("album credit seeds");
    let verdict = rig
        .service
        .retire_artist("artist-old", "artist-new", ARTIST_MBID, true, None);
    assert_eq!(verdict, SubstitutionVerdict::Allowed);
    assert_eq!(rig.store.resolve("artist-old"), "artist-new");

    // Durable proof row: allowed.
    let rig = retirement_rig(1);
    rig.store.save_proof(proof_row(
        "album-1",
        "t1",
        "artist-old",
        ARTIST_MBID,
        RELEASE_A1,
    ));
    let verdict = rig
        .service
        .retire_artist("artist-old", "artist-new", ARTIST_MBID, true, None);
    assert_eq!(verdict, SubstitutionVerdict::Allowed);

    // Conflicting proof: a curator decides, nothing retargets.
    let rig = retirement_rig(1);
    rig.store.save_proof(proof_row(
        "album-1",
        "t1",
        "artist-old",
        ARTIST_MBID,
        RELEASE_A1,
    ));
    rig.store.save_proof(proof_row(
        "album-1",
        "t1",
        "artist-old",
        OTHER_ARTIST_MBID,
        RELEASE_A1,
    ));
    let verdict = rig
        .service
        .retire_artist("artist-old", "artist-new", ARTIST_MBID, true, None);
    assert_eq!(verdict, SubstitutionVerdict::NeedsReview);
    assert_eq!(rig.store.resolve("artist-old"), "artist-old");
}

#[test]
fn name_only_merge_refused() {
    assert_eq!(
        evaluate_reconciliation(&ReconciliationProof {
            name_similarity: 0.99,
            ..ReconciliationProof::default()
        }),
        ReconciliationVerdict::RefusedNameOnly
    );
    assert_eq!(
        evaluate_reconciliation(&ReconciliationProof {
            name_similarity: 0.99,
            accepted_release_identity: true,
            ..ReconciliationProof::default()
        }),
        ReconciliationVerdict::Merge
    );
    assert_eq!(
        evaluate_reconciliation(&ReconciliationProof {
            name_similarity: 0.2,
            durable_proof_rows: 2,
            ..ReconciliationProof::default()
        }),
        ReconciliationVerdict::Merge
    );
    assert_eq!(
        evaluate_reconciliation(&ReconciliationProof {
            name_similarity: 0.99,
            durable_proof_rows: 1,
            contradictory_proof: true,
            ..ReconciliationProof::default()
        }),
        ReconciliationVerdict::NeedsReview
    );
    assert_eq!(
        evaluate_reconciliation(&ReconciliationProof::default()),
        ReconciliationVerdict::Waiting
    );
}

#[test]
fn retired_ids_keep_resolving() {
    let rig = retirement_rig(1);
    rig.store.save_proof(proof_row(
        "album-1",
        "t1",
        "artist-old",
        ARTIST_MBID,
        RELEASE_A1,
    ));
    rig.store.add_favorite("user-1", "artist", "artist-old");
    rig.db
        .execute(
            "INSERT INTO library_play_history (id, user_id, local_artist_id, track_name, \
             artist_name, played_at) VALUES ('play-1', 'user-1', 'artist-old', 'Song', \
             'Old Name', '2026-01-01T00:00:00Z')",
            [],
        )
        .expect("play seeds");
    let verdict = rig
        .service
        .retire_artist("artist-old", "artist-new", ARTIST_MBID, true, None);
    assert_eq!(verdict, SubstitutionVerdict::Allowed);
    // Compat lookups resolve through the alias.
    assert_eq!(rig.store.resolve("artist-old"), "artist-new");
    let aliases = rig.store.aliases_for("artist-new");
    assert_eq!(aliases.len(), 1);
    assert_eq!(aliases[0].kind, AliasKind::MergedArtist);
    // Live references followed the survivor.
    assert!(rig.store.favorite_holds("user-1", "artist", "artist-new"));
    assert!(!rig.store.favorite_holds("user-1", "artist", "artist-old"));
    let played: String = rig
        .db
        .query_row(
            "SELECT local_artist_id FROM library_play_history WHERE id = 'play-1'",
            [],
            |row| row.get(0),
        )
        .expect("play reads");
    assert_eq!(played, "artist-new");
}

#[test]
fn exact_contributors_become_appearances() {
    let rig = rig_with_recall(RecallResult::default());
    rig.seed_artist("artist-owned");
    rig.seed_album(&album_facts("album-1", &[("t1", Some(RECORDING_1))]));
    rig.store.save_owned_artist(ARTIST_MBID, "artist-owned");
    rig.store.save_track_credits(
        "t1",
        vec![
            ArtistCredit {
                position: 0,
                artist_mbid: ARTIST_MBID.to_owned(),
                canonical_name: "Owned".to_owned(),
                credited_name: "Owned".to_owned(),
            },
            ArtistCredit {
                position: 1,
                artist_mbid: OTHER_ARTIST_MBID.to_owned(),
                canonical_name: "Guest".to_owned(),
                credited_name: "Guest Star".to_owned(),
            },
        ],
    );
    let appearances = rig.service.appearances_for_track("t1");
    assert_eq!(appearances.len(), 1);
    assert_eq!(appearances[0].artist_mbid, OTHER_ARTIST_MBID);
    assert_eq!(appearances[0].credited_name, "Guest Star");
    // The guest never became an owned artist.
    assert_eq!(rig.store.owned_artist_by_mbid(OTHER_ARTIST_MBID), None);
    assert_eq!(
        rig.store.owned_artist_by_mbid(ARTIST_MBID).as_deref(),
        Some("artist-owned")
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
    );
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
    let stored = rig.store.get(review).expect("review");
    assert_eq!(stored.state, ReviewState::Approved);
    let identity = rig.store.album_identity("album-1").expect("identity");
    assert_eq!(identity.release_group_mbid.as_deref(), Some(GROUP_A));
    assert_eq!(identity.decision_source, DecisionSource::Manual);

    // A later automatic pass, whose evidence points at another group,
    // cannot move the curator's choice.
    rig.service.enqueue_album(
        "job-2",
        "album-1",
        IdentifyKind::Automatic,
        "rev-2",
        None,
        0,
    );
    rig.store.claim(0, 60_000);
    rig.service
        .run_claimed_job("job-2", 0)
        .await
        .expect("report");
    let held = rig.store.album_identity("album-1").expect("identity");
    assert_eq!(held.release_group_mbid.as_deref(), Some(GROUP_A));
    assert_eq!(held.decision_source, DecisionSource::Manual);
}

#[test]
fn review_reject_keeps_tagged() {
    let rig = rig_with_recall(RecallResult::default());
    rig.seed_album(&album_facts("album-1", &[("t1", Some(RECORDING_1))]));
    file_edition_review(&rig, "rev-1", "album-1");
    let review = "rev-1";
    assert!(rig.service.reject_candidates(review, "curator-1"));
    assert!(!rig.service.reject_candidates(review, "curator-1"));
    assert_eq!(
        rig.service
            .approve_candidate(review, "curator-1", &key(GROUP_A, RELEASE_A1)),
        Ok(false)
    );
    assert_eq!(
        rig.store.get(review).expect("review").state,
        ReviewState::Rejected
    );
    assert!(rig.store.album_identity("album-1").is_none());
    assert!(!rig.service.review_is_pending(review));
    assert!(rig.service.pending_reviews("album-1").is_empty());
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
    rig.service.enqueue_album(
        "job-1",
        "album-1",
        IdentifyKind::Automatic,
        "rev-1",
        None,
        0,
    );
    rig.store.claim(0, 60_000);
    let report = rig
        .service
        .run_claimed_job("job-1", 0)
        .await
        .expect("report");
    assert_eq!(report.outcome, IdentificationOutcome::EditionUncertain);
    let identity = rig.store.album_identity("album-1").expect("identity");
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
    rig.store.save_album_identity(AlbumIdentity {
        local_album_id: "album-1".to_owned(),
        provider: "musicbrainz".to_owned(),
        release_group_mbid: Some(GROUP_A.to_owned()),
        release_mbid: Some(RELEASE_A1.to_owned()),
        decision_source: DecisionSource::Automatic,
        row_revision: 1,
    });
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
        .expect("report");
    // Same-group weaker evidence must not demote the sealed exact.
    let identity = rig.store.album_identity("album-1").expect("identity");
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
    rig.service.enqueue_album(
        "job-backlog",
        "album-b",
        IdentifyKind::Historical,
        "r",
        None,
        0,
    );
    rig.service
        .enqueue_album("job-manual", "album-m", IdentifyKind::Manual, "r", None, 0);
    rig.service
        .enqueue_album("job-auto", "album-a", IdentifyKind::Automatic, "r", None, 0);
    assert_eq!(rig.store.jobs_for_album("album-a").len(), 1);
    let first = rig.store.claim(0, LEASE_SECONDS * 1000).expect("claim");
    assert_eq!(first.id, "job-auto");
    assert_eq!(first.priority, PRIORITY_NEW_OR_CHANGED);
    // Backoff ladder matches the owner-signed F-IDENT-04 sequence.
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
    rig.service.enqueue_album(
        "job-1",
        "album-1",
        IdentifyKind::Automatic,
        "rev-1",
        None,
        0,
    );
    rig.store.claim(0, 60_000);
    let report = rig
        .service
        .run_claimed_job("job-1", 1_000)
        .await
        .expect("report");
    assert_eq!(report.outcome, IdentificationOutcome::ProviderDeferred);
    let job = rig.store.job("job-1").expect("job");
    assert_eq!(job.state, identify::models::JobState::Deferred);
    assert_eq!(job.not_before_ms, 1_000 + 30_000);
    // No identity was written on a deferred attempt.
    assert!(rig.store.album_identity("album-1").is_none());
}

#[test]
fn brief_carries_the_whole_case() {
    let rig = rig_with_recall(RecallResult::default());
    let empty = rig.service.identity_brief("album-1");
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

#[test]
fn artist_identity_rows_round_trip() {
    let rig = rig_with_recall(RecallResult::default());
    assert_eq!(rig.store.artist_identity("artist-1"), None);
    rig.store.save_artist_identity(ArtistIdentity {
        local_artist_id: "artist-1".to_owned(),
        provider: "musicbrainz".to_owned(),
        provider_artist_mbid: Some(ARTIST_MBID.to_owned()),
        decision_source: DecisionSource::Manual,
        row_revision: 1,
    });
    let stored = rig.store.artist_identity("artist-1").expect("artist");
    assert_eq!(stored.provider_artist_mbid.as_deref(), Some(ARTIST_MBID));
    assert!(!stored.decision_source.automatic_may_overwrite());
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
    for table in [
        "library_identify_jobs",
        "library_identify_reviews",
        "library_identify_credit_proofs",
        "library_identify_track_credits",
    ] {
        let found: Option<String> =
            sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?1")
                .bind(table)
                .fetch_optional(&pool)
                .await
                .expect("schema reads");
        assert_eq!(found.as_deref(), Some(table));
    }
}
