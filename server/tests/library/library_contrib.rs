//! Contribution briefs: the submission path and the verification worker,
//! on the SQLite store over a private copy of the migrated database.
//!
//! The outside world is scripted (`contrib::scripted`): no network, no live
//! provider writes. The seed form is POSTed to `FakeReleaseEditor`, which
//! stands in for the MusicBrainz release editor. Albums, identities and
//! freshness are rows a test seeds; attachments are decided by the library
//! matcher.

use droppedneedle_library::contrib;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use contrib::error::ContribError;
use contrib::models::*;
use contrib::rules::*;
use contrib::scripted::*;
use contrib::seams::*;
use contrib::service::ContributionService;
use contrib::sqlite::SqliteContributions;
use contrib::worker::{Claim, SPEC, VERIFY, Verdict, VerificationWorker};
use droppedneedle_kernel::clock::ManualClock;
use droppedneedle_kernel::db::{Db, Lane};
use droppedneedle_kernel::jobs::queue::{Handlers, JobQueue, Outcome, runner, start_runners};
use droppedneedle_library::contrib::attachment::MatchingAttachmentEvidence;
use droppedneedle_providers::RequestPriority;
use futures_util::future::BoxFuture;
use rusqlite::{OptionalExtension as _, params};

use crate::common::{ScratchDir, migrated_db};

const NOW: f64 = 1_800_000_000.0;
const ALBUM: &str = "album-1";
const ARTIST: &str = "artist-1";
const ACTOR: &str = "user-1";
const POLICY: &str = "policy-1";

/// Unix seconds as the clock's milliseconds.
fn millis(seconds: f64) -> i64 {
    (seconds * 1000.0).round() as i64
}

const MBID_RELEASE: &str = "11111111-1111-4111-8111-111111111111";
const MBID_GROUP: &str = "22222222-2222-4222-8222-222222222222";
const MBID_ARTIST: &str = "33333333-3333-4333-8333-333333333333";
const MBID_OTHER: &str = "44444444-4444-4444-8444-444444444444";

/// What identification was told, in place of the port to it.
#[derive(Default)]
struct RecordingCatalog {
    scopes: Mutex<Vec<(Vec<String>, Vec<String>)>>,
    sweeps: Mutex<usize>,
    identified: Mutex<Vec<(String, String)>>,
}

impl RecordingCatalog {
    fn scopes(&self) -> Vec<(Vec<String>, Vec<String>)> {
        self.scopes.lock().unwrap().clone()
    }

    fn sweeps(&self) -> usize {
        *self.sweeps.lock().unwrap()
    }

    fn identified_calls(&self) -> Vec<(String, String)> {
        self.identified.lock().unwrap().clone()
    }
}

impl ContributionCatalog for RecordingCatalog {
    fn invalidate_identity_scope<'a>(
        &'a self,
        album_mbids: &'a [String],
        artist_mbids: &'a [String],
    ) -> BoxFuture<'a, ()> {
        self.scopes
            .lock()
            .unwrap()
            .push((album_mbids.to_vec(), artist_mbids.to_vec()));
        Box::pin(async {})
    }

    fn invalidate_identification<'a>(&'a self) -> BoxFuture<'a, ()> {
        *self.sweeps.lock().unwrap() += 1;
        Box::pin(async {})
    }

    fn after_identified<'a>(
        &'a self,
        local_album_id: &'a str,
        input_policy_revision: &'a str,
    ) -> BoxFuture<'a, ()> {
        self.identified
            .lock()
            .unwrap()
            .push((local_album_id.to_owned(), input_policy_revision.to_owned()));
        Box::pin(async {})
    }
}

/// A verification job as its row holds it.
struct Job {
    state: String,
}

/// A contribution verification attempt as its row holds it.
struct Attempt {
    state: String,
    reason: String,
    candidate_count: i64,
}

struct Rig {
    db: rusqlite::Connection,
    handle: Db,
    store: Arc<SqliteContributions>,
    discogs: Arc<ScriptedDiscogs>,
    musicbrainz: Arc<ScriptedMusicBrainz>,
    catalog: Arc<RecordingCatalog>,
    clock: ManualClock,
    service: Arc<ContributionService>,
    _dir: ScratchDir,
}

