//! The operations service: what the HTTP handlers and the worker loop call.
//!
//! Every store call runs a short transaction on the identify store's
//! connection, so it must run off the async workers (handlers use
//! `spawn_blocking`; the loop drives its tick on a blocking thread). The
//! identify store's own methods lock the same connection, so they are
//! never called from inside a transaction here.

use std::sync::Arc;

use rusqlite::{Connection, OptionalExtension as _, Transaction, TransactionBehavior};

use super::decisions::indexed_track_ids;
use super::models::{
    CandidateChoice, Control, EditionChoice, OperationDetail, OperationError, OperationJob,
    ReidentificationCandidate, ReidentifyInput, ReleaseSearch, UndoOutcome,
};
use super::plan::{self, EditionPlan};
use super::reasons;
use super::reidentify::{MAX_ATTEMPTS, RETRY_SECS, evaluation, unavailable};
use super::store::{self, NewReidentification};
use super::{choice, decisions, reviews};
use crate::ids::IdGenerator;
use crate::library::clock::now_unix;
use crate::library::identify::models::IdentifyKind;
use crate::library::identify::models::LocalAlbumFacts;
use crate::library::identify::service::IdentifyService;
use crate::library::identify::sources::EditionQuery;
use crate::library::identify::sqlite::SqliteIdentifyStore;
use crate::library::identify::sqlite::album_identity_row;
use crate::library::identify::stores::{FactsSource as _, IdentityStore as _};
use crate::library::matching::model::Release;
use crate::library::wiring::{LibrarySetup, ScanCoordinator};

/// Most reviews one "retry all" or bulk action takes on.
pub const BULK_MAX: u32 = 5_000;

/// Largest edition-finder page, as in v2.
pub const RELEASE_PAGE_MAX: u32 = 12;

/// Operation jobs over the library's identify store and service.
#[derive(Clone)]
pub struct Operations {
    store: Arc<SqliteIdentifyStore>,
    identify: Arc<IdentifyService>,
    ids: Arc<dyn IdGenerator>,
    coordinator: Arc<ScanCoordinator>,
}

impl Operations {
    pub fn new(setup: &LibrarySetup) -> Self {
        Self {
            store: setup.identify_store.clone(),
            identify: setup.identify.clone(),
            ids: setup.ids.clone(),
            coordinator: setup.coordinator.clone(),
        }
    }

    /// True while a scan runs: it may be changing the files a job reads.
    fn scan_running(&self) -> bool {
        !self.coordinator.current().is_empty()
    }

    fn read<T>(
        &self,
        op: impl FnOnce(&Connection) -> Result<T, OperationError>,
    ) -> Result<T, OperationError> {
        self.store.with_connection(|conn| op(conn))
    }

    /// Run a read on the store's connection. Blocking.
    pub fn read_with<T>(
        &self,
        op: impl FnOnce(&Connection) -> rusqlite::Result<T>,
    ) -> Result<T, OperationError> {
        self.read(|conn| Ok(op(conn)?))
    }

    fn write<T>(
        &self,
        op: impl FnOnce(&Transaction<'_>) -> Result<T, OperationError>,
    ) -> Result<T, OperationError> {
        self.store.with_connection(|conn| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let value = op(&tx)?;
            tx.commit()?;
            Ok(value)
        })
    }

    /// One job with its results and, for a re-identification, its
    /// candidates. Blocking.
    pub fn get(&self, job_id: &str) -> Result<OperationDetail, OperationError> {
        self.read(|conn| {
            let job = store::job(conn, job_id)?
                .ok_or(OperationError::NotFound(reasons::OPERATION_NOT_FOUND))?;
            let (results, results_truncated) = store::work_results(conn, job_id)?;
            let (candidates, selected_candidate_key) = if job.is_reidentification() {
                match store::snapshot(conn, job_id)? {
                    Some(snap) => (
                        snap.evaluation.map(|e| e.candidates).unwrap_or_default(),
                        snap.selected_candidate_key,
                    ),
                    None => (Vec::new(), None),
                }
            } else {
                (Vec::new(), None)
            };
            Ok(OperationDetail {
                job,
                results,
                results_truncated,
                candidates,
                selected_candidate_key,
            })
        })
    }

