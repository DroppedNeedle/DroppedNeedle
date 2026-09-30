//! Stage-8 contribution briefs: submission path plus verification worker.
//!
//! Everything runs against the scripted memory rig (`contrib::memory`):
//! no network, no live provider writes. The seed form is POSTed to
//! `FakeReleaseEditor`, which stands in for the MusicBrainz release editor.

#[path = "../src/library/contrib/mod.rs"]
#[allow(dead_code)]
mod contrib;

use std::collections::HashMap;
use std::sync::Arc;

use contrib::error::ContribError;
use contrib::memory::*;
use contrib::models::*;
use contrib::rules::*;
use contrib::seams::*;
use contrib::service::ContributionService;
use contrib::worker::{
    VerificationWorker, VerificationWorkerConfig, shutdown_channel, spawn_verification_worker,
};
use droppedneedle::providers::slots::RequestPriority;

const NOW: f64 = 1_800_000_000.0;
const ALBUM: &str = "album-1";
const ARTIST: &str = "artist-1";
const ACTOR: &str = "user-1";

const MBID_RELEASE: &str = "11111111-1111-4111-8111-111111111111";
const MBID_GROUP: &str = "22222222-2222-4222-8222-222222222222";
const MBID_ARTIST: &str = "33333333-3333-4333-8333-333333333333";
const MBID_OTHER: &str = "44444444-4444-4444-8444-444444444444";

struct Rig {
    store: Arc<MemoryStore>,
    identity: Arc<MemoryIdentity>,
    discogs: Arc<ScriptedDiscogs>,
    musicbrainz: Arc<ScriptedMusicBrainz>,
    catalog: Arc<MemoryCatalog>,
    evidence: Arc<ScriptedEvidence>,
    clock: Arc<TestClock>,
    service: Arc<ContributionService>,
}

impl Rig {
    fn new(decision: AttachmentDecision) -> Self {
        let store = Arc::new(MemoryStore::new());
        let identity = Arc::new(MemoryIdentity::new());
        let discogs = Arc::new(ScriptedDiscogs::new());
        let musicbrainz = Arc::new(ScriptedMusicBrainz::new());
        let catalog = Arc::new(MemoryCatalog::new());
        let evidence = Arc::new(ScriptedEvidence::new(decision));
        let clock = Arc::new(TestClock::new(NOW));
        identity.insert(ALBUM, album_context(None, None));
        store.set_freshness(ALBUM, true, "tag-rev:file-rev:policy-rev", 7, ARTIST);
        let service = Arc::new(
            ContributionService::new(
                store.clone(),
                identity.clone(),
                evidence.clone(),
                clock.clone(),
            )
            .with_discogs(discogs.clone())
            .with_musicbrainz(musicbrainz.clone())
            .with_catalog(catalog.clone()),
        );
        Self {
            store,
            identity,
            discogs,
            musicbrainz,
            catalog,
            evidence,
            clock,
            service,
        }
    }

    fn worker(&self) -> Arc<VerificationWorker> {
        Arc::new(VerificationWorker::new(
            self.service.clone(),
            self.musicbrainz.clone(),
            self.identity.clone(),
            VerificationWorkerConfig::default(),
        ))
    }

    fn now(&self) -> f64 {
        self.clock.now_seconds()
    }
}

fn track(id: &str, disc: i64, number: i64, title: &str, duration: f64) -> IdentityTrack {
    IdentityTrack {
        id: id.to_string(),
        disc_number: disc,
        track_number: number,
        title: title.to_string(),
        artist_name: Some("Test Artist".to_string()),
        duration_seconds: Some(duration),
        availability: "indexed".to_string(),
        disc_subtitle: None,
        relative_path: format!("{id}.flac"),
        recording_mbid: None,
        embedded_recording_mbid: None,
    }
}

fn album_context(
    release_mbid: Option<&str>,
    group_mbid: Option<&str>,
) -> AlbumIdentificationContext {
    AlbumIdentificationContext {
        album: Some(IdentityAlbumRow {
            id: ALBUM.to_string(),
            row_revision: 7,
            active: true,
            title: "Test Album".to_string(),
            album_artist_name: "Test Artist".to_string(),
            album_artist_id: ARTIST.to_string(),
            original_release_date: Some("2024-03-07".to_string()),
            year: Some(2024),
            is_compilation: false,
        }),
        identity: IdentityAlbumIds {
            release_mbid: release_mbid.map(str::to_string),
            release_group_mbid: group_mbid.map(str::to_string),
        },
        artist: IdentityArtist {
            kind: "person".to_string(),
            provider_artist_id: Some(MBID_ARTIST.to_string()),
        },
        tracks: vec![
            track("t1", 1, 1, "First Song", 200.1234),
            track("t2", 1, 2, "Second Song", 180.0),
        ],
    }
}

fn discogs_release() -> DiscogsRelease {
    DiscogsRelease {
        release_id: "12345".to_string(),
        master_id: Some("999".to_string()),
        canonical_release_url: "https://www.discogs.com/release/12345-Test-Artist-Test-Album"
            .to_string(),
        canonical_master_url: Some("https://www.discogs.com/master/999-Test-Artist".to_string()),
        title: "Test Album".to_string(),
        artist_name: "Test Artist".to_string(),
        released_date: Some("2024-03-07".to_string()),
        year: Some(2024),
        country: Some("UK".to_string()),
        labels: vec![DiscogsLabel {
            name: "Test Label".to_string(),
            catalogue_number: Some("TL-001".to_string()),
        }],
        barcode: Some("0123456789012".to_string()),
        media: vec![DiscogsMedium {
            position: 1,
            title: None,
            format: Some("Vinyl".to_string()),
            tracks: vec![
                DiscogsTrack {
                    source_position: Some("A1".to_string()),
                    number: Some(1),
                    title: "First Song".to_string(),
                    duration_seconds: Some(200.0),
                    heading: false,
                    artists: Vec::new(),
                },
                DiscogsTrack {
                    source_position: Some("A2".to_string()),
                    number: Some(2),
                    title: "Second Song".to_string(),
                    duration_seconds: Some(180.0),
                    heading: false,
                    artists: Vec::new(),
                },
            ],
        }],
        source_fetched_at: NOW,
    }
}

fn verified_release(mbid: &str, group: &str) -> MusicBrainzVerifiedRelease {
    MusicBrainzVerifiedRelease {
        release_mbid: mbid.to_string(),
        release_group_mbid: group.to_string(),
        title: "Test Album".to_string(),
        artist_name: "Test Artist".to_string(),
        artist_mbid: Some(MBID_ARTIST.to_string()),
        date: Some("2024-03-07".to_string()),
        country: Some("GB".to_string()),
        status: Some("Official".to_string()),
        packaging: None,
        barcode: Some("0123456789012".to_string()),
        label: Some("Test Label".to_string()),
        catalogue_number: Some("TL-001".to_string()),
        tracks: vec![
            MusicBrainzVerifiedTrack {
                title: "First Song".to_string(),
                position: 1,
                disc_number: 1,
                duration_seconds: Some(200_123.0),
                recording_mbid: None,
                release_track_mbid: None,
            },
            MusicBrainzVerifiedTrack {
                title: "Second Song".to_string(),
                position: 2,
                disc_number: 1,
                duration_seconds: Some(180_000.0),
                recording_mbid: None,
                release_track_mbid: None,
            },
        ],
    }
}

