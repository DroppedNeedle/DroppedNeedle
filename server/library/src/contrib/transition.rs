//! Every change to a contribution row, in one pure function.
//!
//! [`transition`] takes the row as the store read it (with its freshness
//! join and [`AlbumLinks`]), the change asked for and the time, and answers
//! the row to write back plus the [`Effect`]s that go with it, or the
//! error that refuses the change. It reads nothing and writes nothing: the
//! store loads the row, runs it, saves the result under the revision it
//! read and runs the effects in the same transaction.

use super::error::ContribError;
use super::models::*;
use super::seams::*;

/// A curator's (or the upkeep's) change, applied under the row revision
/// the caller read.
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
        tracks: Vec<TrackEvidence>,
        /// The album's input policy revision, for the identified hook.
        policy_revision: String,
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

/// The end of one verification run.
#[derive(Debug, Clone)]
pub struct Finish {
    pub expected_row_revision: i64,
    pub expected_album_revision: i64,
    pub attempt: ContributionVerificationAttempt,
    pub outcome: VerificationOutcome,
    pub failure_code: Option<String>,
    pub identities: FinishIdentities,
    /// The album's input policy revision, for the identified hook.
    pub policy_revision: String,
}

/// One change to a contribution row.
#[derive(Debug, Clone)]
pub enum Update {
    Edit {
        expected_row_revision: i64,
        actor_user_id: String,
        update: Box<ContributionUpdate>,
    },
    /// The album moved under an open contribution.
    MarkStale {
        expected_row_revision: i64,
    },
    /// The release editor sent the curator back with a release.
    Callback {
        release_mbid: String,
    },
    /// Rebuild retires the row; the store then inserts a fresh one.
    Retire {
        expected_row_revision: i64,
        album_row_revision: i64,
        input_revision: String,
    },
    FinishVerification(Box<Finish>),
    /// Upkeep drops a seed snapshot nobody can use any more.
    ClearSeedSnapshot,
}

/// What the catalog is told after a link commits.
#[derive(Debug, Clone, PartialEq)]
pub enum CatalogScope {
    /// Exactly these identity keys.
    Identity {
        release_group_mbids: Vec<String>,
        artist_mbids: Vec<String>,
    },
    /// The whole identification prefix.
    Identification,
}

/// A catalog invalidation and identified hook, run after the commit.
#[derive(Debug, Clone, PartialEq)]
pub struct Invalidation {
    pub scope: CatalogScope,
    pub local_album_id: String,
    pub policy_revision: String,
}

/// Work that goes with a transition.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Use up the contribution's open callback tokens.
    ConsumeTokens,
    InsertToken {
        token_hash: String,
        requested_by_user_id: String,
        expires_at: f64,
    },
    /// End the contribution's queued and running verification.
    CancelJobs,
    /// Queue a verification; a queued one already there stands.
    EnqueueJob,
    InsertAttempt {
        attempt: ContributionVerificationAttempt,
        input_revision: String,
        state: &'static str,
        reason: Option<String>,
        selected_key: Option<String>,
    },
    /// Commit the release as the album's (and, when free, the artist's)
    /// MusicBrainz identity. `album_revision` is the album's revision now;
    /// the row already holds the one the link leaves.
    LinkIdentity {
        local_album_id: String,
        artist_id: String,
        album_revision: i64,
        release_mbid: String,
        release_group_mbid: String,
        artist_mbid: Option<String>,
        attempt: ContributionVerificationAttempt,
        selected_by: Option<String>,
        tracks: Vec<TrackEvidence>,
    },
    /// After the commit, outside the transaction.
    InvalidateCatalog(Invalidation),
}

fn terminal(state: ContributionState) -> bool {
    matches!(
        state,
        ContributionState::Linked | ContributionState::Cancelled | ContributionState::Stale
    )
}

fn editable(state: ContributionState) -> bool {
    matches!(
        state,
        ContributionState::Draft | ContributionState::Ready | ContributionState::NeedsReview
    )
}

fn revision(row: &ContributionRow, expected: i64, message: &str) -> Result<(), ContribError> {
    if row.row_revision != expected {
        return Err(ContribError::stale(message));
    }
    Ok(())
}

fn input_moved(row: &ContributionRow) -> bool {
    row.input_revision != row.current_input_revision
}

fn clear_duplicates(row: &mut ContributionRow) {
    row.duplicate_result = None;
    row.duplicate_checked_at = None;
    row.duplicate_input_revision = None;
}

/// The album's revision after a link: a link that gives the album its
/// first identity moves it on by one.
fn linked_album_revision(row: &ContributionRow) -> i64 {
    if row.links.identity.is_none() {
        row.current_album_row_revision + 1
    } else {
        row.current_album_row_revision
    }
}

