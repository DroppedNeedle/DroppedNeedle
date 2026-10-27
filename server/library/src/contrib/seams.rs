//! Local seams: the narrow traits contributions need from code they do not
//! own, and the types their store takes.
//!
//! Owned elsewhere (implemented in [`super::attachment`] and [`super::live`]):
//! - [`AttachmentEvidence`] - the identification evidence decision for
//!   attach/verify, from identify (v2 `AlbumEvidenceEngine`).
//! - [`ContributionCatalog`] - catalog invalidation + identified hook, from
//!   the catalog (v2 `invalidate_catalog_scope` / `on_identified`).
//! - Provider reads (`DiscogsContrib`, `MusicBrainzContrib`) sit on the
//!   provider clients; every signature takes an explicit
//!   [`RequestPriority`](droppedneedle_providers::RequestPriority) so
//!   each call site states its lane.
//!
//! Futures are boxed by hand per repo idiom (no async-trait dependency).

use std::collections::HashMap;

use droppedneedle_providers::RequestPriority;
use futures_util::future::BoxFuture;

use super::error::ContribError;
use super::models::*;

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

// ---------------------------------------------------------------------------
// Catalog port (owned by the catalog)
// ---------------------------------------------------------------------------

/// Catalog invalidation plus the post-identify hook.
/// A trait because it cuts the dependency from contributions on
/// identification; [`super::live::IdentifyFollowUp`] implements it.
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
    /// The file-to-track pairing behind an identified decision; empty when
    /// the release needs review.
    pub tracks: Vec<TrackEvidence>,
}

/// One local file paired with one track of the verified release. A link
/// writes it as the file's MusicBrainz track identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackEvidence {
    pub local_track_id: String,
    pub recording_mbid: String,
    pub release_track_mbid: Option<String>,
    pub medium_position: Option<i64>,
    pub track_position: Option<i64>,
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
    /// Code of the last verification that ended without a link, if any.
    pub last_verification_failure: Option<String>,
    // Freshness join (v2 resolves these from the album row on read).
    pub album_active: bool,
    pub current_input_revision: String,
    pub current_album_row_revision: i64,
    /// What linking a release would meet on the album.
    pub links: AlbumLinks,
}

/// The album's MusicBrainz links, read with the contribution row.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AlbumLinks {
    /// The live album's artist; `None` when the album is gone.
    pub artist_id: Option<String>,
    /// The album's release and release group, when it has a match.
    pub identity: Option<(Option<String>, String)>,
    /// The artist's MusicBrainz id when a person or v2 linked it.
    pub chosen_artist_mbid: Option<String>,
}

/// The contribution a callback token belonged to; `None` when the token
/// is unknown, used or expired.
pub type CallbackConsumption = Option<String>;

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

/// Identity the worker asks the store to commit on a linked finish
/// (v2 resolves these from the selected evidence candidate).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FinishIdentities {
    pub release_mbid: Option<String>,
    pub release_group_mbid: Option<String>,
    pub artist_mbid: Option<String>,
    /// Per-file track identities the link writes.
    pub tracks: Vec<TrackEvidence>,
}