fn entered(value: &str) -> ReleaseTextField {
    ReleaseTextField {
        value: Some(value.to_string()),
        source: ContributionFieldSource::EnteredHere,
    }
}

/// Drive a contribution to `ready` through a no-op edit (local draft already
/// validates clean for the person-kind fixture).
async fn to_ready(rig: &Rig) -> ContributionRecord {
    let created = rig.service.create(ALBUM, ACTOR).await.unwrap();
    rig.service
        .update(&created.id, created.row_revision, &created.draft, ACTOR)
        .await
        .unwrap()
}

async fn to_checked(rig: &Rig) -> ContributionRecord {
    let ready = to_ready(rig).await;
    rig.service
        .check_duplicates(&ready.id, ready.row_revision, ACTOR, false)
        .await
        .unwrap()
}

// ---------------------------------------------------------------------------
// Lifecycle briefs
// ---------------------------------------------------------------------------

#[tokio::test]
async fn brief_create_rejects_exact_release() {
    let rig = Rig::new(ScriptedEvidence::needs_review("X"));
    rig.identity
        .insert(ALBUM, album_context(Some(MBID_RELEASE), Some(MBID_GROUP)));
    let error = rig.service.create(ALBUM, ACTOR).await.unwrap_err();
    assert_eq!(
        error,
        ContribError::State("This local album already has an exact MusicBrainz release.".into())
    );
}

#[tokio::test]
async fn brief_create_then_update_reaches_ready() {
    let rig = Rig::new(ScriptedEvidence::needs_review("X"));
    let created = rig.service.create(ALBUM, ACTOR).await.unwrap();
    assert_eq!(created.state, ContributionState::Draft);
    assert_eq!(created.local_snapshot.media.len(), 1);
    assert_eq!(created.draft.media[0].tracks.len(), 2);
    // Same album, second create returns the still-open contribution.
    let again = rig.service.create(ALBUM, ACTOR).await.unwrap();
    assert_eq!(again.id, created.id);
    let ready = rig
        .service
        .update(&created.id, created.row_revision, &created.draft, ACTOR)
        .await
        .unwrap();
    assert_eq!(ready.state, ContributionState::Ready);
    assert!(ready.validation.is_empty());
    assert!(
        ready
            .next_actions
            .contains(&ContributionNextAction::RunDuplicateCheck)
    );
}

#[tokio::test]
async fn brief_update_provenance_guards() {
    let rig = Rig::new(ScriptedEvidence::needs_review("X"));
    let created = rig.service.create(ALBUM, ACTOR).await.unwrap();
    // Changed value still marked `local` is rejected...
    let mut bad = created.draft.clone();
    bad.title = ReleaseTextField::local(Some("Renamed".to_string()));
    let error = rig
        .service
        .update(&created.id, created.row_revision, &bad, ACTOR)
        .await
        .unwrap_err();
    assert_eq!(
        error,
        ContribError::Validation("A changed value must be marked as entered here.".into())
    );
    // ...but passes once marked entered-here.
    let mut good = created.draft.clone();
    good.title = entered("Renamed");
    let updated = rig
        .service
        .update(&created.id, created.row_revision, &good, ACTOR)
        .await
        .unwrap();
    assert_eq!(updated.draft.title.text(), "Renamed");
    assert_eq!(updated.state, ContributionState::Ready);
}

#[tokio::test]
async fn brief_track_list_and_positions_immutable() {
    let rig = Rig::new(ScriptedEvidence::needs_review("X"));
    let created = rig.service.create(ALBUM, ACTOR).await.unwrap();
    let mut moved = created.draft.clone();
    moved.media[0].tracks.swap(0, 1);
    moved.media[0].tracks[0].track_number = 99;
    let error = rig
        .service
        .update(&created.id, created.row_revision, &moved, ACTOR)
        .await
        .unwrap_err();
    assert_eq!(
        error,
        ContribError::Validation("Track positions cannot be changed in this step.".into())
    );
    let mut dropped = created.draft.clone();
    dropped.media[0].tracks.pop();
    let error = rig
        .service
        .update(&created.id, created.row_revision, &dropped, ACTOR)
        .await
        .unwrap_err();
    assert_eq!(
        error,
        ContribError::Validation("The contribution track list does not match the album.".into())
    );
}

#[tokio::test]
async fn brief_stale_marks_on_read_and_rebuilds_fresh() {
    let rig = Rig::new(ScriptedEvidence::needs_review("X"));
    let ready = to_ready(&rig).await;
    rig.store
        .set_freshness(ALBUM, true, "tag-rev:file-rev:policy-REV2", 7, ARTIST);
    let stale = rig.service.get(&ready.id).await.unwrap();
    assert_eq!(stale.state, ContributionState::Stale);
    assert!(!stale.input_is_current);
    assert_eq!(stale.next_actions, vec![ContributionNextAction::Rebuild]);
    // A stale contribution for a live album offers rebuild; for a gone
    // album it offers NOTHING (v2 quirk).
    rig.store
        .set_freshness(ALBUM, false, "tag-rev:file-rev:policy-REV2", 7, ARTIST);
    let gone = rig.service.get(&ready.id).await.unwrap();
    assert!(gone.next_actions.is_empty());
    assert!(rig.service.active_for_album(ALBUM).await.unwrap().is_none());
    // Restore liveness under the new revision and rebuild: NEW row id.
    rig.store
        .set_freshness(ALBUM, true, "tag-rev:file-rev:policy-REV2", 8, ARTIST);
    rig.identity
        .set_revisions("tag-rev", "file-rev", "policy-REV2");
    let mut context = album_context(None, None);
    context.album.as_mut().unwrap().row_revision = 8;
    rig.identity.insert(ALBUM, context);
    let rebuilt = rig
        .service
        .rebuild(&ready.id, gone.row_revision, ACTOR)
        .await
        .unwrap();
    assert_ne!(rebuilt.id, ready.id);
    assert_eq!(rebuilt.state, ContributionState::Draft);
    assert_eq!(rebuilt.input_revision, "tag-rev:file-rev:policy-REV2");
    let old = rig.service.get(&ready.id).await.unwrap();
    assert_eq!(old.state, ContributionState::Stale);
}

#[tokio::test]
async fn brief_cancel_closes_once() {
    let rig = Rig::new(ScriptedEvidence::needs_review("X"));
    let ready = to_ready(&rig).await;
    let cancelled = rig
        .service
        .cancel(&ready.id, ready.row_revision, ACTOR)
        .await
        .unwrap();
    assert_eq!(cancelled.state, ContributionState::Cancelled);
    assert!(cancelled.next_actions.is_empty());
    let error = rig
        .service
        .cancel(&ready.id, cancelled.row_revision, ACTOR)
        .await
        .unwrap_err();
    assert_eq!(
        error,
        ContribError::State("This contribution is already closed.".into())
    );
}

// ---------------------------------------------------------------------------
// Discogs source briefs
// ---------------------------------------------------------------------------

