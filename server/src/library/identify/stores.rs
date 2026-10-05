//! Ports identify needs. Small traits, with memory fakes beside them; no
//! durable adapters exist yet.

use super::models::{
    AlbumIdentity, Alias, ArtistCredit, ArtistIdentity, CreditProof, IdentifyJob, JobState,
    LocalAlbumFacts, ReleasePin, ReviewItem, ReviewState, TrackIdentity,
};

/// Durable identity rows plus the facts the matcher reads.
pub trait IdentityStore: Send + Sync {
    fn album_identity(&self, local_album_id: &str) -> Option<AlbumIdentity>;
    fn save_album_identity(&self, identity: AlbumIdentity);
    fn clear_album_identity(&self, local_album_id: &str);
    fn track_identity(&self, local_track_id: &str) -> Option<TrackIdentity>;
    fn save_track_identity(&self, identity: TrackIdentity);
    fn artist_identity(&self, local_artist_id: &str) -> Option<ArtistIdentity>;
    fn save_artist_identity(&self, identity: ArtistIdentity);
    fn album_facts(&self, local_album_id: &str) -> Option<LocalAlbumFacts>;
    fn save_album_facts(&self, facts: LocalAlbumFacts);
    fn owned_artist_by_mbid(&self, artist_mbid: &str) -> Option<String>;
    fn save_owned_artist(&self, artist_mbid: &str, local_artist_id: &str);
    fn track_credits(&self, local_track_id: &str) -> Vec<ArtistCredit>;
    fn save_track_credits(&self, local_track_id: &str, credits: Vec<ArtistCredit>);
    /// Accepted MusicBrainz release MBID on the artist's albums, if any.
    /// The durable adapter answers from the credits join; fakes seed it.
    fn accepted_release_mbid_for_artist(&self, _source_local_artist_id: &str) -> Option<String> {
        None
    }
}

/// Durable credit proof rows.
pub trait ProofStore: Send + Sync {
    fn proofs_for_artist(&self, source_local_artist_id: &str) -> Vec<CreditProof>;
    fn save_proof(&self, proof: CreditProof);
    fn album_revision(&self, local_album_id: &str) -> u64;
    fn track_revision(&self, local_track_id: &str) -> u64;
}

/// Retired-id aliases plus the live references that follow them.
pub trait AliasStore: Send + Sync {
    fn save_alias(&self, alias: Alias);
    fn resolve(&self, id: &str) -> String;
    fn aliases_for(&self, surviving_id: &str) -> Vec<Alias>;
    fn retarget(&self, retired_id: &str, surviving_id: &str);
    fn favorite_holds(&self, user_id: &str, item_kind: &str, item_id: &str) -> bool;
    fn add_favorite(&self, user_id: &str, item_kind: &str, item_id: &str);
}

/// Release pins: hint-only steering for edition search.
pub trait PinStore: Send + Sync {
    fn pin(&self, release_group_mbid: &str) -> Option<ReleasePin>;
    fn set_pin(&self, pin: ReleasePin);
    fn clear_pin(&self, release_group_mbid: &str) -> bool;
}

/// Durable identification queue.
pub trait QueueStore: Send + Sync {
    fn enqueue(&self, job: IdentifyJob);
    fn claim(&self, now_ms: u64, lease_ms: u64) -> Option<IdentifyJob>;
    fn update(&self, job: IdentifyJob);
    fn job(&self, job_id: &str) -> Option<IdentifyJob>;
    fn jobs_for_album(&self, local_album_id: &str) -> Vec<IdentifyJob>;
}

/// Curator review queue.
pub trait ReviewStore: Send + Sync {
    fn file(&self, review: ReviewItem);
    fn get(&self, review_id: &str) -> Option<ReviewItem>;
    fn pending_for_album(&self, local_album_id: &str) -> Vec<ReviewItem>;
    fn set_state(
        &self,
        review_id: &str,
        state: ReviewState,
        by_user_id: Option<&str>,
        selected_key: Option<&str>,
    ) -> bool;
}

/// States a claimed job can land in after one attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptLanding {
    Done,
    Deferred,
    Attention,
    Failed,
}

pub fn land_job(
    job: &mut IdentifyJob,
    landing: AttemptLanding,
    now_ms: u64,
    failure: Option<&str>,
) {
    match landing {
        AttemptLanding::Done => {
            job.state = JobState::Succeeded;
            job.failure_code = None;
        }
        AttemptLanding::Failed => {
            job.state = JobState::Failed;
            job.failure_code = failure.map(str::to_owned);
        }
        AttemptLanding::Attention => {
            job.state = JobState::Attention;
            job.failure_code = failure.map(str::to_owned);
        }
        AttemptLanding::Deferred => {
            job.attempts += 1;
            job.failure_code = failure.map(str::to_owned);
            if super::queue::terminally_deferred(job.attempts) {
                job.state = JobState::Attention;
            } else {
                job.state = JobState::Deferred;
                job.not_before_ms = now_ms + super::queue::backoff_secs(job.attempts) * 1000;
            }
        }
    }
}
