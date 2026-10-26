//! The identify service: queue jobs in, identities or reviews out.
//!
//! One attempt recalls candidate releases (with tracklists) through the
//! provider seam, scores each with the matching engine, and applies the
//! verdict through the overwrite rules. Curator rows always win; ambiguous
//! and weak matches become reviews with their distances; contradicting
//! ids retract automatic identities.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use droppedneedle_kernel::db::Lane;
use droppedneedle_kernel::jobs::schedule::SplitMix64;
use droppedneedle_providers::RequestPriority;
use tokio::time::Instant;

use super::evidence::{candidate_evidence, edition_ids, local_album, named_group};
use super::models::{
    AlbumIdentity, AutomaticSeal, CandidateEvidence, DecisionSource, EvidenceClass,
    IdentificationOutcome, IdentifyJob, IdentifyKind, IdentityBrief, JobState, LocalAlbumFacts,
    MatchFlag, MatchFlagState, RecallResult, ReviewState, TrackIdentity,
};
use super::providers::{
    IdentifyProviders, RELEASE_UNREADABLE, RELEASES_UNREADABLE, RecallOutcome,
    TAGGED_RELEASE_UNREADABLE,
};
use super::queue::{PRIORITY_HISTORICAL_BACKLOG, PRIORITY_NEW_OR_CHANGED, PRIORITY_REVIEW_RETRY};
use super::review::reject_review;
use super::rules::retracts_on_contradiction;
use super::sources::{EditionPage, EditionQuery, ReleaseAnswer, SourceError};
use super::sqlite::SqliteIdentifyStore;
use super::stores::{Approval, AttemptLanding, StoreError, keep_releases, land_job};
use crate::edition_prefs::{LivePreferences, Preferences};
use crate::identity::AutomaticOutcome;
use crate::matching::decide::{
    EDITION_COHORT, IdStance, candidate_order, eligible, hint_misses, id_stance, named_in_tie,
};
use crate::matching::score::heard_on_candidates;
use crate::matching::{
    EditionHints, EditionPrefs, LocalAlbum, Release, ReleaseMatch, ReviewReason, Verdict, decide,
    match_release,
};

/// Every dependency the identify service needs, injected by constructor.
pub struct IdentifyDeps {
    pub store: Arc<SqliteIdentifyStore>,
    pub providers: Arc<dyn IdentifyProviders>,
}

pub struct IdentifyService {
    deps: IdentifyDeps,
    /// The saved edition preferences, read per attempt; the defaults when
    /// none are given.
    preferences: Option<LivePreferences>,
    /// When artist id lookups may run again after MusicBrainz could not
    /// answer. In memory only: a restart asks again at once.
    artist_hold: Mutex<LookupHold>,
    /// The same for release document lookups.
    document_hold: Mutex<LookupHold>,
}

/// The wait after a lookup MusicBrainz could not answer.
struct LookupHold {
    until: Option<Instant>,
    failures: u32,
    rng: SplitMix64,
    /// The id the last lookup that went unanswered asked about.
    stuck: Option<String>,
}

/// The first wait after a failed lookup with no `Retry-After`; it doubles
/// with each failure in a row, up to [`ARTIST_HOLD_CAP`].
const ARTIST_HOLD: Duration = Duration::from_secs(5 * 60);
const ARTIST_HOLD_CAP: Duration = Duration::from_secs(3600);

impl LookupHold {
    fn new() -> Self {
        Self {
            until: None,
            failures: 0,
            rng: SplitMix64::new(SplitMix64::seed_from_time()),
            stuck: None,
        }
    }

    fn held(&self) -> bool {
        self.until.is_some_and(|until| Instant::now() < until)
    }

    fn clear(&mut self) {
        self.until = None;
        self.failures = 0;
        self.stuck = None;
    }

    /// Hold lookups back after one went unanswered: for the `Retry-After`
    /// it gave, else 5 minutes doubling to an hour, with up to 20% more.
    fn hold(&mut self, retry_after_secs: Option<f64>) {
        let base = match retry_after_secs.filter(|secs| secs.is_finite() && *secs > 0.0) {
            Some(secs) => Duration::from_secs_f64(secs.min(ARTIST_HOLD_CAP.as_secs_f64())),
            None => ARTIST_HOLD
                .saturating_mul(1 << self.failures.min(4))
                .min(ARTIST_HOLD_CAP),
        };
        let jitter = self.rng.below_duration(base / 5);
        self.failures = self.failures.saturating_add(1);
        self.until = Some(Instant::now() + base + jitter);
    }
}

fn lock(hold: &Mutex<LookupHold>) -> std::sync::MutexGuard<'_, LookupHold> {
    hold.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// What one pass of [`IdentifyService::fetch_waiting_documents`] did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DocumentPass {
    /// Documents fetched and kept.
    pub fetched: usize,
    /// Releases MusicBrainz does not have.
    pub not_found: usize,
    /// Releases whose document MusicBrainz refused for good.
    pub refused: usize,
}