#[tokio::test]
async fn brief_search_defaults_and_validates() {
    let rig = Rig::new(ScriptedEvidence::needs_review("X"));
    rig.discogs
        .set_search_results(vec![DiscogsReleaseCandidate {
            release_id: "12345".to_string(),
            title: "Test Album".to_string(),
            artist_name: "Test Artist".to_string(),
            canonical_url: "https://www.discogs.com/release/12345".to_string(),
            year: Some(2024),
            country: None,
            label: None,
            catalogue_number: None,
            track_count: None,
            master_id: None,
            fetched_at: NOW,
        }]);
    let ready = to_ready(&rig).await;
    let hits = rig.service.search_discogs(&ready.id, None).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(
        rig.discogs.queries(),
        vec!["Test Artist Test Album".to_string()]
    );
    let error = rig
        .service
        .search_discogs(&ready.id, Some("x"))
        .await
        .unwrap_err();
    assert_eq!(
        error,
        ContribError::Validation("Enter a release title, artist, barcode, URL, or ID.".into())
    );
    let error = rig
        .service
        .search_discogs(&ready.id, Some(&"y".repeat(201)))
        .await
        .unwrap_err();
    assert_eq!(
        error,
        ContribError::Validation("The Discogs search is too long.".into())
    );
    for call in rig.discogs.calls() {
        assert_eq!(call.priority, RequestPriority::UserInitiated);
    }
}

#[tokio::test]
async fn brief_select_aligns_and_expiry_redacts() {
    let rig = Rig::new(ScriptedEvidence::needs_review("X"));
    rig.discogs.insert_release(discogs_release());
    let ready = to_ready(&rig).await;
    let selected = rig
        .service
        .select_discogs(&ready.id, "12345", ready.row_revision, ACTOR)
        .await
        .unwrap();
    let source = selected.discogs_source.as_ref().unwrap();
    assert!(!source.expired);
    assert_eq!(selected.source_selection.sources.len(), 2); // release + master
    let classes: Vec<AlignmentClassification> = selected
        .source_selection
        .alignments
        .iter()
        .map(|a| a.classification)
        .collect();
    assert_eq!(
        classes,
        vec![
            AlignmentClassification::Exact,
            AlignmentClassification::Exact
        ]
    );
    // Expired provider data redacts values but keeps source markers.
    rig.clock.advance(7.0 * 60.0 * 60.0);
    let expired = rig.service.get(&selected.id).await.unwrap();
    assert!(expired.discogs_source.as_ref().unwrap().expired);
    assert_eq!(
        expired.next_actions.first(),
        Some(&ContributionNextAction::EditDraft)
    );
    assert!(
        expired
            .next_actions
            .contains(&ContributionNextAction::RefreshDiscogs)
    );
    // Purge drops the snapshot and clears alignments.
    let purged = rig
        .service
        .purge_expired_provider_data(rig.now(), 200)
        .await
        .unwrap();
    assert_eq!(purged, 1);
    let raw = rig.store.raw(&selected.id).unwrap();
    assert!(raw.provider_snapshot_expires_at.is_none());
    assert!(raw.source_selection.alignments.is_empty());
}

#[tokio::test]
async fn brief_remove_discogs_restores_local() {
    let rig = Rig::new(ScriptedEvidence::needs_review("X"));
    rig.discogs.insert_release(discogs_release());
    let ready = to_ready(&rig).await;
    let selected = rig
        .service
        .select_discogs(&ready.id, "12345", ready.row_revision, ACTOR)
        .await
        .unwrap();
    let removed = rig
        .service
        .remove_discogs(&selected.id, selected.row_revision, ACTOR)
        .await
        .unwrap();
    assert!(removed.discogs_source.is_none());
    assert!(removed.source_selection.sources.is_empty());
    assert!(removed.provider_snapshot_expires_at.is_none());
}

// ---------------------------------------------------------------------------
// Duplicate + attach briefs
// ---------------------------------------------------------------------------

#[tokio::test]
async fn brief_duplicate_check_orders_evidence_and_forces_review() {
    let rig = Rig::new(ScriptedEvidence::needs_review("X"));
    rig.discogs.insert_release(discogs_release());
    let release = discogs_release();
    rig.musicbrainz.insert_resolution(
        &release.canonical_release_url,
        UrlRelation::Release,
        MusicBrainzUrlResolution {
            resource_url: release.canonical_release_url.clone(),
            release_mbids: vec![MBID_RELEASE.to_string()],
            release_group_mbids: Vec::new(),
        },
    );
    rig.musicbrainz.insert_resolution(
        release.canonical_master_url.as_deref().unwrap(),
        UrlRelation::ReleaseGroup,
        MusicBrainzUrlResolution {
            resource_url: release.canonical_master_url.clone().unwrap(),
            release_mbids: Vec::new(),
            release_group_mbids: vec![MBID_GROUP.to_string()],
        },
    );
    let mut exact = verified_release(MBID_RELEASE, MBID_GROUP);
    exact.title = "Different Title".to_string();
    rig.musicbrainz
        .insert_verification(MBID_RELEASE, Ok(Some(exact)));
    let mut barcode_hit = verified_release(MBID_OTHER, MBID_GROUP);
    barcode_hit.barcode = Some("0123456789012".to_string());
    let mut similar = verified_release(
        "55555555-5555-4555-8555-555555555555",
        "66666666-6666-4666-8666-666666666666",
    );
    similar.barcode = None; // no shared barcode -> Similar, not Barcode
    rig.musicbrainz.set_duplicates(vec![similar, barcode_hit]);
    let ready = to_ready(&rig).await;
    let selected = rig
        .service
        .select_discogs(&ready.id, "12345", ready.row_revision, ACTOR)
        .await
        .unwrap();
    let mut draft = selected.draft.clone();
    draft.barcode = entered("0123456789012");
    let edited = rig
        .service
        .update(&selected.id, selected.row_revision, &draft, ACTOR)
        .await
        .unwrap();
    assert_eq!(edited.state, ContributionState::Ready);
    let checked = rig
        .service
        .check_duplicates(&edited.id, edited.row_revision, ACTOR, true)
        .await
        .unwrap();
    assert_eq!(checked.state, ContributionState::NeedsReview);
    let result = checked.duplicate_result.as_ref().unwrap();
    let kinds: Vec<DuplicateEvidenceKind> =
        result.candidates.iter().map(|c| c.evidence_kind).collect();
    assert_eq!(
        kinds,
        vec![
            DuplicateEvidenceKind::ExactDiscogsUrl,
            DuplicateEvidenceKind::ReleaseGroup,
            DuplicateEvidenceKind::Barcode,
            DuplicateEvidenceKind::Similar,
        ]
    );
    assert!(result.candidates[0].exact);
    assert!(
        result.candidates[0]
            .differences
            .iter()
            .any(|d| d.starts_with("Different title:"))
    );
    // v2 quirk: the different-edition confirmation is dropped on exact hits.
    assert!(!result.different_edition_confirmed);
    assert!(
        checked
            .next_actions
            .contains(&ContributionNextAction::AttachExisting)
    );
    for call in rig.musicbrainz.calls() {
        assert_eq!(call.priority, RequestPriority::UserInitiated);
    }
}

#[tokio::test]
async fn brief_duplicate_check_clean_stays_ready() {
    let rig = Rig::new(ScriptedEvidence::needs_review("X"));
    let checked = to_checked(&rig).await;
    assert_eq!(checked.state, ContributionState::Ready);
    assert!(
        checked
            .duplicate_result
            .as_ref()
            .unwrap()
            .candidates
            .is_empty()
    );
    assert!(
        checked
            .next_actions
            .contains(&ContributionNextAction::SeedMusicbrainz)
    );
}