    /// Pause, resume, or stop a job. Blocking.
    pub fn control(
        &self,
        job_id: &str,
        control: Control,
        expected_row_revision: i64,
        idempotency_key: Option<&str>,
    ) -> Result<OperationJob, OperationError> {
        self.write(|tx| {
            store::request_control(
                tx,
                job_id,
                control,
                expected_row_revision,
                idempotency_key,
                now_unix(),
            )
        })
    }

    /// Start (or find) an explicit re-identification of one album. Blocking.
    pub fn reidentify(
        &self,
        album_id: &str,
        user_id: &str,
        input: ReidentifyInput,
    ) -> Result<OperationJob, OperationError> {
        let release_mbid = input
            .release_mbid
            .as_deref()
            .map(parse_release)
            .transpose()?;
        let new = NewReidentification {
            job_id: self.ids.new_id(),
            requested_by_user_id: user_id.to_owned(),
            idempotency_key: input.idempotency_key,
            local_album_id: album_id.to_owned(),
            one_off_local_metadata: input.one_off_local_metadata,
            release_mbid,
            expected_album_revision: input.expected_album_revision,
            expected_input_revision: input.expected_input_revision,
        };
        self.write(|tx| store::create_reidentification(tx, &new, now_unix()))
    }

    /// One page of identification reviews. Blocking.
    pub fn reviews(
        &self,
        filter: &reviews::ReviewFilter,
    ) -> Result<(Vec<reviews::ReviewRow>, u64), OperationError> {
        self.read_with(|conn| reviews::list(conn, filter))
    }

    /// One identification review. Blocking.
    pub fn review(&self, review_id: &str) -> Result<reviews::ReviewRow, OperationError> {
        self.read_with(|conn| reviews::get(conn, review_id))?
            .ok_or(OperationError::NotFound(reasons::REVIEW_NOT_FOUND))
    }

    /// Dismiss, detach, exclude or restore from a review, then return the
    /// settled review. Blocking.
    pub fn settle_review(
        &self,
        review_id: &str,
        action: reviews::ReviewAction,
        actor: &str,
    ) -> Result<reviews::ReviewRow, OperationError> {
        self.write(|tx| reviews::settle(tx, review_id, action, actor, now_unix()))?;
        self.review(review_id)
    }

    /// Start a fresh re-identification of a pending review's album. The
    /// job carries the decision from here, so the review is dismissed.
    /// Blocking.
    pub fn retry_review(
        &self,
        review_id: &str,
        actor: &str,
        idempotency_key: Option<String>,
        one_off_local_metadata: bool,
    ) -> Result<OperationJob, OperationError> {
        let row = self.review(review_id)?;
        if row.review.state != crate::library::identify::models::ReviewState::Pending {
            return Err(OperationError::Conflict(reasons::REVIEW_SETTLED));
        }
        let job = self.reidentify(
            &row.review.local_album_id,
            actor,
            ReidentifyInput {
                idempotency_key,
                one_off_local_metadata,
                ..ReidentifyInput::default()
            },
        )?;
        self.write(|tx| {
            Ok(reviews::mark(
                tx,
                review_id,
                crate::library::identify::models::ReviewState::Dismissed,
                actor,
            )?)
        })?;
        Ok(job)
    }

    /// Start identification again for every pending review's album in
    /// `root_ids` (every folder when empty), up to [`BULK_MAX`]. Albums
    /// that cannot start (excluded, no files, Local metadata without the
    /// confirmation) keep their review and are counted by reason. Blocking.
    pub fn retry_pending_reviews(
        &self,
        actor: &str,
        one_off_local_metadata: bool,
        root_ids: Vec<String>,
    ) -> Result<reviews::Tally, OperationError> {
        let filter = reviews::ReviewFilter {
            state: Some(crate::library::identify::models::ReviewState::Pending),
            root_ids,
            limit: BULK_MAX,
            ..reviews::ReviewFilter::default()
        };
        let (rows, _) = self.reviews(&filter)?;
        let mut outcome = reviews::Tally::default();
        for row in rows {
            match self.retry_review(&row.review.id, actor, None, one_off_local_metadata) {
                Ok(_) => outcome.done += 1,
                Err(
                    OperationError::NotFound(reason)
                    | OperationError::Invalid(reason)
                    | OperationError::Conflict(reason),
                ) => outcome.skip(reason),
                Err(other) => return Err(other),
            }
        }
        Ok(outcome)
    }