impl IdentifyService {
    pub fn new(deps: IdentifyDeps) -> Self {
        Self {
            deps,
            preferences: None,
            artist_hold: Mutex::new(LookupHold::new()),
            document_hold: Mutex::new(LookupHold::new()),
        }
    }

    /// Break ties between equally fitting editions with these saved
    /// preferences (read on every attempt).
    #[must_use]
    pub fn with_preferences(mut self, preferences: LivePreferences) -> Self {
        self.preferences = Some(preferences);
        self
    }

    /// Enqueue one album, on the caller's lane. Fresh and changed work
    /// runs first, curator retries next, historical backlog last.
    #[allow(clippy::too_many_arguments)]
    pub fn enqueue_album(
        &self,
        job_id: &str,
        local_album_id: &str,
        kind: IdentifyKind,
        input_revision: &str,
        requested_by_user_id: Option<&str>,
        now_ms: u64,
        lane: Lane,
    ) -> Result<IdentifyJob, StoreError> {
        let priority = match kind {
            IdentifyKind::Automatic => PRIORITY_NEW_OR_CHANGED,
            IdentifyKind::Manual => PRIORITY_REVIEW_RETRY,
            IdentifyKind::Historical => PRIORITY_HISTORICAL_BACKLOG,
        };
        let job = IdentifyJob {
            id: job_id.to_owned(),
            local_album_id: local_album_id.to_owned(),
            kind,
            priority,
            state: JobState::Queued,
            attempts: 0,
            not_before_ms: now_ms,
            input_revision: input_revision.to_owned(),
            requested_by_user_id: requested_by_user_id.map(str::to_owned),
            failure_code: None,
            lease_expires_ms: None,
        };
        self.deps.store.enqueue(job, lane)
    }

    /// Run the claimed job `job_id` to a landing, under the lease the job
    /// holds now ([`run_claimed`](Self::run_claimed)). `None` when the job
    /// is gone.
    pub async fn run_claimed_job(
        &self,
        job_id: &str,
        now_ms: u64,
    ) -> Result<Option<AttemptReport>, StoreError> {
        let Some(job) = self.deps.store.job(job_id)? else {
            return Ok(None);
        };
        self.run_claimed(job, now_ms).await
    }

    /// Run one claimed job to a terminal landing. Async only because the
    /// provider seam is async; every rule inside is sync and pure. Every
    /// landing names the lease `job` was claimed with, and what the attempt
    /// writes lands in the same transaction. `None` when the job went back
    /// to the queue, or another worker took it over after its lease
    /// lapsed (nothing of this attempt is written). A store error leaves
    /// the job where it was: the caller decides what happens to it.
    pub async fn run_claimed(
        &self,
        mut job: IdentifyJob,
        now_ms: u64,
    ) -> Result<Option<AttemptReport>, StoreError> {
        let Some(facts) = self.deps.store.album_facts(&job.local_album_id)? else {
            // The album left the catalog: nothing to identify.
            land_job(&mut job, AttemptLanding::Failed, now_ms, Some("ALBUM_GONE"));
            return self.land(job, IdentificationOutcome::Failed, "ALBUM_GONE");
        };
        let current = self.deps.store.album_identity(&facts.local_album_id)?;
        // A person's choice is final: automatic passes never look again.
        if current
            .as_ref()
            .is_some_and(|row| row.decision_source.is_persons_choice())
        {
            land_job(&mut job, AttemptLanding::Done, now_ms, None);
            return self.land(
                job,
                IdentificationOutcome::Identified,
                "CHOSEN_EDITION_KEPT",
            );
        }
        // Preferences that do not read put the job back as it was, with no
        // attempt spent; the loop claims nothing until they read again.
        let Some(preferences) = self.preferences() else {
            job.state = JobState::Queued;
            self.deps.store.land(&job)?;
            return Ok(None);
        };
        let named = named_group(&facts, current.as_ref());
        let RecallOutcome {
            result: mut recall, ..
        } = self
            .deps
            .providers
            .recall_candidates(&facts, named.as_deref(), RequestPriority::BackgroundSync)
            .await;
        // The edition the album already has is always weighed, so a recall
        // that missed it cannot flip it.
        let held = current
            .as_ref()
            .and_then(|row| row.release_mbid.clone())
            .filter(|_| !recall.provider_deferred);
        if let Some(held) = held.as_deref()
            && !recall
                .releases
                .iter()
                .any(|release| release.answers_to(held))
        {
            let extra = self
                .deps
                .providers
                .recall_release(&facts, held, RequestPriority::BackgroundSync)
                .await;
            // Without the held edition `keep` cannot hold the album, so a
            // held release MusicBrainz cannot answer for defers the attempt
            // as a busy recall does. A stale stored copy is not used: the
            // move rules compare against what the held edition states now.
            // One it answers in a form asking again will not change ends
            // the attempt at once in Attention, where the Scanning panel
            // counts it: the album keeps its edition.
            if extra.result.provider_deferred {
                recall.provider_deferred = true;
                recall.failure_code = extra.result.failure_code;
            } else if extra.result.failure_code.as_deref() == Some(RELEASE_UNREADABLE) {
                land_job(
                    &mut job,
                    AttemptLanding::Attention,
                    now_ms,
                    Some(HELD_RELEASE_UNREADABLE),
                );
                return self.land(job, IdentificationOutcome::Failed, HELD_RELEASE_UNREADABLE);
            } else {
                recall.releases.extend(extra.result.releases);
                // The merged recording ids the held release's files need
                // count against every candidate.
                for (retired, kept) in extra.result.recording_aliases {
                    recall.recording_aliases.entry(retired).or_insert(kept);
                }
            }
        }
        if recall.provider_deferred {
            land_job(
                &mut job,
                AttemptLanding::Deferred,
                now_ms,
                recall.failure_code.as_deref(),
            );
            return self.land(
                job,
                IdentificationOutcome::ProviderDeferred,
                "PROVIDER_DEFERRED",
            );
        }
        // Every candidate's document stays on file: the identity's release
        // for tagging, the others because a curator may approve them.
        keep_releases(&self.deps.store, recall.releases.clone(), Lane::Background).await?;
        // A guess sealed because the files named another group stays a
        // guess while nothing names a group.
        let doubted = match current.as_ref() {
            Some(row) if row.decision_source == DecisionSource::Automatic => self
                .deps
                .store
                .match_flag(&facts.local_album_id)?
                .filter(|flag| flag.reason_code == NAMED_GROUP_DIFFERS)
                .and(row.release_group_mbid.clone()),
            _ => None,
        };
        // An administrator who undid an automatic edition of the album's
        // group got the edition they wanted back: it is not moved again.
        let may_move = match current.as_ref() {
            Some(row) if held.is_some() => match row.release_group_mbid.as_deref() {
                Some(group) => !self
                    .deps
                    .store
                    .edition_change_undone(&facts.local_album_id, group)?,
                None => false,
            },
            _ => false,
        };
        let (ranking, decision) = self.ranked(
            &facts,
            &recall,
            Held {
                release: held.as_deref(),
                doubted_group: doubted.as_deref(),
                may_move,
            },
            &preferences,
        );
        // A release MusicBrainz refused may be the one the files are: with
        // it missing, matching nothing is not evidence, so the job waits as
        // for an outage instead of marking the album unmatched.
        let refused = matches!(
            recall.failure_code.as_deref(),
            Some(RELEASES_UNREADABLE | TAGGED_RELEASE_UNREADABLE)
        );
        if refused && matches!(decision, Decision::Terminal(..) | Decision::Contradictory) {
            land_job(
                &mut job,
                AttemptLanding::Deferred,
                now_ms,
                Some(RELEASES_UNREADABLE),
            );
            return self.land(
                job,
                IdentificationOutcome::ProviderDeferred,
                "PROVIDER_DEFERRED",
            );
        }
        self.apply_decision(job, &facts, &ranking, decision, now_ms)
    }

