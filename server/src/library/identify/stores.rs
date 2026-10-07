//! Ports identify needs. Production binds them to SQLite (`sqlite`);
//! tests may bind the memory fakes (`memory`).

use super::models::{
    AlbumIdentity, Alias, ArtistCredit, ArtistIdentity, AutomaticSeal, CreditProof, DecisionSource,
    IdentifyJob, JobState, LocalAlbumFacts, MatchFlag, ReviewItem, ReviewState, TrackIdentity,
};
use crate::library::matching::Release;

/// Durable identity rows plus the facts the matcher reads.
pub trait IdentityStore: Send + Sync {
    fn album_identity(&self, local_album_id: &str) -> Option<AlbumIdentity>;
    fn save_album_identity(&self, identity: AlbumIdentity);
    fn clear_album_identity(&self, local_album_id: &str);
    fn track_identity(&self, local_track_id: &str) -> Option<TrackIdentity>;
    fn save_track_identity(&self, identity: TrackIdentity);
    fn artist_identity(&self, local_artist_id: &str) -> Option<ArtistIdentity>;
    fn save_artist_identity(&self, identity: ArtistIdentity);
    fn owned_artist_by_mbid(&self, artist_mbid: &str) -> Option<String>;
    fn save_owned_artist(&self, artist_mbid: &str, local_artist_id: &str);
    fn track_credits(&self, local_track_id: &str) -> Vec<ArtistCredit>;
    fn save_track_credits(&self, local_track_id: &str, credits: Vec<ArtistCredit>);
    /// Accepted MusicBrainz release MBID on the artist's albums, if any.
    /// The durable adapter answers from the credits join; fakes seed it.
    fn accepted_release_mbid_for_artist(&self, _source_local_artist_id: &str) -> Option<String> {
        None
    }
    /// The album's match flag, if identification is unsure of it.
    fn match_flag(&self, _local_album_id: &str) -> Option<MatchFlag> {
        None
    }
    /// Record (or with `None`, clear) the album's match flag. Stores that
    /// keep no flags accept and ignore it.
    fn set_match_flag(
        &self,
        _local_album_id: &str,
        _flag: Option<&MatchFlag>,
    ) -> Result<(), StoreError> {
        Ok(())
    }
    /// Seal an automatic win: the album row and the given track rows, never
    /// over a curator's row. Durable stores do it in one transaction and,
    /// for an exact edition, keep what it replaced so an administrator can
    /// undo it. `false` when a curator's row holds the album (nothing is
    /// written). This default suits stores without transactions.
    fn seal_automatic(&self, seal: &AutomaticSeal) -> bool {
        let current = self.album_identity(&seal.local_album_id);
        if current
            .as_ref()
            .is_some_and(|row| !row.decision_source.automatic_may_overwrite())
        {
            return false;
        }
        if let Err(error) = self.set_match_flag(&seal.local_album_id, seal.flag.as_ref()) {
            tracing::error!(%error, album = seal.local_album_id, "match flag not written");
            return false;
        }
        self.save_album_identity(AlbumIdentity {
            local_album_id: seal.local_album_id.clone(),
            provider: "musicbrainz".to_owned(),
            release_group_mbid: Some(seal.release_group_mbid.clone()),
            release_mbid: seal.release_mbid.clone(),
            decision_source: DecisionSource::Automatic,
            row_revision: current.map(|row| row.row_revision + 1).unwrap_or(1),
        });
        for track in &seal.tracks {
            if self
                .track_identity(&track.local_track_id)
                .is_some_and(|row| !row.decision_source.automatic_may_overwrite())
            {
                continue;
            }
            self.save_track_identity(track.clone());
        }
        true
    }
}

/// Local album facts the matcher reads. Production answers from the
/// catalog the scan writes; `None` means the album does not exist.
pub trait FactsSource: Send + Sync {
    fn album_facts(&self, local_album_id: &str) -> Option<LocalAlbumFacts>;
}

