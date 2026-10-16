//! Repair job types: what a recheck is for, what it found per album, and
//! the report it leaves for the administrator.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::operations::models::ReidentificationCandidate;

/// What a repair job checks. Stored in `library_repair_snapshots.scope_json`
/// under `purpose`, with v2's names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Purpose {
    /// Identity repair: recheck existing automatic and imported matches
    /// against MusicBrainz and offer to detach the ones that fail.
    ExistingMatches,
    /// Identity preparation: make sure every album Library Management may
    /// touch has an exact release with each file placed on it.
    ManagementReadiness,
    /// Catalog cleanup after an upgrade: retire empty album shells and
    /// fold albums an imported identity split off back together.
    CatalogIdentityHygiene,
    /// File plays that name no track under the track they were plays of.
    PlayHistoryFiling,
}

impl Purpose {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ExistingMatches => "existing_matches",
            Self::ManagementReadiness => "management_readiness",
            Self::CatalogIdentityHygiene => "catalog_identity_hygiene",
            Self::PlayHistoryFiling => "play_history_filing",
        }
    }

    /// v2 treated a snapshot without a purpose as an identity repair.
    pub fn parse(raw: Option<&str>) -> Option<Self> {
        match raw.unwrap_or("existing_matches") {
            "existing_matches" => Some(Self::ExistingMatches),
            "management_readiness" => Some(Self::ManagementReadiness),
            "catalog_identity_hygiene" => Some(Self::CatalogIdentityHygiene),
            "play_history_filing" => Some(Self::PlayHistoryFiling),
            _ => None,
        }
    }

    /// The finding Apply acts on.
    pub fn apply_finding(self) -> &'static str {
        match self {
            Self::ManagementReadiness => "mapping_ready",
            _ => "safe_detach",
        }
    }
}

/// Matcher version a repair records when the caller names none (v2's).
pub const REPAIR_TARGET_VERSION: &str = "quiet-reconfirm-v1";
/// Version of the readiness rules; a report made under older rules must be
/// refreshed before it is applied.
pub const MANAGEMENT_MAPPING_VERSION: &str = "management-edition-readiness-v6";
/// Version of the catalog cleanup rules.
pub const HYGIENE_VERSION: &str = "catalog-identity-hygiene-v1";

/// The scope a job was created with, as kept in `scope_json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Scope {
    #[serde(default)]
    pub root_ids: Vec<String>,
    #[serde(default)]
    pub album_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    #[serde(default)]
    pub legacy_only: bool,
    /// Cleanup after one scan: only albums that run changed, plus empty
    /// shells.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scan_run_id: Option<String>,
}

/// The album as it was when it was checked. Apply refuses to act on an
/// album that has moved on since.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Guard {
    pub album_revision: i64,
    pub input_revision: String,
    pub identity_revision: String,
}

/// One file placed on a track of the album's release.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Mapping {
    pub local_track_id: String,
    pub recording_mbid: String,
    pub release_track_mbid: String,
    pub medium_position: u32,
    pub release_track_position: u32,
}

/// A suggested exact edition, as the readiness report shows it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EditionSummary {
    pub title: String,
    pub date: Option<String>,
    pub country: Option<String>,
    pub status: Option<String>,
    pub track_count: u32,
    /// How many editions fit the files; more than one means a choice.
    pub competing_count: u32,
    /// With automatic edition acceptance on: `AUTO_ACCEPT`, or why the
    /// suggestion waits for a person (`BELOW_MIN_SCORE`, `TIE`,
    /// `MARGIN_TOO_NARROW`, `NON_QUALIFYING_REASON`).
    #[serde(default)]
    pub auto_gate: Option<String>,
}