    /// Land `job` with nothing else written, and report it. `None` when
    /// its lease was lost.
    fn land(
        &self,
        job: IdentifyJob,
        outcome: IdentificationOutcome,
        reason: &str,
    ) -> Result<Option<AttemptReport>, StoreError> {
        if !self.deps.store.land(&job)? {
            return Ok(None);
        }
        Ok(Some(AttemptReport {
            job,
            outcome,
            reason_code: reason.to_owned(),
            review_id: None,
        }))
    }

    /// Recall candidates for one album outside the queue: every release
    /// the usual recall finds, or only `exact_release` when an
    /// administrator named one. MusicBrainz is asked at the caller's
    /// `priority`. Fetched documents stay on file, as for a queued
    /// attempt, written on the caller's lane.
    pub async fn recall(
        &self,
        facts: &LocalAlbumFacts,
        exact_release: Option<&str>,
        lane: Lane,
        priority: RequestPriority,
    ) -> Result<RecallResult, StoreError> {
        let outcome = match exact_release {
            Some(mbid) => {
                self.deps
                    .providers
                    .recall_release(facts, mbid, priority)
                    .await
            }
            None => {
                let current = self.deps.store.album_identity(&facts.local_album_id)?;
                let named = named_group(facts, current.as_ref());
                self.deps
                    .providers
                    .recall_candidates(facts, named.as_deref(), priority)
                    .await
            }
        };
        if !outcome.result.provider_deferred {
            keep_releases(&self.deps.store, outcome.result.releases.clone(), lane).await?;
        }
        Ok(outcome.result)
    }