    /// What a bulk action would do to the reviews `filter` selects: the
    /// reviews it changes, the ones it skips by reason, and the token that
    /// apply needs. Blocking.
    pub fn preview_bulk_reviews(
        &self,
        action: reviews::BulkAction,
        filter: &reviews::ReviewFilter,
        candidate_key: Option<&str>,
    ) -> Result<reviews::BulkPlan, OperationError> {
        let filter = reviews::ReviewFilter {
            limit: BULK_MAX,
            offset: 0,
            ..filter.clone()
        };
        let (rows, matched) = self.reviews(&filter)?;
        Ok(reviews::plan_bulk(&rows, matched, action, candidate_key))
    }

    /// Apply a previewed bulk action. Refused when the selection changed
    /// since the preview (the token no longer matches). Blocking.
    pub fn apply_bulk_reviews(
        &self,
        action: reviews::BulkAction,
        filter: &reviews::ReviewFilter,
        candidate_key: Option<&str>,
        preview_token: &str,
        actor: &str,
    ) -> Result<reviews::Tally, OperationError> {
        let plan = self.preview_bulk_reviews(action, filter, candidate_key)?;
        if plan.token != preview_token {
            return Err(OperationError::Conflict(reasons::REVIEW_SELECTION_CHANGED));
        }
        let mut outcome = reviews::Tally {
            done: 0,
            skipped: plan.skipped.clone(),
        };
        for item in &plan.items {
            let applied = match (action, item.candidate_key.as_deref()) {
                (reviews::BulkAction::KeepTagged, _) => {
                    Ok(self.identify.reject_candidates(&item.review_id, actor))
                }
                (reviews::BulkAction::Accept, Some(key)) => self
                    .identify
                    .approve_candidate(&item.review_id, actor, key)
                    .map_err(|error| error.to_string()),
                (reviews::BulkAction::Accept, None) => Ok(false),
                (reviews::BulkAction::Exclude, _) => {
                    match self.write(|tx| {
                        reviews::settle(
                            tx,
                            &item.review_id,
                            reviews::ReviewAction::Exclude,
                            actor,
                            now_unix(),
                        )
                    }) {
                        Ok(()) => Ok(true),
                        Err(
                            OperationError::NotFound(reason)
                            | OperationError::Invalid(reason)
                            | OperationError::Conflict(reason),
                        ) => {
                            outcome.skip(reason);
                            continue;
                        }
                        Err(other) => Err(other.to_string()),
                    }
                }
            };
            match applied {
                Ok(true) => outcome.done += 1,
                Ok(false) => outcome.skip(reasons::REVIEW_SETTLED),
                Err(cause) => {
                    tracing::warn!(review = item.review_id, %cause, "bulk review action failed");
                    outcome.skip(reasons::REVIEW_ACTION_FAILED);
                }
            }
        }
        Ok(outcome)
    }

    /// Settle a re-identification with the administrator's choice. Blocking.
    pub fn select_candidate(
        &self,
        job_id: &str,
        choice: &CandidateChoice,
        user_id: &str,
    ) -> Result<OperationJob, OperationError> {
        self.write(|tx| decisions::select_candidate(tx, job_id, choice, user_id, now_unix()))
    }

