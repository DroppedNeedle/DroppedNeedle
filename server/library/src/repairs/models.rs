//! Repair job types: what a recheck is for, what it found per album, and
//! the report it leaves for the administrator.

use std::collections::BTreeMap;

use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use serde::{Deserialize, Serialize};

use crate::operations::models::ReidentificationCandidate;

/// One SQL conversion for a closed set stored as text: the value is its
/// `as_str`, and text outside the set fails decoding.
macro_rules! sql_text {
    ($type:ty) => {
        impl ToSql for $type {
            fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
                Ok(ToSqlOutput::from(self.as_str()))
            }
        }

        impl FromSql for $type {
            fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
                let text = value.as_str()?;
                Self::parse(text).ok_or_else(|| {
                    FromSqlError::Other(
                        format!(concat!("not a ", stringify!($type), ": {}"), text).into(),
                    )
                })
            }
        }
    };
}

/// A closed set of stored names: `as_str`, `parse` and the SQL conversion.
macro_rules! named {
    ($(#[$meta:meta])* $type:ident { $($(#[$vmeta:meta])* $variant:ident => $name:literal,)+ }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum $type {
            $($(#[$vmeta])* $variant,)+
        }

        impl $type {
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $name,)+
                }
            }

            pub fn parse(raw: &str) -> Option<Self> {
                match raw {
                    $($name => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }

        sql_text!($type);
    };
}

named! {
    /// What a repair job checks, in `library_repair_snapshots.purpose`
    /// with v2's names.
    #[derive(Serialize, Deserialize, utoipa::ToSchema)]
    #[serde(rename_all = "snake_case")]
    Purpose {
        /// Identity repair: recheck existing automatic and imported
        /// matches against MusicBrainz and offer to detach the ones that
        /// fail.
        ExistingMatches => "existing_matches",
        /// Identity preparation: make sure every album Library Management
        /// may touch has an exact release with each file placed on it.
        ManagementReadiness => "management_readiness",
        /// Catalog cleanup after an upgrade: retire empty album shells and
        /// fold albums an imported identity split off back together.
        CatalogIdentityHygiene => "catalog_identity_hygiene",
        /// File plays that name no track under the track they were plays
        /// of.
        PlayHistoryFiling => "play_history_filing",
    }
}

impl Purpose {
    /// The finding Apply acts on.
    pub fn apply_finding(self) -> FindingCode {
        match self {
            Self::ManagementReadiness => FindingCode::MappingReady,
            _ => FindingCode::SafeDetach,
        }
    }
}

named! {
    /// One album's verdict in a repair report.
    #[derive(Serialize, utoipa::ToSchema)]
    #[serde(rename_all = "snake_case")]
    FindingCode {
        /// The match holds.
        Valid => "valid",
        /// The match fails; Apply removes it.
        SafeDetach => "safe_detach",
        /// A person's match: reported, never removed.
        ManualIdentity => "manual_identity",
        NeedsReview => "needs_review",
        Unverifiable => "unverifiable",
        /// The album changed or went before it was checked.
        Stale => "stale",
        /// Every file is placed on its exact release.
        Ready => "ready",
        /// Apply can place every file on the release.
        MappingReady => "mapping_ready",
        ExactReleaseRequired => "exact_release_required",
        /// Apply can choose the suggested edition.
        ExactReleaseSuggested => "exact_release_suggested",
        /// The check chose the suggested edition by itself.
        ExactReleaseAutoAccepted => "exact_release_auto_accepted",
    }
}

named! {
    /// Where a finding is.
    #[derive(Serialize, utoipa::ToSchema)]
    #[serde(rename_all = "snake_case")]
    FindingState {
        Open => "open",
        Applied => "applied",
        Skipped => "skipped",
        Stale => "stale",
    }
}

named! {
    /// What Apply did with a finding.
    #[derive(Serialize, utoipa::ToSchema)]
    #[serde(rename_all = "SCREAMING_SNAKE_CASE")]
    ApplyResult {
        Detached => "DETACHED",
        MappingsAccepted => "MAPPINGS_ACCEPTED",
        EditionAccepted => "EDITION_ACCEPTED",
        EditionAutoAccepted => "EDITION_AUTO_ACCEPTED",
        /// The album changed since it was checked.
        StaleSubject => "STALE_SUBJECT",
    }
}

named! {
    /// How far a finding can be trusted, in the finding's `confidence`.
    #[derive(Serialize, utoipa::ToSchema)]
    #[serde(rename_all = "snake_case")]
    Confidence {
        /// Apply may act on it.
        Complete => "complete",
        Bounded => "bounded",
    }
}

named! {
    /// Phase a repair job is in: checking or applying.
    Phase {
        DryRun => "dry_run",
        Apply => "apply",
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
    pub finding_code: FindingCode,
    pub reason_code: &'static str,
    pub apply_eligible: bool,
    pub payload: FindingPayload,
}

impl Verdict {
    pub fn new(finding_code: FindingCode, reason_code: &'static str) -> Self {
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
#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
pub struct FindingRow {
    pub id: String,
    pub local_album_id: String,
    pub album_title: String,
    pub album_artist_name: Option<String>,
    pub album_year: Option<i64>,
    pub cover_available: bool,
    pub review_id: Option<String>,
    pub finding_code: FindingCode,
    pub reason_code: String,
    pub confidence: Confidence,
    pub apply_eligible: bool,
    pub state: FindingState,
    pub apply_result: Option<ApplyResult>,
    #[serde(skip)]
    pub suggested_release_mbid: Option<String>,
    #[serde(skip)]
    pub suggested_release_group_mbid: Option<String>,
    #[serde(skip)]
    pub payload: FindingPayload,
    /// The album and identity revisions an undo of its automatic edition
    /// echoes.
    #[serde(skip)]
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

/// What a finished check found.
// Kept in the snapshot's `result_json`. A count missing from an older
// summary reads as zero; a summary without a known purpose does not read,
// like any unreadable summary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(as = RepairSummaryView)]
pub struct Summary {
    /// Albums checked.
    #[serde(default)]
    #[schema(required = true)]
    pub total_identities: i64,
    #[serde(default)]
    #[schema(required = true)]
    pub remaining_identities: i64,
    #[serde(default)]
    #[schema(required = true)]
    pub input_track_count: i64,
    #[serde(default)]
    #[schema(required = true)]
    pub playable_after_detach_track_count: i64,
    /// Albums Apply would change.
    #[serde(default)]
    #[schema(required = true)]
    pub estimated_apply_changes: i64,
    #[serde(default)]
    #[schema(required = true)]
    pub catalog_snapshot_revision: i64,
    #[serde(default)]
    #[schema(required = true)]
    pub target_matcher_version: String,
    #[serde(default)]
    #[schema(required = true)]
    pub counts_by_finding: BTreeMap<String, i64>,
    #[serde(default)]
    #[schema(required = true)]
    pub counts_by_reason: BTreeMap<String, i64>,
    #[serde(default)]
    #[schema(required = true)]
    pub album_counts_by_root: BTreeMap<String, i64>,
    #[serde(default)]
    #[schema(required = true)]
    pub provider_deferred_count: i64,
    #[serde(default)]
    #[schema(required = true)]
    pub failed_evidence_count: i64,
    pub purpose: Purpose,
    #[serde(default)]
    #[schema(required = true)]
    pub ready_album_count: i64,
    #[serde(default)]
    #[schema(required = true)]
    pub mapping_candidate_count: i64,
    #[serde(default)]
    #[schema(required = true)]
    pub exact_release_required_count: i64,
    #[serde(default)]
    #[schema(required = true)]
    pub needs_review_count: i64,
}

impl Summary {
    /// An empty report for a job of `purpose`.
    pub fn new(purpose: Purpose, target_matcher_version: String) -> Self {
        Self {
            total_identities: 0,
            remaining_identities: 0,
            input_track_count: 0,
            playable_after_detach_track_count: 0,
            estimated_apply_changes: 0,
            catalog_snapshot_revision: 0,
            target_matcher_version,
            counts_by_finding: BTreeMap::new(),
            counts_by_reason: BTreeMap::new(),
            album_counts_by_root: BTreeMap::new(),
            provider_deferred_count: 0,
            failed_evidence_count: 0,
            purpose,
            ready_album_count: 0,
            mapping_candidate_count: 0,
            exact_release_required_count: 0,
            needs_review_count: 0,
        }
    }
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