/// What Apply needs to repeat a finding's change, kept in the finding's
/// `suggested_edition_json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FindingPayload {
    pub guard: Option<Guard>,
    /// For `mapping_ready`: the release and where each file lands.
    pub release_mbid: Option<String>,
    pub mapping: Vec<Mapping>,
    /// For `exact_release_suggested`: the edition and how it scored.
    pub summary: Option<EditionSummary>,
    pub candidate: Option<ReidentificationCandidate>,
    /// The suggestion passed the automatic acceptance gate: the check
    /// applies it at once, as an unconfirmed automatic edition.
    pub auto_accept: bool,
}

/// One album's verdict, before it is stored.
#[derive(Debug, Clone, PartialEq)]
pub struct Verdict {
    pub finding_code: &'static str,
    pub reason_code: &'static str,
    pub apply_eligible: bool,
    pub payload: FindingPayload,
}

impl Verdict {
    pub fn new(finding_code: &'static str, reason_code: &'static str) -> Self {
        Self {
            finding_code,
            reason_code,
            apply_eligible: false,
            payload: FindingPayload::default(),
        }
    }

    pub fn eligible(mut self) -> Self {
        self.apply_eligible = true;
        self
    }

    pub fn with(mut self, payload: FindingPayload) -> Self {
        self.payload = payload;
        self
    }
}

/// One stored finding with the album it is about.
#[derive(Debug, Clone, PartialEq)]
pub struct FindingRow {
    pub id: String,
    pub local_album_id: String,
    pub album_title: String,
    pub album_artist_name: Option<String>,
    pub album_year: Option<i64>,
    pub cover_available: bool,
    pub review_id: Option<String>,
    pub finding_code: String,
    pub reason_code: String,
    pub confidence: String,
    pub apply_eligible: bool,
    pub state: String,
    pub apply_result: Option<String>,
    pub suggested_release_mbid: Option<String>,
    pub suggested_release_group_mbid: Option<String>,
    pub payload: FindingPayload,
    pub automatic_undo: Option<(i64, i64)>,
    pub updated_at: f64,
    pub row_revision: i64,
}

/// One page of findings.
#[derive(Debug, Clone, Default)]
pub struct FindingPage {
    pub items: Vec<FindingRow>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
    pub current_counts_by_finding: BTreeMap<String, i64>,
    pub refresh_required: bool,
}

/// What a finished check found, kept in the snapshot's `result_json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Summary {
    pub total_identities: i64,
    pub remaining_identities: i64,
    pub input_track_count: i64,
    pub playable_after_detach_track_count: i64,
    pub estimated_apply_changes: i64,
    pub catalog_snapshot_revision: i64,
    pub target_matcher_version: String,
    pub counts_by_finding: BTreeMap<String, i64>,
    pub counts_by_reason: BTreeMap<String, i64>,
    pub album_counts_by_root: BTreeMap<String, i64>,
    pub provider_deferred_count: i64,
    pub failed_evidence_count: i64,
    pub purpose: String,
    pub ready_album_count: i64,
    pub mapping_candidate_count: i64,
    pub exact_release_required_count: i64,
    pub needs_review_count: i64,
}

/// Start a repair or a preparation.
#[derive(Debug, Clone)]
pub struct NewRepair {
    pub job_id: String,
    pub purpose: Purpose,
    pub requested_by_user_id: Option<String>,
    pub idempotency_key: String,
    pub root_ids: Vec<String>,
    pub album_ids: Vec<String>,
    /// Hygiene after this scan run only.
    pub scan_run_id: Option<String>,
    /// Only identities this matcher version wrote (v2's filter).
    pub source_matcher_version: Option<String>,
    pub target_matcher_version: String,
}

/// How many albums an identity repair would recheck.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RepairEstimate {
    pub identity_count: i64,
    pub queued_repair_count: i64,
}

/// Where the albums Library Management may touch stand today.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PreparationEstimate {
    pub album_count: i64,
    pub ready_album_count: i64,
    pub mapping_required_count: i64,
    pub exact_release_required_count: i64,
    pub queued_preparation_count: i64,
}