    /// Ask MusicBrainz about up to `limit` artist ids waiting for an
    /// answer, newest first, and write each answer in its own transaction.
    /// A lookup that gets no answer stops the batch and holds lookups back
    /// for the `Retry-After` it gave, else 5 minutes doubling to an hour,
    /// each with up to 20% more; that id is asked last in the next batch,
    /// so it does not hold up the others. Returns how many ids were
    /// answered. An answer that cannot be written holds lookups back too,
    /// and only a written answer ends a hold.
    pub async fn resolve_merged_artist_ids(&self, limit: usize) -> Result<usize, StoreError> {
        if lock(&self.artist_hold).held() {
            return Ok(0);
        }
        let mut answered = 0;
        let mut pending = self.deps.store.pending_artist_ids(limit)?;
        let stuck = lock(&self.artist_hold).stuck.clone();
        if let Some(stuck) = stuck
            && let Some(at) = pending.iter().position(|mbid| *mbid == stuck)
        {
            let id = pending.remove(at);
            pending.push(id);
        }
        for mbid in pending {
            match self.deps.providers.canonical_artist(&mbid).await {
                Ok(answer) => {
                    if let Err(error) = self.deps.store.answer_artist_id(&mbid, &answer) {
                        lock(&self.artist_hold).hold(None);
                        return Err(error);
                    }
                    lock(&self.artist_hold).clear();
                    answered += 1;
                }
                Err(unanswered) => {
                    let mut hold = lock(&self.artist_hold);
                    hold.hold(unanswered.retry_after_secs);
                    hold.stuck = Some(mbid.clone());
                    drop(hold);
                    tracing::debug!(
                        artist_mbid = mbid,
                        error = unanswered.error.0,
                        "MusicBrainz could not answer about an artist id; lookups wait"
                    );
                    break;
                }
            }
        }
        Ok(answered)
    }

    /// Fetch up to `limit` release documents that albums with an accepted
    /// match wait for (an upgrade carries matches but no documents), the
    /// longest wait first, and keep each one, which sends its albums
    /// through the next artist link sweep. A release MusicBrainz does not
    /// have, or refuses in a way asking again will not change, is recorded
    /// on its albums so it is not asked for again, and the pass goes on; an
    /// album whose release is gone is offered for identification again. A
    /// lookup that gets no answer stops the pass and holds these lookups
    /// back the way artist id lookups are held. A busy database stops the
    /// pass and the next one tries again.
    pub async fn fetch_waiting_documents(&self, limit: usize) -> Result<DocumentPass, StoreError> {
        let mut pass = DocumentPass::default();
        if lock(&self.document_hold).held() {
            return Ok(pass);
        }
        let waiting = match self.deps.store.releases_waiting_for_documents(limit) {
            Ok(waiting) => waiting,
            Err(StoreError::Busy(_)) => return Ok(pass),
            Err(error) => return Err(error),
        };
        for mbid in waiting {
            match self.deps.providers.release_document(&mbid).await {
                Ok(answer) => {
                    lock(&self.document_hold).clear();
                    let kept = match answer {
                        ReleaseAnswer::Found(release) => {
                            keep_releases(&self.deps.store, vec![*release], Lane::Background)
                                .await
                                .map(|()| pass.fetched += 1)
                        }
                        ReleaseAnswer::Missing => self
                            .deps
                            .store
                            .release_not_found(&mbid)
                            .map(|_| pass.not_found += 1),
                        ReleaseAnswer::Refused(reason) => {
                            tracing::info!(
                                release_mbid = mbid,
                                reason,
                                "MusicBrainz refused a release document; its albums stop asking"
                            );
                            self.deps
                                .store
                                .release_refused(&mbid)
                                .map(|_| pass.refused += 1)
                        }
                    };
                    match kept {
                        Ok(()) => {}
                        Err(StoreError::Busy(_)) => return Ok(pass),
                        Err(error) => {
                            lock(&self.document_hold).hold(None);
                            return Err(error);
                        }
                    }
                }
                Err(unanswered) => {
                    lock(&self.document_hold).hold(unanswered.retry_after_secs);
                    tracing::debug!(
                        release_mbid = mbid,
                        error = unanswered.error.0,
                        "MusicBrainz could not answer about a release; document lookups wait"
                    );
                    break;
                }
            }
        }
        Ok(pass)
    }

    /// One page of MusicBrainz releases for a curator choosing an edition,
    /// asked at the caller's `priority`.
    pub async fn search_editions(
        &self,
        query: EditionQuery,
        priority: RequestPriority,
    ) -> Result<EditionPage, SourceError> {
        self.deps.providers.search_editions(query, priority).await
    }

    /// Score every recalled release and decide, the way a queued attempt
    /// does, without sealing anything. `None` while the edition
    /// preferences do not read.
    pub fn rank(&self, facts: &LocalAlbumFacts, recall: &RecallResult) -> Option<Ranking> {
        let preferences = self.preferences()?;
        let (ranking, _) = self.ranked(facts, recall, Held::default(), &preferences);
        Some(ranking)
    }

    /// The edition preferences now; the defaults only when none are wired
    /// (tests), `None` while the saved ones do not read.
    fn preferences(&self) -> Option<Preferences> {
        match &self.preferences {
            Some(live) => live.current(),
            None => Some(Preferences::default()),
        }
    }

