//! Undo and baseline restore planning.
//!
//! Undo is a new previewed operation over the exact immediate before
//! state, never an in-place rewind. Planning pages the source
//! operation's successful per-file snapshots and compares the current
//! file to the operation's exact published fingerprint, root, and
//! relative path. A later scan, edit, move, replacement, identity or
//! override change, expired snapshot, missing root, or newly occupied
//! restore path makes that file stale, and the whole album bundle
//! stays non-writable while any sibling is stale. Undo never rolls
//! back later external edits: a file whose bytes no longer match the
//! published fingerprint is left alone.
//!
//! Baseline restore is selected restoration of the immutable
//! pre-DroppedNeedle state, and purge is a separate global
//! retention removal behind its own confirmation contract.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::PublishError;
use super::planner::{FileFingerprint, ReleaseIdentity};
use super::tags_seam::TagDocument;

/// Exact before state captured by a successful publisher call: the
/// semantic audio snapshot, the prior management state, and the
/// source location. Serialized into content-addressed snapshot blobs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeforeState {
    /// Semantic tag document before the operation.
    pub doc: TagDocument,
    /// Source root and relative path before the operation.
    pub source_root: String,
    pub source_rel: String,
    /// Source bytes before the operation.
    pub source_sha256: String,
    /// Catalog management state before the operation (`None` when the
    /// track had no prior state, restoring as `undone`).
    pub mgmt_state_before: Option<String>,
}

impl BeforeState {
    /// Serialize for snapshot blobs.
    pub fn to_bytes(&self) -> Result<Vec<u8>, PublishError> {
        serde_json::to_vec(self).map_err(PublishError::from)
    }

    /// Parse back from a snapshot blob.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, PublishError> {
        serde_json::from_slice(bytes).map_err(PublishError::from)
    }
}

/// One file's undo evidence: the before state plus the published
/// facts the current file must still match.
#[derive(Debug, Clone)]
pub struct UndoInput {
    /// Stable local track id.
    pub track_id: String,
    /// Captured before state.
    pub before: BeforeState,
    /// Fingerprint the operation published.
    pub published: FileFingerprint,
    /// Root and relative path the operation published to.
    pub published_root: String,
    pub published_rel: String,
    /// Identity pinned by the source operation.
    pub identity: ReleaseIdentity,
    /// Override revision pinned by the source operation.
    pub override_revision: u64,
    /// Snapshot expiry day.
    pub expires_day: i64,
}

/// Live state undo planning checks against.
#[derive(Debug, Clone)]
pub struct UndoLive {
    /// Current fingerprints by track id.
    pub fingerprints: BTreeMap<String, FileFingerprint>,
    /// Current catalog locations by track id: (root, rel path).
    pub locations: BTreeMap<String, (String, String)>,
    /// Current accepted identities by track id.
    pub identities: BTreeMap<String, ReleaseIdentity>,
    /// Current override revisions by track id.
    pub overrides: BTreeMap<String, u64>,
    /// Today as a unix day.
    pub today_day: i64,
}

/// Why one file cannot be undone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UndoBlock {
    /// Current bytes no longer match the published fingerprint: a
    /// later edit, replacement, or external change owns the file now.
    ExternallyChanged,
    /// The file moved since the operation.
    Moved,
    /// Identity changed since the operation.
    IdentityChanged,
    /// Override changed since the operation.
    OverrideChanged,
    /// The operation snapshot expired.
    SnapshotExpired,
    /// The restore path is newly occupied.
    RestoreOccupied,
}

/// One eligible undo file.
#[derive(Debug, Clone)]
pub struct UndoItem {
    /// Stable local track id.
    pub track_id: String,
    /// Restore target: the exact prior root and relative path.
    pub restore_root: String,
    pub restore_rel: String,
    /// Semantic document to restore.
    pub doc: TagDocument,
    /// Catalog management state to restore (`None` records `undone`
    /// with no applied profile).
    pub mgmt_state_before: Option<String>,
}

/// Undo plan over one album bundle.
#[derive(Debug, Clone)]
pub struct UndoPlan {
    /// Source operation id.
    pub source_operation: String,
    /// Eligible files.
    pub eligible: Vec<UndoItem>,
    /// Blocked files with reasons.
    pub blocked: Vec<(String, UndoBlock)>,
}

impl UndoPlan {
    /// The bundle is writable only when no sibling is stale.
    pub fn writable(&self) -> bool {
        !self.eligible.is_empty() && self.blocked.is_empty()
    }
}

/// Plan undo for one album bundle. `restore_occupied` reports whether
/// a restore path currently exists; identical occupancy still blocks,
/// because the occupier may be a later external file.
pub fn plan_undo(
    source_operation: &str,
    inputs: &[UndoInput],
    live: &UndoLive,
    restore_occupied: &dyn Fn(&str, &str) -> bool,
) -> UndoPlan {
    let mut eligible = Vec::new();
    let mut blocked = Vec::new();
    for input in inputs {
        let block = check_undo_file(input, live, restore_occupied);
        match block {
            Some(reason) => blocked.push((input.track_id.clone(), reason)),
            None => eligible.push(UndoItem {
                track_id: input.track_id.clone(),
                restore_root: input.before.source_root.clone(),
                restore_rel: input.before.source_rel.clone(),
                doc: input.before.doc.clone(),
                mgmt_state_before: input.before.mgmt_state_before.clone(),
            }),
        }
    }
    UndoPlan {
        source_operation: source_operation.to_string(),
        eligible,
        blocked,
    }
}