impl Rig {
    /// One curator, and one two-track album by an artist MusicBrainz knows.
    fn new() -> Self {
        let dir = ScratchDir::new("lib-contrib");
        let path = dir.join("app.db");
        migrated_db(&path);
        let db = droppedneedle_kernel::db::open_connection(&path).unwrap();
        let handle = Db::open_for_tests(&path).unwrap();
        let clock = ManualClock::at(millis(NOW));
        let queue = JobQueue::new(handle.clone(), Arc::new(clock.clone()));
        let store = Arc::new(SqliteContributions::new(handle.clone(), queue));
        let discogs = Arc::new(ScriptedDiscogs::new());
        let musicbrainz = Arc::new(ScriptedMusicBrainz::new());
        let catalog = Arc::new(RecordingCatalog::default());
        let service = Arc::new(ContributionService::new(
            store.clone(),
            Arc::new(MatchingAttachmentEvidence),
            contrib::ContribProviders {
                discogs: discogs.clone(),
                musicbrainz: musicbrainz.clone(),
            },
            catalog.clone(),
            Arc::new(clock.clone()),
        ));
        let rig = Self {
            db,
            handle,
            store,
            discogs,
            musicbrainz,
            catalog,
            clock,
            service,
            _dir: dir,
        };
        rig.db
            .execute(
                "INSERT INTO auth_users (id, display_name, created_at) \
                 VALUES (?1, 'Curator', '2026-01-01T00:00:00Z')",
                params![ACTOR],
            )
            .unwrap();
        rig.seed_album(ALBUM, ARTIST, 7, ["t1", "t2"]);
        rig.db
            .execute(
                "INSERT INTO local_artist_external_identities (local_artist_id, provider, \
                 provider_artist_id, decision_source, selected_at) \
                 VALUES (?1, 'musicbrainz', ?2, 'automatic', ?3)",
                params![ARTIST, MBID_ARTIST, NOW],
            )
            .unwrap();
        rig
    }