/// Whether the artist already carries a different MusicBrainz id a person
/// chose.
fn artist_conflict(row: &ContributionRow, wanted: Option<&str>) -> bool {
    wanted.is_some_and(|wanted| {
        row.links
            .chosen_artist_mbid
            .as_deref()
            .is_some_and(|existing| !existing.eq_ignore_ascii_case(wanted))
    })
}

/// Apply `update` to `row` at `now`: the row to save and the effects to
/// run with it. A row returned at its old revision is unchanged and is
/// not saved.
pub fn transition(
    row: &ContributionRow,
    update: Update,
    now: f64,
) -> Result<(ContributionRow, Vec<Effect>), ContribError> {
    let mut next = row.clone();
    let mut effects = Vec::new();
    match update {
        Update::Edit {
            expected_row_revision,
            actor_user_id,
            update,
        } => edit(
            &mut next,
            expected_row_revision,
            actor_user_id,
            *update,
            now,
            &mut effects,
        )?,
        Update::MarkStale {
            expected_row_revision,
        } => {
            if terminal(next.state) {
                return Ok((next, effects));
            }
            revision(
                &next,
                expected_row_revision,
                "The contribution changed before it could be marked stale.",
            )?;
            next.state = ContributionState::Stale;
            next.terminal_at = Some(now);
            next.seed_snapshot_json = None;
            next.updated_at = now;
            next.row_revision += 1;
            effects.push(Effect::CancelJobs);
        }
        Update::Callback { release_mbid } => {
            if !matches!(
                next.state,
                ContributionState::Seeded | ContributionState::Verifying | ContributionState::Stale
            ) {
                return Err(ContribError::state(
                    "This contribution is not waiting for MusicBrainz.",
                ));
            }
            if next
                .result_release_mbid
                .as_deref()
                .is_some_and(|current| current != release_mbid)
            {
                return Err(ContribError::state(
                    "This contribution already has a different MusicBrainz result.",
                ));
            }
            effects.push(Effect::ConsumeTokens);
            let verifying = next.album_active && next.state != ContributionState::Stale;
            next.state = if verifying {
                ContributionState::Verifying
            } else {
                ContributionState::Stale
            };
            next.result_release_mbid = Some(release_mbid);
            next.result_source = Some("callback".into());
            next.result_received_at = Some(now);
            next.updated_at = now;
            next.row_revision += 1;
            if verifying {
                effects.push(Effect::EnqueueJob);
            }
        }
        Update::Retire {
            expected_row_revision,
            album_row_revision,
            input_revision,
        } => {
            revision(
                &next,
                expected_row_revision,
                "The contribution changed before it could be rebuilt.",
            )?;
            if !next.album_active {
                return Err(ContribError::AlbumNotFound);
            }
            if next.current_album_row_revision != album_row_revision
                || next.current_input_revision != input_revision
            {
                return Err(ContribError::stale(
                    "The album changed before the contribution could be rebuilt.",
                ));
            }
            next.state = ContributionState::Stale;
            next.terminal_at = Some(now);
            next.updated_at = now;
            next.row_revision += 1;
            effects.push(Effect::ConsumeTokens);
            effects.push(Effect::CancelJobs);
        }
        Update::FinishVerification(finish) => {
            finish_verification(&mut next, *finish, now, &mut effects)?
        }
        Update::ClearSeedSnapshot => {
            if next.seed_snapshot_json.is_some() {
                next.seed_snapshot_json = None;
                next.row_revision += 1;
            }
        }
    }
    Ok((next, effects))
}

