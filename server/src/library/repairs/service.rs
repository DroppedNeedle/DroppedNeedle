//! The repairs service: what the HTTP handlers and the worker loop call.
//!
//! Store calls run short transactions on the identify store's connection,
//! so they must run off the async workers (handlers use `spawn_blocking`;
//! the loop drives its tick on a blocking thread). The identify store's
//! own reads lock the same connection, so they are never called from
//! inside a transaction here.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use rusqlite::{Connection, Transaction, TransactionBehavior};

use super::apply::apply_album;
use super::classify;
use super::hygiene;
use super::models::{
    EditionSummary, FindingPage, HYGIENE_VERSION, MANAGEMENT_MAPPING_VERSION, NewRepair,
    PreparationEstimate, Purpose, RepairEstimate, Summary, Verdict,
};
use super::reasons;
use super::store::{self, Phase, RepairSnapshot};
use crate::ids::IdGenerator;
use crate::library::clock::now_unix;
use crate::library::identify::models::{LocalAlbumFacts, RecallResult};
use crate::library::identify::service::IdentifyService;
use crate::library::identify::sqlite::SqliteIdentifyStore;
use crate::library::identify::stores::FactsSource as _;
use crate::library::operations::models::{
    Control, OperationDetail, OperationError, OperationJob, OperationKind,
    ReidentificationCandidate,
};
use crate::library::operations::reidentify::evaluation;
use crate::library::operations::service::Operations;
use crate::library::operations::store as jobs;
use crate::library::wiring::{LibrarySetup, ScanCoordinator};

/// How long one worker turn may run before the job goes back to the
/// queue, so other operations get their turn.
const TURN: Duration = Duration::from_secs(2);
/// Wait before an album MusicBrainz could not answer for is tried again.
const PROVIDER_RETRY_SECS: f64 = 120.0;
/// Largest history page.
const HISTORY_MAX: usize = 50;
/// Largest findings page.
const FINDINGS_MAX: usize = 200;

/// A repair job with its report summary.
#[derive(Debug, Clone)]
pub struct RepairDetail {
    pub detail: OperationDetail,
    pub summary: Option<Summary>,
}

/// What a check made of one album.
enum Checked {
    Found(Box<Verdict>, Option<i64>),
    /// MusicBrainz did not answer; try the album again later.
    Deferred,
}

/// Identity repairs, identity preparations and catalog cleanup.
#[derive(Clone)]
pub struct Repairs {
    ops: Operations,
    store: Arc<SqliteIdentifyStore>,
    identify: Arc<IdentifyService>,
    ids: Arc<dyn IdGenerator>,
    coordinator: Arc<ScanCoordinator>,
    hygiene_checked: Arc<AtomicBool>,
}