    /// A live album at `revision` with two indexed tracks and its own artist.
    fn seed_album(&self, album: &str, artist: &str, revision: i64, tracks: [&str; 2]) {
        self.db
            .execute(
                "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, \
                 updated_at) VALUES (?1, 'Test Artist', 'test artist', 'person', ?2, ?2)",
                params![artist, NOW],
            )
            .unwrap();
        self.db
            .execute(
                "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
                 album_artist_name, album_artist_id, year, original_release_date, \
                 grouping_source, created_at, updated_at, row_revision) \
                 VALUES (?1, 'root-1', ?1, 'Test Album', 'test album', 'Test Artist', ?2, 2024, \
                 '2024-03-07', 'automatic', ?3, ?3, ?4)",
                params![album, artist, NOW, revision],
            )
            .unwrap();
        let rows = [
            (tracks[0], 1, "First Song", 200.1234),
            (tracks[1], 2, "Second Song", 180.0),
        ];
        for (id, number, title, seconds) in rows {
            self.db
                .execute(
                    "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, \
                     relative_path, path_hash, file_size_bytes, file_mtime_ns, stat_revision, \
                     tag_revision, title, title_folded, artist_name, album_title, \
                     album_title_folded, disc_number, track_number, duration_seconds, \
                     file_format, ingest_source, imported_at, membership_source, \
                     applied_policy_revision) \
                     VALUES (?1, ?2, 'root-1', '/music/' || ?1 || '.flac', ?1 || '.flac', ?1, 1, \
                     1, 'stat-1', 'tag-1', ?3, lower(?3), 'Test Artist', 'Test Album', \
                     'test album', 1, ?4, ?5, 'flac', 'scan', ?6, 'automatic', ?7)",
                    params![id, album, title, number, seconds, NOW, POLICY],
                )
                .unwrap();
        }
    }

    /// The album already carries this MusicBrainz release.
    fn seed_album_identity(&self, album: &str, group: &str, release: &str) {
        self.db
            .execute(
                "INSERT INTO local_album_external_identities (local_album_id, provider, \
                 release_group_mbid, release_mbid, decision_source, selected_at) \
                 VALUES (?1, 'musicbrainz', ?2, ?3, 'automatic', ?4)",
                params![album, group, release, NOW],
            )
            .unwrap();
    }

    /// The management policy was applied again: the album's input moved.
    fn set_policy_revision(&self, album: &str, policy: &str) {
        self.db
            .execute(
                "UPDATE local_tracks SET applied_policy_revision = ?2 WHERE local_album_id = ?1",
                params![album, policy],
            )
            .unwrap();
    }

    /// `missing` takes every file of the album away, `indexed` brings them back.
    fn set_availability(&self, album: &str, availability: &str) {
        self.db
            .execute(
                "UPDATE local_tracks SET availability = ?2 WHERE local_album_id = ?1",
                params![album, availability],
            )
            .unwrap();
    }

    fn set_album_revision(&self, album: &str, revision: i64) {
        self.db
            .execute(
                "UPDATE local_albums SET row_revision = ?2 WHERE id = ?1",
                params![album, revision],
            )
            .unwrap();
    }

    async fn raw(&self, contribution_id: &str) -> ContributionRow {
        self.store.get(contribution_id).await.unwrap().unwrap()
    }

    fn jobs_for(&self, contribution_id: &str) -> Vec<Job> {
        self.db
            .prepare(
                "SELECT state FROM durable_jobs WHERE kind = ?1 AND idempotency_key = ?2 \
                 ORDER BY id",
            )
            .unwrap()
            .query_map(params![VERIFY.name(), contribution_id], |row| {
                Ok(Job { state: row.get(0)? })
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    fn attempts(&self) -> Vec<Attempt> {
        self.db
            .prepare(
                "SELECT state, terminal_reason_code, candidate_count \
                 FROM library_identification_attempts \
                 WHERE trigger = 'contribution_submission' ORDER BY started_at, rowid",
            )
            .unwrap()
            .query_map([], |row| {
                Ok(Attempt {
                    state: row.get(0)?,
                    reason: row.get(1)?,
                    candidate_count: row.get(2)?,
                })
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    fn live_tokens(&self) -> i64 {
        self.db
            .query_row(
                "SELECT COUNT(*) FROM library_contribution_callback_tokens \
                 WHERE consumed_at IS NULL",
                [],
                |row| row.get(0),
            )
            .unwrap()
    }

    /// The album's MusicBrainz identity: release, release group, who decided.
    fn album_identity(&self, album: &str) -> Option<(String, String, String)> {
        self.db
            .query_row(
                "SELECT release_mbid, release_group_mbid, decision_source \
                 FROM local_album_external_identities WHERE local_album_id = ?1",
                params![album],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .unwrap()
    }

    fn worker(&self) -> Arc<VerificationWorker> {
        Arc::new(VerificationWorker::new(
            self.service.clone(),
            self.musicbrainz.clone(),
        ))
    }

    fn now(&self) -> f64 {
        self.service.now()
    }
}

/// A run's contribution outcome.
fn outcome(verdict: Option<Verdict>) -> Option<VerificationOutcome> {
    verdict.map(|verdict| verdict.outcome)
}

/// When a retry asked to run again, in seconds, and its code.
fn retry(verdict: Option<Verdict>) -> (f64, &'static str) {
    match verdict.map(|verdict| verdict.job) {
        Some(Outcome::Retry { code, at: Some(at) }) => (at as f64 / 1000.0, code),
        other => panic!("expected a timed retry, got {other:?}"),
    }
}

/// The input revision of a seeded album whose tracks carry `policy`.
fn input_revision(tracks: [&str; 2], policy: &str) -> (String, String, String) {
    let input = TrackInput {
        tag_revision: Some("tag-1".to_owned()),
        stat_revision: "stat-1".to_owned(),
        applied_policy_revision: policy.to_owned(),
        applied_policy: "automatic".to_owned(),
    };
    album_input_revisions(tracks.iter().map(|id| (*id, &input)))
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
            ..Default::default()
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
        ..Default::default()
    }
}

/// The release MusicBrainz returns for the seeded album: same titles, artist
/// and lengths, so the matcher accepts it.
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
                duration_seconds: Some(200.123),
                recording_mbid: None,
                release_track_mbid: None,
            },
            MusicBrainzVerifiedTrack {
                title: "Second Song".to_string(),
                position: 2,
                disc_number: 1,
                duration_seconds: Some(180.0),
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
    let rig = Rig::new();
    rig.seed_album_identity(ALBUM, MBID_GROUP, MBID_RELEASE);
    let error = rig.service.create(ALBUM, ACTOR).await.unwrap_err();
    assert_eq!(
        error,
        ContribError::State("This local album already has an exact MusicBrainz release.".into())
    );
}

#[tokio::test]
async fn brief_create_then_update_reaches_ready() {
    let rig = Rig::new();
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
    let rig = Rig::new();
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
    let rig = Rig::new();
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
    let rig = Rig::new();
    let ready = to_ready(&rig).await;
    let (tag, file, policy) = input_revision(["t1", "t2"], POLICY);
    assert_eq!(ready.input_revision, format!("{tag}:{file}:{policy}"));
    // The policy is applied again: the album's input moved under the draft.
    rig.set_policy_revision(ALBUM, "policy-2");
    let stale = rig.service.get(&ready.id).await.unwrap();
    assert_eq!(stale.state, ContributionState::Stale);
    assert!(!stale.input_is_current);
    assert_eq!(stale.next_actions, vec![ContributionNextAction::Rebuild]);
    // A stale contribution for a live album offers rebuild; for a gone
    // album it offers NOTHING (v2 quirk).
    rig.set_availability(ALBUM, "missing");
    let gone = rig.service.get(&ready.id).await.unwrap();
    assert!(gone.next_actions.is_empty());
    assert!(rig.service.active_for_album(ALBUM).await.unwrap().is_none());
    // The files come back and the album row has moved on: rebuild makes a
    // NEW row from the album as it is now.
    rig.set_availability(ALBUM, "indexed");
    rig.set_album_revision(ALBUM, 8);
    let rebuilt = rig
        .service
        .rebuild(&ready.id, gone.row_revision, ACTOR)
        .await
        .unwrap();
    assert_ne!(rebuilt.id, ready.id);
    assert_eq!(rebuilt.state, ContributionState::Draft);
    let (tag, file, policy) = input_revision(["t1", "t2"], "policy-2");
    assert_eq!(rebuilt.input_revision, format!("{tag}:{file}:{policy}"));
    assert_eq!(rebuilt.local_snapshot.album_row_revision, 8);
    let old = rig.service.get(&ready.id).await.unwrap();
    assert_eq!(old.state, ContributionState::Stale);
}

// ---------------------------------------------------------------------------
// Discogs source briefs
// ---------------------------------------------------------------------------

#[tokio::test]
async fn brief_search_defaults_and_validates() {
    let rig = Rig::new();
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
            ..Default::default()
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
    let rig = Rig::new();
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
    rig.clock.advance(7 * 60 * 60 * 1000);
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
    let raw = rig.raw(&selected.id).await;
    assert!(raw.provider_snapshot_expires_at.is_none());
    assert!(raw.source_selection.alignments.is_empty());
}

// ---------------------------------------------------------------------------
// Duplicate + attach briefs
// ---------------------------------------------------------------------------

#[tokio::test]
async fn brief_duplicate_check_orders_evidence_and_forces_review() {
    let rig = Rig::new();
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
    let rig = Rig::new();
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
    let rig = Rig::new();
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
    let rig = Rig::new();
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
    // Exactly the touched identity keys, before returning.
    assert_eq!(
        rig.catalog.scopes(),
        vec![(vec![MBID_GROUP.to_string()], vec![MBID_ARTIST.to_string()])]
    );
    assert_eq!(
        rig.catalog.identified_calls(),
        vec![(ALBUM.to_string(), input_revision(["t1", "t2"], POLICY).2)]
    );
    // The curator named this release: the album takes it as a manual identity.
    assert_eq!(
        rig.album_identity(ALBUM),
        Some((
            MBID_RELEASE.to_string(),
            MBID_GROUP.to_string(),
            "manual".to_string()
        ))
    );
    let attempts = rig.attempts();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].state, "identified");
    let raw = rig.raw(&linked.id).await;
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
    let rig = Rig::new();
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

/// Attach the album to `MBID_RELEASE`, whose artist is `MBID_ARTIST`, the
/// way a curator does from the duplicate check.
async fn attach_release(rig: &Rig) -> Result<ContributionRecord, ContribError> {
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
    let ready = to_ready(rig).await;
    let selected = rig
        .service
        .select_discogs(&ready.id, "12345", ready.row_revision, ACTOR)
        .await?;
    let edited = rig
        .service
        .update(&selected.id, selected.row_revision, &selected.draft, ACTOR)
        .await?;
    let checked = rig
        .service
        .check_duplicates(&edited.id, edited.row_revision, ACTOR, false)
        .await?;
    rig.service
        .attach_existing(&checked.id, MBID_RELEASE, checked.row_revision, ACTOR)
        .await
}

/// Every MusicBrainz link of local artists: (artist, id, decision source).
fn artist_links(rig: &Rig) -> Vec<(String, String, String)> {
    rig.db
        .prepare(
            "SELECT local_artist_id, provider_artist_id, decision_source \
             FROM local_artist_external_identities ORDER BY local_artist_id",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

/// A curator's link replaces one the app made: the album's artist, linked
/// by the app to another MusicBrainz artist, takes the curator's.
#[tokio::test]
async fn brief_attach_replaces_the_apps_link_of_the_artist() {
    let rig = Rig::new();
    rig.db
        .execute(
            "UPDATE local_artist_external_identities SET provider_artist_id = ?2 \
             WHERE local_artist_id = ?1",
            params![ARTIST, MBID_OTHER],
        )
        .unwrap();
    let linked = attach_release(&rig).await.expect("not refused");
    assert_eq!(linked.state, ContributionState::Linked);
    assert_eq!(
        artist_links(&rig),
        vec![(
            ARTIST.to_owned(),
            MBID_ARTIST.to_owned(),
            "manual".to_owned()
        )]
    );
}

/// A curator's link takes the MusicBrainz artist from another record the
/// app linked to it, with no merge suggested.
#[tokio::test]
async fn brief_attach_takes_the_id_from_the_apps_link_elsewhere() {
    let rig = Rig::new();
    rig.db
        .execute_batch(&format!(
            "DELETE FROM local_artist_external_identities; \
             INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, \
             updated_at) VALUES ('artist-2', 'Test Artist', 'test artist', 'person', 1, 1); \
             INSERT INTO local_artist_external_identities (local_artist_id, provider, \
             provider_artist_id, decision_source, selected_at) \
             VALUES ('artist-2', 'musicbrainz', '{MBID_ARTIST}', 'automatic', 1);"
        ))
        .unwrap();
    let linked = attach_release(&rig).await.expect("not refused");
    assert_eq!(linked.state, ContributionState::Linked);
    assert_eq!(
        artist_links(&rig),
        vec![(
            ARTIST.to_owned(),
            MBID_ARTIST.to_owned(),
            "manual".to_owned()
        )]
    );
    let candidates: i64 = rig
        .db
        .query_row(
            "SELECT COUNT(*) FROM local_artist_merge_candidates",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(candidates, 0);
}

// ---------------------------------------------------------------------------
// Submission briefs (mocked editor - never a live provider write)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn brief_seed_builds_exact_editor_form() {
    let rig = Rig::new();
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
        "https://needle.example.com/api/v3/library/contributions/musicbrainz/callback?token="
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
    let raw = rig.raw(&checked.id).await;
    let snapshot = raw.seed_snapshot_json.clone().unwrap();
    assert!(snapshot.contains("input_revision"));
    assert!(!snapshot.contains("redirect_uri"));
    assert_eq!(seed.contribution_revision, raw.row_revision);
    assert_eq!(rig.live_tokens(), 1);
    let seeded = rig.service.get(&checked.id).await.unwrap();
    assert_eq!(seeded.state, ContributionState::Seeded);
}

#[tokio::test]
async fn brief_seed_guards_duplicates_race_and_base_url() {
    let rig = Rig::new();
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
    // A second album, to avoid the still-open contribution above. Its
    // artist has no MusicBrainz identity: one MusicBrainz artist belongs to
    // one local artist.
    rig.seed_album("album-2", "artist-2", 3, ["u1", "u2"]);
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

#[tokio::test]
async fn brief_full_submission_roundtrip_through_mocked_editor() {
    let rig = Rig::new();
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
    assert_eq!(rig.jobs_for(&checked.id).len(), 1);
    // The background worker confirms the release and links the album.
    rig.musicbrainz.insert_verification(
        MBID_RELEASE,
        Ok(Some(verified_release(MBID_RELEASE, MBID_GROUP))),
    );
    let worker = rig.worker();
    let outcome = outcome(worker.run_once().await.unwrap());
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
        rig.album_identity(ALBUM),
        Some((
            MBID_RELEASE.to_string(),
            MBID_GROUP.to_string(),
            "manual".to_string()
        ))
    );
}

#[tokio::test]
async fn brief_callback_rejects_bad_and_reused_tokens() {
    let rig = Rig::new();
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
    let rig = Rig::new();
    let verifying = to_verifying(&rig).await;
    rig.musicbrainz.insert_verification(MBID_RELEASE, Ok(None)); // not propagated yet
    let worker = rig.worker();
    // First failure: 15s backoff (15 * 2^min(1-1, 6)).
    let verdict = worker.run_once().await.unwrap();
    assert_eq!(outcome(verdict), Some(VerificationOutcome::RetryScheduled));
    let jobs = rig.jobs_for(&verifying.id);
    assert_eq!(jobs.len(), 1);
    let (mut not_before, code) = retry(verdict);
    assert_eq!(not_before, NOW + 15.0);
    assert_eq!(code, FAILURE_MB_NOT_PROPAGATED);
    // Nine more scheduled retries, then the window/attempt budget runs out.
    for _ in 0..8 {
        rig.clock.set(millis(not_before));
        let verdict = worker.run_once().await.unwrap();
        assert_eq!(outcome(verdict), Some(VerificationOutcome::RetryScheduled));
        not_before = retry(verdict).0;
    }
    rig.clock.set(millis(not_before));
    let outcome = outcome(worker.run_once().await.unwrap());
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
    let attempts = rig.attempts();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].reason, FAILURE_MB_NOT_PROPAGATED);
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
    let rig = Rig::new();
    let verifying = to_verifying(&rig).await;
    rig.musicbrainz
        .insert_verification(MBID_RELEASE, Err(ProviderFailure::Unmappable));
    let outcome = outcome(rig.worker().run_once().await.unwrap());
    assert_eq!(outcome, Some(VerificationOutcome::NeedsReview));
    let jobs = rig.jobs_for(&verifying.id);
    assert_eq!(jobs[0].state, "done");
    assert_eq!(
        rig.raw(&verifying.id)
            .await
            .last_verification_failure
            .as_deref(),
        Some(UNMAPPABLE_PROVIDER_PAYLOAD)
    );
    // No evidence ran on the payload failure: the attempt has no candidate.
    let attempts = rig.attempts();
    assert_eq!(attempts[0].reason, UNMAPPABLE_PROVIDER_PAYLOAD);
    assert_eq!(attempts[0].candidate_count, 0);
}

#[tokio::test]
async fn brief_worker_release_mismatch_reviews() {
    let rig = Rig::new();
    let _verifying = to_verifying(&rig).await;
    rig.musicbrainz.insert_verification(
        MBID_RELEASE,
        Ok(Some(verified_release(MBID_OTHER, MBID_GROUP))),
    );
    let outcome = outcome(rig.worker().run_once().await.unwrap());
    assert_eq!(outcome, Some(VerificationOutcome::NeedsReview));
    assert_eq!(outcome.unwrap().as_str(), "needs_review");
    let attempts = rig.attempts();
    assert_eq!(attempts[0].reason, FAILURE_RETURNED_RELEASE_MISMATCH);
}

#[tokio::test]
async fn brief_worker_contradiction_reviews_with_reason() {
    let rig = Rig::new();
    let _verifying = to_verifying(&rig).await;
    // MusicBrainz returns the named release, but it is another album.
    let mut other = verified_release(MBID_RELEASE, MBID_GROUP);
    other.title = "Unrelated Record".to_string();
    other.artist_name = "Somebody Else".to_string();
    rig.musicbrainz
        .insert_verification(MBID_RELEASE, Ok(Some(other)));
    let outcome = outcome(rig.worker().run_once().await.unwrap());
    assert_eq!(outcome, Some(VerificationOutcome::NeedsReview));
    let attempts = rig.attempts();
    assert_eq!(attempts[0].state, "needs_review");
    assert_eq!(attempts[0].reason, "ATTACHMENT_NAME_MISMATCH");
    assert_eq!(attempts[0].candidate_count, 1);
    assert!(rig.album_identity(ALBUM).is_none());
}

#[tokio::test]
async fn brief_worker_retry_after_floor_and_bogus_drop() {
    let rig = Rig::new();
    let verifying = to_verifying(&rig).await;
    rig.musicbrainz.insert_verification(
        MBID_RELEASE,
        Err(ProviderFailure::Unavailable {
            retry_after_seconds: Some(120.0),
        }),
    );
    let verdict = rig.worker().run_once().await.unwrap();
    assert_eq!(outcome(verdict), Some(VerificationOutcome::RetryScheduled));
    assert_eq!(retry(verdict).0, NOW + 120.0);
    assert_eq!(rig.jobs_for(&verifying.id).len(), 1);
    // A bogus retry-after (NaN) is dropped, not clamped: plain 15s backoff.
    rig.musicbrainz.insert_verification(
        MBID_RELEASE,
        Err(ProviderFailure::Unavailable {
            retry_after_seconds: Some(f64::NAN),
        }),
    );
    rig.clock.set(millis(NOW + 120.0));
    let worker = rig.worker();
    // Drain the queued job from the first part, then re-run: attempt 2 -> 30s.
    let verdict = worker.run_once().await.unwrap();
    assert_eq!(outcome(verdict), Some(VerificationOutcome::RetryScheduled));
    assert_eq!(retry(verdict).0, NOW + 120.0 + 30.0);
}

/// A claimed job whose contribution was cancelled since does nothing,
/// and the cancel took its job away. Lease recovery is the jobs module's,
/// covered by its own suite.
#[tokio::test]
async fn brief_worker_skips_non_verifying() {
    let rig = Rig::new();
    let verifying = to_verifying(&rig).await;
    let worker = rig.worker();
    // Claim out-of-band, then cancel the contribution: the job no longer applies.
    let queue = rig.store.queue().clone();
    let claimed = {
        let queue = queue.clone();
        rig.handle
            .write(Lane::Background, "test.claim", move |tx| {
                queue.claim_in(tx, &SPEC)
            })
            .await
            .unwrap()
            .unwrap()
    };
    let record = rig.service.get(&verifying.id).await.unwrap();
    rig.service
        .cancel(&record.id, record.row_revision, ACTOR)
        .await
        .unwrap();
    // Fresh worker run finds no queued job (cancel retired it).
    assert!(worker.run_once().await.unwrap().is_none());
    assert!(rig.jobs_for(&verifying.id).is_empty());
    let lease = claimed.lease.clone();
    let claim = Claim {
        contribution_id: claimed.key.clone(),
        attempt: claimed.attempt,
        complete: Arc::new(move |tx| queue.complete(tx, &lease)),
    };
    let verdict = worker.run_claimed(&claim, rig.now()).await.unwrap();
    assert_eq!(verdict.outcome, VerificationOutcome::NoLongerVerifying);
    assert_eq!(verdict.job, Outcome::Done);
}

/// The jobs module runs verification as the server registers it: the
/// runner links the contribution, then stops with the supervisor.
#[tokio::test]
async fn brief_worker_runner_links_then_shuts_down() {
    let rig = Rig::new();
    let verifying = to_verifying(&rig).await;
    rig.musicbrainz.insert_verification(
        MBID_RELEASE,
        Ok(Some(verified_release(MBID_RELEASE, MBID_GROUP))),
    );
    let supervisor = droppedneedle_kernel::jobs::Supervisor::start(
        rig.handle.clone(),
        Arc::new(droppedneedle_kernel::clock::SystemClock),
        droppedneedle_kernel::jobs::CancellationToken::new(),
    )
    .await
    .unwrap();
    start_runners(
        rig.store.queue(),
        &supervisor,
        Handlers::default().with(SPEC, rig.worker().handler()),
    )
    .unwrap();
    // The runner's store calls run on blocking threads, so the bound is
    // wall-clock time.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut linked = rig.service.get(&verifying.id).await.unwrap();
    while linked.state != ContributionState::Linked && std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        linked = rig.service.get(&verifying.id).await.unwrap();
    }
    assert_eq!(
        linked.state,
        ContributionState::Linked,
        "the runner should link the contribution"
    );
    let jobs = rig.jobs_for(&verifying.id);
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].state, "done");
    supervisor.token().cancel();
    let name = runner::runner_name(VERIFY);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while supervisor.is_running(&name) {
        assert!(
            std::time::Instant::now() < deadline,
            "the runner should shut down"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    supervisor
        .shutdown(std::time::Duration::from_secs(30))
        .await;
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