    /// Score every recalled release and decide. Candidates come back
    /// chosen edition first, then by distance and edition order (see
    /// `candidate_order`), so of two equally close guesses the one the
    /// files complete comes first. `held.release` is the edition the album
    /// already has: it stays unless another edition of its group fits the
    /// files better by more than the edition cohort, or is confirmed as the
    /// edition the files' barcode or catalog number names (see
    /// [`moves_to_named_edition`]).
    fn ranked(
        &self,
        facts: &LocalAlbumFacts,
        recall: &RecallResult,
        held: Held<'_>,
        preferences: &Preferences,
    ) -> (Ranking, Decision) {
        let local = local_album(
            facts,
            &heard_on_candidates(&recall.fingerprint_support, &recall.releases),
        );
        let matches: Vec<_> = recall
            .releases
            .iter()
            .map(|release| match_release(&local, release, &recall.recording_aliases))
            .collect();
        let tagged = local.tagged_release();
        let named = recall.named_group.as_deref();
        let prefs = EditionPrefs {
            tagged: tagged.as_deref(),
            preferences: Some(preferences),
            named_group: named,
        };
        let in_group = |index: usize, group: &str| {
            recall.releases[index]
                .release_group_id
                .eq_ignore_ascii_case(group)
        };
        let verdict = decide(&local, &recall.releases, &matches, prefs);
        let ids = edition_ids(facts);
        let keep = |index: usize, confirmed: bool| -> usize {
            let Some(held_release) = held.release else {
                return index;
            };
            let chosen = &recall.releases[index];
            let Some(kept) = recall
                .releases
                .iter()
                .position(|release| release.answers_to(held_release))
                .filter(|kept| {
                    let release = &recall.releases[*kept];
                    let matched = &matches[*kept];
                    release
                        .release_group_id
                        .eq_ignore_ascii_case(&chosen.release_group_id)
                        && matched.conflicts.is_empty()
                        && matched.names_agree
                        && matched.library_distance()
                            <= matches[index].library_distance() + EDITION_COHORT
                })
            else {
                return index;
            };
            let moves = confirmed
                && held.may_move
                && kept != index
                && moves_to_named_edition(&local, &ids, &recall.releases, kept, index);
            if moves {
                tracing::info!(
                    album = facts.local_album_id,
                    from = recall.releases[kept].id,
                    to = chosen.id,
                    "the files' barcode or catalog number name another edition; the album moves to it"
                );
                index
            } else {
                kept
            }
        };
        // Without the release the files' tags name, nothing is confirmed:
        // the pick is a guess and never moves the album to another edition.
        let tag_unreadable = recall.failure_code.as_deref() == Some(TAGGED_RELEASE_UNREADABLE);
        let verdict = match verdict {
            Verdict::Identified(index) => Verdict::Identified(keep(index, !tag_unreadable)),
            Verdict::EditionUncertain(index) => Verdict::EditionUncertain(keep(index, false)),
            Verdict::OutsideNamedGroup(index) => Verdict::OutsideNamedGroup(keep(index, false)),
            Verdict::AudioDisagrees(index) => Verdict::AudioDisagrees(keep(index, false)),
            other => other,
        };
        let lead = match verdict {
            Verdict::Identified(index)
            | Verdict::EditionUncertain(index)
            | Verdict::OutsideNamedGroup(index)
            | Verdict::AudioDisagrees(index) => Some(index),
            _ => None,
        };
        let mut order: Vec<usize> = (0..recall.releases.len()).collect();
        order.sort_by(|a, b| {
            (Some(*b) == lead)
                .cmp(&(Some(*a) == lead))
                .then_with(|| candidate_order(*a, *b, &local, &recall.releases, &matches, prefs))
        });
        let scored: Vec<CandidateEvidence> = order
            .iter()
            .map(|index| candidate_evidence(&local, &recall.releases[*index], &matches[*index]))
            .collect();
        // The best guess: the lead, else the closest candidate nothing
        // rules out (no vetoed file, names agree, within the review
        // ceiling). A weak match where the group the files name won its
        // tie guesses within that group. Without one the album matches
        // nothing.
        let plausible = |index: usize| {
            let matched = &matches[index];
            eligible(matched) && matched.names_agree && !matched.pairs.is_empty()
        };
        let named_weak = named.filter(|named| {
            verdict == Verdict::Review(ReviewReason::WeakMatch)
                && named_in_tie(&matches, &recall.releases, named)
        });
        let guess = named_weak
            .and_then(|named| {
                order
                    .iter()
                    .position(|index| in_group(*index, named) && plausible(*index))
            })
            .or_else(|| {
                order
                    .iter()
                    .position(|index| Some(*index) == lead || plausible(*index))
            });
        let decision = match verdict {
            Verdict::Identified(_) | Verdict::EditionUncertain(_) if tag_unreadable => {
                Decision::Ambiguous(TAGGED_RELEASE_UNREADABLE.to_owned())
            }
            Verdict::Identified(_) => Decision::Identified(0),
            Verdict::EditionUncertain(_) => Decision::EditionUncertain,
            Verdict::Review(reason) => Decision::Ambiguous(reason.code().to_owned()),
            Verdict::OutsideNamedGroup(_) => Decision::Ambiguous(NAMED_GROUP_DIFFERS.to_owned()),
            Verdict::AudioDisagrees(_) => Decision::Ambiguous(AUDIO_DISAGREES.to_owned()),
            Verdict::Contradictory => Decision::Contradictory,
            Verdict::Insufficient => Decision::Terminal(
                IdentificationOutcome::InsufficientEvidence,
                "INSUFFICIENT_EVIDENCE".to_owned(),
            ),
            Verdict::NoCandidate => Decision::Terminal(
                IdentificationOutcome::NoCandidate,
                "NO_CANDIDATE".to_owned(),
            ),
        };
        // A pick in the group of a guess sealed because the files named
        // another group stays that guess, whatever else this pass found,
        // while nothing names a group.
        let doubted = named.is_none()
            && held.doubted_group.is_some_and(|group| {
                guess
                    .and_then(|index| order.get(index))
                    .is_some_and(|index| in_group(*index, group))
            });
        let decision = if doubted {
            Decision::Ambiguous(NAMED_GROUP_DIFFERS.to_owned())
        } else {
            decision
        };
        let ranking = Ranking {
            local,
            order,
            matches,
            candidates: scored,
            outcome: decision.outcome(),
            reason_code: decision.reason_code(),
            lead_identified: matches!(decision, Decision::Identified(_)),
            guess,
            named_group: recall.named_group.clone(),
        };
        (ranking, decision)
    }

