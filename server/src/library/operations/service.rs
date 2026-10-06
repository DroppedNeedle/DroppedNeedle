//! The operations service: what the HTTP handlers and the worker loop call.
//!
//! Every store call runs a short transaction on the identify store's
//! connection, so it must run off the async workers (handlers use
//! `spawn_blocking`; the loop drives its tick on a blocking thread). The
//! identify store's own methods lock the same connection, so they are
//! never called from inside a transaction here.

use std::sync::Arc;

use rusqlite::{Connection, Transaction, TransactionBehavior};

use super::decisions;
use super::models::{
    CandidateChoice, Control, OperationDetail, OperationError, OperationJob, ReidentifyInput,
    ReleaseSearch, UndoOutcome,
};
use super::reasons;
use super::reidentify::{MAX_ATTEMPTS, RETRY_SECS, evaluation, unavailable};
use super::store::{self, NewReidentification};
use crate::ids::IdGenerator;
use crate::library::clock::now_unix;
use crate::library::identify::service::IdentifyService;
use crate::library::identify::sources::EditionQuery;
use crate::library::identify::sqlite::SqliteIdentifyStore;
use crate::library::identify::stores::{FactsSource as _, IdentityStore as _};
use crate::library::wiring::{LibrarySetup, ScanCoordinator};

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
            .map(|raw| {
                uuid::Uuid::parse_str(raw.trim())
                    .map(|id| id.hyphenated().to_string())
                    .map_err(|_| OperationError::Invalid(reasons::RELEASE_MBID_INVALID))
            })
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

    /// Settle a re-identification with the administrator's choice. Blocking.
    pub fn select_candidate(
        &self,
        job_id: &str,
        choice: &CandidateChoice,
        user_id: &str,
    ) -> Result<OperationJob, OperationError> {
        self.write(|tx| decisions::select_candidate(tx, job_id, choice, user_id, now_unix()))
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

/// The live automatic-edition undo for one album: the revisions to echo.
/// Blocking.
pub fn automatic_edition_undo(
    setup: &LibrarySetup,
    album_id: &str,
) -> Result<Option<(i64, i64)>, OperationError> {
    Operations::new(setup).read(|conn| Ok(decisions::live_automatic_edition_undo(conn, album_id)?))
}