    /// Choose the album's edition: any MusicBrainz release, from the
    /// album's release group or another one. MusicBrainz is asked for the
    /// release's tracklist, the files are placed on it where they fit, and
    /// the release becomes the album's protected edition.
    pub async fn choose_edition(
        &self,
        album_id: &str,
        raw_release: &str,
        actor: Option<&str>,
    ) -> Result<EditionChoice, OperationError> {
        let release = parse_release(raw_release)?;
        let candidate = self.candidate_for(album_id, &release).await?;
        let (ops, album, actor) = (self.clone(), album_id.to_owned(), actor.map(str::to_owned));
        tokio::task::spawn_blocking(move || {
            ops.write(|tx| choice::choose(tx, &album, &candidate, actor.as_deref(), now_unix()))
        })
        .await
        .map_err(|error| OperationError::Store(error.to_string()))?
    }

    /// The release scored against the album's files, as a candidate.
    async fn candidate_for(
        &self,
        album_id: &str,
        release_mbid: &str,
    ) -> Result<ReidentificationCandidate, OperationError> {
        Ok(self.scored(album_id, release_mbid).await?.2)
    }

    /// The album's files, the release as MusicBrainz lists it, and the
    /// release scored against the files as a candidate.
    async fn scored(
        &self,
        album_id: &str,
        release_mbid: &str,
    ) -> Result<(LocalAlbumFacts, Release, ReidentificationCandidate), OperationError> {
        let facts = {
            let store = self.store.clone();
            let album = album_id.to_owned();
            tokio::task::spawn_blocking(move || store.album_facts(&album))
                .await
                .map_err(|error| OperationError::Store(error.to_string()))?
        }
        .filter(|facts| !facts.tracks.is_empty())
        .ok_or(OperationError::NotFound(reasons::ALBUM_NOT_FOUND))?;
        let recall = self.identify.recall(&facts, Some(release_mbid)).await;
        if recall.provider_deferred {
            return Err(OperationError::Unavailable(
                recall
                    .failure_code
                    .unwrap_or_else(|| "musicbrainz_unavailable".to_owned()),
            ));
        }
        if recall.releases.is_empty() {
            return Err(OperationError::NotFound(reasons::EDITION_NOT_FOUND));
        }
        let ranking = self.identify.rank(&facts, &recall);
        let candidate = evaluation(&facts, &recall, &ranking, true)
            .candidates
            .into_iter()
            .next()
            .ok_or(OperationError::NotFound(reasons::EDITION_NOT_FOUND))?;
        let scored = candidate
            .evidence
            .release_mbid
            .clone()
            .unwrap_or_else(|| release_mbid.to_owned());
        let release = recall
            .releases
            .into_iter()
            .find(|release| release.answers_to(&scored))
            .ok_or(OperationError::NotFound(reasons::EDITION_NOT_FOUND))?;
        Ok((facts, release, candidate))
    }

    /// The library's copy of a release group that has an edition: a copy a
    /// person chose first, then the oldest, as acquisition picks it.
    pub async fn album_for_group(
        &self,
        release_group_mbid: &str,
    ) -> Result<Option<String>, OperationError> {
        let (ops, group) = (self.clone(), release_group_mbid.trim().to_ascii_lowercase());
        tokio::task::spawn_blocking(move || {
            ops.read_with(|conn| {
                conn.query_row(
                    "SELECT e.local_album_id FROM local_album_external_identities e \
                     JOIN local_albums b ON b.id = e.local_album_id \
                     WHERE b.retired_into_album_id IS NULL AND e.provider = ?2 \
                       AND lower(e.release_group_mbid) = ?1 AND e.release_mbid IS NOT NULL \
                     ORDER BY e.decision_source <> 'manual', b.created_at LIMIT 1",
                    rusqlite::params![group, store::PROVIDER],
                    |row| row.get(0),
                )
                .optional()
            })
        })
        .await
        .map_err(|error| OperationError::Store(error.to_string()))?
    }

