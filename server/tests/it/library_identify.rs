//! Stage-8 identify briefs: proofs decide, curators overrule, names never merge.
//!
//! Brief-first: every behavior reads as one identity brief — the album,
//! what the library believes about it, and why. Memory fakes stand in
//! for the durable adapters; scripted transports stand in for the wire.
//! Nothing here touches the network or the production database.

use droppedneedle::library::identify;

use std::sync::Arc;

use droppedneedle::library::matching::{CreditedArtist, Release, ReleaseTrack};
use identify::memory::{
    MemoryAliasStore, MemoryIdentityStore, MemoryPinStore, MemoryProofStore, MemoryQueueStore,
    MemoryReleaseStore, MemoryReviewStore,
};
use identify::models::IdentifyJob;
use identify::models::JobState;
use identify::models::{
    AlbumIdentity, AliasKind, ArtistCredit, ArtistIdentity, CreditProof, DecisionSource,
    IdentificationOutcome, IdentifyKind, IdentityBrief, LocalAlbumFacts, LocalTrackFacts,
    RecallResult, ReleasePin, ReviewState, TrackIdentity,
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
use identify::stores::{
    AliasStore, AttemptLanding, IdentityStore, PinStore, ProofStore, QueueStore, ReleaseStore,
    ReviewStore, land_job,
};

const GROUP_A: &str = "fc97b087-221c-4ea4-9dd9-5277a52eb84a";
const RELEASE_A1: &str = "aff0622e-7bd3-4fb6-9ca3-0fa19dd2340b";
const RELEASE_A2: &str = "0687c8a5-40a2-4a0c-bdc9-c1d80d94bef5";
const GROUP_B: &str = "dcff25f1-702d-3b5e-b0da-d48172e6e62a";
const RELEASE_B1: &str = "c85ad49c-6bfb-4bdc-96f8-f5f305a8799e";
const ARTIST_MBID: &str = "5441c29d-3602-4898-b1a1-b77fa23b8e50";
const OTHER_ARTIST_MBID: &str = "9cb4af06-1c2d-4e5f-8a7b-6c5d4e3f2a10";
const RECORDING_1: &str = "beaf82cd-24f9-4163-b1a9-022339a30f77";
const RECORDING_2: &str = "5224cfc7-b3bb-4008-a41b-21b168dc631f";

struct Rig {
    service: IdentifyService,
    releases: Arc<MemoryReleaseStore>,
    identities: Arc<MemoryIdentityStore>,
    proofs: Arc<MemoryProofStore>,
    aliases: Arc<MemoryAliasStore>,
    pins: Arc<MemoryPinStore>,
    queue: Arc<MemoryQueueStore>,
    reviews: Arc<MemoryReviewStore>,
    providers: Arc<FakeProviders>,
}

fn rig_with_recall(recall: RecallResult) -> Rig {
    let identities = Arc::new(MemoryIdentityStore::default());
    let proofs = Arc::new(MemoryProofStore::default());
    let aliases = Arc::new(MemoryAliasStore::default());
    let pins = Arc::new(MemoryPinStore::default());
    let queue = Arc::new(MemoryQueueStore::default());
    let reviews = Arc::new(MemoryReviewStore::linked(identities.clone()));
    let providers = Arc::new(FakeProviders::with_recall(recall));
    let releases = Arc::new(MemoryReleaseStore::default());
    let service = IdentifyService::new(IdentifyDeps {
        identities: identities.clone(),
        facts: identities.clone(),
        proofs: proofs.clone(),
        aliases: aliases.clone(),
        pins: pins.clone(),
        queue: queue.clone(),
        reviews: reviews.clone(),
        releases: releases.clone(),
        providers: providers.clone(),
    });
    Rig {
        service,
        releases,
        identities,
        proofs,
        aliases,
        pins,
        queue,
        reviews,
        providers,
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
    rig.identities
        .save_album_facts(album_facts("album-1", &[("t1", Some(RECORDING_1))]));
    rig.service.enqueue_album(
        "job-1",
        "album-1",
        IdentifyKind::Automatic,
        "rev-1",
        None,
        0,
    );
    rig.queue.claim(0, 60_000);
    let report = rig
        .service
        .run_claimed_job("job-1", 0)
        .await
        .expect("report");
    assert_eq!(report.outcome, IdentificationOutcome::Identified);
    assert_eq!(report.job.state, JobState::Succeeded);
    assert_eq!(
        rig.identities
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
    rig.queue.claim(0, 60_000);
    let report = rig
        .service
        .run_claimed_job("job-2", 0)
        .await
        .expect("report");
    assert_eq!(report.outcome, IdentificationOutcome::Identified);
    let identity = rig.identities.album_identity("album-1").expect("identity");
    assert_eq!(identity.release_group_mbid.as_deref(), Some(GROUP_B));
    assert_eq!(identity.decision_source, DecisionSource::Automatic);
    let brief = rig.service.identity_brief("album-1");
    assert!(brief.revisable);
    assert_eq!(brief.protected_source, None);
}

#[tokio::test]
async fn automatic_retracts_on_contradiction() {
    let rig = rig_with_recall(recall(vec![release(GROUP_A, RELEASE_A1, &[RECORDING_2])]));
    rig.identities
        .save_album_facts(album_facts("album-1", &[("t1", Some(RECORDING_1))]));
    rig.identities.save_album_identity(AlbumIdentity {
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
    rig.queue.claim(0, 60_000);
    let report = rig
        .service
        .run_claimed_job("job-1", 0)
        .await
        .expect("report");
    assert_eq!(report.outcome, IdentificationOutcome::Contradictory);
    assert!(rig.identities.album_identity("album-1").is_none());
    let review = report.review_id.expect("review filed");
    assert!(rig.service.review_is_pending(&review));
}

#[tokio::test]
async fn manual_survives_rescan() {
    let rig = rig_with_recall(recall(vec![release(GROUP_B, RELEASE_B1, &[RECORDING_1])]));
    rig.identities
        .save_album_facts(album_facts("album-1", &[("t1", Some(RECORDING_1))]));
    rig.identities.save_album_identity(AlbumIdentity {
        local_album_id: "album-1".to_owned(),
        provider: "musicbrainz".to_owned(),
        release_group_mbid: Some(GROUP_A.to_owned()),
        release_mbid: Some(RELEASE_A1.to_owned()),
        decision_source: DecisionSource::Manual,
        row_revision: 3,
    });
    // A rescan re-reads the files, then a fresh automatic pass disagrees.
    rig.identities
        .save_album_facts(album_facts("album-1", &[("t1", Some(RECORDING_1))]));
    rig.service.enqueue_album(
        "job-1",
        "album-1",
        IdentifyKind::Automatic,
        "rev-2",
        None,
        0,
    );
    rig.queue.claim(0, 60_000);
    let report = rig
        .service
        .run_claimed_job("job-1", 0)
        .await
        .expect("report");
    assert_eq!(report.reason_code, "PROTECTED_IDENTITY");
    let identity = rig.identities.album_identity("album-1").expect("identity");
    assert_eq!(identity.release_group_mbid.as_deref(), Some(GROUP_A));
    assert_eq!(identity.decision_source, DecisionSource::Manual);
    assert_eq!(identity.row_revision, 3);
    assert!(report.review_id.is_some());
    let brief: IdentityBrief = rig.service.identity_brief("album-1");
    assert!(!brief.revisable);
    assert_eq!(brief.protected_source, Some(DecisionSource::Manual));
}

#[tokio::test]
async fn legacy_import_survives_rescan() {
    let rig = rig_with_recall(recall(vec![release(GROUP_B, RELEASE_B1, &[RECORDING_1])]));
    rig.identities
        .save_album_facts(album_facts("album-1", &[("t1", Some(RECORDING_1))]));
    rig.identities.save_album_identity(AlbumIdentity {
        local_album_id: "album-1".to_owned(),
        provider: "musicbrainz".to_owned(),
        release_group_mbid: Some(GROUP_A.to_owned()),
        release_mbid: Some(RELEASE_A1.to_owned()),
        decision_source: DecisionSource::LegacyImport,
        row_revision: 1,
    });
    rig.identities
        .save_album_facts(album_facts("album-1", &[("t1", Some(RECORDING_1))]));
    rig.service.enqueue_album(
        "job-1",
        "album-1",
        IdentifyKind::Automatic,
        "rev-2",
        None,
        0,
    );
    rig.queue.claim(0, 60_000);
    rig.service
        .run_claimed_job("job-1", 0)
        .await
        .expect("report");
    let identity = rig.identities.album_identity("album-1").expect("identity");
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
    rig.identities.save_album_facts(album_facts(
        "album-1",
        &[("t1", Some(RECORDING_1)), ("t2", Some(RECORDING_2))],
    ));
    rig.identities.save_track_identity(TrackIdentity {
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
    rig.queue.claim(0, 60_000);
    rig.service
        .run_claimed_job("job-1", 0)
        .await
        .expect("report");
    // The automatic track sealed; the manual track kept its own row.
    assert_eq!(
        rig.identities
            .track_identity("t1")
            .expect("t1")
            .decision_source,
        DecisionSource::Automatic
    );
    let kept = rig.identities.track_identity("t2").expect("t2");
    assert_eq!(kept.decision_source, DecisionSource::Manual);
    assert_eq!(kept.row_revision, 7);
}

#[tokio::test]
async fn quiet_reconfirm_files_no_review() {
    let rig = rig_with_recall(recall(vec![release(GROUP_A, RELEASE_A1, &[RECORDING_2])]));
    rig.identities
        .save_album_facts(album_facts("album-1", &[("t1", Some(RECORDING_1))]));
    rig.identities.save_album_identity(AlbumIdentity {
        local_album_id: "album-1".to_owned(),
        provider: "musicbrainz".to_owned(),
        release_group_mbid: Some(GROUP_A.to_owned()),
        release_mbid: Some(RELEASE_A1.to_owned()),
        decision_source: DecisionSource::Manual,
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
    rig.queue.claim(0, 60_000);
    let report = rig
        .service
        .run_claimed_job("job-1", 0)
        .await
        .expect("report");
    assert_eq!(report.reason_code, "QUIET_RECONFIRM");
    assert_eq!(report.review_id, None);
    assert!(rig.reviews.pending_for_album("album-1").is_empty());
}

#[tokio::test]
async fn protected_identified_agreement_quietly_reconfirms() {
    // An Identified winner that agrees with a protected row is a
    // settled question, not new information: no review, no write.
    let rig = rig_with_recall(recall(vec![release(GROUP_A, RELEASE_A1, &[RECORDING_1])]));
    rig.identities
        .save_album_facts(album_facts("album-1", &[("t1", Some(RECORDING_1))]));
    rig.identities.save_album_identity(AlbumIdentity {
        local_album_id: "album-1".to_owned(),
        provider: "musicbrainz".to_owned(),
        release_group_mbid: Some(GROUP_A.to_owned()),
        release_mbid: Some(RELEASE_A1.to_owned()),
        decision_source: DecisionSource::Manual,
        row_revision: 3,
    });
    rig.service.enqueue_album(
        "job-1",
        "album-1",
        IdentifyKind::Automatic,
        "rev-1",
        None,
        0,
    );
    rig.queue.claim(0, 60_000);
    let report = rig
        .service
        .run_claimed_job("job-1", 0)
        .await
        .expect("report");
    assert_eq!(report.reason_code, "QUIET_RECONFIRM");
    assert_eq!(report.review_id, None);
    assert!(rig.reviews.pending_for_album("album-1").is_empty());
    let identity = rig.identities.album_identity("album-1").expect("identity");
    assert_eq!(identity.release_group_mbid.as_deref(), Some(GROUP_A));
    assert_eq!(identity.decision_source, DecisionSource::Manual);
    assert_eq!(identity.row_revision, 3);
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

#[test]
fn stale_proof_rows_do_not_count() {
    let rig = rig_with_recall(RecallResult::default());
    rig.proofs.save_proof(proof_row(
        "album-1",
        "t1",
        "artist-old",
        ARTIST_MBID,
        RELEASE_A1,
    ));
    // The live revisions moved on, so the stored row is stale.
    rig.proofs.set_album_revision("album-1", 2);
    rig.proofs.set_track_revision("t1", 1);
    let verdict = rig
        .service
        .retire_artist("artist-old", "artist-new", ARTIST_MBID, true, None);
    assert_eq!(
        verdict,
        SubstitutionVerdict::Refused(SubstitutionRefusal::NameOnly)
    );
    assert_eq!(rig.aliases.resolve("artist-old"), "artist-old");
}

#[test]
fn retire_artist_through_each_gate() {
    // Accepted release: allowed.
    let rig = rig_with_recall(RecallResult::default());
    rig.identities
        .seed_accepted_release("artist-old", RELEASE_A1);
    let verdict = rig
        .service
        .retire_artist("artist-old", "artist-new", ARTIST_MBID, true, None);
    assert_eq!(verdict, SubstitutionVerdict::Allowed);
    assert_eq!(rig.aliases.resolve("artist-old"), "artist-new");

    // Durable proof row: allowed.
    let rig = rig_with_recall(RecallResult::default());
    rig.proofs.save_proof(proof_row(
        "album-1",
        "t1",
        "artist-old",
        ARTIST_MBID,
        RELEASE_A1,
    ));
    rig.proofs.set_album_revision("album-1", 1);
    rig.proofs.set_track_revision("t1", 1);
    let verdict = rig
        .service
        .retire_artist("artist-old", "artist-new", ARTIST_MBID, true, None);
    assert_eq!(verdict, SubstitutionVerdict::Allowed);

    // Conflicting proof: a curator decides, nothing retargets.
    let rig = rig_with_recall(RecallResult::default());
    rig.proofs.save_proof(proof_row(
        "album-1",
        "t1",
        "artist-old",
        ARTIST_MBID,
        RELEASE_A1,
    ));
    rig.proofs.save_proof(proof_row(
        "album-1",
        "t1",
        "artist-old",
        OTHER_ARTIST_MBID,
        RELEASE_A1,
    ));
    rig.proofs.set_album_revision("album-1", 1);
    rig.proofs.set_track_revision("t1", 1);
    let verdict = rig
        .service
        .retire_artist("artist-old", "artist-new", ARTIST_MBID, true, None);
    assert_eq!(verdict, SubstitutionVerdict::NeedsReview);
    assert_eq!(rig.aliases.resolve("artist-old"), "artist-old");
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
    let rig = rig_with_recall(RecallResult::default());
    rig.proofs.save_proof(proof_row(
        "album-1",
        "t1",
        "artist-old",
        ARTIST_MBID,
        RELEASE_A1,
    ));
    rig.proofs.set_album_revision("album-1", 1);
    rig.proofs.set_track_revision("t1", 1);
    rig.aliases.add_favorite("user-1", "artist", "artist-old");
    rig.aliases.add_playlist_ref("playlist-1", "artist-old");
    rig.aliases.add_history_ref("row-1", "artist-old");
    let verdict = rig
        .service
        .retire_artist("artist-old", "artist-new", ARTIST_MBID, true, None);
    assert_eq!(verdict, SubstitutionVerdict::Allowed);
    // Compat lookups resolve through the alias.
    assert_eq!(rig.aliases.resolve("artist-old"), "artist-new");
    let aliases = rig.aliases.aliases_for("artist-new");
    assert_eq!(aliases.len(), 1);
    assert_eq!(aliases[0].kind, AliasKind::MergedArtist);
    // Live references followed the survivor.
    assert!(rig.aliases.favorite_holds("user-1", "artist", "artist-new"));
    assert!(!rig.aliases.favorite_holds("user-1", "artist", "artist-old"));
    assert_eq!(
        rig.aliases.playlist_ref("playlist-1").as_deref(),
        Some("artist-new")
    );
    assert_eq!(
        rig.aliases.history_ref("row-1").as_deref(),
        Some("artist-new")
    );
}

/// A pin picks among editions that match equally well, and never
/// rescues an edition the files' own ids rule out.
#[tokio::test]
async fn pin_orders_editions_but_never_beats_proof() {
    let rig = rig_with_recall(recall(vec![
        release(GROUP_A, RELEASE_A1, &[RECORDING_1]),
        release(GROUP_A, RELEASE_A2, &[RECORDING_1]),
    ]));
    rig.identities
        .save_album_facts(album_facts("album-1", &[("t1", Some(RECORDING_1))]));
    rig.pins.set_pin(ReleasePin {
        release_group_mbid: GROUP_A.to_owned(),
        release_mbid: RELEASE_A2.to_owned(),
    });
    assert!(!rig.pins.clear_pin("missing-group"));
    rig.service.enqueue_album(
        "job-1",
        "album-1",
        IdentifyKind::Automatic,
        "rev-1",
        None,
        0,
    );
    rig.queue.claim(0, 60_000);
    let report = rig
        .service
        .run_claimed_job("job-1", 0)
        .await
        .expect("report");
    assert_eq!(report.outcome, IdentificationOutcome::Identified);
    let identity = rig.identities.album_identity("album-1").expect("identity");
    assert_eq!(identity.release_mbid.as_deref(), Some(RELEASE_A2));
    // The sealed release stays on file for tagging.
    assert!(rig.releases.release(RELEASE_A2, None).is_some());

    // The pinned edition lacks the file's recording: proof wins.
    rig.providers.set_recall(recall(vec![
        release(GROUP_A, RELEASE_A1, &[RECORDING_1]),
        release(GROUP_A, RELEASE_A2, &[RECORDING_2]),
    ]));
    rig.service.enqueue_album(
        "job-2",
        "album-1",
        IdentifyKind::Automatic,
        "rev-2",
        None,
        0,
    );
    rig.queue.claim(0, 60_000);
    let report = rig
        .service
        .run_claimed_job("job-2", 0)
        .await
        .expect("report");
    assert_eq!(report.outcome, IdentificationOutcome::Identified);
    let identity = rig.identities.album_identity("album-1").expect("identity");
    assert_eq!(identity.release_mbid.as_deref(), Some(RELEASE_A1));
}

#[test]
fn exact_contributors_become_appearances() {
    let rig = rig_with_recall(RecallResult::default());
    rig.identities
        .save_owned_artist(ARTIST_MBID, "artist-owned");
    rig.identities.save_track_credits(
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
    assert_eq!(rig.identities.owned_artist_by_mbid(OTHER_ARTIST_MBID), None);
    assert_eq!(
        rig.identities.owned_artist_by_mbid(ARTIST_MBID).as_deref(),
        Some("artist-owned")
    );
}

#[tokio::test]
async fn review_approve_seals_manual_and_holds() {
    let rig = rig_with_recall(recall(vec![
        release(GROUP_A, RELEASE_A1, &[RECORDING_1]),
        release(GROUP_B, RELEASE_B1, &[RECORDING_1]),
    ]));
    rig.identities
        .save_album_facts(album_facts("album-1", &[("t1", Some(RECORDING_1))]));
    rig.service.enqueue_album(
        "job-1",
        "album-1",
        IdentifyKind::Automatic,
        "rev-1",
        None,
        0,
    );
    rig.queue.claim(0, 60_000);
    let report = rig
        .service
        .run_claimed_job("job-1", 0)
        .await
        .expect("report");
    assert_eq!(report.outcome, IdentificationOutcome::Ambiguous);
    let review = report.review_id.expect("review");
    // Unknown candidate keys never approve.
    assert_eq!(
        rig.service.approve_candidate(&review, "curator-1", "nope"),
        Ok(false)
    );
    assert_eq!(
        rig.service
            .approve_candidate(&review, "curator-1", &key(GROUP_A, RELEASE_A1)),
        Ok(true)
    );
    // Settling twice is a no-op.
    assert_eq!(
        rig.service
            .approve_candidate(&review, "curator-1", &key(GROUP_B, RELEASE_B1)),
        Ok(false)
    );
    let stored = rig.reviews.get(&review).expect("review");
    assert_eq!(stored.state, ReviewState::Approved);
    let identity = rig.identities.album_identity("album-1").expect("identity");
    assert_eq!(identity.release_group_mbid.as_deref(), Some(GROUP_A));
    assert_eq!(identity.decision_source, DecisionSource::Manual);

    // A later automatic pass cannot move the curator's choice.
    *rig.providers.recall.lock().expect("recall") =
        Some(recall(vec![release(GROUP_B, RELEASE_B1, &[RECORDING_1])]));
    rig.service.enqueue_album(
        "job-2",
        "album-1",
        IdentifyKind::Automatic,
        "rev-2",
        None,
        0,
    );
    rig.queue.claim(0, 60_000);
    rig.service
        .run_claimed_job("job-2", 0)
        .await
        .expect("report");
    let held = rig.identities.album_identity("album-1").expect("identity");
    assert_eq!(held.release_group_mbid.as_deref(), Some(GROUP_A));
    assert_eq!(held.decision_source, DecisionSource::Manual);
}

#[tokio::test]
async fn review_reject_keeps_tagged() {
    let rig = rig_with_recall(recall(vec![
        release(GROUP_A, RELEASE_A1, &[RECORDING_1]),
        release(GROUP_B, RELEASE_B1, &[RECORDING_1]),
    ]));
    rig.identities
        .save_album_facts(album_facts("album-1", &[("t1", Some(RECORDING_1))]));
    rig.service.enqueue_album(
        "job-1",
        "album-1",
        IdentifyKind::Automatic,
        "rev-1",
        None,
        0,
    );
    rig.queue.claim(0, 60_000);
    let report = rig
        .service
        .run_claimed_job("job-1", 0)
        .await
        .expect("report");
    let review = report.review_id.expect("review");
    assert!(rig.service.reject_candidates(&review, "curator-1"));
    assert!(!rig.service.reject_candidates(&review, "curator-1"));
    assert_eq!(
        rig.service
            .approve_candidate(&review, "curator-1", &key(GROUP_A, RELEASE_A1)),
        Ok(false)
    );
    assert_eq!(
        rig.reviews.get(&review).expect("review").state,
        ReviewState::Rejected
    );
    assert!(rig.identities.album_identity("album-1").is_none());
    assert!(!rig.service.review_is_pending(&review));
    assert!(rig.service.pending_reviews("album-1").is_empty());
}

/// A live release needs confirmation: without release-track ids on the
/// files, only the release group is pinned.
#[tokio::test]
async fn edition_uncertain_pins_group_only() {
    let rig = rig_with_recall(recall(vec![
        live(release(GROUP_A, RELEASE_A1, &[RECORDING_1])),
        live(release(GROUP_A, RELEASE_A2, &[RECORDING_1])),
    ]));
    rig.identities
        .save_album_facts(album_facts("album-1", &[("t1", None)]));
    rig.service.enqueue_album(
        "job-1",
        "album-1",
        IdentifyKind::Automatic,
        "rev-1",
        None,
        0,
    );
    rig.queue.claim(0, 60_000);
    let report = rig
        .service
        .run_claimed_job("job-1", 0)
        .await
        .expect("report");
    assert_eq!(report.outcome, IdentificationOutcome::EditionUncertain);
    let identity = rig.identities.album_identity("album-1").expect("identity");
    assert_eq!(identity.release_group_mbid.as_deref(), Some(GROUP_A));
    assert_eq!(identity.release_mbid, None);
}

#[tokio::test]
async fn held_exact_survives_weaker_tier() {
    let rig = rig_with_recall(recall(vec![
        live(release(GROUP_A, RELEASE_A1, &[RECORDING_1])),
        live(release(GROUP_A, RELEASE_A2, &[RECORDING_1])),
    ]));
    rig.identities
        .save_album_facts(album_facts("album-1", &[("t1", None)]));
    rig.identities.save_album_identity(AlbumIdentity {
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
    rig.queue.claim(0, 60_000);
    rig.service
        .run_claimed_job("job-1", 0)
        .await
        .expect("report");
    // Same-group weaker evidence must not demote the sealed exact.
    let identity = rig.identities.album_identity("album-1").expect("identity");
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
    assert_eq!(rig.queue.jobs_for_album("album-a").len(), 1);
    let first = rig.queue.claim(0, LEASE_SECONDS * 1000).expect("claim");
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
    rig.identities
        .save_album_facts(album_facts("album-1", &[("t1", Some(RECORDING_1))]));
    rig.service.enqueue_album(
        "job-1",
        "album-1",
        IdentifyKind::Automatic,
        "rev-1",
        None,
        0,
    );
    rig.queue.claim(0, 60_000);
    let report = rig
        .service
        .run_claimed_job("job-1", 1_000)
        .await
        .expect("report");
    assert_eq!(report.outcome, IdentificationOutcome::ProviderDeferred);
    let job = rig.queue.job("job-1").expect("job");
    assert_eq!(job.state, identify::models::JobState::Deferred);
    assert_eq!(job.not_before_ms, 1_000 + 30_000);
    // No identity was written on a deferred attempt.
    assert!(rig.identities.album_identity("album-1").is_none());
}

#[test]
fn brief_carries_the_whole_case() {
    assert_eq!(DecisionSource::Automatic.as_str(), "automatic");
    assert_eq!(DecisionSource::Manual.as_str(), "manual");
    assert_eq!(DecisionSource::LegacyImport.as_str(), "legacy_import");
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
    assert_eq!(rig.identities.artist_identity("artist-1"), None);
    rig.identities.save_artist_identity(ArtistIdentity {
        local_artist_id: "artist-1".to_owned(),
        provider: "musicbrainz".to_owned(),
        provider_artist_mbid: Some(ARTIST_MBID.to_owned()),
        decision_source: DecisionSource::Manual,
        row_revision: 1,
    });
    let stored = rig.identities.artist_identity("artist-1").expect("artist");
    assert_eq!(stored.provider_artist_mbid.as_deref(), Some(ARTIST_MBID));
    assert!(!stored.decision_source.automatic_may_overwrite());
}

/// Migration 0011 adds the identify tables on a fresh database and on one
/// migrated through 0010, keeping the catalog rows already there.
#[tokio::test]
async fn identify_state_migration_upgrades_in_place() {
    use droppedneedle::schema::{MIGRATOR, apply_migrations};
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
        "local_track_aliases",
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