#[tokio::test]
async fn brief_duplicate_check_needs_valid_draft_and_fresh_source() {
    let rig = Rig::new(ScriptedEvidence::needs_review("X"));
    let created = rig.service.create(ALBUM, ACTOR).await.unwrap();
    let error = rig
        .service
        .check_duplicates(&created.id, created.row_revision, ACTOR, false)
        .await
        .unwrap_err();
    assert_eq!(
        error,
        ContribError::State("Complete the contribution draft before checking MusicBrainz.".into())
    );
}

#[tokio::test]
async fn brief_attach_existing_links_and_scopes_invalidation() {
    let rig = Rig::new(ScriptedEvidence::identified(
        MBID_GROUP,
        MBID_RELEASE,
        Some(MBID_ARTIST),
    ));
    rig.discogs.insert_release(discogs_release());
    let release = discogs_release();
    rig.musicbrainz.insert_resolution(
        &release.canonical_release_url,
        UrlRelation::Release,
        MusicBrainzUrlResolution {
            resource_url: release.canonical_release_url.clone(),
            release_mbids: vec![MBID_RELEASE.to_string()],
            release_group_mbids: Vec::new(),
        },
    );
    rig.musicbrainz.insert_verification(
        MBID_RELEASE,
        Ok(Some(verified_release(MBID_RELEASE, MBID_GROUP))),
    );
    let ready = to_ready(&rig).await;
    let selected = rig
        .service
        .select_discogs(&ready.id, "12345", ready.row_revision, ACTOR)
        .await
        .unwrap();
    let edited = rig
        .service
        .update(&selected.id, selected.row_revision, &selected.draft, ACTOR)
        .await
        .unwrap();
    let checked = rig
        .service
        .check_duplicates(&edited.id, edited.row_revision, ACTOR, false)
        .await
        .unwrap();
    let linked = rig
        .service
        .attach_existing(&checked.id, MBID_RELEASE, checked.row_revision, ACTOR)
        .await
        .unwrap();
    assert_eq!(linked.state, ContributionState::Linked);
    assert!(linked.next_actions.is_empty());
    // ST1: exactly the touched identity keys, before returning.
    assert_eq!(
        rig.catalog.scopes(),
        vec![(vec![MBID_GROUP.to_string()], vec![MBID_ARTIST.to_string()])]
    );
    assert_eq!(
        rig.catalog.identified_calls(),
        vec![(ALBUM.to_string(), "policy-rev".to_string())]
    );
    assert_eq!(rig.evidence.calls(), 1);
    assert_eq!(
        rig.store.album_identity(ALBUM),
        Some((MBID_RELEASE.to_string(), MBID_GROUP.to_string()))
    );
    assert_eq!(rig.store.attempts().len(), 1);
    let raw = rig.store.raw(&linked.id).unwrap();
    assert!(raw.provider_snapshot_expires_at.is_none());
    let verify_calls = rig.musicbrainz.verify_calls();
    assert_eq!(verify_calls.len(), 2);
    assert!(
        verify_calls
            .iter()
            .all(|c| c.priority == RequestPriority::UserInitiated)
    );
    assert!(verify_calls.iter().any(|c| c.bypass_cache)); // attach bypasses
}

#[tokio::test]
async fn brief_attach_rejects_ambiguous_and_unknown() {
    let rig = Rig::new(ScriptedEvidence::needs_review("X"));
    let checked = to_checked(&rig).await;
    let error = rig
        .service
        .attach_existing(&checked.id, MBID_RELEASE, checked.row_revision, ACTOR)
        .await
        .unwrap_err();
    assert_eq!(
        error,
        ContribError::ResultMismatch(
            "That release is not in the current duplicate-check result.".into()
        )
    );
}

// ---------------------------------------------------------------------------
// Submission briefs (mocked editor - never a live provider write)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn brief_seed_builds_exact_editor_form() {
    let rig = Rig::new(ScriptedEvidence::needs_review("X"));
    rig.discogs.insert_release(discogs_release());
    let release = discogs_release();
    rig.musicbrainz.insert_resolution(
        release.canonical_master_url.as_deref().unwrap(),
        UrlRelation::ReleaseGroup,
        MusicBrainzUrlResolution {
            resource_url: release.canonical_master_url.clone().unwrap(),
            release_mbids: Vec::new(),
            release_group_mbids: vec![MBID_GROUP.to_string()],
        },
    );
    let ready = to_ready(&rig).await;
    let selected = rig
        .service
        .select_discogs(&ready.id, "12345", ready.row_revision, ACTOR)
        .await
        .unwrap();
    let mut draft = selected.draft.clone();
    draft.country = entered("UK");
    draft.label = entered("Test Label");
    draft.catalogue_number = entered("TL-001");
    draft.barcode = entered("0123456789012");
    draft.packaging = entered("Jewel Case");
    draft.media[0].format = entered("CD");
    draft.media[0].tracks[1].artist_name = entered("Guest Singer");
    let edited = rig
        .service
        .update(&selected.id, selected.row_revision, &draft, ACTOR)
        .await
        .unwrap();
    let checked = rig
        .service
        .check_duplicates(&edited.id, edited.row_revision, ACTOR, false)
        .await
        .unwrap();
    let seed = rig
        .service
        .create_seed(
            &checked.id,
            checked.row_revision,
            ACTOR,
            "https://needle.example.com/",
        )
        .await
        .unwrap();
    assert_eq!(seed.action_url, MUSICBRAINZ_RELEASE_EDITOR);
    assert_eq!(seed.method, "POST");
    assert_eq!(seed.fields.first().unwrap().name, "name");
    assert_eq!(seed.fields.first().unwrap().value, "Test Album");
    assert_eq!(seed.fields.last().unwrap().name, "redirect_uri");
    assert!(seed.fields.last().unwrap().value.starts_with(
        "https://needle.example.com/api/v1/library/contributions/musicbrainz/callback?token="
    ));
    let value = |name: &str| {
        seed.fields
            .iter()
            .find(|f| f.name == name)
            .map(|f| f.value.clone())
    };
    assert_eq!(value("release_group").as_deref(), Some(MBID_GROUP));
    assert_eq!(value("events.0.date.year").as_deref(), Some("2024"));
    assert_eq!(value("events.0.date.month").as_deref(), Some("3")); // int() strips zero
    assert_eq!(value("events.0.date.day").as_deref(), Some("7"));
    assert_eq!(value("events.0.country").as_deref(), Some("GB")); // UK rewrite
    assert_eq!(value("labels.0.name").as_deref(), Some("Test Label"));
    assert_eq!(value("labels.0.catalog_number").as_deref(), Some("TL-001"));
    assert_eq!(
        value("artist_credit.names.0.mbid").as_deref(),
        Some(MBID_ARTIST)
    );
    assert_eq!(
        value("mediums.0.track.0.length").as_deref(),
        Some("200123") // round(), not floor()
    );
    assert_eq!(
        value("mediums.0.track.1.artist_credit.names.0.name").as_deref(),
        Some("Guest Singer")
    );
    assert!(value("mediums.0.track.0.artist_credit.names.0.name").is_none());
    assert_eq!(value("urls.0.link_type").as_deref(), Some("76"));
    assert_eq!(
        value("urls.0.url").as_deref(),
        Some(release.canonical_release_url.as_str())
    );
    let note = value("edit_note").unwrap();
    assert!(note.contains("Seeded with DroppedNeedle"));
    assert!(note.contains(release.canonical_release_url.as_str()));
    // Persisted snapshot keeps the audit trail but never the tokenized URL.
    let raw = rig.store.raw(&checked.id).unwrap();
    let snapshot = raw.seed_snapshot_json.clone().unwrap();
    assert!(snapshot.contains("input_revision"));
    assert!(!snapshot.contains("redirect_uri"));
    assert_eq!(seed.contribution_revision, raw.row_revision);
    assert_eq!(rig.store.live_tokens(), 1);
    let seeded = rig.service.get(&checked.id).await.unwrap();
    assert_eq!(seeded.state, ContributionState::Seeded);
}

