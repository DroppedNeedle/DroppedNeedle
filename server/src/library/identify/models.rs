//! Identity types: who decided, what proves it, and what survives.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::library::matching::{PenaltyShare, Release};

/// Who last set an identity row. Ported verbatim from v2: automatic rows
/// are revisable, manual and legacy-import rows are curator-protected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionSource {
    Automatic,
    Manual,
    LegacyImport,
}

impl DecisionSource {
    /// Only automatic rows may be overwritten by a later automatic pass.
    /// Manual and legacy-import rows survive rescans, moves, and re-id.
    pub fn automatic_may_overwrite(self) -> bool {
        matches!(self, DecisionSource::Automatic)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            DecisionSource::Automatic => "automatic",
            DecisionSource::Manual => "manual",
            DecisionSource::LegacyImport => "legacy_import",
        }
    }
}

/// Per-track verdict against one candidate, ported from v2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceClass {
    Supported,
    Unknown,
    Contradictory,
}

/// Terminal outcomes of one identification attempt, ported from v2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentificationOutcome {
    Identified,
    EditionUncertain,
    NoCandidate,
    Ambiguous,
    Contradictory,
    InsufficientEvidence,
    KeptTagged,
    ProviderDeferred,
    Failed,
}

/// One candidate the matcher scored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CandidateEvidence {
    pub candidate_key: String,
    pub release_group_mbid: String,
    pub release_mbid: Option<String>,
    pub album_title: String,
    pub album_artist_name: String,
    pub track_evidence: Vec<TrackEvidence>,
    /// `1 - distance`, for sorting and display.
    pub score: f64,
    pub reason_code: String,
    /// Library distance from the matcher: 0 is a perfect match.
    #[serde(default)]
    pub distance: f64,
    /// What the distance is made of, largest share first.
    #[serde(default)]
    pub penalties: Vec<PenaltyShare>,
}

impl CandidateEvidence {
    pub fn supported_count(&self) -> usize {
        self.track_evidence
            .iter()
            .filter(|t| t.classification == EvidenceClass::Supported)
            .count()
    }

    pub fn contradictory_count(&self) -> usize {
        self.track_evidence
            .iter()
            .filter(|t| t.classification == EvidenceClass::Contradictory)
            .count()
    }
}

/// Per-track evidence inside one candidate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackEvidence {
    pub local_track_id: String,
    pub classification: EvidenceClass,
    pub evidence_kinds: Vec<String>,
    pub recording_mbid: Option<String>,
    pub release_track_mbid: Option<String>,
}

/// The durable album identity row: what the library believes, and why.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlbumIdentity {
    pub local_album_id: String,
    pub provider: String,
    pub release_group_mbid: Option<String>,
    pub release_mbid: Option<String>,
    pub decision_source: DecisionSource,
    pub row_revision: u64,
}

/// The durable track identity row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackIdentity {
    pub local_track_id: String,
    pub provider: String,
    pub recording_mbid: Option<String>,
    pub release_track_mbid: Option<String>,
    pub decision_source: DecisionSource,
    pub row_revision: u64,
}

/// What an automatic exact-edition seal replaced: the album and track
/// identity rows before it, and the album identity revision it wrote. An
/// administrator can put these back while nothing else touched the album.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EditionUndo {
    pub local_album_id: String,
    pub prior: Option<AlbumIdentity>,
    pub prior_tracks: Vec<TrackIdentity>,
    /// Album identity revision the seal wrote.
    pub identity_revision: u64,
}

/// The durable artist identity row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtistIdentity {
    pub local_artist_id: String,
    pub provider: String,
    pub provider_artist_mbid: Option<String>,
    pub decision_source: DecisionSource,
    pub row_revision: u64,
}

/// A stored album-level credit proof row (v2 `library_artist_credit_proofs`).
/// Durable only while its identity revisions still match the live rows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreditProof {
    pub local_album_id: String,
    pub local_track_id: String,
    pub source_local_artist_id: String,
    pub artist_mbid: String,
    pub release_mbid: String,
    pub album_identity_revision: u64,
    pub track_identity_revision: u64,
}

