//! Artist reconciliation types.

use std::collections::BTreeMap;

use super::reasons::Reason;

/// What kind of duplicate group this is. The wire values match v2's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum GroupState {
    WaitingForIdentity,
    ProviderConflict,
    AmbiguousCreditStructure,
    SameNameOnly,
    ResolvedAutomatically,
}

impl GroupState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WaitingForIdentity => "waiting_for_identity",
            Self::ProviderConflict => "provider_conflict",
            Self::AmbiguousCreditStructure => "ambiguous_credit_structure",
            Self::SameNameOnly => "same_name_only",
            Self::ResolvedAutomatically => "resolved_automatically",
        }
    }

    /// True for the states that need an administrator's judgement.
    pub fn needs_review(self) -> bool {
        matches!(
            self,
            Self::ProviderConflict | Self::AmbiguousCreditStructure | Self::SameNameOnly
        )
    }
}

/// How often an artist record is referenced. Credits count only albums
/// that still have indexed files.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReferenceCounts {
    pub album_credits: i64,
    pub track_credits: i64,
    pub primary_albums: i64,
    pub favorites: i64,
    pub playlist_snapshots: i64,
    pub history: i64,
    pub compatibility_ids: i64,
    /// Credits backed by a MusicBrainz proof at the current identity.
    pub proven_credits: i64,
}

impl ReferenceCounts {
    pub fn active_credits(&self) -> i64 {
        self.album_credits + self.track_credits
    }

    /// Everything a merge of this record would move.
    pub fn affected(&self) -> i64 {
        self.active_credits()
            + self.favorites
            + self.playlist_snapshots
            + self.history
            + self.compatibility_ids
    }
}

/// One artist record in a group.
#[derive(Debug, Clone, PartialEq)]
pub struct Member {
    pub id: String,
    pub name: String,
    pub sort_name: Option<String>,
    pub row_revision: i64,
    pub provider_mbid: Option<String>,
    pub counts: ReferenceCounts,
}

/// A current artist record that shares its folded name with another, with
/// the MusicBrainz artists its proven credits point at.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub member: Member,
    pub folded_name: String,
    pub created_at: f64,
    pub proof_mbids: Vec<String>,
}

/// A recorded "these are distinct" decision for one pair, at the revisions
/// the administrator saw.
#[derive(Debug, Clone)]
pub struct Dismissal {
    pub left_id: String,
    pub right_id: String,
    pub left_revision: i64,
    pub right_revision: i64,
}

/// An automatic merge from the catalog action log.
#[derive(Debug, Clone)]
pub struct MergeAction {
    pub id: String,
    pub reason_code: String,
    pub created_at: f64,
    pub survivor_id: String,
    pub retired_ids: Vec<String>,
    pub provider_mbid: Option<String>,
}

/// Everything the grouping rules read, loaded in one pass.
#[derive(Debug, Clone, Default)]
pub struct GroupInputs {
    pub candidates: Vec<Candidate>,
    pub dismissals: Vec<Dismissal>,
    /// Album artists of albums whose credit could not be split cleanly.
    pub ambiguous_artist_ids: Vec<String>,
    pub merges: Vec<MergeAction>,
    /// Members of past merges, by id.
    pub merged_artists: BTreeMap<String, Member>,
    /// Every reference a merged member has now, by id.
    pub merged_reference_totals: BTreeMap<String, i64>,
}

/// One duplicate group.
#[derive(Debug, Clone)]
pub struct Group {
    pub id: String,
    pub display_name: String,
    pub state: GroupState,
    pub members: Vec<Member>,
    pub provider_mbids: Vec<String>,
    pub recommended_survivor_id: Option<String>,
    pub affected_reference_count: i64,
    pub reason_code: String,
    pub reason: Reason,
    pub resolved_at: Option<f64>,
}

/// A page of groups with the per-state counts over every group.
#[derive(Debug, Clone)]
pub struct GroupPage {
    pub items: Vec<Group>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
    pub total: usize,
    pub counts: BTreeMap<&'static str, usize>,
}

/// Where the reconciliation pass stands.
#[derive(Debug, Clone, Default)]
pub struct Progress {
    pub state: String,
    pub completed_count: i64,
    pub expected_count: i64,
    pub automatically_resolved_count: i64,
    pub waiting_for_identity_count: usize,
    pub genuine_review_count: usize,
    pub provider_conflict_count: usize,
    pub ambiguous_credit_structure_count: usize,
    pub same_name_only_count: usize,
    pub operation_job_id: Option<String>,
}

/// The newest reconciliation job, if any.
#[derive(Debug, Clone)]
pub struct ReconcileJob {
    pub id: String,
    pub state: String,
    pub completed_count: i64,
    pub expected_count: i64,
}

/// A MusicBrainz credit proof behind a member's credit.
#[derive(Debug, Clone)]
pub struct CreditEvidence {
    pub subject_kind: String,
    pub subject_id: String,
    pub subject_name: String,
    pub source_local_artist_id: Option<String>,
    pub local_artist_id: String,
    pub artist_mbid: String,
    pub canonical_name: String,
    pub credited_name: String,
    pub join_phrase: String,
    pub release_mbid: String,
    pub release_track_mbid: Option<String>,
    pub album_identity_revision: i64,
    pub track_identity_revision: Option<i64>,
    pub evidence_hash: String,
}

/// An album or track a group's members are credited on.
#[derive(Debug, Clone)]
pub struct OwnedReference {
    pub id: String,
    pub name: String,
    pub row_revision: i64,
    pub identity_ready: bool,
    pub exact_track_mapping_ready: bool,
}

/// What the members are credited on, for the detail view.
#[derive(Debug, Clone, Default)]
pub struct GroupReferences {
    pub evidence: Vec<CreditEvidence>,
    pub releases: Vec<OwnedReference>,
    pub tracks: Vec<OwnedReference>,
}

/// One group with its evidence and references.
#[derive(Debug, Clone)]
pub struct GroupDetail {
    pub group: Group,
    pub references: GroupReferences,
}

/// Why a reconciliation request failed.
#[derive(Debug)]
pub enum ReconcileError {
    NotFound(Reason),
    Invalid(Reason),
    Conflict(Reason),
    /// A store fault; the cause is for the log.
    Store(String),
}

impl std::fmt::Display for ReconcileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(reason) | Self::Invalid(reason) | Self::Conflict(reason) => {
                write!(f, "{}: {}", reason.code, reason.message)
            }
            Self::Store(cause) => write!(f, "{cause}"),
        }
    }
}

impl std::error::Error for ReconcileError {}

impl From<rusqlite::Error> for ReconcileError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Store(error.to_string())
    }
}