/// A curator's change (v2's per-operation store methods).
fn edit(
    row: &mut ContributionRow,
    expected: i64,
    actor: String,
    update: ContributionUpdate,
    now: f64,
    effects: &mut Vec<Effect>,
) -> Result<(), ContribError> {
    match update {
        ContributionUpdate::Draft { draft, state } => {
            if !row.album_active {
                return Err(ContribError::ContributionNotFound);
            }
            if !editable(row.state) {
                return Err(ContribError::state(
                    "This contribution can no longer be edited.",
                ));
            }
            revision(
                row,
                expected,
                "The contribution changed before this edit was saved.",
            )?;
            // A race guard only: the service's read already marks a moved
            // album stale before it edits.
            if input_moved(row) || row.album_row_revision != row.current_album_row_revision {
                return Err(ContribError::stale(
                    "The local album changed. Rebuild the contribution before editing.",
                ));
            }
            row.draft = draft;
            row.state = state;
            clear_duplicates(row);
        }
        ContributionUpdate::SelectDiscogs {
            release: _,
            selection,
            expires_at,
        } => {
            if !row.album_active {
                return Err(ContribError::ContributionNotFound);
            }
            if !editable(row.state) {
                return Err(ContribError::state(
                    "This contribution can no longer be edited.",
                ));
            }
            revision(
                row,
                expected,
                "The contribution changed before the source was selected.",
            )?;
            if input_moved(row) {
                return Err(ContribError::stale(
                    "The local album changed. Rebuild the contribution first.",
                ));
            }
            row.source_selection = selection;
            row.provider_snapshot_expires_at = Some(expires_at);
            clear_duplicates(row);
        }
        ContributionUpdate::RemoveDiscogs { draft, state } => {
            if !row.album_active {
                return Err(ContribError::ContributionNotFound);
            }
            if !editable(row.state) {
                return Err(ContribError::state(
                    "This contribution can no longer be edited.",
                ));
            }
            revision(
                row,
                expected,
                "The contribution changed before the source was removed.",
            )?;
            row.source_selection = ContributionSourceSelection::default();
            row.draft = draft;
            row.provider_snapshot_expires_at = None;
            row.state = state;
            clear_duplicates(row);
        }
        ContributionUpdate::DuplicateResult { result, state } => {
            if !row.album_active {
                return Err(ContribError::ContributionNotFound);
            }
            if !matches!(
                row.state,
                ContributionState::Ready | ContributionState::NeedsReview
            ) {
                return Err(ContribError::state(
                    "Complete the contribution draft first.",
                ));
            }
            revision(
                row,
                expected,
                "The contribution changed before duplicate results were saved.",
            )?;
            if input_moved(row) || result.input_revision != row.input_revision {
                return Err(ContribError::stale(
                    "The local album changed before duplicate results were saved.",
                ));
            }
            row.duplicate_input_revision = Some(result.input_revision.clone());
            row.duplicate_result = Some(result);
            row.duplicate_checked_at = Some(now);
            row.state = state;
        }
        ContributionUpdate::AttachExisting {
            release_mbid,
            release_group_mbid,
            artist_mbid,
            attempt,
            tracks,
            policy_revision,
        } => {
            if !row.album_active {
                return Err(ContribError::ContributionNotFound);
            }
            if !matches!(
                row.state,
                ContributionState::Ready | ContributionState::NeedsReview
            ) {
                return Err(ContribError::state(
                    "This contribution cannot attach a release now.",
                ));
            }
            revision(
                row,
                expected,
                "The contribution changed before the release could be attached.",
            )?;
            if input_moved(row)
                || row.duplicate_input_revision.as_deref() != Some(row.input_revision.as_str())
            {
                return Err(ContribError::stale(
                    "The local album changed before the release could be attached.",
                ));
            }
            let in_result = row.duplicate_result.as_ref().is_some_and(|d| {
                d.candidates
                    .iter()
                    .any(|c| c.release_mbid.as_deref() == Some(release_mbid.as_str()))
            });
            if !in_result {
                return Err(ContribError::state(
                    "The release is not in the current duplicate-check result.",
                ));
            }
            let expected_key = format!("{release_group_mbid}:{release_mbid}");
            if attempt.selected_candidate_key.as_deref() != Some(expected_key.as_str())
                || attempt.local_album_id != row.local_album_id
            {
                return Err(ContribError::state(
                    "The verified release evidence does not match this contribution.",
                ));
            }
            if let Some((existing_release, existing_group)) = &row.links.identity
                && (existing_release.as_deref() != Some(release_mbid.as_str())
                    || *existing_group != release_group_mbid)
            {
                return Err(ContribError::state(
                    "The local album already has a different MusicBrainz identity.",
                ));
            }
            let artist_id = row
                .links
                .artist_id
                .clone()
                .ok_or(ContribError::ContributionNotFound)?;
            if artist_conflict(row, artist_mbid.as_deref()) {
                return Err(ContribError::state(
                    "The local artist already has a different MusicBrainz identity.",
                ));
            }
            effects.push(Effect::InsertAttempt {
                attempt: attempt.clone(),
                input_revision: row.input_revision.clone(),
                state: "identified",
                reason: attempt.terminal_reason_code.clone(),
                selected_key: attempt.selected_candidate_key.clone(),
            });
            let album_revision = linked_album_revision(row);
            effects.push(Effect::LinkIdentity {
                local_album_id: row.local_album_id.clone(),
                artist_id,
                album_revision: row.current_album_row_revision,
                release_mbid: release_mbid.clone(),
                release_group_mbid: release_group_mbid.clone(),
                artist_mbid: artist_mbid.clone(),
                attempt,
                selected_by: Some(actor.clone()),
                tracks,
            });
            effects.push(Effect::ConsumeTokens);
            effects.push(Effect::InvalidateCatalog(Invalidation {
                scope: CatalogScope::Identity {
                    release_group_mbids: vec![release_group_mbid],
                    artist_mbids: artist_mbid.into_iter().collect(),
                },
                local_album_id: row.local_album_id.clone(),
                policy_revision,
            }));
            row.state = ContributionState::Linked;
            row.album_row_revision = album_revision;
            row.current_album_row_revision = album_revision;
            row.result_release_mbid = Some(release_mbid);
            row.result_source = Some("manual".into());
            row.result_received_at = Some(now);
            row.terminal_at = Some(now);
        }
        ContributionUpdate::PrepareSeed {
            token_hash,
            token_expires_at,
            seed_snapshot_json,
            seed_hash,
        } => {
            if !row.album_active {
                return Err(ContribError::ContributionNotFound);
            }
            if !matches!(
                row.state,
                ContributionState::Ready | ContributionState::Seeded
            ) || row.duplicate_result.is_none()
            {
                return Err(ContribError::DuplicateCheckRequired(
                    "Run the MusicBrainz duplicate check first.".into(),
                ));
            }
            revision(
                row,
                expected,
                "The contribution changed before the editor could be opened.",
            )?;
            if input_moved(row)
                || row.duplicate_input_revision.as_deref() != Some(row.input_revision.as_str())
            {
                return Err(ContribError::stale(
                    "The local album changed before the editor could be opened.",
                ));
            }
            if row
                .duplicate_result
                .as_ref()
                .is_some_and(|d| d.candidates.iter().any(|c| c.exact))
            {
                return Err(ContribError::ExactDuplicate(
                    "An exact MusicBrainz release already exists for this Discogs source.".into(),
                ));
            }
            effects.push(Effect::ConsumeTokens);
            effects.push(Effect::InsertToken {
                token_hash,
                requested_by_user_id: actor.clone(),
                expires_at: token_expires_at,
            });
            row.state = ContributionState::Seeded;
            row.seed_snapshot_json = Some(seed_snapshot_json);
            row.seed_hash = Some(seed_hash);
            row.seeded_at = Some(now);
        }
        ContributionUpdate::ManualResult {
            release_mbid,
            replace_existing,
        } => {
            if !matches!(
                row.state,
                ContributionState::Seeded
                    | ContributionState::Verifying
                    | ContributionState::NeedsReview
                    | ContributionState::Stale
            ) {
                return Err(ContribError::state(
                    "This contribution is not waiting for a MusicBrainz result.",
                ));
            }
            revision(
                row,
                expected,
                "The contribution changed before the result was recorded.",
            )?;
            if let Some(current) = row.result_release_mbid.as_deref()
                && current != release_mbid
                && (!replace_existing
                    || !matches!(
                        row.state,
                        ContributionState::NeedsReview | ContributionState::Stale
                    ))
            {
                return Err(ContribError::state(
                    "Confirm replacement of the existing MusicBrainz result.",
                ));
            }
            effects.push(Effect::ConsumeTokens);
            effects.push(Effect::CancelJobs);
            let verifying = row.album_active && row.state != ContributionState::Stale;
            row.state = if verifying {
                ContributionState::Verifying
            } else {
                ContributionState::Stale
            };
            row.result_release_mbid = Some(release_mbid);
            row.result_source = Some("manual".into());
            row.result_received_at = Some(now);
            row.terminal_at = None;
            if verifying {
                effects.push(Effect::EnqueueJob);
            }
        }
        ContributionUpdate::RequeueVerification => {
            if !row.album_active {
                return Err(ContribError::ContributionNotFound);
            }
            if !matches!(
                row.state,
                ContributionState::Verifying | ContributionState::NeedsReview
            ) {
                return Err(ContribError::state(
                    "This contribution cannot be verified now.",
                ));
            }
            if row.result_release_mbid.is_none() {
                return Err(ContribError::state(
                    "No MusicBrainz result is ready to verify.",
                ));
            }
            revision(
                row,
                expected,
                "The contribution changed before verification was retried.",
            )?;
            effects.push(Effect::EnqueueJob);
            row.state = ContributionState::Verifying;
            row.terminal_at = None;
        }
        ContributionUpdate::Cancel => {
            if !row.album_active {
                return Err(ContribError::ContributionNotFound);
            }
            if terminal(row.state) {
                return Err(ContribError::state("This contribution is already closed."));
            }
            revision(
                row,
                expected,
                "The contribution changed before it could be cancelled.",
            )?;
            effects.push(Effect::ConsumeTokens);
            effects.push(Effect::CancelJobs);
            row.state = ContributionState::Cancelled;
            row.terminal_at = Some(now);
            row.seed_snapshot_json = None;
        }
        ContributionUpdate::PurgeProviderData { draft, selection } => {
            if row.provider_snapshot_expires_at.is_none() || row.row_revision != expected {
                return Err(ContribError::stale(
                    "The contribution changed before cleanup.",
                ));
            }
            row.draft = draft;
            row.source_selection = selection;
            row.provider_snapshot_expires_at = None;
            clear_duplicates(row);
            row.seed_snapshot_json = None;
            row.updated_at = now;
            row.row_revision += 1;
            // Cleanup is not an edit: the last editor stays recorded.
            return Ok(());
        }
    }
    row.updated_by_user_id = Some(actor);
    row.updated_at = now;
    row.row_revision += 1;
    Ok(())
}

