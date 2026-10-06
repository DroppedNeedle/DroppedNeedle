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
    AlbumIdentity, Alias, AliasKind, Appearance, CandidateEvidence, CreditProof, DecisionSource,
    EvidenceClass, IdentificationOutcome, IdentifyJob, IdentifyKind, IdentityBrief, JobState,
    LocalAlbumFacts, RecallResult, ReviewState, TrackIdentity,
};
use super::providers::{IdentifyProviders, RecallOutcome};
use super::queue::{PRIORITY_HISTORICAL_BACKLOG, PRIORITY_NEW_OR_CHANGED, PRIORITY_REVIEW_RETRY};
use super::review::{file_review, reject_review};
use super::rules::{
    SubstitutionCase, SubstitutionVerdict, classify_credit, evaluate_overwrite,
    evaluate_substitution, retracts_on_contradiction,
};
use super::stores::{
    AliasStore, Approval, AttemptLanding, FactsSource, IdentityStore, PinStore, ProofStore,
    QueueStore, ReleaseStore, ReviewStore, StoreError, keep_releases, land_job,
};
use crate::library::matching::{EditionPrefs, Support, Verdict, decide, match_release};

/// Every dependency the identify service needs, injected by constructor.
pub struct IdentifyDeps {
    pub identities: Arc<dyn IdentityStore>,
    pub facts: Arc<dyn FactsSource>,
    pub proofs: Arc<dyn ProofStore>,
    pub aliases: Arc<dyn AliasStore>,
    pub pins: Arc<dyn PinStore>,
    pub queue: Arc<dyn QueueStore>,
    pub reviews: Arc<dyn ReviewStore>,
    pub releases: Arc<dyn ReleaseStore>,
    pub providers: Arc<dyn IdentifyProviders>,
}

pub struct IdentifyService {
    deps: IdentifyDeps,
}

impl IdentifyService {
    pub fn new(deps: IdentifyDeps) -> Self {
        Self { deps }
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
        let RecallOutcome { result: recall, .. } =
            self.deps.providers.recall_candidates(&facts).await;
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
        let (scored, decision) = self.score(&facts, &recall);
        let report = self.apply_decision(&mut job, &facts, &scored, decision, now_ms);
        self.deps.queue.update(job);
        Some(report)
    }

    /// Score every recalled release and decide. Candidates come back
    /// chosen edition first, then by distance; the pin only orders
    /// editions within one release group and never counts as evidence.
    fn score(
        &self,
        facts: &LocalAlbumFacts,
        recall: &RecallResult,
    ) -> (Vec<CandidateEvidence>, Decision) {
        let local = local_album(facts, &recall.fingerprint_support);
        let matches: Vec<_> = recall
            .releases
            .iter()
            .map(|release| match_release(&local, release, &recall.recording_aliases))
            .collect();
        let pinned: Vec<String> = recall
            .releases
            .iter()
            .filter_map(|release| self.deps.pins.pin(&release.release_group_id))
            .map(|pin| pin.release_mbid)
            .collect();
        let tagged = local.tagged_release();
        let verdict = decide(
            &local,
            &recall.releases,
            &matches,
            EditionPrefs {
                tagged: tagged.as_deref(),
                pinned: &pinned,
            },
        );
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
        (scored, decision)
    }

