//! Local seams: the narrow traits contributions need from code they do not
//! own, plus the store and clock they run against.
//!
//! Owned elsewhere (implemented in `library::adapters`):
//! - [`ContributionIdentity`] - album identification context, from scan and
//!   identify (v2 `NativeLibraryStore.get_album_identification_context`).
//! - [`AttachmentEvidence`] - the identification evidence decision for
//!   attach/verify, from identify (v2 `AlbumEvidenceEngine`).
//! - [`ContributionCatalog`] - catalog invalidation + identified hook, from
//!   the catalog (v2 `invalidate_catalog_scope` / `on_identified`).
//! - Provider reads (`DiscogsContrib`, `MusicBrainzContrib`) sit on the
//!   provider clients; every signature takes an explicit
//!   [`RequestPriority`](crate::providers::slots::RequestPriority) so
//!   each call site states its lane.
//!
//! Futures are boxed by hand per repo idiom (no async-trait dependency).

use std::collections::HashMap;

use crate::providers::slots::RequestPriority;
use futures_util::future::BoxFuture;

use super::error::ContribError;
use super::models::*;

// ---------------------------------------------------------------------------
// Clock
// ---------------------------------------------------------------------------

pub trait ContributionClock: Send + Sync {
    fn now_seconds(&self) -> f64;
}

#[derive(Debug, Default)]
pub struct SystemClock;

impl ContributionClock for SystemClock {
    fn now_seconds(&self) -> f64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0)
    }
}

// ---------------------------------------------------------------------------
// Identity context (owned by scan/identify)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct IdentityAlbumRow {
    pub id: String,
    pub row_revision: i64,
    pub active: bool,
    pub title: String,
    pub album_artist_name: String,
    pub album_artist_id: String,
    pub original_release_date: Option<String>,
    pub year: Option<i32>,
    pub is_compilation: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct IdentityAlbumIds {
    pub release_mbid: Option<String>,
    pub release_group_mbid: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct IdentityArtist {
    pub kind: String,
    pub provider_artist_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IdentityTrack {
    pub id: String,
    pub disc_number: i64,
    pub track_number: i64,
    pub title: String,
    pub artist_name: Option<String>,
    pub duration_seconds: Option<f64>,
    pub availability: String,
    pub disc_subtitle: Option<String>,
    pub relative_path: String,
    pub recording_mbid: Option<String>,
    pub embedded_recording_mbid: Option<String>,
    /// The revisions that make up the album's input revision.
    pub input: TrackInput,
}

/// Per-track revisions the contribution input revision is built from
/// (see [`super::rules::album_input_revisions`]).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TrackInput {
    pub tag_revision: Option<String>,
    pub stat_revision: String,
    pub applied_policy_revision: String,
    pub applied_policy: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct AlbumIdentificationContext {
    pub album: Option<IdentityAlbumRow>,
    pub identity: IdentityAlbumIds,
    pub artist: IdentityArtist,
    pub tracks: Vec<IdentityTrack>,
}

/// Reads the album identification context. Implemented against the real
/// library store; contributions only consume it.
pub trait ContributionIdentity: Send + Sync {
    fn album_context<'a>(
        &'a self,
        album_id: &'a str,
    ) -> BoxFuture<'a, Option<AlbumIdentificationContext>>;

    /// Input revisions owned by scan and identify (v2 `album_input_revisions`):
    /// tag, file, policy. The contribution input revision is the
    /// colon-joined triple.
    fn input_revisions(&self, tracks: &[IdentityTrack]) -> (String, String, String);
}

// ---------------------------------------------------------------------------
// Catalog port (owned by the catalog)
// ---------------------------------------------------------------------------

/// Catalog invalidation plus the post-identify hook.
pub trait ContributionCatalog: Send + Sync {
    /// Delete exactly the touched identity-bearing keys before the
    /// commit path returns; lists still sweep.
    fn invalidate_identity_scope<'a>(
        &'a self,
        album_mbids: &'a [String],
        artist_mbids: &'a [String],
    ) -> BoxFuture<'a, ()>;

    /// Wholesale identification-prefix sweep for callers without ids.
    fn invalidate_identification<'a>(&'a self) -> BoxFuture<'a, ()>;

    /// Fired after an album links to a MusicBrainz release.
    fn after_identified<'a>(
        &'a self,
        local_album_id: &'a str,
        input_policy_revision: &'a str,
    ) -> BoxFuture<'a, ()>;
}

