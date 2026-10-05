//! In-memory identify stores for tests and unwired runtimes.

use std::collections::HashMap;
use std::sync::Mutex;

use super::models::{
    AlbumIdentity, Alias, ArtistCredit, ArtistIdentity, CreditProof, IdentifyJob, LocalAlbumFacts,
    ReleasePin, ReviewItem, ReviewState, TrackIdentity,
};
use super::stores::{AliasStore, IdentityStore, PinStore, ProofStore, QueueStore, ReviewStore};

#[derive(Debug, Default)]
pub struct MemoryIdentityStore {
    albums: Mutex<HashMap<String, AlbumIdentity>>,
    tracks: Mutex<HashMap<String, TrackIdentity>>,
    artists: Mutex<HashMap<String, ArtistIdentity>>,
    facts: Mutex<HashMap<String, LocalAlbumFacts>>,
    owned: Mutex<HashMap<String, String>>,
    credits: Mutex<HashMap<String, Vec<ArtistCredit>>>,
    accepted: Mutex<HashMap<String, String>>,
}

impl MemoryIdentityStore {
    #[cfg(any(test, feature = "test-support"))]
    pub fn seed_accepted_release(&self, source_local_artist_id: &str, release_mbid: &str) {
        if let Ok(mut accepted) = self.accepted.lock() {
            accepted.insert(source_local_artist_id.to_owned(), release_mbid.to_owned());
        }
    }
}

impl IdentityStore for MemoryIdentityStore {
    fn album_identity(&self, local_album_id: &str) -> Option<AlbumIdentity> {
        self.albums.lock().ok()?.get(local_album_id).cloned()
    }

    fn save_album_identity(&self, identity: AlbumIdentity) {
        if let Ok(mut albums) = self.albums.lock() {
            albums.insert(identity.local_album_id.clone(), identity);
        }
    }

    fn clear_album_identity(&self, local_album_id: &str) {
        if let Ok(mut albums) = self.albums.lock() {
            albums.remove(local_album_id);
        }
    }

    fn track_identity(&self, local_track_id: &str) -> Option<TrackIdentity> {
        self.tracks.lock().ok()?.get(local_track_id).cloned()
    }

    fn save_track_identity(&self, identity: TrackIdentity) {
        if let Ok(mut tracks) = self.tracks.lock() {
            tracks.insert(identity.local_track_id.clone(), identity);
        }
    }

    fn artist_identity(&self, local_artist_id: &str) -> Option<ArtistIdentity> {
        self.artists.lock().ok()?.get(local_artist_id).cloned()
    }

    fn save_artist_identity(&self, identity: ArtistIdentity) {
        if let Ok(mut artists) = self.artists.lock() {
            artists.insert(identity.local_artist_id.clone(), identity);
        }
    }

    fn album_facts(&self, local_album_id: &str) -> Option<LocalAlbumFacts> {
        self.facts.lock().ok()?.get(local_album_id).cloned()
    }

    fn save_album_facts(&self, facts: LocalAlbumFacts) {
        if let Ok(mut all) = self.facts.lock() {
            all.insert(facts.local_album_id.clone(), facts);
        }
    }

    fn owned_artist_by_mbid(&self, artist_mbid: &str) -> Option<String> {
        self.owned
            .lock()
            .ok()?
            .get(&artist_mbid.to_lowercase())
            .cloned()
    }

    fn save_owned_artist(&self, artist_mbid: &str, local_artist_id: &str) {
        if let Ok(mut owned) = self.owned.lock() {
            owned.insert(artist_mbid.to_lowercase(), local_artist_id.to_owned());
        }
    }

    fn track_credits(&self, local_track_id: &str) -> Vec<ArtistCredit> {
        self.credits
            .lock()
            .ok()
            .and_then(|credits| credits.get(local_track_id).cloned())
            .unwrap_or_default()
    }

    fn save_track_credits(&self, local_track_id: &str, credits: Vec<ArtistCredit>) {
        if let Ok(mut all) = self.credits.lock() {
            all.insert(local_track_id.to_owned(), credits);
        }
    }

    fn accepted_release_mbid_for_artist(&self, source_local_artist_id: &str) -> Option<String> {
        self.accepted
            .lock()
            .ok()?
            .get(source_local_artist_id)
            .cloned()
    }
}