    /// What converting the album to `release_mbid` would do: which files
    /// keep their place, which edition tracks are missing, and which files
    /// the edition has no room for. With no release, the album's current
    /// edition is planned. Nothing changes.
    pub async fn edition_plan(
        &self,
        album_id: &str,
        raw_release: Option<&str>,
    ) -> Result<EditionPlan, OperationError> {
        let (current, indexed) = {
            let ops = self.clone();
            let album = album_id.to_owned();
            tokio::task::spawn_blocking(move || {
                ops.read(|conn| {
                    let identity = album_identity_row(conn, &album)?;
                    Ok((
                        identity.and_then(|row| row.release_mbid),
                        indexed_track_ids(conn, &album)?,
                    ))
                })
            })
            .await
            .map_err(|error| OperationError::Store(error.to_string()))??
        };
        if indexed.is_empty() {
            return Err(OperationError::NotFound(reasons::ALBUM_NOT_FOUND));
        }
        let release = match raw_release {
            Some(raw) => parse_release(raw)?,
            None => current
                .clone()
                .ok_or(OperationError::Invalid(reasons::NO_EDITION_TO_PLAN))?,
        };
        let (facts, release, candidate) = self.scored(album_id, &release).await?;
        Ok(plan::build(
            &facts,
            &release,
            &candidate,
            &indexed,
            current.as_deref(),
        ))
    }

    /// "Let DroppedNeedle choose": hand the album's edition back to
    /// automatic best fit and queue an identification. Blocking.
    pub fn hand_back_edition(&self, album_id: &str, actor: &str) -> Result<(), OperationError> {
        let known = self.read(|conn| Ok(store::album_revisions(conn, album_id)?))?;
        if known.is_none() {
            return Err(OperationError::NotFound(reasons::ALBUM_NOT_FOUND));
        }
        self.write(|tx| choice::hand_back(tx, album_id, actor, now_unix()))?;
        let revision = self
            .store
            .input_revision(album_id)
            .ok_or_else(|| OperationError::Store("album revision unreadable".to_owned()))?;
        let queued = self.identify.enqueue_album(
            &self.ids.new_id(),
            album_id,
            IdentifyKind::Manual,
            &revision,
            Some(actor),
            crate::library::clock::now_ms(),
        );
        if queued.is_none() {
            tracing::warn!(
                album = album_id,
                "edition handed back but the identification was not queued"
            );
        }
        Ok(())
    }

    /// "Looks right": confirm the album's unconfirmed match. Blocking.
    pub fn confirm_match(&self, album_id: &str, actor: &str) -> Result<(), OperationError> {
        self.write(|tx| choice::confirm(tx, album_id, actor, now_unix()))
    }

    /// Take back the album's last edition change. Blocking.
    pub fn undo_edition_choice(&self, album_id: &str, actor: &str) -> Result<(), OperationError> {
        self.write(|tx| choice::undo(tx, album_id, actor, now_unix()))
    }

    /// Place the files of the next album whose chosen edition still waits
    /// for them (pins converted by an upgrade, v2 imports). True when one
    /// was handled. Store calls block: run this on a blocking thread.
    pub async fn remap_next(&self) -> Result<bool, OperationError> {
        let now = now_unix();
        let Some(pending) = self.read(|conn| Ok(choice::next_remap(conn, now)?))? else {
            return Ok(false);
        };
        let outcome = self
            .candidate_for(&pending.local_album_id, &pending.release_mbid)
            .await;
        let actor = pending.chosen_by_user_id.as_deref();
        self.write(|tx| {
            let now = now_unix();
            // A person may have chosen again, or handed the album back,
            // while MusicBrainz was asked: then this remap is stale.
            if !choice::remap_still_wanted(tx, &pending)? {
                return Ok(true);
            }
            let placed = match outcome {
                Ok(candidate) => {
                    choice::apply_choice(tx, &pending.local_album_id, &candidate, actor, None, now)
                        .map(|_| ())
                }
                Err(error) => Err(error),
            };
            match placed {
                Ok(()) => decisions::bump_catalog(tx)?,
                Err(OperationError::Unavailable(_)) => {
                    choice::retry_remap(tx, &pending, reasons::MUSICBRAINZ_UNAVAILABLE.code, now)?
                }
                // The album's files are out of reach for now (a scan may
                // be moving them): never a reason to drop the choice.
                Err(OperationError::NotFound(reason))
                    if reason.code == reasons::ALBUM_NOT_FOUND.code =>
                {
                    choice::retry_remap(tx, &pending, reason.code, now)?
                }
                Err(OperationError::NotFound(reason))
                    if reason.code == reasons::EDITION_NOT_FOUND.code =>
                {
                    choice::give_up_remap(tx, &pending, reason.code, now)?;
                    decisions::bump_catalog(tx)?;
                }
                Err(OperationError::Invalid(reason))
                    if reason.code == reasons::EDITION_FITS_NO_FILE.code
                        && pending.from_group_pin =>
                {
                    choice::give_up_remap(tx, &pending, reason.code, now)?;
                    decisions::bump_catalog(tx)?;
                }
                Err(OperationError::Invalid(reason) | OperationError::NotFound(reason)) => {
                    choice::keep_unplaceable_choice(tx, &pending, reason.code, now)?;
                    decisions::bump_catalog(tx)?;
                }
                Err(other) => return Err(other),
            }
            Ok(true)
        })
    }