// ---------------------------------------------------------------------------
// Attachment evidence (owned by identify)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachmentOutcome {
    Identified,
    NeedsReview,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AttachmentCandidate {
    pub release_group_mbid: String,
    pub release_mbid: Option<String>,
    pub artist_mbid: Option<String>,
}

impl AttachmentCandidate {
    pub fn key(&self) -> String {
        format!(
            "{}:{}",
            self.release_group_mbid,
            self.release_mbid.as_deref().unwrap_or("")
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AttachmentDecision {
    pub outcome: AttachmentOutcome,
    pub reason_code: Option<String>,
    pub selected_candidate_key: Option<String>,
    pub candidates: Vec<AttachmentCandidate>,
}

/// Decides whether a verified MusicBrainz release safely matches the current
/// draft. v2 runs `AlbumEvidenceEngine.decide` with `require_lone_quorum =
/// false` here (the curator verified this exact release; the engine keeps
/// only contradiction detection). Identify owns the engine;
/// this trait is the seam.
pub trait AttachmentEvidence: Send + Sync {
    /// Engine version stamped on verification attempts (v2 MATCHER_VERSION).
    fn matcher_version(&self) -> String;

    fn decide_attachment<'a>(
        &'a self,
        contribution: &'a ContributionRecord,
        verified: &'a MusicBrainzVerifiedRelease,
        recording_mbids: &'a HashMap<String, Option<String>>,
        relative_paths: &'a HashMap<String, String>,
    ) -> BoxFuture<'a, AttachmentDecision>;
}

// ---------------------------------------------------------------------------
// Provider reads (priority is explicit at every call)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum ProviderFailure {
    /// Transient: schedule a retry (v2 `ExternalServiceError` / open circuit).
    Unavailable { retry_after_seconds: Option<f64> },
    /// Deterministic payload-shape failure: review immediately, never retry
    /// (v2 `InvalidExternalPayloadError` -> UNMAPPABLE_PROVIDER_PAYLOAD).
    Unmappable,
}

pub trait DiscogsContrib: Send + Sync {
    fn search_releases<'a>(
        &'a self,
        query: &'a str,
        limit: usize,
        priority: RequestPriority,
    ) -> BoxFuture<'a, Result<Vec<DiscogsReleaseCandidate>, ContribError>>;

    fn get_release<'a>(
        &'a self,
        release_id: &'a str,
        priority: RequestPriority,
    ) -> BoxFuture<'a, Result<Option<DiscogsRelease>, ContribError>>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UrlRelation {
    Release,
    ReleaseGroup,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct DuplicateSearchFacts {
    pub title: String,
    pub artist_name: String,
    pub barcode: Option<String>,
    pub country: Option<String>,
    pub date: Option<String>,
}

pub trait MusicBrainzContrib: Send + Sync {
    fn resolve_url<'a>(
        &'a self,
        url: &'a str,
        relation: UrlRelation,
        priority: RequestPriority,
        bypass_cache: bool,
    ) -> BoxFuture<'a, Result<MusicBrainzUrlResolution, ContribError>>;

    fn get_release_for_verification<'a>(
        &'a self,
        release_mbid: &'a str,
        priority: RequestPriority,
        bypass_cache: bool,
    ) -> BoxFuture<'a, Result<Option<MusicBrainzVerifiedRelease>, ProviderFailure>>;

    fn search_duplicate_releases<'a>(
        &'a self,
        facts: &'a DuplicateSearchFacts,
        limit: usize,
        priority: RequestPriority,
    ) -> BoxFuture<'a, Result<Vec<MusicBrainzVerifiedRelease>, ContribError>>;
}

// ---------------------------------------------------------------------------
// Persisted rows
// ---------------------------------------------------------------------------