    /// Apply a decision and land the job with it, in one transaction.
    /// Always pick: a confident match seals; a close call, weak evidence or
    /// a lone candidate seals the best guess with an "unconfirmed" flag;
    /// only when every candidate is ruled out does the album keep its own
    /// tags, flagged unmatched with the closest candidates. Nothing waits
    /// in a blocking review. Files that changed since the facts were read
    /// get nothing written: the job lands as `INPUT_MOVED` and the album
    /// is identified again.
    fn apply_decision(
        &self,
        mut job: IdentifyJob,
        facts: &LocalAlbumFacts,
        ranking: &Ranking,
        decision: Decision,
        now_ms: u64,
    ) -> Result<Option<AttemptReport>, StoreError> {
        let scored = &ranking.candidates;
        let album = facts.local_album_id.as_str();
        let current = self.deps.store.album_identity(album)?;
        let outcome = decision.outcome();
        let reason_code = decision.reason_code();
        let closest: Vec<CandidateEvidence> = scored.iter().take(CLOSEST).cloned().collect();
        land_job(&mut job, AttemptLanding::Done, now_ms, None);
        let report = |landed: Option<(AutomaticOutcome, IdentifyJob)>, reason: &str| {
            landed.map(|(written, job)| {
                if written == AutomaticOutcome::InputMoved {
                    return AttemptReport {
                        job,
                        outcome: IdentificationOutcome::Failed,
                        reason_code: "INPUT_MOVED".to_owned(),
                        review_id: None,
                    };
                }
                if written == AutomaticOutcome::PersonsChoice {
                    tracing::info!(
                        album,
                        "a curator decided this album meanwhile; automatic seal skipped"
                    );
                }
                AttemptReport {
                    job,
                    outcome,
                    reason_code: reason.to_owned(),
                    review_id: None,
                }
            })
        };
        if let Decision::Identified(index) = decision {
            let landed = self.seal_automatic(&job, facts, &scored[index], None)?;
            return Ok(report(landed, "SUPPORTED"));
        }
        // A confirmed automatic edition stays unless the new evidence rules
        // it out: a weaker pass never demotes or replaces it. One outside
        // the group the files name is ruled out as a confirmed match.
        let confirmed_exact = match current.as_ref() {
            Some(row)
                if row.decision_source.is_revisable()
                    && ranking.named_group.as_deref().is_none_or(|named| {
                        row.release_group_mbid
                            .as_deref()
                            .is_some_and(|group| group.eq_ignore_ascii_case(named))
                    })
                    && self.deps.store.match_flag(album)?.is_none() =>
            {
                row.release_mbid.clone()
            }
            _ => None,
        };
        if let Some(held) = confirmed_exact.as_deref()
            && !ruled_out(ranking, held)
        {
            return self.land(job, outcome, "QUIET_RECONFIRM");
        }
        if let Some(guess) = ranking.guess.and_then(|index| scored.get(index)) {
            let landed = self.seal_automatic(
                &job,
                facts,
                guess,
                Some(MatchFlag {
                    state: MatchFlagState::Unconfirmed,
                    reason_code: reason_code.clone(),
                    release_mbid: guess.release_mbid.clone(),
                    candidates: closest,
                }),
            )?;
            return Ok(report(landed, &reason_code));
        }
        let contradicted = matches!(decision, Decision::Contradictory)
            && retracts_on_contradiction(
                current.as_ref().map(|row| row.decision_source),
                IdentificationOutcome::Contradictory,
            );
        let landed = self.deps.store.retract_and_land(
            album,
            contradicted,
            &MatchFlag {
                state: MatchFlagState::Unmatched,
                reason_code: reason_code.clone(),
                release_mbid: None,
                candidates: closest,
            },
            &facts.input_revision,
            &job,
        )?;
        Ok(report(landed, &reason_code))
    }