/// End a verification: stale when the album moved under it, linked when
/// the evidence holds and nothing contradicts it, review otherwise.
fn finish_verification(
    row: &mut ContributionRow,
    finish: Finish,
    now: f64,
    effects: &mut Vec<Effect>,
) -> Result<(), ContribError> {
    let Finish {
        expected_row_revision,
        expected_album_revision,
        attempt,
        outcome,
        failure_code,
        identities,
        policy_revision,
    } = finish;
    if !row.album_active
        || input_moved(row)
        || row.album_row_revision != row.current_album_row_revision
    {
        row.state = ContributionState::Stale;
        row.terminal_at = Some(now);
        row.updated_at = now;
        row.row_revision += 1;
        row.last_verification_failure = Some("LOCAL_INPUT_CHANGED".to_owned());
        return Ok(());
    }
    revision(
        row,
        expected_row_revision,
        "The contribution changed before verification completed.",
    )?;
    if row.state != ContributionState::Verifying {
        return Err(ContribError::state(
            "This contribution is not being verified.",
        ));
    }
    if attempt.local_album_id != row.local_album_id {
        return Err(ContribError::state(
            "The verification subject does not match.",
        ));
    }
    let artist_id = row
        .links
        .artist_id
        .clone()
        .ok_or(ContribError::AlbumNotFound)?;
    if row.current_album_row_revision != expected_album_revision {
        return Err(ContribError::stale(
            "The album changed before verification completed.",
        ));
    }
    let failure = match outcome {
        VerificationOutcome::Linked => {
            let selected = attempt.selected_candidate_key.is_some()
                && identities.release_mbid.is_some()
                && identities.release_group_mbid.is_some();
            if !selected {
                Some("VERIFICATION_EVIDENCE_MISSING".to_owned())
            } else if identities.release_mbid != row.result_release_mbid {
                Some(FAILURE_RETURNED_RELEASE_MISMATCH.to_owned())
            } else if row.links.identity.as_ref().is_some_and(|(release, group)| {
                release != &identities.release_mbid
                    || Some(group) != identities.release_group_mbid.as_ref()
            }) {
                Some("EXISTING_IDENTITY_CONFLICT".to_owned())
            } else if artist_conflict(row, identities.artist_mbid.as_deref()) {
                Some("EXISTING_ARTIST_IDENTITY_CONFLICT".to_owned())
            } else {
                None
            }
        }
        _ => failure_code,
    };
    let linked = outcome == VerificationOutcome::Linked && failure.is_none();
    effects.push(Effect::InsertAttempt {
        attempt: attempt.clone(),
        input_revision: row.input_revision.clone(),
        state: if linked { "identified" } else { "needs_review" },
        reason: failure
            .clone()
            .or_else(|| attempt.terminal_reason_code.clone()),
        selected_key: attempt.selected_candidate_key.clone().filter(|_| linked),
    });
    if linked {
        // A verified release always names both ids; one without them is
        // never written as an empty match.
        let (Some(release_mbid), Some(release_group_mbid)) = (
            identities.release_mbid.clone(),
            identities.release_group_mbid.clone(),
        ) else {
            return Err(ContribError::Data(
                "A verified release came without its MusicBrainz ids.".to_owned(),
            ));
        };
        let album_revision = linked_album_revision(row);
        effects.push(Effect::LinkIdentity {
            local_album_id: row.local_album_id.clone(),
            artist_id,
            album_revision: row.current_album_row_revision,
            release_mbid,
            release_group_mbid,
            artist_mbid: identities.artist_mbid,
            selected_by: attempt.requested_by_user_id.clone(),
            attempt,
            tracks: identities.tracks,
        });
        effects.push(Effect::InvalidateCatalog(Invalidation {
            scope: CatalogScope::Identification,
            local_album_id: row.local_album_id.clone(),
            policy_revision,
        }));
        row.state = ContributionState::Linked;
        row.album_row_revision = album_revision;
        row.terminal_at = Some(now);
    } else {
        row.state = ContributionState::NeedsReview;
    }
    row.seed_snapshot_json = None;
    row.updated_at = now;
    row.row_revision += 1;
    row.last_verification_failure = failure;
    Ok(())
}