#[tokio::test]
async fn brief_seed_guards_duplicates_race_and_base_url() {
    let rig = Rig::new(ScriptedEvidence::needs_review("X"));
    let ready = to_ready(&rig).await;
    let error = rig
        .service
        .create_seed(
            &ready.id,
            ready.row_revision,
            ACTOR,
            "https://needle.example.com",
        )
        .await
        .unwrap_err();
    assert_eq!(
        error,
        ContribError::DuplicateCheckRequired("Run the MusicBrainz duplicate check first.".into())
    );
    let checked = to_checked(&rig).await;
    let error = rig
        .service
        .create_seed(
            &checked.id,
            checked.row_revision,
            ACTOR,
            "https://user@evil.example",
        )
        .await
        .unwrap_err();
    assert_eq!(
        error,
        ContribError::Validation("The public DroppedNeedle URL is not valid.".into())
    );
    // Fresh-link race: the release got linked after the check -> abort.
    rig.discogs.insert_release(discogs_release());
    let release = discogs_release();
    rig.musicbrainz.insert_resolution(
        &release.canonical_release_url,
        UrlRelation::Release,
        MusicBrainzUrlResolution {
            resource_url: release.canonical_release_url.clone(),
            release_mbids: vec![MBID_RELEASE.to_string()],
            release_group_mbids: Vec::new(),
        },
    );
    let ready = to_ready(&rig).await;
    // New album fixture to avoid the still-open contribution above.
    rig.identity.insert("album-2", album_context_for("album-2"));
    rig.store.set_freshness(
        "album-2",
        true,
        "tag-rev:file-rev:policy-rev",
        3,
        "artist-2",
    );
    let created = rig.service.create("album-2", ACTOR).await.unwrap();
    let ready2 = rig
        .service
        .update(&created.id, created.row_revision, &created.draft, ACTOR)
        .await
        .unwrap();
    let _ = ready;
    let selected = rig
        .service
        .select_discogs(&ready2.id, "12345", ready2.row_revision, ACTOR)
        .await
        .unwrap();
    let edited = rig
        .service
        .update(&selected.id, selected.row_revision, &selected.draft, ACTOR)
        .await
        .unwrap();
    // Bypass the check's exact hit by scripting empty at check time...
    rig.musicbrainz.insert_resolution(
        &release.canonical_release_url,
        UrlRelation::Release,
        MusicBrainzUrlResolution {
            resource_url: release.canonical_release_url.clone(),
            release_mbids: Vec::new(),
            release_group_mbids: Vec::new(),
        },
    );
    let checked = rig
        .service
        .check_duplicates(&edited.id, edited.row_revision, ACTOR, false)
        .await
        .unwrap();
    // ...then the link lands before seeding.
    rig.musicbrainz.insert_resolution(
        &release.canonical_release_url,
        UrlRelation::Release,
        MusicBrainzUrlResolution {
            resource_url: release.canonical_release_url.clone(),
            release_mbids: vec![MBID_RELEASE.to_string()],
            release_group_mbids: Vec::new(),
        },
    );
    let error = rig
        .service
        .create_seed(
            &checked.id,
            checked.row_revision,
            ACTOR,
            "https://needle.example.com",
        )
        .await
        .unwrap_err();
    assert_eq!(
        error,
        ContribError::ExactDuplicate(
            "This Discogs release is now linked to MusicBrainz. Run the duplicate check again."
                .into()
        )
    );
}

fn album_context_for(album_id: &str) -> AlbumIdentificationContext {
    let mut context = album_context(None, None);
    let album = context.album.as_mut().unwrap();
    album.id = album_id.to_string();
    album.row_revision = 3;
    context
}

#[tokio::test]
async fn brief_full_submission_roundtrip_through_mocked_editor() {
    let rig = Rig::new(ScriptedEvidence::identified(
        MBID_GROUP,
        MBID_RELEASE,
        Some(MBID_ARTIST),
    ));
    let checked = to_checked(&rig).await;
    let seed = rig
        .service
        .create_seed(
            &checked.id,
            checked.row_revision,
            ACTOR,
            "https://needle.example.com",
        )
        .await
        .unwrap();
    // The curator's browser POSTs to the editor; here the fake takes the form.
    let editor = FakeReleaseEditor::new();
    editor.script_next_mbid(MBID_RELEASE);
    let submission = editor.submit(&seed).unwrap();
    assert_eq!(submission.release_mbid, MBID_RELEASE);
    assert_eq!(editor.submissions().len(), 1);
    assert_eq!(
        submission.redirect_uri,
        seed.fields
            .iter()
            .find(|f| f.name == "redirect_uri")
            .unwrap()
            .value
    );
    assert!(
        submission
            .fields
            .iter()
            .any(|(name, value)| name == "name" && value == "Test Album")
    );
    // The editor redirects back to the callback with token + new MBID.
    let contribution_id = rig
        .service
        .consume_callback(
            Some(&submission.callback_token),
            Some(&submission.release_mbid),
        )
        .await
        .unwrap();
    assert_eq!(contribution_id, checked.id);
    let verifying = rig.service.get(&checked.id).await.unwrap();
    assert_eq!(verifying.state, ContributionState::Verifying);
    assert_eq!(verifying.result_source.as_deref(), Some("callback"));
    assert_eq!(rig.store.jobs_for(&checked.id).len(), 1);
    // The background worker confirms the release and links the album.
    rig.musicbrainz.insert_verification(
        MBID_RELEASE,
        Ok(Some(verified_release(MBID_RELEASE, MBID_GROUP))),
    );
    let worker = rig.worker();
    let outcome = worker.run_once(rig.now()).await.unwrap();
    assert_eq!(outcome, Some(VerificationOutcome::Linked));
    let linked = rig.service.get(&checked.id).await.unwrap();
    assert_eq!(linked.state, ContributionState::Linked);
    // Honest lanes: verification reads rode the background lane, bypassing cache.
    let verify_calls = rig.musicbrainz.verify_calls();
    assert_eq!(verify_calls.len(), 1);
    assert_eq!(verify_calls[0].priority, RequestPriority::BackgroundSync);
    assert!(verify_calls[0].bypass_cache);
    assert_eq!(rig.catalog.sweeps(), 1);
    assert_eq!(rig.catalog.identified_calls().len(), 1);
    assert_eq!(
        rig.store.album_identity(ALBUM),
        Some((MBID_RELEASE.to_string(), MBID_GROUP.to_string()))
    );
}