/// Release documents count as fresh for recall this long; older ones are
/// fetched again (and kept regardless while an identity names them).
pub const RELEASE_FRESH_SECS: u64 = 7 * 24 * 3600;

/// Release documents identification fetched or sealed against. Recall
/// reuses fresh ones instead of asking MusicBrainz again, and the
/// publisher tags files from the one an album's identity names.
pub trait ReleaseStore: Send + Sync {
    /// The stored document for one release, if it was saved no more than
    /// `max_age_secs` ago (`None` accepts any age).
    fn release(&self, release_mbid: &str, max_age_secs: Option<u64>) -> Option<Release>;
    fn save_release(&self, release: &Release);
}

/// Keep every candidate document the store does not hold fresh, on a
/// blocking thread: the stores are synchronous SQLite.
pub async fn keep_releases(store: &std::sync::Arc<dyn ReleaseStore>, releases: Vec<Release>) {
    if releases.is_empty() {
        return;
    }
    let store = store.clone();
    let kept = tokio::task::spawn_blocking(move || {
        for release in &releases {
            if store
                .release(&release.id, Some(RELEASE_FRESH_SECS))
                .is_none()
            {
                store.save_release(release);
            }
        }
    })
    .await;
    if let Err(error) = kept {
        tracing::warn!(%error, "keeping release documents failed");
    }
}

/// A stored document fresh enough for recall, read on a blocking thread.
pub async fn fresh_release(
    store: &std::sync::Arc<dyn ReleaseStore>,
    release_mbid: &str,
) -> Option<Release> {
    let store = store.clone();
    let release_mbid = release_mbid.to_owned();
    match tokio::task::spawn_blocking(move || {
        store.release(&release_mbid, Some(RELEASE_FRESH_SECS))
    })
    .await
    {
        Ok(found) => found,
        Err(error) => {
            tracing::warn!(%error, "reading a release document failed");
            None
        }
    }
}

/// Fingerprints already taken, keyed by track and its file's stat
/// revision, so an unchanged file is never decoded twice.
pub trait FingerprintStore: Send + Sync {
    /// The stored print and its duration in seconds.
    fn fingerprint(&self, local_track_id: &str, stat_revision: &str) -> Option<(String, u32)>;
    /// Keep a print; `matched` says whether AcoustID knew it.
    fn save_fingerprint(
        &self,
        local_track_id: &str,
        stat_revision: &str,
        fingerprint: &str,
        duration_seconds: u32,
        matched: bool,
    );
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

/// Durable identification queue.
pub trait QueueStore: Send + Sync {
    /// Store a job. When a live job already holds the same album and
    /// input revision, that job comes back instead; `None` means the
    /// store could not record anything.
    fn enqueue(&self, job: IdentifyJob) -> Option<IdentifyJob>;
    fn claim(&self, now_ms: u64, lease_ms: u64) -> Option<IdentifyJob>;
    fn update(&self, job: IdentifyJob);
    fn job(&self, job_id: &str) -> Option<IdentifyJob>;
    fn jobs_for_album(&self, local_album_id: &str) -> Vec<IdentifyJob>;
    /// Boot recovery: jobs a previous process left running go back to
    /// the queue. Returns how many.
    fn recover(&self) -> usize;
}

/// A durable store could not complete a write. The cause is for the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreError {
    pub cause: String,
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "identify store failed: {}", self.cause)
    }
}

impl std::error::Error for StoreError {}

/// A curator's approval: the review it settles and the identities it
/// seals. The album identity's row revision is the store's to set.
#[derive(Debug, Clone)]
pub struct Approval {
    pub review_id: String,
    pub by_user_id: String,
    pub candidate_key: String,
    pub album: AlbumIdentity,
    pub tracks: Vec<TrackIdentity>,
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
    /// Settle a pending review as approved and seal the identities it
    /// chose, all or nothing. `Ok(false)` when the review is no longer
    /// pending. Each sealed row takes its stored revision plus one.
    fn approve(&self, approval: &Approval) -> Result<bool, StoreError>;
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