    /// Undo the album's last automatic edition. Blocking.
    pub fn undo_automatic_edition(
        &self,
        album_id: &str,
        expected_album_revision: i64,
        expected_identity_revision: i64,
        user_id: &str,
    ) -> Result<UndoOutcome, OperationError> {
        self.write(|tx| {
            decisions::undo_automatic_edition(
                tx,
                album_id,
                expected_album_revision,
                expected_identity_revision,
                user_id,
                now_unix(),
            )
        })
    }

    /// One page of MusicBrainz releases for the album's edition finder,
    /// with the album's current identity marked: a title (and artist)
    /// search, or every release of `release_group` so the administrator can
    /// pick any edition of the album. Store reads block, so they run on a
    /// blocking thread.
    pub async fn search_releases(
        &self,
        album_id: &str,
        title: &str,
        artist: &str,
        release_group: Option<&str>,
        limit: u32,
        offset: u32,
    ) -> Result<ReleaseSearch, OperationError> {
        let title_query = title.split_whitespace().collect::<Vec<_>>().join(" ");
        let artist_query = artist.split_whitespace().collect::<Vec<_>>().join(" ");
        let release_group_mbid = release_group
            .map(|raw| {
                uuid::Uuid::parse_str(raw.trim())
                    .map(|id| id.hyphenated().to_string())
                    .map_err(|_| OperationError::Invalid(reasons::RELEASE_GROUP_MBID_INVALID))
            })
            .transpose()?;
        if title_query.is_empty() && release_group_mbid.is_none() {
            return Err(OperationError::Invalid(reasons::SEARCH_NEEDS_TITLE));
        }
        let limit = limit.clamp(1, RELEASE_PAGE_MAX);
        let identity = {
            let ops = self.clone();
            let album_id = album_id.to_owned();
            tokio::task::spawn_blocking(move || {
                let known = ops.read(|conn| Ok(store::album_revisions(conn, &album_id)?))?;
                if known.is_none() {
                    return Err(OperationError::NotFound(reasons::ALBUM_NOT_FOUND));
                }
                Ok(ops.store.album_identity(&album_id))
            })
            .await
            .map_err(|error| OperationError::Store(error.to_string()))??
        };
        let page = self
            .identify
            .search_editions(EditionQuery {
                title: title_query.clone(),
                artist: artist_query.clone(),
                release_group_mbid: release_group_mbid.clone(),
                limit,
                offset,
            })
            .await
            .map_err(|error| OperationError::Unavailable(error.0))?;
        Ok(ReleaseSearch {
            title_query,
            artist_query,
            release_group_query: release_group_mbid,
            current_release_group_mbid: identity
                .as_ref()
                .and_then(|row| row.release_group_mbid.clone()),
            current_release_mbid: identity.and_then(|row| row.release_mbid),
            page,
            limit,
        })
    }