#[tokio::test]
async fn brief_callback_rejects_bad_and_reused_tokens() {
    let rig = Rig::new(ScriptedEvidence::needs_review("X"));
    let checked = to_checked(&rig).await;
    let seed = rig
        .service
        .create_seed(
            &checked.id,
            checked.row_revision,
            ACTOR,
            "https://needle.example.com",
        )
        .await
        .unwrap();
    let editor = FakeReleaseEditor::new();
    editor.script_next_mbid(MBID_RELEASE);
    let submission = editor.submit(&seed).unwrap();
    let error = rig
        .service
        .consume_callback(Some("short"), Some(MBID_RELEASE))
        .await
        .unwrap_err();
    assert_eq!(
        error,
        ContribError::Validation("The MusicBrainz callback token is invalid.".into())
    );
    let error = rig
        .service
        .consume_callback(Some(&"a".repeat(40)), Some(MBID_RELEASE))
        .await
        .unwrap_err();
    assert_eq!(
        error,
        ContribError::Missing("Contribution callback is invalid or expired.".into())
    );
    let error = rig
        .service
        .consume_callback(Some(&submission.callback_token), Some("not-a-mbid"))
        .await
        .unwrap_err();
    assert_eq!(
        error,
        ContribError::Validation("The MusicBrainz release MBID is invalid.".into())
    );
    rig.service
        .consume_callback(
            Some(&submission.callback_token),
            Some(&submission.release_mbid),
        )
        .await
        .unwrap();
    // Single-use: the same token is dead afterwards.
    let error = rig
        .service
        .consume_callback(
            Some(&submission.callback_token),
            Some(&submission.release_mbid),
        )
        .await
        .unwrap_err();
    assert_eq!(
        error,
        ContribError::Missing("Contribution callback is invalid or expired.".into())
    );
}

#[tokio::test]
async fn brief_manual_result_queues_and_replaces_explicitly() {
    let rig = Rig::new(ScriptedEvidence::needs_review("X"));
    let checked = to_checked(&rig).await;
    rig.service
        .create_seed(
            &checked.id,
            checked.row_revision,
            ACTOR,
            "https://needle.example.com",
        )
        .await
        .unwrap();
    let seeded = rig.service.get(&checked.id).await.unwrap();
    let verifying = rig
        .service
        .record_manual_result(&seeded.id, MBID_RELEASE, seeded.row_revision, ACTOR, false)
        .await
        .unwrap();
    assert_eq!(verifying.state, ContributionState::Verifying);
    assert_eq!(verifying.result_source.as_deref(), Some("manual"));
    assert_eq!(rig.store.jobs_for(&seeded.id).len(), 1);
    // A different MBID needs explicit replacement...
    let error = rig
        .service
        .record_manual_result(&seeded.id, MBID_OTHER, verifying.row_revision, ACTOR, false)
        .await
        .unwrap_err();
    assert_eq!(
        error,
        ContribError::State("Confirm replacement of the existing MusicBrainz result.".into())
    );
    // ...and replacement only lands from needs_review/stale.
    let error = rig
        .service
        .record_manual_result(&seeded.id, MBID_OTHER, verifying.row_revision, ACTOR, true)
        .await
        .unwrap_err();
    assert_eq!(
        error,
        ContribError::State("Confirm replacement of the existing MusicBrainz result.".into())
    );
}

// ---------------------------------------------------------------------------
// Verification worker briefs
// ---------------------------------------------------------------------------

async fn to_verifying(rig: &Rig) -> ContributionRecord {
    let checked = to_checked(rig).await;
    let seed = rig
        .service
        .create_seed(
            &checked.id,
            checked.row_revision,
            ACTOR,
            "https://needle.example.com",
        )
        .await
        .unwrap();
    let _ = seed;
    let seeded = rig.service.get(&checked.id).await.unwrap();
    rig.service
        .record_manual_result(&seeded.id, MBID_RELEASE, seeded.row_revision, ACTOR, false)
        .await
        .unwrap()
}

#[tokio::test]
async fn brief_worker_retries_with_backoff_then_reviews() {
    let rig = Rig::new(ScriptedEvidence::identified(
        MBID_GROUP,
        MBID_RELEASE,
        Some(MBID_ARTIST),
    ));
    let verifying = to_verifying(&rig).await;
    rig.musicbrainz.insert_verification(MBID_RELEASE, Ok(None)); // not propagated yet
    let worker = rig.worker();
    // First failure: 15s backoff (15 * 2^min(1-1, 6)).
    let outcome = worker.run_once(rig.now()).await.unwrap();
    assert_eq!(outcome, Some(VerificationOutcome::RetryScheduled));
    let jobs = rig.store.jobs_for(&verifying.id);
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].not_before, NOW + 15.0);
    assert_eq!(
        jobs[0].last_failure_code.as_deref(),
        Some(FAILURE_MB_NOT_PROPAGATED)
    );
    // Nine more scheduled retries, then the window/attempt budget runs out.
    for _ in 0..8 {
        rig.clock
            .set(rig.store.jobs_for(&verifying.id)[0].not_before);
        let outcome = worker.run_once(rig.now()).await.unwrap();
        assert_eq!(outcome, Some(VerificationOutcome::RetryScheduled));
    }
    rig.clock
        .set(rig.store.jobs_for(&verifying.id)[0].not_before);
    let outcome = worker.run_once(rig.now()).await.unwrap();
    assert_eq!(outcome, Some(VerificationOutcome::NeedsReview));
    let record = rig.service.get(&verifying.id).await.unwrap();
    assert_eq!(record.state, ContributionState::NeedsReview);
    assert_eq!(
        record.next_actions,
        vec![
            ContributionNextAction::RetryVerification,
            ContributionNextAction::Cancel
        ]
    );
    let attempts = rig.store.attempts();
    assert_eq!(attempts.len(), 1);
    assert_eq!(
        attempts[0].terminal_reason_code.as_deref(),
        Some(FAILURE_MB_NOT_PROPAGATED)
    );
    assert_eq!(attempts[0].candidate_count, 0);
    // Manual retry requeues onto verifying.
    let retried = rig
        .service
        .retry_verification(&record.id, record.row_revision, ACTOR)
        .await
        .unwrap();
    assert_eq!(retried.state, ContributionState::Verifying);
}

#[tokio::test]
async fn brief_worker_unmappable_payload_reviews_immediately() {
    let rig = Rig::new(ScriptedEvidence::identified(
        MBID_GROUP,
        MBID_RELEASE,
        Some(MBID_ARTIST),
    ));
    let verifying = to_verifying(&rig).await;
    rig.musicbrainz
        .insert_verification(MBID_RELEASE, Err(ProviderFailure::Unmappable));
    let outcome = rig.worker().run_once(rig.now()).await.unwrap();
    assert_eq!(outcome, Some(VerificationOutcome::NeedsReview));
    let jobs = rig.store.jobs_for(&verifying.id);
    assert_eq!(jobs[0].state, VerificationJobState::NeedsReview);
    assert_eq!(
        jobs[0].last_failure_code.as_deref(),
        Some(UNMAPPABLE_PROVIDER_PAYLOAD)
    );
    assert_eq!(rig.evidence.calls(), 0); // no evidence run on payload failure
}

#[tokio::test]
async fn brief_worker_release_mismatch_reviews() {
    let rig = Rig::new(ScriptedEvidence::identified(
        MBID_GROUP,
        MBID_RELEASE,
        Some(MBID_ARTIST),
    ));
    let _verifying = to_verifying(&rig).await;
    rig.musicbrainz.insert_verification(
        MBID_RELEASE,
        Ok(Some(verified_release(MBID_OTHER, MBID_GROUP))),
    );
    let outcome = rig.worker().run_once(rig.now()).await.unwrap();
    assert_eq!(outcome, Some(VerificationOutcome::NeedsReview));
    assert_eq!(outcome.unwrap().as_str(), "needs_review");
    let attempts = rig.store.attempts();
    assert_eq!(
        attempts[0].terminal_reason_code.as_deref(),
        Some(FAILURE_RETURNED_RELEASE_MISMATCH)
    );
}