    /// Seal an automatic win and land `job` with it: album row and
    /// supported track rows (curator tracks keep theirs). The store writes
    /// them together, never over a curator's album row, and an exact
    /// edition keeps what it replaced so an administrator can undo it.
    fn seal_automatic(
        &self,
        job: &IdentifyJob,
        facts: &LocalAlbumFacts,
        winner: &CandidateEvidence,
        flag: Option<MatchFlag>,
    ) -> Result<Option<(AutomaticOutcome, IdentifyJob)>, StoreError> {
        let tracks = winner
            .track_evidence
            .iter()
            .filter(|track| track.classification == EvidenceClass::Supported)
            .filter_map(|track| {
                Some(TrackIdentity {
                    local_track_id: track.local_track_id.clone(),
                    provider: "musicbrainz".to_owned(),
                    recording_mbid: Some(track.recording_mbid.clone()?),
                    release_track_mbid: track.release_track_mbid.clone(),
                    decision_source: DecisionSource::Automatic,
                    row_revision: 1,
                })
            })
            .collect();
        self.deps.store.seal_and_land(
            &AutomaticSeal {
                local_album_id: facts.local_album_id.clone(),
                release_group_mbid: winner.release_group_mbid.clone(),
                release_mbid: winner.release_mbid.clone(),
                tracks,
                flag,
            },
            &facts.input_revision,
            job,
        )
    }

    /// A curator approves a review: the chosen candidate becomes the
    /// album's match as their choice, which later automatic passes can
    /// never overwrite, and the files are placed on its release in the
    /// background. The review settles with it, together or not at all.
    /// `Ok(false)` for an unknown review or candidate, or a settled review.
    pub fn approve_candidate(
        &self,
        review_id: &str,
        by_user_id: &str,
        candidate_key: &str,
    ) -> Result<bool, StoreError> {
        let Some(review) = self.deps.store.get(review_id)? else {
            return Ok(false);
        };
        if review.state != ReviewState::Pending {
            return Ok(false);
        }
        let Some(winner) = review
            .candidates
            .iter()
            .find(|c| c.candidate_key == candidate_key)
        else {
            return Ok(false);
        };
        self.deps.store.approve(&Approval {
            review_id: review_id.to_owned(),
            by_user_id: by_user_id.to_owned(),
            candidate_key: candidate_key.to_owned(),
            album: AlbumIdentity {
                local_album_id: review.local_album_id.clone(),
                provider: "musicbrainz".to_owned(),
                release_group_mbid: Some(winner.release_group_mbid.clone()),
                release_mbid: winner.release_mbid.clone(),
                decision_source: DecisionSource::Manual,
                row_revision: 1,
            },
        })
    }

    /// A curator rejects a review: the album keeps its tags, nothing seals.
    pub fn reject_candidates(&self, review_id: &str, by_user_id: &str) -> Result<bool, StoreError> {
        reject_review(self.deps.store.as_ref(), review_id, by_user_id)
    }

    /// One card with the whole case for an album.
    pub fn identity_brief(&self, local_album_id: &str) -> Result<IdentityBrief, StoreError> {
        let identity = self.deps.store.album_identity(local_album_id)?;
        let pending = self.deps.store.pending_for_album(local_album_id)?;
        let revisable = identity
            .as_ref()
            .is_none_or(|row| row.decision_source.is_revisable());
        Ok(IdentityBrief {
            local_album_id: local_album_id.to_owned(),
            protected_source: identity.as_ref().and_then(|row| {
                (!row.decision_source.is_revisable()).then_some(row.decision_source)
            }),
            reason_code: pending
                .first()
                .map(|review| review.reason_code.clone())
                .unwrap_or_else(|| {
                    if identity.is_some() {
                        "IDENTIFIED".to_owned()
                    } else {
                        "NO_IDENTITY".to_owned()
                    }
                }),
            outcome: None,
            pending_review_id: pending.first().map(|review| review.id.clone()),
            candidates: pending
                .first()
                .map(|review| review.candidates.clone())
                .unwrap_or_default(),
            aliases: self.deps.store.aliases_for(local_album_id)?,
            identity,
            revisable,
        })
    }

    /// Pending reviews for one album, oldest filing first.
    pub fn pending_reviews(
        &self,
        local_album_id: &str,
    ) -> Result<Vec<super::models::ReviewItem>, StoreError> {
        self.deps.store.pending_for_album(local_album_id)
    }

    /// Review states still awaiting a curator.
    pub fn review_is_pending(&self, review_id: &str) -> Result<bool, StoreError> {
        Ok(self
            .deps
            .store
            .get(review_id)?
            .is_some_and(|review| review.state == ReviewState::Pending))
    }
}

/// What one attempt decided, before overwrite protection applies.
/// Indexes point into the scored candidates.
#[derive(Debug, Clone, PartialEq)]
enum Decision {
    Identified(usize),
    Contradictory,
    Ambiguous(String),
    EditionUncertain,
    Terminal(IdentificationOutcome, String),
}

impl Decision {
    fn outcome(&self) -> IdentificationOutcome {
        match self {
            Decision::Identified(_) => IdentificationOutcome::Identified,
            Decision::Contradictory => IdentificationOutcome::Contradictory,
            Decision::Ambiguous(_) => IdentificationOutcome::Ambiguous,
            Decision::EditionUncertain => IdentificationOutcome::EditionUncertain,
            Decision::Terminal(outcome, _) => *outcome,
        }
    }