    /// Requeue jobs whose worker vanished, then claim and evaluate the next
    /// due re-identification. Returns the job as it was left, or `None`
    /// when nothing was due. Store calls block: run this on a blocking
    /// thread.
    pub async fn run_next(&self, worker: &str) -> Result<Option<OperationJob>, OperationError> {
        let now = now_unix();
        let claimed = self.write(|tx| {
            store::recover_expired(tx, now)?;
            let Some(job) = store::claim_reidentification(tx, worker, now)? else {
                return Ok(None);
            };
            if store::claim_work(tx, &job.id, now)?.is_none() {
                return Ok(Some((
                    job.clone(),
                    None,
                    store::fail(tx, &job.id, worker, "MISSING_WORK", now)?,
                )));
            }
            match store::snapshot(tx, &job.id)? {
                Some(snap) => Ok(Some((job, Some(snap), None))),
                None => Ok(Some((
                    job.clone(),
                    None,
                    store::fail(tx, &job.id, worker, "MISSING_SNAPSHOT", now)?,
                ))),
            }
        })?;
        let Some((job, snap, failed)) = claimed else {
            return Ok(None);
        };
        let Some(snap) = snap else {
            return Ok(failed);
        };

        let facts = self
            .store
            .album_facts(&snap.local_album_id)
            .filter(|facts| !facts.tracks.is_empty());
        let Some(facts) = facts else {
            return self.write(|tx| {
                Ok(store::fail(
                    tx,
                    &job.id,
                    worker,
                    "SUBJECT_NOT_AVAILABLE",
                    now_unix(),
                )?)
            });
        };
        let scanning = self.scan_running();
        let halted = self.write(|tx| {
            if scanning {
                return Ok(Some(store::requeue_for_scan(
                    tx,
                    &job.id,
                    worker,
                    now_unix(),
                )?));
            }
            if !store::matches_snapshot(tx, &snap)? {
                return Ok(Some(store::fail(
                    tx,
                    &job.id,
                    worker,
                    "STALE_INPUT",
                    now_unix(),
                )?));
            }
            Ok(store::checkpoint(tx, &job.id, worker, now_unix())?.map(Some))
        })?;
        if let Some(left) = halted {
            return Ok(left);
        }

        let exact = snap.requested_release_mbid.as_deref();
        let recall = self.identify.recall(&facts, exact).await;
        if recall.provider_deferred {
            return self.write(|tx| {
                let now = now_unix();
                if job.reidentification_attempt_count < MAX_ATTEMPTS {
                    let reason = super::control::RESUMABLE_FAILURE;
                    Ok(store::defer(
                        tx,
                        &job.id,
                        worker,
                        reason,
                        now + RETRY_SECS,
                        now,
                    )?)
                } else {
                    store::finish_evaluation(tx, &job.id, worker, &unavailable(), now)
                }
            });
        }
        // A scan that started during recall may have moved the files: wait
        // for it and evaluate again rather than calling the input stale.
        let scanning = self.scan_running();
        let left = self.write(|tx| {
            if scanning {
                return Ok(store::requeue_for_scan(tx, &job.id, worker, now_unix())?);
            }
            Ok(store::checkpoint(tx, &job.id, worker, now_unix())?)
        })?;
        if let Some(left) = left {
            return Ok(Some(left));
        }
        let ranking = self.identify.rank(&facts, &recall);
        let found = evaluation(&facts, &recall, &ranking, exact.is_some());
        self.write(|tx| store::finish_evaluation(tx, &job.id, worker, &found, now_unix()))
    }
}

/// A MusicBrainz release id, hyphenated and lowercase.
fn parse_release(raw: &str) -> Result<String, OperationError> {
    uuid::Uuid::parse_str(raw.trim())
        .map(|id| id.hyphenated().to_string())
        .map_err(|_| OperationError::Invalid(reasons::RELEASE_MBID_INVALID))
}

/// Whether the album's last edition change can still be undone. Blocking.
pub fn edition_undo_available(
    setup: &LibrarySetup,
    album_id: &str,
) -> Result<bool, OperationError> {
    Operations::new(setup).read(|conn| Ok(choice::undo_available(conn, album_id)?))
}

/// The live automatic-edition undo for one album: the revisions to echo.
/// Blocking.
pub fn automatic_edition_undo(
    setup: &LibrarySetup,
    album_id: &str,
) -> Result<Option<(i64, i64)>, OperationError> {
    Operations::new(setup).read(|conn| Ok(decisions::live_automatic_edition_undo(conn, album_id)?))
}
