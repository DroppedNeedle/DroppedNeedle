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
use tokio::time::Instant;

use super::evidence::{candidate_evidence, local_album};
use super::models::{
    AlbumIdentity, AutomaticSeal, CandidateEvidence, DecisionSource, EvidenceClass,
    IdentificationOutcome, IdentifyJob, IdentifyKind, IdentityBrief, JobState, LocalAlbumFacts,
    MatchFlag, MatchFlagState, RecallResult, ReviewState, TrackIdentity,
};
use super::providers::{IdentifyProviders, RecallOutcome};
use super::queue::{PRIORITY_HISTORICAL_BACKLOG, PRIORITY_NEW_OR_CHANGED, PRIORITY_REVIEW_RETRY};
use super::review::reject_review;
use super::rules::retracts_on_contradiction;
use super::sources::{EditionPage, EditionQuery, SourceError};
use super::sqlite::SqliteIdentifyStore;
use super::stores::{Approval, AttemptLanding, StoreError, keep_releases, land_job};
use crate::edition_prefs::{LivePreferences, Preferences};
use crate::identity::AutomaticOutcome;
use crate::matching::decide::{EDITION_COHORT, candidate_order, eligible};
use crate::matching::{EditionPrefs, LocalAlbum, ReleaseMatch, Verdict, decide, match_release};

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
    artist_hold: Mutex<ArtistHold>,
}

/// The wait after an artist id lookup MusicBrainz could not answer.
struct ArtistHold {
    until: Option<Instant>,
    failures: u32,
    rng: SplitMix64,
}

/// The first wait after a failed lookup with no `Retry-After`; it doubles
/// with each failure in a row, up to [`ARTIST_HOLD_CAP`].
const ARTIST_HOLD: Duration = Duration::from_secs(5 * 60);
const ARTIST_HOLD_CAP: Duration = Duration::from_secs(3600);

impl IdentifyService {
    pub fn new(deps: IdentifyDeps) -> Self {
        Self {
            deps,
            preferences: None,
            artist_hold: Mutex::new(ArtistHold {
                until: None,
                failures: 0,
                rng: SplitMix64::new(SplitMix64::seed_from_time()),
            }),
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
        let RecallOutcome {
            result: mut recall, ..
        } = self.deps.providers.recall_candidates(&facts).await;
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
            let extra = self.deps.providers.recall_release(&facts, held).await;
            if !extra.result.provider_deferred {
                recall.releases.extend(extra.result.releases);
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
        let (ranking, decision) = self.ranked(&facts, &recall, held.as_deref(), &preferences);
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
    /// administrator named one. Fetched documents stay on file, as for a
    /// queued attempt, written on the caller's lane.
    pub async fn recall(
        &self,
        facts: &LocalAlbumFacts,
        exact_release: Option<&str>,
        lane: Lane,
    ) -> Result<RecallResult, StoreError> {
        let outcome = match exact_release {
            Some(mbid) => self.deps.providers.recall_release(facts, mbid).await,
            None => self.deps.providers.recall_candidates(facts).await,
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
    /// each with up to 20% more. Returns how many ids were answered. An
    /// answer that cannot be written holds lookups back too.
    pub async fn resolve_merged_artist_ids(&self, limit: usize) -> Result<usize, StoreError> {
        if self
            .hold_guard()
            .until
            .is_some_and(|until| Instant::now() < until)
        {
            return Ok(0);
        }
        let mut answered = 0;
        for mbid in self.deps.store.pending_artist_ids(limit)? {
            match self.deps.providers.canonical_artist(&mbid).await {
                Ok(answer) => {
                    {
                        let mut hold = self.hold_guard();
                        hold.until = None;
                        hold.failures = 0;
                    }
                    if let Err(error) = self.deps.store.answer_artist_id(&mbid, &answer) {
                        self.hold(None);
                        return Err(error);
                    }
                    answered += 1;
                }
                Err(unanswered) => {
                    self.hold(unanswered.retry_after_secs);
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

    fn hold_guard(&self) -> std::sync::MutexGuard<'_, ArtistHold> {
        self.artist_hold
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Hold artist id lookups back after one went unanswered.
    fn hold(&self, retry_after_secs: Option<f64>) {
        let mut hold = self.hold_guard();
        let base = match retry_after_secs.filter(|secs| secs.is_finite() && *secs > 0.0) {
            Some(secs) => Duration::from_secs_f64(secs.min(ARTIST_HOLD_CAP.as_secs_f64())),
            None => ARTIST_HOLD
                .saturating_mul(1 << hold.failures.min(4))
                .min(ARTIST_HOLD_CAP),
        };
        let jitter = hold.rng.below_duration(base / 5);
        hold.failures = hold.failures.saturating_add(1);
        hold.until = Some(Instant::now() + base + jitter);
    }

    /// One page of MusicBrainz releases for a curator choosing an edition.
    pub async fn search_editions(&self, query: EditionQuery) -> Result<EditionPage, SourceError> {
        self.deps.providers.search_editions(query).await
    }

    /// Score every recalled release and decide, the way a queued attempt
    /// does, without sealing anything. `None` while the edition
    /// preferences do not read.
    pub fn rank(&self, facts: &LocalAlbumFacts, recall: &RecallResult) -> Option<Ranking> {
        let preferences = self.preferences()?;
        let (ranking, _) = self.ranked(facts, recall, None, &preferences);
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
    /// files complete comes first. `held` is the edition the album already
    /// has: it stays unless another edition of its group fits the files
    /// better by more than the edition cohort.
    fn ranked(
        &self,
        facts: &LocalAlbumFacts,
        recall: &RecallResult,
        held: Option<&str>,
        preferences: &Preferences,
    ) -> (Ranking, Decision) {
        let local = local_album(facts, &recall.fingerprint_support);
        let matches: Vec<_> = recall
            .releases
            .iter()
            .map(|release| match_release(&local, release, &recall.recording_aliases))
            .collect();
        let tagged = local.tagged_release();
        let prefs = EditionPrefs {
            tagged: tagged.as_deref(),
            preferences: Some(preferences),
        };
        let verdict = decide(&local, &recall.releases, &matches, prefs);
        let keep = |index: usize| -> usize {
            let Some(held) = held else {
                return index;
            };
            let chosen = &recall.releases[index];
            recall
                .releases
                .iter()
                .position(|release| release.answers_to(held))
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
                .unwrap_or(index)
        };
        let verdict = match verdict {
            Verdict::Identified(index) => Verdict::Identified(keep(index)),
            Verdict::EditionUncertain(index) => Verdict::EditionUncertain(keep(index)),
            other => other,
        };
        let lead = match verdict {
            Verdict::Identified(index) | Verdict::EditionUncertain(index) => Some(index),
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
        // ceiling). Without one the album matches nothing.
        let guess = order.iter().position(|index| {
            Some(*index) == lead || {
                let matched = &matches[*index];
                eligible(matched) && matched.names_agree && !matched.pairs.is_empty()
            }
        });
        let decision = match verdict {
            Verdict::Identified(_) => Decision::Identified(0),
            Verdict::EditionUncertain(_) => Decision::EditionUncertain,
            Verdict::Review(reason) => Decision::Ambiguous(reason.code().to_owned()),
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
        let ranking = Ranking {
            local,
            order,
            matches,
            candidates: scored,
            outcome: decision.outcome(),
            reason_code: decision.reason_code(),
            lead_identified: matches!(decision, Decision::Identified(_)),
            guess,
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
        // it out: a weaker pass never demotes or replaces it.
        let confirmed_exact = match current.as_ref() {
            Some(row)
                if row.decision_source.is_revisable()
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
}

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