/// What a finished row means to the worker.
pub fn verification_outcome(row: &ContributionRow) -> VerificationOutcome {
    match row.state {
        ContributionState::Linked => VerificationOutcome::Linked,
        ContributionState::Stale => VerificationOutcome::Stale,
        _ => VerificationOutcome::NeedsReview,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ContributionState::*;

    const NOW: f64 = 1_800_000_000.0;
    const ALBUM: &str = "album-1";
    const RELEASE: &str = "release-1";
    const GROUP: &str = "group-1";
    const REVISION: i64 = 3;

    fn blank<T: serde::de::DeserializeOwned>() -> T {
        serde_json::from_str("{}").unwrap()
    }

    fn duplicates(exact: bool) -> DuplicateCheckResult {
        DuplicateCheckResult {
            schema_version: 1,
            checked_at: NOW,
            input_revision: "input-1".into(),
            candidates: vec![DuplicateCandidate {
                release_mbid: Some(RELEASE.into()),
                release_group_mbid: Some(GROUP.into()),
                title: String::new(),
                artist_name: String::new(),
                evidence_kind: DuplicateEvidenceKind::Similar,
                exact,
                differences: Vec::new(),
            }],
            different_edition_confirmed: false,
        }
    }

    /// A checked contribution on a live, unmatched album.
    fn row(state: ContributionState) -> ContributionRow {
        ContributionRow {
            id: "contribution-1".into(),
            local_album_id: ALBUM.into(),
            created_by_user_id: Some("user-1".into()),
            updated_by_user_id: Some("user-1".into()),
            state,
            album_row_revision: 7,
            input_revision: "input-1".into(),
            local_snapshot: blank(),
            draft: blank(),
            source_selection: ContributionSourceSelection::default(),
            provider_snapshot_expires_at: None,
            discogs_release_id: None,
            discogs_canonical_url: None,
            duplicate_result: Some(duplicates(false)),
            duplicate_checked_at: Some(NOW),
            duplicate_input_revision: Some("input-1".into()),
            result_release_mbid: None,
            result_source: None,
            result_received_at: None,
            seeded_at: None,
            seed_token_hash: None,
            seed_token_expires_at: None,
            seed_snapshot_json: None,
            seed_hash: None,
            terminal_at: None,
            created_at: NOW,
            updated_at: NOW,
            row_revision: REVISION,
            last_verification_failure: None,
            album_active: true,
            current_input_revision: "input-1".into(),
            current_album_row_revision: 7,
            links: AlbumLinks {
                artist_id: Some("artist-1".into()),
                identity: None,
                chosen_artist_mbid: None,
            },
        }
    }

    fn with(
        state: ContributionState,
        change: impl FnOnce(&mut ContributionRow),
    ) -> ContributionRow {
        let mut row = row(state);
        change(&mut row);
        row
    }

    fn attempt() -> ContributionVerificationAttempt {
        ContributionVerificationAttempt {
            id: "attempt-1".into(),
            local_album_id: ALBUM.into(),
            requested_by_user_id: Some("user-1".into()),
            matcher_version: "test".into(),
            state: "identified".into(),
            terminal_reason_code: None,
            selected_candidate_key: Some(format!("{GROUP}:{RELEASE}")),
            candidate_count: 1,
            candidate_keys: vec![format!("{GROUP}:{RELEASE}")],
            started_at: NOW,
            completed_at: NOW,
        }
    }

    fn edit(update: ContributionUpdate) -> Update {
        Update::Edit {
            expected_row_revision: REVISION,
            actor_user_id: "user-2".into(),
            update: Box::new(update),
        }
    }

    fn attach() -> Update {
        edit(ContributionUpdate::AttachExisting {
            release_mbid: RELEASE.into(),
            release_group_mbid: GROUP.into(),
            artist_mbid: Some("artist-mbid".into()),
            attempt: attempt(),
            tracks: Vec::new(),
            policy_revision: "policy-1".into(),
        })
    }

    fn seed() -> Update {
        edit(ContributionUpdate::PrepareSeed {
            token_hash: "hash".into(),
            token_expires_at: NOW + 60.0,
            seed_snapshot_json: "{}".into(),
            seed_hash: "seed".into(),
        })
    }

    fn manual(release: &str, replace_existing: bool) -> Update {
        edit(ContributionUpdate::ManualResult {
            release_mbid: release.into(),
            replace_existing,
        })
    }

    fn finish(outcome: VerificationOutcome) -> Update {
        Update::FinishVerification(Box::new(Finish {
            expected_row_revision: REVISION,
            expected_album_revision: 7,
            attempt: attempt(),
            outcome,
            failure_code: (outcome != VerificationOutcome::Linked)
                .then(|| "ATTACHMENT_CONTRADICTION".into()),
            identities: FinishIdentities {
                release_mbid: Some(RELEASE.into()),
                release_group_mbid: Some(GROUP.into()),
                artist_mbid: None,
                tracks: Vec::new(),
            },
            policy_revision: "policy-1".into(),
        }))
    }

    fn effect_name(effect: &Effect) -> &'static str {
        match effect {
            Effect::ConsumeTokens => "consume tokens",
            Effect::InsertToken { .. } => "insert token",
            Effect::CancelJobs => "cancel jobs",
            Effect::EnqueueJob => "enqueue job",
            Effect::InsertAttempt { .. } => "insert attempt",
            Effect::LinkIdentity { .. } => "link identity",
            Effect::InvalidateCatalog(_) => "invalidate catalog",
        }
    }

    /// The state a row moves to, whether it is saved (its revision moves)
    /// and the effects that go with it, or the error that refuses it.
    type Outcome = Result<(ContributionState, bool, Vec<&'static str>), ContribError>;

    fn moved(state: ContributionState, effects: &[&'static str]) -> Outcome {
        Ok((state, true, effects.to_vec()))
    }

    /// Each kind of change, from a state that takes it and from one that
    /// refuses it.
    #[test]
    fn transitions_follow_the_contribution_rules() {
        let verifying = |state| with(state, |r| r.result_release_mbid = Some(RELEASE.into()));
        let cases: Vec<(&str, ContributionRow, Update, Outcome)> = vec![
            (
                "an edit readies a draft",
                row(Draft),
                edit(ContributionUpdate::Draft {
                    draft: blank(),
                    state: Ready,
                }),
                moved(Ready, &[]),
            ),
            (
                "a linked contribution takes no edit",
                row(Linked),
                edit(ContributionUpdate::Draft {
                    draft: blank(),
                    state: Ready,
                }),
                Err(ContribError::state(
                    "This contribution can no longer be edited.",
                )),
            ),
            (
                "an edit read at an old revision is refused",
                row(Draft),
                Update::Edit {
                    expected_row_revision: REVISION - 1,
                    actor_user_id: "user-2".into(),
                    update: Box::new(ContributionUpdate::Draft {
                        draft: blank(),
                        state: Ready,
                    }),
                },
                Err(ContribError::stale(
                    "The contribution changed before this edit was saved.",
                )),
            ),
            (
                "selecting Discogs keeps the state",
                row(Ready),
                edit(ContributionUpdate::SelectDiscogs {
                    release: DiscogsRelease::default(),
                    selection: ContributionSourceSelection::default(),
                    expires_at: NOW + 60.0,
                }),
                moved(Ready, &[]),
            ),
            (
                "removing Discogs takes the state asked for",
                row(Ready),
                edit(ContributionUpdate::RemoveDiscogs {
                    draft: blank(),
                    state: Draft,
                }),
                moved(Draft, &[]),
            ),
            (
                "a duplicate result can force review",
                row(Ready),
                edit(ContributionUpdate::DuplicateResult {
                    result: duplicates(false),
                    state: NeedsReview,
                }),
                moved(NeedsReview, &[]),
            ),
            (
                "an incomplete draft takes no duplicate result",
                row(Draft),
                edit(ContributionUpdate::DuplicateResult {
                    result: duplicates(false),
                    state: Ready,
                }),
                Err(ContribError::state(
                    "Complete the contribution draft first.",
                )),
            ),
            (
                "attaching a checked release links it",
                row(Ready),
                attach(),
                moved(
                    Linked,
                    &[
                        "insert attempt",
                        "link identity",
                        "consume tokens",
                        "invalidate catalog",
                    ],
                ),
            ),
            (
                "an album matched elsewhere takes no attach",
                with(Ready, |r| {
                    r.links.identity = Some((Some("other".into()), "other-group".into()))
                }),
                attach(),
                Err(ContribError::state(
                    "The local album already has a different MusicBrainz identity.",
                )),
            ),
            (
                "an artist a person linked elsewhere takes no attach",
                with(Ready, |r| {
                    r.links.chosen_artist_mbid = Some("someone-else".into())
                }),
                attach(),
                Err(ContribError::state(
                    "The local artist already has a different MusicBrainz identity.",
                )),
            ),
            (
                "opening the editor issues a fresh token",
                row(Ready),
                seed(),
                moved(Seeded, &["consume tokens", "insert token"]),
            ),
            (
                "an exact duplicate keeps the editor closed",
                with(Ready, |r| r.duplicate_result = Some(duplicates(true))),
                seed(),
                Err(ContribError::ExactDuplicate(
                    "An exact MusicBrainz release already exists for this Discogs source.".into(),
                )),
            ),
            (
                "a manual result queues verification",
                row(Seeded),
                manual(RELEASE, false),
                moved(Verifying, &["consume tokens", "cancel jobs", "enqueue job"]),
            ),
            (
                "a manual result on a stale row queues nothing",
                row(Stale),
                manual(RELEASE, false),
                moved(Stale, &["consume tokens", "cancel jobs"]),
            ),
            (
                "a different result needs confirming",
                verifying(Verifying),
                manual("other", false),
                Err(ContribError::state(
                    "Confirm replacement of the existing MusicBrainz result.",
                )),
            ),
            (
                "a confirmed replacement is refused while verifying",
                verifying(Verifying),
                manual("other", true),
                Err(ContribError::state(
                    "Confirm replacement of the existing MusicBrainz result.",
                )),
            ),
            (
                "a retry queues verification again",
                verifying(NeedsReview),
                edit(ContributionUpdate::RequeueVerification),
                moved(Verifying, &["enqueue job"]),
            ),
            (
                "a retry needs a result",
                row(NeedsReview),
                edit(ContributionUpdate::RequeueVerification),
                Err(ContribError::state(
                    "No MusicBrainz result is ready to verify.",
                )),
            ),
            (
                "cancel closes and ends tokens and jobs",
                row(Ready),
                edit(ContributionUpdate::Cancel),
                moved(Cancelled, &["consume tokens", "cancel jobs"]),
            ),
            (
                "cancel closes once",
                row(Cancelled),
                edit(ContributionUpdate::Cancel),
                Err(ContribError::state("This contribution is already closed.")),
            ),
            (
                "purging provider data keeps the state",
                with(Ready, |r| r.provider_snapshot_expires_at = Some(NOW)),
                edit(ContributionUpdate::PurgeProviderData {
                    draft: blank(),
                    selection: ContributionSourceSelection::default(),
                }),
                moved(Ready, &[]),
            ),
            (
                "a row without provider data has nothing to purge",
                row(Ready),
                edit(ContributionUpdate::PurgeProviderData {
                    draft: blank(),
                    selection: ContributionSourceSelection::default(),
                }),
                Err(ContribError::stale(
                    "The contribution changed before cleanup.",
                )),
            ),
            (
                "an open row goes stale and its jobs end",
                row(Ready),
                Update::MarkStale {
                    expected_row_revision: REVISION,
                },
                moved(Stale, &["cancel jobs"]),
            ),
            (
                "a closed row never goes stale",
                row(Linked),
                Update::MarkStale {
                    expected_row_revision: REVISION,
                },
                Ok((Linked, false, Vec::new())),
            ),
            (
                "the editor's callback queues verification",
                row(Seeded),
                Update::Callback {
                    release_mbid: RELEASE.into(),
                },
                moved(Verifying, &["consume tokens", "enqueue job"]),
            ),
            (
                "a callback for a row not waiting is refused",
                row(Ready),
                Update::Callback {
                    release_mbid: RELEASE.into(),
                },
                Err(ContribError::state(
                    "This contribution is not waiting for MusicBrainz.",
                )),
            ),
            (
                "rebuild retires the stale row",
                row(Stale),
                Update::Retire {
                    expected_row_revision: REVISION,
                    album_row_revision: 7,
                    input_revision: "input-1".into(),
                },
                moved(Stale, &["consume tokens", "cancel jobs"]),
            ),
            (
                "rebuild from an album that moved again is refused",
                row(Stale),
                Update::Retire {
                    expected_row_revision: REVISION,
                    album_row_revision: 6,
                    input_revision: "input-1".into(),
                },
                Err(ContribError::stale(
                    "The album changed before the contribution could be rebuilt.",
                )),
            ),
            (
                "a verified release links",
                verifying(Verifying),
                finish(VerificationOutcome::Linked),
                moved(
                    Linked,
                    &["insert attempt", "link identity", "invalidate catalog"],
                ),
            ),
            (
                "a contradicted release goes to review",
                verifying(Verifying),
                finish(VerificationOutcome::NeedsReview),
                moved(NeedsReview, &["insert attempt"]),
            ),
            (
                "an album that moved mid-verification goes stale",
                with(Verifying, |r| r.current_input_revision = "input-2".into()),
                finish(VerificationOutcome::Linked),
                moved(Stale, &[]),
            ),
            (
                "upkeep drops a seed snapshot",
                with(Linked, |r| r.seed_snapshot_json = Some("{}".into())),
                Update::ClearSeedSnapshot,
                moved(Linked, &[]),
            ),
            (
                "upkeep leaves a row without one alone",
                row(Linked),
                Update::ClearSeedSnapshot,
                Ok((Linked, false, Vec::new())),
            ),
        ];
        for (name, before, update, want) in cases {
            let got = transition(&before, update, NOW).map(|(after, effects)| {
                (
                    after.state,
                    after.row_revision != before.row_revision,
                    effects.iter().map(effect_name).collect::<Vec<_>>(),
                )
            });
            assert_eq!(got, want, "{name}");
        }
    }
}