impl Repairs {
    pub fn new(setup: &LibrarySetup) -> Self {
        Self {
            ops: Operations::new(setup),
            store: setup.identify_store.clone(),
            identify: setup.identify.clone(),
            ids: setup.ids.clone(),
            coordinator: setup.coordinator.clone(),
            hygiene_checked: Arc::new(AtomicBool::new(false)),
        }
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

    // -----------------------------------------------------------------------
    // Requests. All blocking.
    // -----------------------------------------------------------------------

    /// Start an identity repair or preparation, or find the one this
    /// idempotency key already started.
    pub fn create(
        &self,
        purpose: Purpose,
        user_id: &str,
        idempotency_key: &str,
        root_ids: Vec<String>,
        source_matcher_version: Option<String>,
        target_matcher_version: String,
    ) -> Result<OperationJob, OperationError> {
        let new = NewRepair {
            job_id: self.ids.new_id(),
            purpose,
            requested_by_user_id: Some(user_id.to_owned()),
            idempotency_key: idempotency_key.to_owned(),
            root_ids: dedupe(root_ids),
            album_ids: Vec::new(),
            source_matcher_version,
            target_matcher_version,
        };
        self.write(|tx| store::create(tx, &new, now_unix()))
    }

    pub fn estimate_repair(&self, root_ids: Vec<String>) -> Result<RepairEstimate, OperationError> {
        let roots = dedupe(root_ids);
        self.read(|conn| Ok(store::estimate_repair(conn, &roots)?))
    }

    pub fn estimate_preparation(
        &self,
        root_ids: Vec<String>,
    ) -> Result<PreparationEstimate, OperationError> {
        let roots = dedupe(root_ids);
        self.read(|conn| Ok(store::estimate_preparation(conn, &roots)?))
    }

    /// One repair job with its summary; not found when it is not a job of
    /// `purpose`.
    pub fn get(&self, job_id: &str, purpose: Purpose) -> Result<RepairDetail, OperationError> {
        let (_, snap) = self.read(|conn| store::repair_job(conn, job_id, purpose))?;
        Ok(RepairDetail {
            detail: self.ops.get(job_id)?,
            summary: snap.summary,
        })
    }

    /// A page of a purpose's jobs, newest first, and the next cursor.
    pub fn history(
        &self,
        purpose: Purpose,
        limit: usize,
        cursor: Option<&str>,
    ) -> Result<(Vec<RepairDetail>, Option<String>), OperationError> {
        if !(1..=HISTORY_MAX).contains(&limit) {
            return Err(OperationError::Invalid(reasons::PAGE_SIZE_INVALID));
        }
        let before = cursor.map(parse_cursor).transpose()?;
        let ids = self.read(|conn| Ok(store::history(conn, purpose, limit + 1, before)?))?;
        let mut items = Vec::with_capacity(limit);
        for id in ids.iter().take(limit) {
            items.push(self.get(id, purpose)?);
        }
        let next = (ids.len() > limit)
            .then(|| items.last())
            .flatten()
            .map(|last| format!("{}:{}", last.detail.job.created_at, last.detail.job.id));
        Ok((items, next))
    }

    /// One page of a job's findings.
    pub fn findings(
        &self,
        job_id: &str,
        purpose: Purpose,
        limit: usize,
        cursor: Option<&str>,
        category: Option<&str>,
    ) -> Result<FindingPage, OperationError> {
        if !(1..=FINDINGS_MAX).contains(&limit) {
            return Err(OperationError::Invalid(reasons::PAGE_SIZE_INVALID));
        }
        let codes = category
            .map(|category| {
                store::category_codes(purpose, category)
                    .ok_or(OperationError::Invalid(reasons::CATEGORY_INVALID))
            })
            .transpose()?;
        let cursor = cursor.map(parse_cursor).transpose()?;
        self.read(|conn| {
            let (_, snap) = store::repair_job(conn, job_id, purpose)?;
            let mut page = store::findings_page(conn, job_id, purpose, codes, cursor, limit)?;
            page.refresh_required = purpose == Purpose::ManagementReadiness
                && snap.target_matcher_version != MANAGEMENT_MAPPING_VERSION;
            Ok(page)
        })
    }

    /// Apply a ready report's safe changes in the background.
    pub fn begin_apply(
        &self,
        job_id: &str,
        purpose: Purpose,
        expected_row_revision: i64,
        confirmation: bool,
    ) -> Result<OperationJob, OperationError> {
        if !confirmation {
            return Err(OperationError::Invalid(reasons::CONFIRMATION_REQUIRED));
        }
        self.write(|tx| store::begin_apply(tx, job_id, purpose, expected_row_revision, now_unix()))
    }

    /// Dismiss a ready preparation report.
    pub fn discard(
        &self,
        job_id: &str,
        expected_row_revision: i64,
    ) -> Result<OperationJob, OperationError> {
        self.write(|tx| store::discard(tx, job_id, expected_row_revision, now_unix()))
    }

    /// Pause, resume, or stop a repair job.
    pub fn control(
        &self,
        job_id: &str,
        purpose: Purpose,
        control: Control,
        expected_row_revision: i64,
        idempotency_key: Option<&str>,
    ) -> Result<OperationJob, OperationError> {
        self.read(|conn| store::repair_job(conn, job_id, purpose).map(|_| ()))?;
        self.ops
            .control(job_id, control, expected_row_revision, idempotency_key)
    }

    // -----------------------------------------------------------------------
    // The worker.
    // -----------------------------------------------------------------------

    /// Queue the one-time catalog cleanup when this database has albums
    /// it could apply to (imported groupings, empty shells).
    fn queue_hygiene_once(&self) -> Result<(), OperationError> {
        if self.hygiene_checked.swap(true, Ordering::Relaxed) {
            return Ok(());
        }
        self.write(|tx| {
            if store::hygiene_backfill_queued(tx)? || !store::hygiene_candidates_exist(tx)? {
                return Ok(());
            }
            let new = NewRepair {
                job_id: self.ids.new_id(),
                purpose: Purpose::CatalogIdentityHygiene,
                requested_by_user_id: None,
                idempotency_key: store::HYGIENE_BACKFILL_KEY.to_owned(),
                root_ids: Vec::new(),
                album_ids: Vec::new(),
                source_matcher_version: None,
                target_matcher_version: HYGIENE_VERSION.to_owned(),
            };
            store::create(tx, &new, now_unix()).map(|_| ())
        })
    }

    /// Claim the next due repair job and work on it for one turn. Returns
    /// the job as it was left, or `None` when nothing was due. Store calls
    /// block: run this on a blocking thread.
    pub async fn run_next(&self, worker: &str) -> Result<Option<OperationJob>, OperationError> {
        self.queue_hygiene_once()?;
        let now = now_unix();
        let claimed = self.write(|tx| {
            jobs::recover_expired(tx, now)?;
            Ok(jobs::claim_kind(tx, OperationKind::Repair, worker, now)?)
        })?;
        let Some(job) = claimed else {
            return Ok(None);
        };
        let Some(snap) = self.read(|conn| Ok(store::snapshot(conn, &job.id)?))? else {
            return self.write(|tx| {
                Ok(store::end(
                    tx,
                    &job.id,
                    worker,
                    "failed",
                    "MISSING_SNAPSHOT",
                    now_unix(),
                )?)
            });
        };
        let actor = job.requested_by_user_id.clone();
        let started = Instant::now();
        loop {
            let scanning = !self.coordinator.current().is_empty();
            let halted = self.write(|tx| {
                let now = now_unix();
                if scanning {
                    return Ok(jobs::requeue_for_scan(tx, &job.id, worker, now)?);
                }
                if let Some(left) = jobs::checkpoint(tx, &job.id, worker, now)? {
                    return Ok(Some(left));
                }
                if started.elapsed() >= TURN {
                    return Ok(store::yield_job(tx, &job.id, worker, None, None, now)?);
                }
                Ok(None)
            })?;
            if let Some(left) = halted {
                return Ok(Some(left));
            }
            let next = self.write(|tx| {
                let now = now_unix();
                match store::claim_album(tx, &job.id, now)? {
                    Some(work) => {
                        jobs::extend_lease(tx, &job.id, worker, now)?;
                        Ok(Ok(work))
                    }
                    None => Ok(Err(self.finish(tx, &job.id, worker, &snap, now)?)),
                }
            })?;
            let (ordinal, album_id) = match next {
                Ok(work) => work,
                Err(left) => return Ok(left),
            };
            match (snap.purpose, snap.phase) {
                (Purpose::CatalogIdentityHygiene, _) => self.write(|tx| {
                    let now = now_unix();
                    let changed = hygiene::clean_album(tx, &album_id, &job.id, now)?;
                    let state = if changed.is_some() {
                        "succeeded"
                    } else {
                        "skipped"
                    };
                    store::finish_work(
                        tx,
                        &job.id,
                        worker,
                        ordinal,
                        state,
                        None,
                        changed.as_ref(),
                        now,
                    )?;
                    Ok(())
                })?,
                (_, Phase::Apply) => self.write(|tx| {
                    apply_album(
                        tx,
                        &job.id,
                        worker,
                        ordinal,
                        &album_id,
                        actor.as_deref(),
                        now_unix(),
                    )
                })?,
                (purpose, Phase::DryRun) => match self.check(purpose, &album_id).await? {
                    Checked::Found(verdict, identity_revision) => self.write(|tx| {
                        if !jobs::holds(tx, &job.id, worker)? {
                            return Ok(());
                        }
                        store::save_finding(
                            tx,
                            &job.id,
                            worker,
                            ordinal,
                            &album_id,
                            &verdict,
                            identity_revision,
                            now_unix(),
                        )
                    })?,
                    Checked::Deferred => {
                        return self.write(|tx| {
                            let now = now_unix();
                            Ok(store::yield_job(
                                tx,
                                &job.id,
                                worker,
                                Some("PROVIDER_DEFERRED"),
                                Some(now + PROVIDER_RETRY_SECS),
                                now,
                            )?)
                        });
                    }
                },
            }
        }
    }

    /// No album left: a check's report is ready; Apply and cleanup are done.
    fn finish(
        &self,
        tx: &Transaction<'_>,
        job_id: &str,
        worker: &str,
        snap: &RepairSnapshot,
        now: f64,
    ) -> Result<Option<OperationJob>, OperationError> {
        Ok(match (snap.purpose, snap.phase) {
            (Purpose::CatalogIdentityHygiene, _) => store::end(
                tx,
                job_id,
                worker,
                "succeeded",
                "CATALOG_IDENTITY_HYGIENE_COMPLETED",
                now,
            )?,
            (_, Phase::Apply) => {
                store::end(tx, job_id, worker, "succeeded", "APPLY_COMPLETED", now)?
            }
            (_, Phase::DryRun) => store::mark_ready(tx, job_id, worker, snap, now)?,
        })
    }

    fn facts(&self, album_id: &str) -> Option<LocalAlbumFacts> {
        self.store
            .album_facts(album_id)
            .filter(|facts| !facts.tracks.is_empty())
    }

    /// Check one album against MusicBrainz.
    async fn check(&self, purpose: Purpose, album_id: &str) -> Result<Checked, OperationError> {
        let subject = self.read(|conn| Ok(store::subject(conn, album_id)?))?;
        let Some(subject) = subject else {
            return Ok(Checked::Found(
                Box::new(Verdict::new("stale", "IDENTITY_CHANGED")),
                None,
            ));
        };
        let found = |mut verdict: Verdict| {
            verdict.payload.guard = Some(subject.guard.clone());
            Ok(Checked::Found(Box::new(verdict), subject.identity_revision))
        };
        // Folders kept to their own tags are never looked up.
        if subject.policies.iter().any(|policy| policy != "automatic") {
            return found(Verdict::new("unverifiable", "LOCAL_METADATA_POLICY"));
        }
        if purpose == Purpose::ManagementReadiness
            && let Some(current) = subject.custom_current
        {
            return found(if current {
                Verdict::new("ready", "CUSTOM_EDITION_MANIFEST_VERIFIED")
            } else {
                Verdict::new("needs_review", "CUSTOM_MANIFEST_STALE")
            });
        }
        let identity = subject.identity.clone();
        if purpose == Purpose::ExistingMatches {
            match &identity {
                None => return found(Verdict::new("stale", "IDENTITY_CHANGED")),
                Some(identity) if identity.manual => {
                    return found(classify::existing_match(identity, None, 0, 0));
                }
                Some(identity) if identity.release_mbid.is_none() => {
                    return found(Verdict::new("unverifiable", "EVIDENCE_UNAVAILABLE"));
                }
                Some(_) => {}
            }
        }
        let Some(facts) = self.facts(album_id) else {
            return Ok(Checked::Found(
                Box::new(Verdict::new("stale", "IDENTITY_CHANGED")),
                None,
            ));
        };
        let release = identity.as_ref().and_then(|id| id.release_mbid.clone());
        let recall = self.identify.recall(&facts, release.as_deref()).await;
        if recall.provider_deferred {
            return Ok(Checked::Deferred);
        }
        let ranking = self.identify.rank(&facts, &recall);
        let candidates = evaluation(&facts, &recall, &ranking, false).candidates;
        let identity = identity.unwrap_or_default();
        let verdict = match release {
            Some(release) => {
                let held = recall
                    .releases
                    .iter()
                    .find(|candidate| candidate.answers_to(&release));
                let candidate = held.and_then(|held| {
                    candidates.iter().find(|candidate| {
                        candidate
                            .evidence
                            .release_mbid
                            .as_deref()
                            .is_some_and(|id| id.eq_ignore_ascii_case(&held.id))
                    })
                });
                if purpose == Purpose::ExistingMatches {
                    classify::existing_match(
                        &identity,
                        candidate,
                        held.map_or(0, |held| held.tracks.len()),
                        facts.tracks.len(),
                    )
                } else {
                    classify::readiness_exact(
                        &identity,
                        candidate,
                        &subject.tracks,
                        &recall.recording_aliases,
                    )
                }
            }
            None => classify::readiness_suggest(&identity, &candidates, |candidate| {
                summary_for(&recall, candidate)
            }),
        };
        found(verdict)
    }
}

/// What the readiness report shows about a suggested edition.
fn summary_for(recall: &RecallResult, candidate: &ReidentificationCandidate) -> EditionSummary {
    let release = candidate.evidence.release_mbid.as_deref().and_then(|id| {
        recall
            .releases
            .iter()
            .find(|release| release.answers_to(id))
    });
    EditionSummary {
        title: release.map_or_else(
            || candidate.evidence.album_title.clone(),
            |release| release.title.clone(),
        ),
        date: release
            .and_then(|release| release.date.clone())
            .or_else(|| candidate.release_date.clone()),
        country: release.and_then(|release| release.country.clone()),
        status: release.and_then(|release| release.status.clone()),
        track_count: release.map_or(
            (candidate.tracks.len() + candidate.unmatched_expected_tracks.len()) as u32,
            |release| release.tracks.len() as u32,
        ),
        competing_count: 1,
    }
}

fn dedupe(ids: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    ids.into_iter()
        .map(|id| id.trim().to_owned())
        .filter(|id| !id.is_empty() && seen.insert(id.clone()))
        .collect()
}

/// A `"<float>:<id>"` page cursor.
fn parse_cursor(raw: &str) -> Result<(f64, String), OperationError> {
    raw.split_once(':')
        .and_then(|(at, id)| Some((at.parse::<f64>().ok()?, id.to_owned())))
        .filter(|(at, id)| at.is_finite() && !id.is_empty())
        .ok_or(OperationError::Invalid(reasons::CURSOR_INVALID))
}

/// A repair job's report summary, for the generic operation view. Blocking.
pub fn repair_summary(setup: &LibrarySetup, job_id: &str) -> Option<Summary> {
    let store = setup.identify_store.clone();
    store.with_connection(|conn| match store::snapshot(conn, job_id) {
        Ok(snap) => snap.and_then(|snap| snap.summary),
        Err(error) => {
            tracing::warn!(%error, job_id, "repair summary unreadable");
            None
        }
    })
}
