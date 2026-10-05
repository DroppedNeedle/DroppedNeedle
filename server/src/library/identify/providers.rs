//! Provider seams: MusicBrainz recall plus AcoustID support evidence.
//!
//! Both seams sit on the stage-5 clients. Priority is honest: proof
//! lookups run [`Criticality::IdentityCritical`] so a dead provider
//! defers the job instead of silently misidentifying, while edition
//! steering and display enrichment run [`Criticality::BestEffort`].
//! Fingerprint evidence is support-only, never authoritative proof.

use std::collections::HashMap;

use crate::providers::acoustid::{AcoustIdClient, Outcome};
use crate::providers::degradation::DegradationSink;
use crate::providers::limiter::Pacer;
use crate::providers::musicbrainz::{
    Criticality, MbTransport, MusicBrainzClient, ReleaseSearchHit, credit_display_name, hit_score,
};

use super::models::{CandidateEvidence, LocalAlbumFacts, RecallResult};

/// What the identify service needs from providers. The live adapter owns
/// stage-5 clients; the fake answers from scripts. No live network in tests.
pub trait IdentifyProviders: Send + Sync {
    /// Recall release candidates for the album facts.
    fn recall_candidates(
        &self,
        facts: &LocalAlbumFacts,
        limit: u32,
    ) -> std::pin::Pin<Box<dyn Future<Output = RecallOutcome> + Send + '_>>;
}

/// Recall result with the honest criticality each call ran under.
#[derive(Debug, Clone, Default)]
pub struct RecallOutcome {
    pub result: RecallResult,
    /// Criticality used for the MusicBrainz recall calls.
    pub recall_criticality: Option<Criticality>,
}

/// Live adapter over the stage-5 provider clients.
pub struct LiveProviders<T, P, S>
where
    T: MbTransport,
    P: Pacer,
    S: DegradationSink,
{
    musicbrainz: MusicBrainzClient<T, S>,
    acoustid: AcoustIdClient<P, S>,
    acoustid_api_key: String,
}

impl<T, P, S> LiveProviders<T, P, S>
where
    T: MbTransport,
    P: Pacer,
    S: DegradationSink,
{
    pub fn new(
        musicbrainz: MusicBrainzClient<T, S>,
        acoustid: AcoustIdClient<P, S>,
        acoustid_api_key: String,
    ) -> Self {
        Self {
            musicbrainz,
            acoustid,
            acoustid_api_key,
        }
    }

    /// Edition search steered by a pin. Best-effort on purpose: a dead
    /// provider here only loses a ranking hint, never identity.
    pub async fn search_editions_best_effort(
        &self,
        title: &str,
        artist: &str,
        limit: u32,
    ) -> Vec<ReleaseSearchHit> {
        match self
            .musicbrainz
            .search_releases(title, artist, limit, Criticality::BestEffort)
            .await
        {
            Ok(page) => page.items,
            Err(_) => Vec::new(),
        }
    }
}

impl<T, P, S> IdentifyProviders for LiveProviders<T, P, S>
where
    T: MbTransport,
    P: Pacer,
    S: DegradationSink,
{
    fn recall_candidates(
        &self,
        facts: &LocalAlbumFacts,
        limit: u32,
    ) -> std::pin::Pin<Box<dyn Future<Output = RecallOutcome> + Send + '_>> {
        // Proof recall is identity-critical: transport death must surface
        // as a typed failure so the job defers instead of guessing.
        let criticality = Criticality::IdentityCritical;
        let facts = facts.clone();
        Box::pin(async move { self.recall_inner(&facts, limit, criticality).await })
    }
}

impl<T, P, S> LiveProviders<T, P, S>
where
    T: MbTransport,
    P: Pacer,
    S: DegradationSink,
{
    async fn recall_inner(
        &self,
        facts: &LocalAlbumFacts,
        limit: u32,
        criticality: Criticality,
    ) -> RecallOutcome {
        let page = match self
            .musicbrainz
            .search_releases(&facts.title, &facts.album_artist_name, limit, criticality)
            .await
        {
            Ok(page) => page,
            Err(_) => {
                return RecallOutcome {
                    result: RecallResult {
                        provider_deferred: true,
                        failure_code: Some("musicbrainz_unavailable".to_owned()),
                        ..RecallResult::default()
                    },
                    recall_criticality: Some(criticality),
                };
            }
        };
        let mut candidates = Vec::with_capacity(page.items.len());
        for hit in &page.items {
            let group = hit
                .release_group
                .as_ref()
                .map(|rg| rg.id.as_str())
                .unwrap_or("");
            candidates.push(CandidateEvidence {
                candidate_key: format!("{group}:{}", hit.id),
                release_group_mbid: group.to_owned(),
                release_mbid: Some(hit.id.clone()),
                album_title: hit.title.clone().unwrap_or_default(),
                album_artist_name: credit_display_name(&hit.artist_credit)
                    .unwrap_or("")
                    .to_owned(),
                track_evidence: Vec::new(),
                score: hit_score(hit.score, hit.ext_score) as f64,
                margin: 0.0,
                reason_code: String::new(),
            });
        }
        let mut fingerprint_support = HashMap::new();
        for track in &facts.tracks {
            let Some(fingerprint) = track.fingerprint.as_deref() else {
                continue;
            };
            let duration = track.duration_secs.unwrap_or(0);
            match self
                .acoustid
                .lookup(&self.acoustid_api_key, fingerprint, duration)
                .await
            {
                Outcome::Found(best) => {
                    fingerprint_support.insert(track.local_track_id.clone(), best.recording_id);
                }
                Outcome::Missing | Outcome::Unavailable { .. } => {}
            }
        }
        RecallOutcome {
            result: RecallResult {
                candidates,
                fingerprint_support,
                provider_deferred: false,
                failure_code: None,
            },
            recall_criticality: Some(criticality),
        }
    }
}

/// Scripted providers for briefs: no network, fully deterministic.
#[derive(Debug, Default)]
pub struct FakeProviders {
    pub recall: std::sync::Mutex<Option<RecallResult>>,
    pub criticality_seen: std::sync::Mutex<Vec<Criticality>>,
}

impl FakeProviders {
    pub fn with_recall(result: RecallResult) -> Self {
        Self {
            recall: std::sync::Mutex::new(Some(result)),
            criticality_seen: std::sync::Mutex::new(Vec::new()),
        }
    }

    pub fn criticalities(&self) -> Vec<Criticality> {
        self.criticality_seen
            .lock()
            .map(|seen| seen.clone())
            .unwrap_or_default()
    }

    /// Replace the scripted recall. The stage-8 wiring uses this to
    /// seed case-specific recall after scan assigns real track ids.
    pub fn set_recall(&self, result: RecallResult) {
        if let Ok(mut slot) = self.recall.lock() {
            *slot = Some(result);
        }
    }
}

impl IdentifyProviders for FakeProviders {
    fn recall_candidates(
        &self,
        _facts: &LocalAlbumFacts,
        _limit: u32,
    ) -> std::pin::Pin<Box<dyn Future<Output = RecallOutcome> + Send + '_>> {
        // The fake mirrors the live adapter's honest priority: proof
        // recall always runs identity-critical.
        if let Ok(mut seen) = self.criticality_seen.lock() {
            seen.push(Criticality::IdentityCritical);
        }
        let result = self
            .recall
            .lock()
            .map(|slot| slot.clone().unwrap_or_default())
            .unwrap_or_default();
        Box::pin(async move {
            RecallOutcome {
                result,
                recall_criticality: Some(Criticality::IdentityCritical),
            }
        })
    }
}