    fn apply_decision(
        &self,
        job: &mut IdentifyJob,
        facts: &LocalAlbumFacts,
        scored: &[CandidateEvidence],
        decision: Decision,
        now_ms: u64,
    ) -> AttemptReport {
        let current = self.deps.identities.album_identity(&facts.local_album_id);
        let current_source = current.as_ref().map(|row| row.decision_source);
        let current_rg = current
            .as_ref()
            .and_then(|row| row.release_group_mbid.clone());
        let _ = now_ms;
        match decision {
            Decision::Identified(index) => {
                let winner = &scored[index];
                // Agreement comes from the two release-group MBIDs: an
                // Identified winner that agrees with a protected row
                // quietly reconfirms instead of filing a review.
                let verdict = evaluate_overwrite(
                    current_source,
                    Some(winner.release_group_mbid.as_str()),
                    current_rg.as_deref(),
                    false,
                );
                match verdict {
                    super::rules::OverwriteVerdict::MayWrite => {
                        self.seal_automatic(facts, winner);
                        land_job(job, AttemptLanding::Done, 0, None);
                        AttemptReport {
                            job: job.clone(),
                            outcome: IdentificationOutcome::Identified,
                            reason_code: "SUPPORTED".to_owned(),
                            review_id: None,
                        }
                    }
                    super::rules::OverwriteVerdict::ProtectedFileReview => {
                        let review = file_review(
                            self.deps.reviews.as_ref(),
                            &format!("review-{}-{}", job.id, job.attempts),
                            &facts.local_album_id,
                            "PROTECTED_IDENTITY",
                            scored.to_vec(),
                        );
                        land_job(job, AttemptLanding::Done, 0, None);
                        AttemptReport {
                            job: job.clone(),
                            outcome: IdentificationOutcome::Identified,
                            reason_code: "PROTECTED_IDENTITY".to_owned(),
                            review_id: Some(review.id),
                        }
                    }
                    super::rules::OverwriteVerdict::QuietReconfirm => {
                        land_job(job, AttemptLanding::Done, 0, None);
                        AttemptReport {
                            job: job.clone(),
                            outcome: IdentificationOutcome::Identified,
                            reason_code: "QUIET_RECONFIRM".to_owned(),
                            review_id: None,
                        }
                    }
                }
            }
            Decision::Contradictory => {
                if retracts_on_contradiction(current_source, IdentificationOutcome::Contradictory) {
                    self.deps
                        .identities
                        .clear_album_identity(&facts.local_album_id);
                }
                let top_agrees = scored.first().is_some_and(|top| {
                    current_rg
                        .as_deref()
                        .is_some_and(|rg| rg.eq_ignore_ascii_case(&top.release_group_mbid))
                });
                if current_source.is_some_and(|s| !s.automatic_may_overwrite()) && top_agrees {
                    land_job(job, AttemptLanding::Done, 0, None);
                    return AttemptReport {
                        job: job.clone(),
                        outcome: IdentificationOutcome::Contradictory,
                        reason_code: "QUIET_RECONFIRM".to_owned(),
                        review_id: None,
                    };
                }
                let review = file_review(
                    self.deps.reviews.as_ref(),
                    &format!("review-{}-{}", job.id, job.attempts),
                    &facts.local_album_id,
                    "CONFLICTING_TRACK_EVIDENCE",
                    scored.to_vec(),
                );
                land_job(job, AttemptLanding::Done, 0, None);
                AttemptReport {
                    job: job.clone(),
                    outcome: IdentificationOutcome::Contradictory,
                    reason_code: "CONFLICTING_TRACK_EVIDENCE".to_owned(),
                    review_id: Some(review.id),
                }
            }
            Decision::Ambiguous(reason) => {
                let review = file_review(
                    self.deps.reviews.as_ref(),
                    &format!("review-{}-{}", job.id, job.attempts),
                    &facts.local_album_id,
                    &reason,
                    scored.to_vec(),
                );
                land_job(job, AttemptLanding::Done, 0, None);
                AttemptReport {
                    job: job.clone(),
                    outcome: IdentificationOutcome::Ambiguous,
                    reason_code: reason,
                    review_id: Some(review.id),
                }
            }
            Decision::EditionUncertain => {
                self.seal_edition_uncertain(facts, &current, scored);
                land_job(job, AttemptLanding::Done, 0, None);
                AttemptReport {
                    job: job.clone(),
                    outcome: IdentificationOutcome::EditionUncertain,
                    reason_code: "EDITION_UNCERTAIN".to_owned(),
                    review_id: None,
                }
            }
            Decision::Terminal(outcome, reason) => {
                land_job(job, AttemptLanding::Done, 0, None);
                AttemptReport {
                    job: job.clone(),
                    outcome,
                    reason_code: reason,
                    review_id: None,
                }
            }
        }
    }

    /// Seal an automatic win: album row, supported track rows (skipping
    /// curator-protected tracks), and fresh credit proof rows.
    fn seal_automatic(&self, facts: &LocalAlbumFacts, winner: &CandidateEvidence) {
        let revision = self
            .deps
            .identities
            .album_identity(&facts.local_album_id)
            .map(|row| row.row_revision + 1)
            .unwrap_or(1);
        self.deps.identities.save_album_identity(AlbumIdentity {
            local_album_id: facts.local_album_id.clone(),
            provider: "musicbrainz".to_owned(),
            release_group_mbid: Some(winner.release_group_mbid.clone()),
            release_mbid: winner.release_mbid.clone(),
            decision_source: DecisionSource::Automatic,
            row_revision: revision,
        });
        for track in &winner.track_evidence {
            if track.classification != EvidenceClass::Supported {
                continue;
            }
            let Some(recording) = track.recording_mbid.as_deref() else {
                continue;
            };
            if let Some(existing) = self.deps.identities.track_identity(&track.local_track_id)
                && !existing.decision_source.automatic_may_overwrite()
            {
                continue;
            }
            self.deps.identities.save_track_identity(TrackIdentity {
                local_track_id: track.local_track_id.clone(),
                provider: "musicbrainz".to_owned(),
                recording_mbid: Some(recording.to_owned()),
                release_track_mbid: track.release_track_mbid.clone(),
                decision_source: DecisionSource::Automatic,
                row_revision: 1,
            });
        }
        self.bank_proofs(facts, winner);
    }

    /// Edition-uncertain tier: pin the release GROUP only, never an exact
    /// edition. Protected identities stay untouched, and a sealed
    /// automatic exact of the same group holds (no demotion on weaker
    /// evidence); anything else demotes so the gap stays visible.
    fn seal_edition_uncertain(
        &self,
        facts: &LocalAlbumFacts,
        current: &Option<AlbumIdentity>,
        scored: &[CandidateEvidence],
    ) {
        let Some(group) = scored.first().map(|top| top.release_group_mbid.clone()) else {
            return;
        };
        let protected = current
            .as_ref()
            .is_some_and(|row| !row.decision_source.automatic_may_overwrite());
        if protected {
            return;
        }
        let held_exact = current.as_ref().is_some_and(|row| {
            row.decision_source == DecisionSource::Automatic
                && row.release_mbid.is_some()
                && row
                    .release_group_mbid
                    .as_deref()
                    .is_some_and(|rg| rg.eq_ignore_ascii_case(&group))
        });
        if held_exact {
            return;
        }
        let revision = current
            .as_ref()
            .map(|row| row.row_revision + 1)
            .unwrap_or(1);
        self.deps.identities.save_album_identity(AlbumIdentity {
            local_album_id: facts.local_album_id.clone(),
            provider: "musicbrainz".to_owned(),
            release_group_mbid: Some(group),
            release_mbid: None,
            decision_source: DecisionSource::Automatic,
            row_revision: revision,
        });
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

/// One attempt's report: where the job landed and why.
#[derive(Debug, Clone)]
pub struct AttemptReport {
    pub job: IdentifyJob,
    pub outcome: IdentificationOutcome,
    pub reason_code: String,
    pub review_id: Option<String>,
}