/// Check one file's undo eligibility.
fn check_undo_file(
    input: &UndoInput,
    live: &UndoLive,
    restore_occupied: &dyn Fn(&str, &str) -> bool,
) -> Option<UndoBlock> {
    if live.today_day > input.expires_day {
        return Some(UndoBlock::SnapshotExpired);
    }
    match live.fingerprints.get(&input.track_id) {
        Some(current) if *current == input.published => {}
        _ => return Some(UndoBlock::ExternallyChanged),
    }
    match live.locations.get(&input.track_id) {
        Some((root, rel)) if *root == input.published_root && *rel == input.published_rel => {}
        _ => return Some(UndoBlock::Moved),
    }
    match live.identities.get(&input.track_id) {
        Some(current) if *current == input.identity => {}
        _ => return Some(UndoBlock::IdentityChanged),
    }
    match live.overrides.get(&input.track_id) {
        Some(current) if *current == input.override_revision => {}
        _ => return Some(UndoBlock::OverrideChanged),
    }
    let restore_is_published = input.before.source_root == input.published_root
        && input.before.source_rel == input.published_rel;
    if !restore_is_published
        && restore_occupied(&input.before.source_root, &input.before.source_rel)
    {
        return Some(UndoBlock::RestoreOccupied);
    }
    None
}

/// One file's baseline-restore evidence.
#[derive(Debug, Clone)]
pub struct BaselineInput {
    /// Stable local track id.
    pub track_id: String,
    /// Immutable baseline before state, when one exists.
    pub baseline: Option<BeforeState>,
    /// Current file fingerprint.
    pub current: FileFingerprint,
    /// Fingerprint the restore preview pinned.
    pub pinned_current: FileFingerprint,
    /// Current accepted identity.
    pub identity: ReleaseIdentity,
    /// Identity pinned at preview time.
    pub pinned_identity: ReleaseIdentity,
    /// Current container format, lowercased.
    pub format: String,
    /// Format pinned at preview time.
    pub pinned_format: String,
}

/// Why one baseline restore is blocked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BaselineBlock {
    /// No baseline was ever captured for this track.
    MissingBaseline,
    /// The original root is gone.
    MissingRoot,
    /// The container format changed.
    FormatMismatch,
    /// The current file changed or became unreadable.
    CurrentChanged,
    /// Identity changed since preview.
    IdentityChanged,
    /// The original destination is occupied.
    OriginalOccupied,
}

/// Baseline restore plan over one album bundle.
#[derive(Debug, Clone)]
pub struct BaselineRestorePlan {
    /// Eligible files: (track id, baseline before state).
    pub eligible: Vec<(String, BeforeState)>,
    /// Blocked files with reasons.
    pub blocked: Vec<(String, BaselineBlock)>,
}

impl BaselineRestorePlan {
    /// The bundle restores only when every file is eligible.
    pub fn writable(&self) -> bool {
        !self.eligible.is_empty() && self.blocked.is_empty()
    }
}

/// Plan baseline restore for one album bundle. `original_state`
/// reports the original location: `None` when the root is gone,
/// `Some(occupied)` otherwise.
pub fn plan_baseline_restore(
    inputs: &[BaselineInput],
    original_state: &dyn Fn(&str, &str) -> Option<bool>,
) -> BaselineRestorePlan {
    let mut eligible = Vec::new();
    let mut blocked = Vec::new();
    for input in inputs {
        let Some(baseline) = input.baseline.as_ref() else {
            blocked.push((input.track_id.clone(), BaselineBlock::MissingBaseline));
            continue;
        };
        let block = match original_state(&baseline.source_root, &baseline.source_rel) {
            None => Some(BaselineBlock::MissingRoot),
            Some(true) => Some(BaselineBlock::OriginalOccupied),
            Some(false) => {
                if input.format != input.pinned_format {
                    Some(BaselineBlock::FormatMismatch)
                } else if input.current != input.pinned_current {
                    Some(BaselineBlock::CurrentChanged)
                } else if input.identity != input.pinned_identity {
                    Some(BaselineBlock::IdentityChanged)
                } else {
                    None
                }
            }
        };
        match block {
            Some(reason) => blocked.push((input.track_id.clone(), reason)),
            None => eligible.push((input.track_id.clone(), baseline.clone())),
        }
    }
    BaselineRestorePlan { eligible, blocked }
}

/// Exact confirmation phrase for baseline purge.
pub const PURGE_PHRASE: &str = "PURGE BASELINES";

/// Confirm a global baseline purge: requires the exact phrase, the
/// matching impact token, and zero nonterminal journals or active
/// restores. Returns the purgeable baseline count.
pub fn confirm_baseline_purge(
    phrase: &str,
    presented_token: &str,
    expected_token: &str,
    baseline_count: usize,
    active_journals: usize,
) -> Result<usize, PublishError> {
    if phrase != PURGE_PHRASE {
        return Err(PublishError::Validation(
            "baseline purge needs the exact phrase".into(),
        ));
    }
    if presented_token != expected_token || expected_token.is_empty() {
        return Err(PublishError::Validation(
            "baseline purge token mismatch".into(),
        ));
    }
    if active_journals > 0 {
        return Err(PublishError::Validation(
            "active journals block baseline purge".into(),
        ));
    }
    if baseline_count == 0 {
        return Err(PublishError::Validation("no baselines to purge".into()));
    }
    Ok(baseline_count)
}