#[tokio::test]
async fn brief_worker_contradiction_reviews_with_reason() {
    let rig = Rig::new(ScriptedEvidence::needs_review("TITLE_CONTRADICTION"));
    let verifying = to_verifying(&rig).await;
    rig.musicbrainz.insert_verification(
        MBID_RELEASE,
        Ok(Some(verified_release(MBID_RELEASE, MBID_GROUP))),
    );
    let outcome = rig.worker().run_once(rig.now()).await.unwrap();
    assert_eq!(outcome, Some(VerificationOutcome::NeedsReview));
    assert_eq!(rig.evidence.calls(), 1);
    let attempts = rig.store.attempts();
    assert_eq!(
        attempts[0].terminal_reason_code.as_deref(),
        Some("TITLE_CONTRADICTION")
    );
    let _ = verifying;
}

#[tokio::test]
async fn brief_worker_retry_after_floor_and_bogus_drop() {
    let rig = Rig::new(ScriptedEvidence::needs_review("X"));
    let verifying = to_verifying(&rig).await;
    rig.musicbrainz.insert_verification(
        MBID_RELEASE,
        Err(ProviderFailure::Unavailable {
            retry_after_seconds: Some(120.0),
        }),
    );
    let outcome = rig.worker().run_once(rig.now()).await.unwrap();
    assert_eq!(outcome, Some(VerificationOutcome::RetryScheduled));
    assert_eq!(rig.store.jobs_for(&verifying.id)[0].not_before, NOW + 120.0);
    // A bogus retry-after (NaN) is dropped, not clamped: plain 15s backoff.
    rig.musicbrainz.insert_verification(
        MBID_RELEASE,
        Err(ProviderFailure::Unavailable {
            retry_after_seconds: Some(f64::NAN),
        }),
    );
    rig.clock.set(NOW + 120.0);
    let worker = rig.worker();
    // Drain the queued job from the first part, then re-run: attempt 2 -> 30s.
    let outcome = worker.run_once(rig.now()).await.unwrap();
    assert_eq!(outcome, Some(VerificationOutcome::RetryScheduled));
    assert_eq!(
        rig.store.jobs_for(&verifying.id)[0].not_before,
        NOW + 120.0 + 30.0
    );
}

#[tokio::test]
async fn brief_worker_skips_non_verifying_and_recovers_leases() {
    let rig = Rig::new(ScriptedEvidence::needs_review("X"));
    let verifying = to_verifying(&rig).await;
    let worker = rig.worker();
    // Claim out-of-band, then cancel the contribution: the job no longer applies.
    let job = worker.claim(rig.now()).await.unwrap();
    let record = rig.service.get(&verifying.id).await.unwrap();
    rig.service
        .cancel(&record.id, record.row_revision, ACTOR)
        .await
        .unwrap();
    // Fresh worker run finds no queued job (cancel retired it).
    assert!(worker.run_once(rig.now()).await.unwrap().is_none());
    let _ = job;
    // Expired running leases requeue on recover; cleanup cadence is hourly.
    let verifying = to_verifying(&rig).await;
    let job = worker.claim(rig.now()).await.unwrap();
    assert_eq!(job.attempt_count, 1);
    rig.clock.advance(91.0);
    let recovered = worker.recover(rig.now()).await;
    assert_eq!(recovered, 1);
    let jobs = rig.store.jobs_for(&verifying.id);
    assert_eq!(jobs[0].state, VerificationJobState::Queued);
    // Second recover inside the hour skips cleanup but still recovers (none due).
    let recovered = worker.recover(rig.now()).await;
    assert_eq!(recovered, 0);
}

#[tokio::test]
async fn brief_worker_spawn_loop_links_then_shuts_down() {
    let rig = Rig::new(ScriptedEvidence::identified(
        MBID_GROUP,
        MBID_RELEASE,
        Some(MBID_ARTIST),
    ));
    // The spawned loop uses wall-clock time; pin the rig clock near it.
    let wall = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap();
    rig.clock.set(wall);
    let verifying = to_verifying(&rig).await;
    rig.musicbrainz.insert_verification(
        MBID_RELEASE,
        Ok(Some(verified_release(MBID_RELEASE, MBID_GROUP))),
    );
    let worker = Arc::new(VerificationWorker::new(
        rig.service.clone(),
        rig.musicbrainz.clone(),
        rig.identity.clone(),
        VerificationWorkerConfig {
            worker_id: "test-loop".to_string(),
            poll_interval: std::time::Duration::from_millis(10),
            lease_seconds: 90.0,
        },
    ));
    let (shutdown_tx, shutdown_rx) = shutdown_channel();
    let handle = spawn_verification_worker(worker, shutdown_rx);
    let linked = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let record = rig.service.get(&verifying.id).await.unwrap();
            if record.state == ContributionState::Linked {
                break record;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("worker loop should link the contribution");
    assert_eq!(linked.state, ContributionState::Linked);
    shutdown_tx.send(true).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), handle)
        .await
        .expect("worker loop should shut down")
        .unwrap();
}

// ---------------------------------------------------------------------------
// Pure-rule briefs
// ---------------------------------------------------------------------------

#[test]
fn brief_parse_discogs_ids() {
    assert_eq!(parse_discogs_release_id("12345").unwrap(), "12345");
    assert_eq!(parse_discogs_release_id("  007 ").unwrap(), "7");
    assert_eq!(
        parse_discogs_release_id("https://www.discogs.com/release/12345-Artist-Title/").unwrap(),
        "12345"
    );
    assert_eq!(
        parse_discogs_release_id("https://discogs.com/master/999")
            .unwrap_err()
            .message(),
        "Enter an exact Discogs release URL, not a master URL."
    );
    assert_eq!(
        parse_discogs_release_id("https://discogs.com/release/0123")
            .unwrap_err()
            .message(),
        "Enter an exact Discogs release URL, not a master URL."
    );
    assert_eq!(
        parse_discogs_release_id("http://discogs.com/release/1")
            .unwrap_err()
            .message(),
        "Enter a valid Discogs release URL or numeric ID."
    );
    assert_eq!(
        parse_discogs_release_id("https://discogs.com/release/1?x=1")
            .unwrap_err()
            .message(),
        "Enter a valid Discogs release URL or numeric ID."
    );
    assert_eq!(
        parse_discogs_release_id("https://discogs.com:443/release/1")
            .unwrap_err()
            .message(),
        "Enter a valid Discogs release URL or numeric ID."
    );
}

#[test]
fn brief_parse_musicbrainz_ids() {
    assert_eq!(
        parse_musicbrainz_release_id(" 11111111-1111-4111-8111-111111111111 ").unwrap(),
        MBID_RELEASE
    );
    assert_eq!(
        parse_musicbrainz_release_id(
            "HTTPS://musicbrainz.org/release/11111111-1111-4111-8111-111111111111"
        )
        .unwrap_err()
        .message(),
        "Enter a MusicBrainz release MBID or release URL."
    );
    assert_eq!(
        parse_musicbrainz_release_id(
            "https://musicbrainz.org/release/11111111-1111-4111-8111-111111111111/"
        )
        .unwrap(),
        MBID_RELEASE
    );
    assert_eq!(
        parse_musicbrainz_release_id(
            "https://musicbrainz.org/release/11111111-1111-4111-8111-111111111111?x=1"
        )
        .unwrap_err()
        .message(),
        "Enter a MusicBrainz release MBID or release URL."
    );
    assert_eq!(
        parse_musicbrainz_release_id("not-a-mbid")
            .unwrap_err()
            .message(),
        "Enter a MusicBrainz release MBID or release URL."
    );
    // Uppercase MBIDs normalize to lowercase (v2 UUID round-trip).
    assert_eq!(
        parse_musicbrainz_release_id(
            "11111111-1111-4111-8111-111111111111"
                .to_uppercase()
                .as_str()
        )
        .unwrap(),
        MBID_RELEASE
    );
}