#[derive(Debug, Default)]
pub struct MemoryProofStore {
    proofs: Mutex<Vec<CreditProof>>,
    album_revisions: Mutex<HashMap<String, u64>>,
    track_revisions: Mutex<HashMap<String, u64>>,
}

impl MemoryProofStore {
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_album_revision(&self, local_album_id: &str, revision: u64) {
        if let Ok(mut revisions) = self.album_revisions.lock() {
            revisions.insert(local_album_id.to_owned(), revision);
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn set_track_revision(&self, local_track_id: &str, revision: u64) {
        if let Ok(mut revisions) = self.track_revisions.lock() {
            revisions.insert(local_track_id.to_owned(), revision);
        }
    }
}

impl ProofStore for MemoryProofStore {
    fn proofs_for_artist(&self, source_local_artist_id: &str) -> Vec<CreditProof> {
        self.proofs
            .lock()
            .map(|proofs| {
                proofs
                    .iter()
                    .filter(|p| p.source_local_artist_id == source_local_artist_id)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    fn save_proof(&self, proof: CreditProof) {
        if let Ok(mut proofs) = self.proofs.lock() {
            proofs.push(proof);
        }
    }

    fn album_revision(&self, local_album_id: &str) -> u64 {
        self.album_revisions
            .lock()
            .ok()
            .and_then(|revisions| revisions.get(local_album_id).copied())
            .unwrap_or(0)
    }

    fn track_revision(&self, local_track_id: &str) -> u64 {
        self.track_revisions
            .lock()
            .ok()
            .and_then(|revisions| revisions.get(local_track_id).copied())
            .unwrap_or(0)
    }
}

#[derive(Debug, Default)]
pub struct MemoryAliasStore {
    aliases: Mutex<HashMap<String, Alias>>,
    favorites: Mutex<Vec<(String, String, String)>>,
    playlists: Mutex<HashMap<String, String>>,
    history: Mutex<HashMap<String, String>>,
}

impl MemoryAliasStore {
    #[cfg(any(test, feature = "test-support"))]
    pub fn add_playlist_ref(&self, playlist_id: &str, album_id: &str) {
        if let Ok(mut playlists) = self.playlists.lock() {
            playlists.insert(playlist_id.to_owned(), album_id.to_owned());
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn playlist_ref(&self, playlist_id: &str) -> Option<String> {
        self.playlists.lock().ok()?.get(playlist_id).cloned()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn add_history_ref(&self, row_id: &str, album_id: &str) {
        if let Ok(mut history) = self.history.lock() {
            history.insert(row_id.to_owned(), album_id.to_owned());
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn history_ref(&self, row_id: &str) -> Option<String> {
        self.history.lock().ok()?.get(row_id).cloned()
    }
}

impl AliasStore for MemoryAliasStore {
    fn save_alias(&self, alias: Alias) {
        if let Ok(mut aliases) = self.aliases.lock() {
            aliases.insert(alias.retired_id.clone(), alias);
        }
    }

    fn resolve(&self, id: &str) -> String {
        self.aliases
            .lock()
            .ok()
            .and_then(|aliases| aliases.get(id).map(|alias| alias.surviving_id.clone()))
            .unwrap_or_else(|| id.to_owned())
    }

    fn aliases_for(&self, surviving_id: &str) -> Vec<Alias> {
        self.aliases
            .lock()
            .map(|aliases| {
                aliases
                    .values()
                    .filter(|alias| alias.surviving_id == surviving_id)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    fn retarget(&self, retired_id: &str, surviving_id: &str) {
        if let Ok(mut favorites) = self.favorites.lock() {
            for favorite in favorites.iter_mut() {
                if favorite.2 == retired_id {
                    favorite.2 = surviving_id.to_owned();
                }
            }
        }
        if let Ok(mut playlists) = self.playlists.lock() {
            for target in playlists.values_mut() {
                if target == retired_id {
                    *target = surviving_id.to_owned();
                }
            }
        }
        if let Ok(mut history) = self.history.lock() {
            for target in history.values_mut() {
                if target == retired_id {
                    *target = surviving_id.to_owned();
                }
            }
        }
    }

    fn favorite_holds(&self, user_id: &str, item_kind: &str, item_id: &str) -> bool {
        self.favorites
            .lock()
            .map(|favorites| {
                favorites.iter().any(|(user, kind, item)| {
                    user == user_id && kind == item_kind && item == item_id
                })
            })
            .unwrap_or(false)
    }

    fn add_favorite(&self, user_id: &str, item_kind: &str, item_id: &str) {
        if let Ok(mut favorites) = self.favorites.lock() {
            favorites.push((user_id.to_owned(), item_kind.to_owned(), item_id.to_owned()));
        }
    }
}

#[derive(Debug, Default)]
pub struct MemoryPinStore {
    pins: Mutex<HashMap<String, ReleasePin>>,
}

impl PinStore for MemoryPinStore {
    fn pin(&self, release_group_mbid: &str) -> Option<ReleasePin> {
        self.pins
            .lock()
            .ok()?
            .get(&release_group_mbid.to_lowercase())
            .cloned()
    }

    fn set_pin(&self, pin: ReleasePin) {
        if let Ok(mut pins) = self.pins.lock() {
            pins.insert(pin.release_group_mbid.to_lowercase(), pin);
        }
    }

    fn clear_pin(&self, release_group_mbid: &str) -> bool {
        self.pins
            .lock()
            .map(|mut pins| pins.remove(&release_group_mbid.to_lowercase()).is_some())
            .unwrap_or(false)
    }
}

#[derive(Debug, Default)]
pub struct MemoryQueueStore {
    jobs: Mutex<HashMap<String, IdentifyJob>>,
    order: Mutex<Vec<String>>,
}

impl QueueStore for MemoryQueueStore {
    fn enqueue(&self, job: IdentifyJob) {
        if let Ok(mut order) = self.order.lock() {
            order.push(job.id.clone());
        }
        if let Ok(mut jobs) = self.jobs.lock() {
            jobs.insert(job.id.clone(), job);
        }
    }

    fn claim(&self, now_ms: u64, _lease_ms: u64) -> Option<IdentifyJob> {
        let mut jobs = self.jobs.lock().ok()?;
        let order = self.order.lock().ok()?;
        let mut best: Option<String> = None;
        for id in order.iter() {
            let Some(job) = jobs.get(id) else { continue };
            let ready = matches!(
                job.state,
                super::models::JobState::Queued | super::models::JobState::Deferred
            ) && job.not_before_ms <= now_ms;
            if !ready {
                continue;
            }
            let better = match best.as_ref().and_then(|id| jobs.get(id)) {
                None => true,
                Some(current) => job.priority < current.priority,
            };
            if better {
                best = Some(id.clone());
            }
        }
        let id = best?;
        let job = jobs.get_mut(&id)?;
        job.state = super::models::JobState::Running;
        Some(job.clone())
    }

    fn update(&self, job: IdentifyJob) {
        if let Ok(mut jobs) = self.jobs.lock() {
            jobs.insert(job.id.clone(), job);
        }
    }

    fn job(&self, job_id: &str) -> Option<IdentifyJob> {
        self.jobs.lock().ok()?.get(job_id).cloned()
    }

    fn jobs_for_album(&self, local_album_id: &str) -> Vec<IdentifyJob> {
        self.jobs
            .lock()
            .map(|jobs| {
                jobs.values()
                    .filter(|job| job.local_album_id == local_album_id)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[derive(Debug, Default)]
pub struct MemoryReviewStore {
    reviews: Mutex<HashMap<String, ReviewItem>>,
}

impl ReviewStore for MemoryReviewStore {
    fn file(&self, review: ReviewItem) {
        if let Ok(mut reviews) = self.reviews.lock() {
            reviews.insert(review.id.clone(), review);
        }
    }

    fn get(&self, review_id: &str) -> Option<ReviewItem> {
        self.reviews.lock().ok()?.get(review_id).cloned()
    }

    fn pending_for_album(&self, local_album_id: &str) -> Vec<ReviewItem> {
        self.reviews
            .lock()
            .map(|reviews| {
                reviews
                    .values()
                    .filter(|review| {
                        review.local_album_id == local_album_id
                            && review.state == ReviewState::Pending
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    fn set_state(
        &self,
        review_id: &str,
        state: ReviewState,
        by_user_id: Option<&str>,
        selected_key: Option<&str>,
    ) -> bool {
        let Ok(mut reviews) = self.reviews.lock() else {
            return false;
        };
        let Some(review) = reviews.get_mut(review_id) else {
            return false;
        };
        review.state = state;
        review.resolved_by_user_id = by_user_id.map(str::to_owned);
        review.selected_candidate_key = selected_key.map(str::to_owned);
        true
    }
}
