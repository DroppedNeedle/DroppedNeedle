//! The identify service: queue jobs in, identities or reviews out.
//!
//! One attempt recalls candidate releases (with tracklists) through the
//! provider seam, scores each with the matching engine, and applies the
//! verdict through the overwrite rules. Curator rows always win; ambiguous
//! and weak matches become reviews with their distances; contradicting
//! ids retract automatic identities.

use std::collections::HashMap;
use std::sync::Arc;

use super::evidence::{candidate_evidence, local_album};
use super::models::{
    AlbumIdentity, Alias, AliasKind, Appearance, AutomaticSeal, CandidateEvidence, CreditProof,
    DecisionSource, EvidenceClass, IdentificationOutcome, IdentifyJob, IdentifyKind, IdentityBrief,
    JobState, LocalAlbumFacts, MatchFlag, MatchFlagState, RecallResult, ReviewState, TrackIdentity,
};
use super::providers::{IdentifyProviders, RecallOutcome};
use super::queue::{PRIORITY_HISTORICAL_BACKLOG, PRIORITY_NEW_OR_CHANGED, PRIORITY_REVIEW_RETRY};
use super::review::reject_review;
use super::rules::{
    SubstitutionCase, SubstitutionVerdict, classify_credit, evaluate_substitution,
    retracts_on_contradiction,
};
use super::sources::{EditionPage, EditionQuery, SourceError};
use super::stores::{
    AliasStore, Approval, AttemptLanding, FactsSource, IdentityStore, ProofStore, QueueStore,
    ReleaseStore, ReviewStore, StoreError, keep_releases, land_job,
};
use crate::library::edition_prefs::Preferences;
use crate::library::matching::decide::{EDITION_COHORT, eligible};
use crate::library::matching::{
    EditionPrefs, LocalAlbum, ReleaseMatch, Support, Verdict, decide, match_release,
};

/// Every dependency the identify service needs, injected by constructor.
pub struct IdentifyDeps {
    pub identities: Arc<dyn IdentityStore>,
    pub facts: Arc<dyn FactsSource>,
    pub proofs: Arc<dyn ProofStore>,
    pub aliases: Arc<dyn AliasStore>,
    pub queue: Arc<dyn QueueStore>,
    pub reviews: Arc<dyn ReviewStore>,
    pub releases: Arc<dyn ReleaseStore>,
    pub providers: Arc<dyn IdentifyProviders>,
}

/// Reads the saved edition preferences, per call.
pub type PreferenceSource = Arc<dyn Fn() -> Preferences + Send + Sync>;

pub struct IdentifyService {
    deps: IdentifyDeps,
    preferences: PreferenceSource,
}

impl IdentifyService {
    pub fn new(deps: IdentifyDeps) -> Self {
        Self {
            deps,
            preferences: Arc::new(Preferences::default),
        }
    }

    /// Break ties between equally fitting editions with these saved
    /// preferences (read on every attempt).
    #[must_use]
    pub fn with_preferences(mut self, preferences: PreferenceSource) -> Self {
        self.preferences = preferences;
        self
    }