/// Persisted contribution row (v2 `library_contributions` row shape, typed).
#[derive(Debug, Clone, PartialEq)]
pub struct ContributionRow {
    pub id: String,
    pub local_album_id: String,
    pub created_by_user_id: Option<String>,
    pub updated_by_user_id: Option<String>,
    pub state: ContributionState,
    pub album_row_revision: i64,
    pub input_revision: String,
    pub local_snapshot: LocalReleaseSnapshot,
    pub draft: ReleaseDraft,
    pub source_selection: ContributionSourceSelection,
    pub provider_snapshot_expires_at: Option<f64>,
    pub discogs_release_id: Option<String>,
    pub discogs_canonical_url: Option<String>,
    pub duplicate_result: Option<DuplicateCheckResult>,
    pub duplicate_checked_at: Option<f64>,
    pub duplicate_input_revision: Option<String>,
    pub result_release_mbid: Option<String>,
    pub result_source: Option<String>,
    pub result_received_at: Option<f64>,
    pub seeded_at: Option<f64>,
    pub seed_token_hash: Option<String>,
    pub seed_token_expires_at: Option<f64>,
    pub seed_snapshot_json: Option<String>,
    pub seed_hash: Option<String>,
    pub terminal_at: Option<f64>,
    pub created_at: f64,
    pub updated_at: f64,
    pub row_revision: i64,
    /// Failure code of the latest finished verification job, if any.
    pub last_verification_failure: Option<String>,
    // Freshness join (v2 resolves these from the album row on read).
    pub album_active: bool,
    pub current_input_revision: String,
    pub current_album_row_revision: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerificationJobState {
    Queued,
    Running,
    Succeeded,
    NeedsReview,
    // v2 terminal state, kept so the store enum stays complete.
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VerificationJobRow {
    pub id: String,
    pub contribution_id: String,
    pub state: VerificationJobState,
    pub worker_id: Option<String>,
    pub attempt_count: u32,
    pub not_before: f64,
    pub created_at: f64,
    pub lease_expires_at: Option<f64>,
    pub last_failure_code: Option<String>,
    pub requested_by_user_id: Option<String>,
    pub terminal_at: Option<f64>,
    pub row_revision: i64,
}

/// Callback consumption result: contribution id plus the verification job
/// id when one was (re)queued (`None` when the contribution went stale).
pub type CallbackConsumption = Option<(String, Option<String>)>;

/// Minimal attempt record (v2 `IdentificationAttempt`, trigger fixed to
/// `contribution_submission`; the full evidence shape stays with identify).
#[derive(Debug, Clone, PartialEq)]
pub struct ContributionVerificationAttempt {
    pub id: String,
    pub local_album_id: String,
    pub requested_by_user_id: Option<String>,
    pub matcher_version: String,
    pub state: String,
    pub terminal_reason_code: Option<String>,
    pub selected_candidate_key: Option<String>,
    pub candidate_count: usize,
    pub candidate_keys: Vec<String>,
    pub started_at: f64,
    pub completed_at: f64,
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

pub trait ContributionStore: Send + Sync {
    // Store seam mirrors the v2 persistence signatures argument-for-argument
    // so guards stay comparable; bundling them would hide the mapping.
    #[allow(clippy::too_many_arguments)]
    fn create_or_get<'a>(
        &'a self,
        local_album_id: &'a str,
        actor_user_id: &'a str,
        album_row_revision: i64,
        input_revision: &'a str,
        snapshot: &'a LocalReleaseSnapshot,
        draft: &'a ReleaseDraft,
        selection: &'a ContributionSourceSelection,
        now: f64,
    ) -> BoxFuture<'a, Result<ContributionRow, ContribError>>;

    fn get<'a>(&'a self, contribution_id: &'a str) -> BoxFuture<'a, Option<ContributionRow>>;

    fn get_active_for_album<'a>(
        &'a self,
        album_id: &'a str,
    ) -> BoxFuture<'a, Option<ContributionRow>>;

    /// Optimistic-lock helper: runs `update` against the current row; the
    /// store rejects stale `expected_row_revision` reads.
    fn compare_and_set<'a>(
        &'a self,
        contribution_id: &'a str,
        expected_row_revision: i64,
        actor_user_id: &'a str,
        now: f64,
        update: ContributionUpdate,
    ) -> BoxFuture<'a, Result<ContributionRow, ContribError>>;