/// A retired local id that keeps resolving to its survivor, so favorites,
/// playlists, play history, and compat references never break.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Alias {
    pub retired_id: String,
    pub surviving_id: String,
    pub kind: AliasKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(clippy::enum_variant_names)]
pub enum AliasKind {
    MergedAlbum,
    MergedArtist,
    MergedTrack,
}

/// A curator release pin: which edition to display and acquire. Hint-only:
/// it orders editions within one release group when identifying, and is
/// never identity evidence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReleasePin {
    pub release_group_mbid: String,
    pub release_mbid: String,
}

/// A provider artist credit on one track.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtistCredit {
    pub position: u32,
    pub artist_mbid: String,
    pub canonical_name: String,
    pub credited_name: String,
}

/// Exact track contributors become appearances on the track, never newly
/// owned artists in the catalog.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Appearance {
    pub local_track_id: String,
    pub artist_mbid: String,
    pub credited_name: String,
    pub position: u32,
}

/// One durable identification queue job.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IdentifyJob {
    pub id: String,
    pub local_album_id: String,
    pub kind: IdentifyKind,
    pub priority: u32,
    pub state: JobState,
    pub attempts: u32,
    pub not_before_ms: u64,
    pub input_revision: String,
    pub requested_by_user_id: Option<String>,
    pub failure_code: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentifyKind {
    Automatic,
    Manual,
    Historical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Deferred,
    Succeeded,
    Failed,
    Attention,
}

/// One ambiguous case waiting on a curator.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReviewItem {
    pub id: String,
    pub local_album_id: String,
    pub reason_code: String,
    pub candidates: Vec<CandidateEvidence>,
    pub state: ReviewState,
    pub resolved_by_user_id: Option<String>,
    pub selected_candidate_key: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewState {
    Pending,
    Approved,
    Rejected,
}

/// Identity summary: the whole case for one album on one card.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IdentityBrief {
    pub local_album_id: String,
    pub identity: Option<AlbumIdentity>,
    pub outcome: Option<IdentificationOutcome>,
    pub reason_code: String,
    pub revisable: bool,
    pub protected_source: Option<DecisionSource>,
    pub candidates: Vec<CandidateEvidence>,
    pub pending_review_id: Option<String>,
    pub aliases: Vec<Alias>,
}

impl IdentityBrief {
    pub fn unresolved(local_album_id: &str, reason_code: &str) -> Self {
        Self {
            local_album_id: local_album_id.to_owned(),
            identity: None,
            outcome: None,
            reason_code: reason_code.to_owned(),
            revisable: true,
            protected_source: None,
            candidates: Vec::new(),
            pending_review_id: None,
            aliases: Vec::new(),
        }
    }
}

/// Local album facts the matcher reads: tags plus embedded provider ids.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LocalAlbumFacts {
    pub local_album_id: String,
    pub title: String,
    pub album_artist_name: String,
    pub year: Option<i32>,
    pub tracks: Vec<LocalTrackFacts>,
    /// Track ids whose membership is locked to another album.
    pub locked_track_ids: Vec<String>,
    /// Compilation flag from tags; composite credits need care.
    pub is_compilation: bool,
}

/// Local track facts the matcher reads.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LocalTrackFacts {
    pub local_track_id: String,
    pub title: String,
    pub artist_name: String,
    pub track_number: u32,
    pub disc_number: u32,
    pub duration_secs: Option<u64>,
    pub recording_mbid: Option<String>,
    pub release_track_mbid: Option<String>,
    pub release_mbid: Option<String>,
    pub release_group_mbid: Option<String>,
    /// Where the file lives, for fingerprinting.
    #[serde(default)]
    pub root_id: String,
    #[serde(default)]
    pub relative_path: String,
    /// The file's size and mtime revision, which keys stored prints.
    #[serde(default)]
    pub stat_revision: String,
}

/// Provider recall result: candidate releases with their tracklists,
/// plus what fingerprints and redirects added.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RecallResult {
    pub releases: Vec<Release>,
    /// AcoustID recording MBIDs per local track id.
    pub fingerprint_support: HashMap<String, Vec<String>>,
    /// Retired recording MBIDs mapped to the ones MusicBrainz merged
    /// them into.
    pub recording_aliases: HashMap<String, String>,
    pub provider_deferred: bool,
    pub failure_code: Option<String>,
}