    fn reason_code(&self) -> String {
        match self {
            Decision::Identified(_) => "SUPPORTED".to_owned(),
            Decision::Contradictory => "CONFLICTING_TRACK_EVIDENCE".to_owned(),
            Decision::Ambiguous(reason) | Decision::Terminal(_, reason) => reason.clone(),
            Decision::EditionUncertain => "EDITION_UNCERTAIN".to_owned(),
        }
    }
}

/// Every recalled release scored against one album, best first, with the
/// verdict an automatic attempt would reach. Nothing is sealed.
#[derive(Debug, Clone)]
pub struct Ranking {
    /// The matcher's view of the album.
    pub local: LocalAlbum,
    /// Indexes into the recalled releases, in candidate order.
    pub order: Vec<usize>,
    /// One match per recalled release, in recall order.
    pub matches: Vec<ReleaseMatch>,
    /// The candidates, in `order`.
    pub candidates: Vec<CandidateEvidence>,
    pub outcome: IdentificationOutcome,
    pub reason_code: String,
    /// True when the first candidate would seal on its own.
    pub lead_identified: bool,
    /// Index into `candidates` of the best guess, when one is not ruled out.
    pub guess: Option<usize>,
    /// The release group the files name, as recall resolved it.
    pub named_group: Option<String>,
}

/// What the album already holds that this pass weighs.
#[derive(Debug, Clone, Copy, Default)]
struct Held<'a> {
    /// The album's edition, kept unless another fits clearly better.
    release: Option<&'a str>,
    /// The group of an automatic guess sealed because the files named
    /// another group: a pick in it stays that guess while nothing names a
    /// group.
    doubted_group: Option<&'a str>,
    /// The album may move to the edition its files' barcode or catalog
    /// number names: false once an administrator undid an automatic
    /// edition of its group.
    may_move: bool,
}

/// True when a confirmation of `releases[chosen]` takes the album off
/// `releases[held]`, another edition of the same group: no file carries a
/// release ID that could be of that group (the release-ID tag outranks
/// the barcode and catalog number), the chosen edition states a barcode or
/// catalog number most files carry and contradicts neither, the held one
/// states another value and agrees with neither, and the chosen one
/// contradicts no more of the files' media, country and disc count. A
/// file's ID blocks when it is the held release's, a recalled release of
/// the group, or a release recall did not fetch.
fn moves_to_named_edition(
    local: &LocalAlbum,
    ids: &EditionHints,
    releases: &[Release],
    held: usize,
    chosen: usize,
) -> bool {
    let (from, to) = (&releases[held], &releases[chosen]);
    let tag_names_group = local
        .tracks
        .iter()
        .filter_map(|track| track.release_mbid.as_deref())
        .any(|mbid| {
            from.answers_to(mbid)
                || releases
                    .iter()
                    .find(|release| release.answers_to(mbid))
                    .is_none_or(|release| {
                        release
                            .release_group_id
                            .eq_ignore_ascii_case(&from.release_group_id)
                    })
        });
    !tag_names_group
        && to
            .release_group_id
            .eq_ignore_ascii_case(&from.release_group_id)
        && id_stance(to, ids) == IdStance::Names
        && id_stance(from, ids) == IdStance::Contradicts
        && hint_misses(to, &local.hints) <= hint_misses(from, &local.hints)
}

/// The reason a release outside the group the files name is only a guess.
pub const NAMED_GROUP_DIFFERS: &str = "NAMED_GROUP_DIFFERS";

/// The reason a release some file's audio disagrees with is only a guess.
pub const AUDIO_DISAGREES: &str = "AUDIO_DISAGREES";

/// The reason an attempt ended in Attention with the album left on its
/// edition: MusicBrainz answers for that edition in a form that does not
/// read.
pub const HELD_RELEASE_UNREADABLE: &str = "HELD_RELEASE_UNREADABLE";

/// True when the new evidence rules `release` out: it was scored and a
/// file's own id contradicts it, or its album title or artist do not
/// agree. A release this pass did not score is not ruled out.
fn ruled_out(ranking: &Ranking, release: &str) -> bool {
    ranking
        .candidates
        .iter()
        .zip(&ranking.order)
        .find(|(candidate, _)| {
            candidate
                .release_mbid
                .as_deref()
                .is_some_and(|mbid| mbid.eq_ignore_ascii_case(release))
        })
        .and_then(|(_, index)| ranking.matches.get(*index))
        .is_some_and(|matched| !matched.conflicts.is_empty() || !matched.names_agree)
}

/// Closest candidates kept with an unsure match.
const CLOSEST: usize = 5;

/// One attempt's report: where the job landed and why.
#[derive(Debug, Clone)]
pub struct AttemptReport {
    pub job: IdentifyJob,
    pub outcome: IdentificationOutcome,
    pub reason_code: String,
    pub review_id: Option<String>,
}