    fn mark_stale<'a>(
        &'a self,
        contribution_id: &'a str,
        expected_row_revision: i64,
        now: f64,
    ) -> BoxFuture<'a, Result<ContributionRow, ContribError>>;

    /// Returns the contribution id plus the verification job id when one
    /// was (re)queued; `None` means the token is unknown, consumed, or
    /// expired (v2 `ResourceNotFoundError`). State conflicts surface as
    /// `Err` so they keep their own message.
    fn consume_callback_token<'a>(
        &'a self,
        token_hash: &'a str,
        release_mbid: &'a str,
        now: f64,
    ) -> BoxFuture<'a, Result<CallbackConsumption, ContribError>>;

    fn list_for_provider_purge<'a>(
        &'a self,
        now: f64,
        limit: usize,
    ) -> BoxFuture<'a, Vec<ContributionRow>>;

    fn claim_verification<'a>(
        &'a self,
        worker_id: &'a str,
        now: f64,
        lease_seconds: f64,
    ) -> BoxFuture<'a, Option<VerificationJobRow>>;

    /// Rebuild retires the stale row and inserts a fresh row (new id,
    /// state draft); returns the new row (v2 `rebuild_library_contribution`).
    #[allow(clippy::too_many_arguments)]
    fn rebuild<'a>(
        &'a self,
        contribution_id: &'a str,
        expected_row_revision: i64,
        actor_user_id: &'a str,
        album_row_revision: i64,
        input_revision: &'a str,
        snapshot: &'a LocalReleaseSnapshot,
        draft: &'a ReleaseDraft,
        selection: &'a ContributionSourceSelection,
        now: f64,
    ) -> BoxFuture<'a, Result<ContributionRow, ContribError>>;

    fn heartbeat_verification<'a>(
        &'a self,
        job_id: &'a str,
        worker_id: &'a str,
        expected_row_revision: i64,
        now: f64,
        lease_seconds: f64,
    ) -> BoxFuture<'a, Result<i64, ContribError>>;

    fn retry_verification<'a>(
        &'a self,
        job_id: &'a str,
        worker_id: &'a str,
        expected_row_revision: i64,
        failure_code: &'a str,
        not_before: f64,
        now: f64,
    ) -> BoxFuture<'a, Result<(), ContribError>>;

    #[allow(clippy::too_many_arguments)]
    fn finish_verification<'a>(
        &'a self,
        job_id: &'a str,
        worker_id: &'a str,
        expected_job_revision: i64,
        expected_contribution_revision: i64,
        expected_album_revision: i64,
        attempt: &'a ContributionVerificationAttempt,
        outcome: VerificationOutcome,
        failure_code: Option<&'a str>,
        identities: &'a FinishIdentities,
        now: f64,
    ) -> BoxFuture<'a, Result<VerificationOutcome, ContribError>>;

    fn recover_verification_leases<'a>(&'a self, now: f64) -> BoxFuture<'a, u64>;

    fn clean_records<'a>(&'a self, now: f64) -> BoxFuture<'a, ()>;
}

/// Identity the worker asks the store to commit on a linked finish
/// (v2 resolves these from the selected evidence candidate).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FinishIdentities {
    pub release_mbid: Option<String>,
    pub release_group_mbid: Option<String>,
    pub artist_mbid: Option<String>,
}

/// Store-side mutation applied under the row-revision guard.
#[derive(Debug, Clone)]
pub enum ContributionUpdate {
    Draft {
        draft: ReleaseDraft,
        state: ContributionState,
    },
    SelectDiscogs {
        release: DiscogsRelease,
        selection: ContributionSourceSelection,
        expires_at: f64,
    },
    RemoveDiscogs {
        draft: ReleaseDraft,
        state: ContributionState,
    },
    DuplicateResult {
        result: DuplicateCheckResult,
        state: ContributionState,
    },
    AttachExisting {
        release_mbid: String,
        release_group_mbid: String,
        artist_mbid: Option<String>,
        attempt: ContributionVerificationAttempt,
    },
    PrepareSeed {
        token_hash: String,
        token_expires_at: f64,
        seed_snapshot_json: String,
        seed_hash: String,
    },
    ManualResult {
        release_mbid: String,
        replace_existing: bool,
    },
    RequeueVerification,
    Cancel,
    PurgeProviderData {
        draft: ReleaseDraft,
        selection: ContributionSourceSelection,
    },
}