    /// Enqueue one album. Fresh and changed work runs first, curator
    /// retries next, historical backlog last.
    pub fn enqueue_album(
        &self,
        job_id: &str,
        local_album_id: &str,
        kind: IdentifyKind,
        input_revision: &str,
        requested_by_user_id: Option<&str>,
        now_ms: u64,
    ) -> Option<IdentifyJob> {
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
        };
        self.deps.queue.enqueue(job)
    }

    /// Run one claimed job to a terminal landing. Async only because the
    /// provider seam is async; every rule inside is sync and pure.
    pub async fn run_claimed_job(&self, job_id: &str, now_ms: u64) -> Option<AttemptReport> {
        let mut job = self.deps.queue.job(job_id)?;
        let Some(facts) = self.deps.facts.album_facts(&job.local_album_id) else {
            // The album left the catalog: nothing to identify.
            land_job(&mut job, AttemptLanding::Failed, now_ms, Some("ALBUM_GONE"));
            self.deps.queue.update(job.clone());
            return Some(AttemptReport {
                job,
                outcome: IdentificationOutcome::Failed,
                reason_code: "ALBUM_GONE".to_owned(),
                review_id: None,
            });
        };
        let current = self.deps.identities.album_identity(&facts.local_album_id);
        // A person's choice is final: automatic passes never look again.
        if current
            .as_ref()
            .is_some_and(|row| !row.decision_source.automatic_may_overwrite())
        {
            land_job(&mut job, AttemptLanding::Done, now_ms, None);
            self.deps.queue.update(job.clone());
            return Some(AttemptReport {
                job,
                outcome: IdentificationOutcome::Identified,
                reason_code: "CHOSEN_EDITION_KEPT".to_owned(),
                review_id: None,
            });
        }
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
            self.deps.queue.update(job.clone());
            return Some(AttemptReport {
                job,
                outcome: IdentificationOutcome::ProviderDeferred,
                reason_code: "PROVIDER_DEFERRED".to_owned(),
                review_id: None,
            });
        }
        // Every candidate's document stays on file: the identity's release
        // for tagging, the others because a curator may approve them.
        // Live recall already stored what it fetched.
        keep_releases(&self.deps.releases, recall.releases.clone()).await;
        let (ranking, decision) = self.ranked(&facts, &recall, held.as_deref());
        let report = self.apply_decision(&mut job, &facts, &ranking, decision, now_ms);
        self.deps.queue.update(job);
        Some(report)
    }

    /// Recall candidates for one album outside the queue: every release
    /// the usual recall finds, or only `exact_release` when an
    /// administrator named one. Fetched documents stay on file, as for a
    /// queued attempt.
    pub async fn recall(
        &self,
        facts: &LocalAlbumFacts,
        exact_release: Option<&str>,
    ) -> RecallResult {
        let outcome = match exact_release {
            Some(mbid) => self.deps.providers.recall_release(facts, mbid).await,
            None => self.deps.providers.recall_candidates(facts).await,
        };
        if !outcome.result.provider_deferred {
            keep_releases(&self.deps.releases, outcome.result.releases.clone()).await;
        }
        outcome.result
    }

    /// One page of MusicBrainz releases for a curator choosing an edition.
    pub async fn search_editions(&self, query: EditionQuery) -> Result<EditionPage, SourceError> {
        self.deps.providers.search_editions(query).await
    }

    /// Score every recalled release and decide, the way a queued attempt
    /// does, without sealing anything.
    pub fn rank(&self, facts: &LocalAlbumFacts, recall: &RecallResult) -> Ranking {
        let (ranking, _) = self.ranked(facts, recall, None);
        ranking
    }

    /// Score every recalled release and decide. Candidates come back
    /// chosen edition first, then by distance. `held` is the edition the
    /// album already has: it stays unless another edition of its group
    /// fits the files better by more than the edition cohort.
    fn ranked(
        &self,
        facts: &LocalAlbumFacts,
        recall: &RecallResult,
        held: Option<&str>,
    ) -> (Ranking, Decision) {
        let local = local_album(facts, &recall.fingerprint_support);
        let matches: Vec<_> = recall
            .releases
            .iter()
            .map(|release| match_release(&local, release, &recall.recording_aliases))
            .collect();
        let tagged = local.tagged_release();
        let preferences = (self.preferences)();
        let verdict = decide(
            &local,
            &recall.releases,
            &matches,
            EditionPrefs {
                tagged: tagged.as_deref(),
                preferences: Some(&preferences),
            },
        );
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
            (Some(*b) == lead).cmp(&(Some(*a) == lead)).then_with(|| {
                matches[*a]
                    .library_distance()
                    .total_cmp(&matches[*b].library_distance())
            })
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

    /// Apply a decision. Always pick: a confident match seals; a close
    /// call, weak evidence or a lone candidate seals the best guess with an
    /// "unconfirmed" flag; only when every candidate is ruled out does the
    /// album keep its own tags, flagged unmatched with the closest
    /// candidates. Nothing waits in a blocking review.
    fn apply_decision(
        &self,
        job: &mut IdentifyJob,
        facts: &LocalAlbumFacts,
        ranking: &Ranking,
        decision: Decision,
        now_ms: u64,
    ) -> AttemptReport {
        let scored = &ranking.candidates;
        let album = facts.local_album_id.as_str();
        let current = self.deps.identities.album_identity(album);
        let outcome = decision.outcome();
        let reason_code = decision.reason_code();
        let closest: Vec<CandidateEvidence> = scored.iter().take(CLOSEST).cloned().collect();
        land_job(job, AttemptLanding::Done, now_ms, None);
        let report = |reason: &str| AttemptReport {
            job: job.clone(),
            outcome,
            reason_code: reason.to_owned(),
            review_id: None,
        };
        if let Decision::Identified(index) = decision {
            self.seal_automatic(facts, &scored[index], None);
            return report("SUPPORTED");
        }
        // A confirmed automatic edition stays unless the new evidence rules
        // it out: a weaker pass never demotes or replaces it.
        let confirmed_exact = current.as_ref().and_then(|row| {
            (row.decision_source == DecisionSource::Automatic
                && self.deps.identities.match_flag(album).is_none())
            .then(|| row.release_mbid.clone())
            .flatten()
        });
        if let Some(held) = confirmed_exact.as_deref()
            && !ruled_out(ranking, held)
        {
            return report("QUIET_RECONFIRM");
        }
        if let Some(guess) = ranking.guess.and_then(|index| scored.get(index)) {
            self.seal_automatic(
                facts,
                guess,
                Some(MatchFlag {
                    state: MatchFlagState::Unconfirmed,
                    reason_code: reason_code.clone(),
                    release_mbid: guess.release_mbid.clone(),
                    candidates: closest,
                }),
            );
            return report(&reason_code);
        }
        if matches!(decision, Decision::Contradictory)
            && retracts_on_contradiction(
                current.as_ref().map(|row| row.decision_source),
                IdentificationOutcome::Contradictory,
            )
        {
            self.deps.identities.clear_album_identity(album);
        }
        if self.deps.identities.album_identity(album).is_none()
            && let Err(error) = self.deps.identities.set_match_flag(
                album,
                Some(&MatchFlag {
                    state: MatchFlagState::Unmatched,
                    reason_code: reason_code.clone(),
                    release_mbid: None,
                    candidates: closest,
                }),
            )
        {
            tracing::error!(%error, album, "unmatched flag not recorded");
        }
        report(&reason_code)
    }

    /// Seal an automatic win: album row, supported track rows (curator
    /// tracks keep theirs), and fresh credit proof rows. The store writes
    /// them together, never over a curator's album row, and an exact
    /// edition keeps what it replaced so an administrator can undo it.
    fn seal_automatic(
        &self,
        facts: &LocalAlbumFacts,
        winner: &CandidateEvidence,
        flag: Option<MatchFlag>,
    ) -> bool {
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
        let sealed = self.deps.identities.seal_automatic(&AutomaticSeal {
            local_album_id: facts.local_album_id.clone(),
            release_group_mbid: winner.release_group_mbid.clone(),
            release_mbid: winner.release_mbid.clone(),
            tracks,
            flag,
        });
        if sealed {
            self.bank_proofs(facts, winner);
        } else {
            tracing::info!(
                album = facts.local_album_id,
                "a curator decided this album meanwhile; automatic seal skipped"
            );
        }
        sealed
    }

    /// Bank one proof row per owned-artist credit on supported tracks.
    fn bank_proofs(&self, facts: &LocalAlbumFacts, winner: &CandidateEvidence) {
        let Some(release_mbid) = winner.release_mbid.as_deref() else {
            return;
        };
        let album_revision = self.deps.proofs.album_revision(&facts.local_album_id);
        for track in &winner.track_evidence {
            // Proof rows need provider proof: an embedded id or the
            // audio's fingerprint, never a title and length match.
            let proven = track.evidence_kinds.iter().any(|kind| {
                kind == Support::EmbeddedId.code() || kind == Support::Fingerprint.code()
            });
            if track.classification != EvidenceClass::Supported || !proven {
                continue;
            }
            let track_revision = self.deps.proofs.track_revision(&track.local_track_id);
            for credit in self.deps.identities.track_credits(&track.local_track_id) {
                let Some(owned) = self
                    .deps
                    .identities
                    .owned_artist_by_mbid(&credit.artist_mbid)
                else {
                    continue;
                };
                self.deps.proofs.save_proof(CreditProof {
                    local_album_id: facts.local_album_id.clone(),
                    local_track_id: track.local_track_id.clone(),
                    source_local_artist_id: owned,
                    artist_mbid: credit.artist_mbid.clone(),
                    release_mbid: release_mbid.to_owned(),
                    album_identity_revision: album_revision,
                    track_identity_revision: track_revision,
                });
            }
        }
    }

    /// A curator approves a review: the chosen candidate seals as a manual
    /// identity, which later automatic passes can never overwrite. The
    /// review settles and every identity lands together or not at all.
    /// `Ok(false)` for an unknown review or candidate, or a settled review.
    pub fn approve_candidate(
        &self,
        review_id: &str,
        by_user_id: &str,
        candidate_key: &str,
    ) -> Result<bool, StoreError> {
        let Some(review) = self.deps.reviews.get(review_id) else {
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
        let tracks = winner
            .track_evidence
            .iter()
            .filter(|track| track.classification == EvidenceClass::Supported)
            .filter_map(|track| {
                let recording = track.recording_mbid.as_deref()?;
                Some(TrackIdentity {
                    local_track_id: track.local_track_id.clone(),
                    provider: "musicbrainz".to_owned(),
                    recording_mbid: Some(recording.to_owned()),
                    release_track_mbid: track.release_track_mbid.clone(),
                    decision_source: DecisionSource::Manual,
                    row_revision: 1,
                })
            })
            .collect();
        self.deps.reviews.approve(&Approval {
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
            tracks,
        })
    }

    /// A curator rejects a review: the album keeps its tags, nothing seals.
    pub fn reject_candidates(&self, review_id: &str, by_user_id: &str) -> bool {
        reject_review(self.deps.reviews.as_ref(), review_id, by_user_id)
    }

    /// Retire one local artist into another through the album-level proof
    /// gate. Allowed retirements keep the retired id as an
    /// alias and retarget live references; anything else parks or refuses.
    pub fn retire_artist(
        &self,
        source_local_artist_id: &str,
        surviving_local_artist_id: &str,
        expected_artist_mbid: &str,
        credits_unambiguous: bool,
        direct_identity_mbid: Option<&str>,
    ) -> SubstitutionVerdict {
        let durable: Vec<String> = self
            .deps
            .proofs
            .proofs_for_artist(source_local_artist_id)
            .into_iter()
            .filter(|proof| {
                self.deps.proofs.album_revision(&proof.local_album_id)
                    == proof.album_identity_revision
                    && self.deps.proofs.track_revision(&proof.local_track_id)
                        == proof.track_identity_revision
            })
            .map(|proof| proof.artist_mbid.clone())
            .collect();
        let accepted_release_mbid = self
            .deps
            .identities
            .accepted_release_mbid_for_artist(source_local_artist_id);
        let verdict = evaluate_substitution(&SubstitutionCase {
            accepted_release_mbid,
            durable_proof_mbids: durable,
            expected_artist_mbid: expected_artist_mbid.to_owned(),
            credits_unambiguous,
            direct_identity_mbid: direct_identity_mbid.map(str::to_owned),
            // Retirement is only ever attempted on a name anchor: the
            // folded-name match is what nominated this pair.
            name_evidence_present: true,
        });
        if verdict == SubstitutionVerdict::Allowed {
            self.deps.aliases.save_alias(Alias {
                retired_id: source_local_artist_id.to_owned(),
                surviving_id: surviving_local_artist_id.to_owned(),
                kind: AliasKind::MergedArtist,
            });
            self.deps
                .aliases
                .retarget(source_local_artist_id, surviving_local_artist_id);
        }
        verdict
    }

    /// Exact track contributors become appearances, never owned artists.
    pub fn appearances_for_track(&self, local_track_id: &str) -> Vec<Appearance> {
        let credits = self.deps.identities.track_credits(local_track_id);
        let mut owned = HashMap::new();
        for credit in &credits {
            if let Some(local_id) = self
                .deps
                .identities
                .owned_artist_by_mbid(&credit.artist_mbid)
            {
                owned.insert(credit.artist_mbid.to_lowercase(), local_id);
            }
        }
        credits
            .iter()
            .filter(|credit| {
                matches!(
                    classify_credit(credit, &owned),
                    super::rules::CreditRole::Appearance
                )
            })
            .map(|credit| Appearance {
                local_track_id: local_track_id.to_owned(),
                artist_mbid: credit.artist_mbid.clone(),
                credited_name: credit.credited_name.clone(),
                position: credit.position,
            })
            .collect()
    }

    /// One card with the whole case for an album.
    pub fn identity_brief(&self, local_album_id: &str) -> IdentityBrief {
        let identity = self.deps.identities.album_identity(local_album_id);
        let pending = self.deps.reviews.pending_for_album(local_album_id);
        let revisable = identity
            .as_ref()
            .is_none_or(|row| row.decision_source.automatic_may_overwrite());
        IdentityBrief {
            local_album_id: local_album_id.to_owned(),
            protected_source: identity.as_ref().and_then(|row| {
                (!row.decision_source.automatic_may_overwrite()).then_some(row.decision_source)
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
            aliases: self.deps.aliases.aliases_for(local_album_id),
            identity,
            revisable,
        }
    }

    /// Pending reviews for one album, oldest filing first.
    pub fn pending_reviews(&self, local_album_id: &str) -> Vec<super::models::ReviewItem> {
        self.deps.reviews.pending_for_album(local_album_id)
    }

    /// Review states still awaiting a curator.
    pub fn review_is_pending(&self, review_id: &str) -> bool {
        self.deps
            .reviews
            .get(review_id)
            .is_some_and(|review| review.state == ReviewState::Pending)
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