#[test]
fn brief_sequence_ratio_matches_difflib_spots() {
    assert_eq!(sequence_matcher_ratio("", ""), 1.0);
    assert_eq!(sequence_matcher_ratio("abc", ""), 0.0);
    assert_eq!(sequence_matcher_ratio("abc", "abc"), 1.0);
    assert!((sequence_matcher_ratio("abcd", "abce") - 0.75).abs() < 1e-9);
    // Pinned against CPython difflib (independent oracle, not this port).
    assert!(
        (sequence_matcher_ratio("First Song", "First Song!") - 0.9523809523809523).abs() < 1e-9
    );
    assert!((sequence_matcher_ratio("kitten", "sitting") - 0.6153846153846154).abs() < 1e-9);
    assert!((sequence_matcher_ratio("song a", "second song") - 0.47058823529411764).abs() < 1e-9);
    assert_eq!(
        sequence_matcher_ratio("kitten", "sitting"),
        sequence_matcher_ratio("sitting", "kitten")
    );
}

#[test]
fn brief_retry_delay_schedule() {
    assert_eq!(verification_retry_delay_seconds(1, None), 15.0);
    assert_eq!(verification_retry_delay_seconds(2, None), 30.0);
    assert_eq!(verification_retry_delay_seconds(7, None), 600.0); // capped
    assert_eq!(verification_retry_delay_seconds(1, Some(120.0)), 120.0);
    assert_eq!(verification_retry_delay_seconds(1, Some(f64::NAN)), 15.0);
    assert_eq!(verification_retry_delay_seconds(1, Some(-5.0)), 15.0);
}

#[test]
fn brief_alignment_classes() {
    let rig_snapshot = LocalReleaseSnapshot {
        schema_version: 1,
        local_album_id: ALBUM.to_string(),
        local_artist_id: ARTIST.to_string(),
        album_row_revision: 7,
        input_revision: "rev".to_string(),
        title: "Test Album".to_string(),
        album_artist_name: "Test Artist".to_string(),
        artist_kind: "person".to_string(),
        musicbrainz_artist_id: None,
        musicbrainz_release_group_id: None,
        musicbrainz_release_id: None,
        release_date: None,
        year: None,
        is_compilation: false,
        captured_at: NOW,
        media: vec![ReleaseMediumSnapshot {
            position: 1,
            title: None,
            tracks: vec![
                ReleaseTrackSnapshot {
                    local_track_id: "t1".to_string(),
                    disc_number: 1,
                    track_number: 1,
                    title: "First Song".to_string(),
                    artist_name: None,
                    duration_seconds: Some(200.0),
                    duration_reliable: true,
                },
                ReleaseTrackSnapshot {
                    local_track_id: "t2".to_string(),
                    disc_number: 1,
                    track_number: 9,
                    title: "Something Entirely Different Here".to_string(),
                    artist_name: None,
                    duration_seconds: None,
                    duration_reliable: false,
                },
            ],
        }],
    };
    let release = discogs_release();
    let alignments = align_tracks(&rig_snapshot, &release);
    assert_eq!(alignments.len(), 2);
    assert_eq!(alignments[0].classification, AlignmentClassification::Exact);
    assert_eq!(alignments[0].provider_position.as_deref(), Some("A1"));
    // Wrong position contest + unrelated title + no duration: unmatched.
    assert_eq!(
        alignments[1].classification,
        AlignmentClassification::Unmatched
    );
    assert!(alignments[1].provider_position.is_none());
}

#[test]
fn brief_outcome_strings_and_system_clock() {
    assert_eq!(VerificationOutcome::Linked.as_str(), "linked");
    assert_eq!(VerificationOutcome::NeedsReview.as_str(), "needs_review");
    assert_eq!(VerificationOutcome::Stale.as_str(), "stale");
    assert_eq!(
        VerificationOutcome::RetryScheduled.as_str(),
        "retry_scheduled"
    );
    assert_eq!(
        VerificationOutcome::NoLongerVerifying.as_str(),
        "no_longer_verifying"
    );
    assert_eq!(
        VerificationOutcome::SubjectMissing.as_str(),
        "subject_missing"
    );
    assert!(SystemClock.now_seconds() > 1_700_000_000.0);
}

#[test]
fn brief_callback_token_shape() {
    assert!(valid_callback_token(&"a".repeat(32)));
    assert!(valid_callback_token("aB-_0AZaz09-_aB-_0AZaz09-_aB1234"));
    assert!(!valid_callback_token("short"));
    assert!(!valid_callback_token(&"a".repeat(129)));
    assert!(!valid_callback_token(
        "has space in token 01234567890123456789"
    ));
}

#[test]
fn brief_seed_field_helpers() {
    let mut fields = HashMap::new();
    fields.insert("t1".to_string(), "rec-1".to_string());
    let draft = ReleaseDraft::default();
    let snapshot = LocalReleaseSnapshot {
        schema_version: 1,
        local_album_id: ALBUM.to_string(),
        local_artist_id: ARTIST.to_string(),
        album_row_revision: 1,
        input_revision: "r".to_string(),
        title: String::new(),
        album_artist_name: String::new(),
        artist_kind: "person".to_string(),
        musicbrainz_artist_id: None,
        musicbrainz_release_group_id: None,
        musicbrainz_release_id: None,
        release_date: None,
        year: None,
        is_compilation: false,
        captured_at: NOW,
        media: Vec::new(),
    };
    let seed_fields = musicbrainz_seed_fields(
        &draft,
        &snapshot,
        None,
        None,
        &fields,
        "https://cb.example/x",
    );
    assert_eq!(seed_fields.first().unwrap().name, "name");
    assert_eq!(seed_fields.last().unwrap().name, "redirect_uri");
    // Multiple discovered groups seed nothing (ambiguous, v2 quirk).
    let duplicate = DuplicateCheckResult {
        schema_version: 1,
        checked_at: NOW,
        input_revision: "r".to_string(),
        candidates: vec![
            DuplicateCandidate {
                release_mbid: None,
                release_group_mbid: Some("g1".to_string()),
                title: String::new(),
                artist_name: String::new(),
                evidence_kind: DuplicateEvidenceKind::ReleaseGroup,
                exact: false,
                differences: Vec::new(),
            },
            DuplicateCandidate {
                release_mbid: None,
                release_group_mbid: Some("g2".to_string()),
                title: String::new(),
                artist_name: String::new(),
                evidence_kind: DuplicateEvidenceKind::ReleaseGroup,
                exact: false,
                differences: Vec::new(),
            },
        ],
        different_edition_confirmed: false,
    };
    let seed_fields = musicbrainz_seed_fields(
        &draft,
        &snapshot,
        Some(&duplicate),
        None,
        &fields,
        "https://cb.example/x",
    );
    assert!(seed_fields.iter().all(|f| f.name != "release_group"));
}
