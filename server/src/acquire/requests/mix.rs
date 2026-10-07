//! "Your Weekly Mix": one playlist per ListenBrainz-linked user.
//!
//! The mix starts from the user's ListenBrainz recommendation playlists
//! (weekly-jams, then weekly-exploration) and is topped up to 100 tracks
//! with the top recording of artists similar to the first eight artists in
//! it. Tracks the library holds point at the local file, so they play
//! straight away. When the user has asked for it and holds the standing
//! grant (admins always do; everyone else needs an admin's approval), up
//! to five missing albums per build are requested.
//!
//! A build is skipped, with a stable reason, when the user has no
//! ListenBrainz link, the mix is less than six days old (ListenBrainz
//! refreshes its playlists weekly), nothing could be gathered, or the
//! account is gone. Builds for one user are serialized: the daily sweep and
//! a manual refresh never race to create the same playlist.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use futures_util::future::BoxFuture;

use super::RequestsState;
use super::auth::{Principal, Role};
use super::error::RequestsError;
use super::models::AlbumIntake;
use super::service::RequestsService;

use crate::events::{EventSink, PersonalMixRefreshed, UserNotice, model::new_event_id};
use crate::plugins::scrobble::{ScrobblePrefsPatch, ScrobblePrefsStore};
use crate::providers::listenbrainz::playlists::{RecommendationPlaylist, RecommendationTrack};
use crate::providers::listenbrainz::{
    ListenBrainzCredentials, SimilarArtist, TopRecording, TopReleaseGroup,
};
use crate::reads::catalog::library::LocalCatalog;
use crate::reads::collections::store::playlists::{NewEntry, PlaylistStore, Written};
use crate::reads::discover::adapters::queue::select::{self, Shuffler};
use crate::reads::discover::models::QueueItemLight;

/// The playlist's display name (v2).
pub const PLAYLIST_NAME: &str = "Your Weekly Mix";
/// Source-ref prefix that marks a user's mix playlist.
pub const SOURCE_REF_PREFIX: &str = "personal-mix:";
/// A mix younger than this is fresh; the daily sweep leaves it alone.
pub const REFRESH_MIN_AGE_SECS: u64 = 6 * 86_400;
/// Recommendation playlists read, in this order.
pub const RECOMMENDATION_PATCHES: [&str; 2] = ["weekly-jams", "weekly-exploration"];
/// Distinct artists seeding the similar-artist top-up.
pub const MAX_SEED_ARTISTS: usize = 8;
/// Similar artists read per seed.
pub const SIMILAR_LIMIT: usize = 10;
/// Albums taken per similar artist.
pub const ALBUMS_PER_SEED: usize = 2;
/// Tracks in a full mix.
pub const TRACK_CAP: usize = 100;
/// Most albums one build may request. Exploration and top-up tracks are
/// missing by construction, so an uncapped walk would request most of the
/// mix every week.
pub const MAX_AUTO_REQUESTS: usize = 5;
/// MusicBrainz "Various Artists": never a useful similar artist.
const VARIOUS_ARTISTS_MBID: &str = "89ad4ac3-39f7-470e-963a-56509c546377";

/// Why a build produced no mix. The wire form is the stable code the
/// `personal_mix_refreshed` event carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// The user has no usable ListenBrainz link.
    NotLinked,
    /// The mix was rebuilt less than six days ago.
    Fresh,
    /// ListenBrainz offered nothing to build from.
    NoTracks,
    /// The account no longer exists.
    UserNotFound,
}

impl SkipReason {
    /// Stable code (v2 spellings).
    pub fn code(self) -> &'static str {
        match self {
            Self::NotLinked => "listenbrainz_not_linked",
            Self::Fresh => "fresh",
            Self::NoTracks => "no_tracks",
            Self::UserNotFound => "user_not_found",
        }
    }
}

/// What one build did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MixOutcome {
    /// The mix playlist, when one exists.
    pub playlist_id: Option<String>,
    /// Tracks written.
    pub track_count: u32,
    /// Albums requested to fill the gaps.
    pub requested_albums: u32,
    /// Set when no mix was written.
    pub skipped: Option<SkipReason>,
}

impl MixOutcome {
    fn skipped(reason: SkipReason, playlist_id: Option<String>) -> Self {
        Self {
            playlist_id,
            track_count: 0,
            requested_albums: 0,
            skipped: Some(reason),
        }
    }

    /// The event the web UI listens for.
    pub fn notice(&self) -> UserNotice {
        UserNotice::PersonalMixRefreshed(PersonalMixRefreshed {
            playlist_id: self.playlist_id.clone(),
            track_count: self.track_count,
            requested_albums: self.requested_albums,
            skipped: self.skipped.is_some(),
            reason: self
                .skipped
                .map(|reason| reason.code().to_owned())
                .unwrap_or_default(),
            event_id: new_event_id(),
        })
    }
}

/// Totals of one all-users sweep.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MixSweep {
    /// Linked users looked at.
    pub considered: u32,
    /// Mixes written.
    pub built: u32,
    /// Users skipped with a reason.
    pub skipped: u32,
    /// Users whose build failed.
    pub errors: u32,
}

/// The ListenBrainz reads a build needs. Errors carry a log-only cause;
/// the builder treats every read as optional and carries on without it.
pub trait MixSources: Send + Sync {
    /// The user's ListenBrainz identity; `None` when unlinked or when the
    /// stored token no longer opens.
    fn identity<'a>(&'a self, user_id: &'a str) -> BoxFuture<'a, Option<ListenBrainzCredentials>>;
    /// Every user with an enabled ListenBrainz link.
    fn linked_users(&self) -> BoxFuture<'_, Result<Vec<String>, String>>;
    /// The recommendation playlists made for the user.
    fn recommendation_playlists<'a>(
        &'a self,
        creds: &'a ListenBrainzCredentials,
    ) -> BoxFuture<'a, Result<Vec<RecommendationPlaylist>, String>>;
    /// One playlist's tracks.
    fn playlist_tracks<'a>(
        &'a self,
        playlist_id: &'a str,
        creds: &'a ListenBrainzCredentials,
    ) -> BoxFuture<'a, Result<Vec<RecommendationTrack>, String>>;
    /// Recording MBID to release-group MBID, for the ones ListenBrainz knows.
    fn release_groups<'a>(
        &'a self,
        recording_mbids: &'a [String],
        creds: &'a ListenBrainzCredentials,
    ) -> BoxFuture<'a, Result<HashMap<String, String>, String>>;
    /// Artists similar to one artist, most listened first.
    fn similar_artists<'a>(
        &'a self,
        artist_mbid: &'a str,
        limit: usize,
    ) -> BoxFuture<'a, Result<Vec<SimilarArtist>, String>>;
    /// An artist's most played release groups.
    fn top_release_groups<'a>(
        &'a self,
        artist_mbid: &'a str,
        count: usize,
    ) -> BoxFuture<'a, Result<Vec<TopReleaseGroup>, String>>;
    /// An artist's most played recording.
    fn top_recording<'a>(
        &'a self,
        artist_mbid: &'a str,
    ) -> BoxFuture<'a, Result<Option<TopRecording>, String>>;
}

/// A library file matched to a mix track: id, track number, disc number.
type LibraryFile = (String, Option<i64>, Option<i64>);

/// One track picked for the mix.
#[derive(Debug, Clone)]
struct MixTrack {
    track_name: String,
    artist_name: String,
    album_name: String,
    release_group_mbid: String,
    artist_mbid: Option<String>,
    recording_mbid: Option<String>,
    in_library: bool,
    library_file_id: Option<String>,
    track_number: Option<i32>,
    disc_number: i32,
}

/// Builds and refreshes personal mixes.
pub struct PersonalMixBuilder {
    sources: Arc<dyn MixSources>,
    prefs: Arc<dyn ScrobblePrefsStore>,
    playlists: PlaylistStore,
    requests: RequestsState,
    events: EventSink,
    locks: tokio::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl PersonalMixBuilder {
    /// Builder over its ports. `requests` supplies the library database,
    /// the approval rows and album intake.
    pub fn new(
        sources: Arc<dyn MixSources>,
        prefs: Arc<dyn ScrobblePrefsStore>,
        playlists: PlaylistStore,
        requests: RequestsState,
        events: EventSink,
    ) -> Self {
        Self {
            sources,
            prefs,
            playlists,
            requests,
            events,
            locks: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Whether the user can build a mix at all.
    pub async fn is_linked(&self, user_id: &str) -> bool {
        self.sources
            .identity(user_id)
            .await
            .is_some_and(|creds| creds.user_token.is_some())
    }

    /// Turn the user's auto-request intent off (after a reject or revoke).
    pub async fn clear_intent(&self, user_id: &str) {
        let patch = ScrobblePrefsPatch {
            auto_request_personal_mix: Some(false),
            ..ScrobblePrefsPatch::default()
        };
        self.prefs.upsert(user_id, &patch).await;
    }

    /// Display state of the user's standing grant
    /// (`none|pending|approved|rejected|revoked`), with the admin override.
    /// An enabled toggle with no row reads `none` (v2).
    pub async fn auto_request_state(&self, user_id: &str, role: Role, toggle_on: bool) -> String {
        if toggle_on && role == Role::Admin {
            return "approved".to_owned();
        }
        let state = match self.requests.mixes.state(user_id).await {
            Ok(state) => state,
            Err(error) => {
                tracing::warn!(?error, "personal mix approval read failed; showing none");
                None
            }
        };
        match (toggle_on, state.as_deref()) {
            (true, Some(state)) => state.to_owned(),
            (true, None) => "none".to_owned(),
            (false, Some(state @ ("rejected" | "revoked"))) => state.to_owned(),
            (false, _) => "none".to_owned(),
        }
    }

    /// Grant bookkeeping after the toggle changed: a non-admin turning it on
    /// joins the approval queue again, even over an earlier grant. Turning
    /// it off keeps the row.
    pub async fn on_toggle(&self, user_id: &str, role: Role, enabled: bool) {
        if !enabled || role == Role::Admin {
            return;
        }
        if let Err(error) = self.requests.mixes.file_pending(user_id, now_epoch()).await {
            tracing::error!(?error, "personal mix approval could not be queued");
        }
    }

    /// Rebuild one user's mix in the background and announce the result
    /// on the user's event stream. The refresh key the route claimed is
    /// released when the build lands, whatever the outcome.
    pub fn spawn_refresh(self: &Arc<Self>, user_id: String) {
        let builder = Arc::clone(self);
        tokio::spawn(async move {
            match builder.build_for_user(&user_id, true).await {
                Ok(outcome) => builder.events.notify(&user_id, outcome.notice()),
                Err(error) => {
                    tracing::error!(?error, "personal mix refresh failed");
                }
            }
            if let Err(error) = builder.requests.mixes.refresh_finish(&user_id) {
                tracing::error!(?error, "personal mix refresh key could not be released");
            }
        });
    }

    /// Rebuild every linked user's mix. One user's failure never stops the
    /// sweep; only a failed user listing fails it.
    pub async fn run_for_all_users(&self) -> Result<MixSweep, String> {
        let users = self.sources.linked_users().await?;
        let mut sweep = MixSweep {
            considered: u32::try_from(users.len()).unwrap_or(u32::MAX),
            ..MixSweep::default()
        };
        for user_id in users {
            match self.build_for_user(&user_id, false).await {
                Ok(outcome) if outcome.skipped.is_some() => sweep.skipped += 1,
                Ok(outcome) => {
                    sweep.built += 1;
                    self.events.notify(&user_id, outcome.notice());
                }
                Err(error) => {
                    sweep.errors += 1;
                    tracing::error!(?error, user_id, "personal mix build failed");
                }
            }
        }
        tracing::info!(?sweep, "personal mix refresh complete");
        Ok(sweep)
    }

    /// Build one user's mix. `force` ignores the six-day freshness window.
    pub async fn build_for_user(
        &self,
        user_id: &str,
        force: bool,
    ) -> Result<MixOutcome, RequestsError> {
        let lock = {
            let mut locks = self.locks.lock().await;
            Arc::clone(locks.entry(user_id.to_owned()).or_default())
        };
        let _held = lock.lock().await;
        self.build_locked(user_id, force).await
    }

    async fn build_locked(&self, user_id: &str, force: bool) -> Result<MixOutcome, RequestsError> {
        let Some(creds) = self
            .sources
            .identity(user_id)
            .await
            .filter(|creds| creds.user_token.is_some())
        else {
            return Ok(MixOutcome::skipped(SkipReason::NotLinked, None));
        };
        let source_ref = format!("{SOURCE_REF_PREFIX}{user_id}");
        let existing = self
            .playlists
            .find_by_source(user_id, &source_ref)
            .await
            .map_err(store_fault)?;
        if !force && let Some(id) = &existing {
            let updated_at = self
                .playlists
                .get(id)
                .await
                .map_err(store_fault)?
                .map(|row| row.updated_at);
            if updated_at.is_some_and(|at| now_epoch().saturating_sub(at) < REFRESH_MIN_AGE_SECS) {
                return Ok(MixOutcome::skipped(SkipReason::Fresh, existing));
            }
        }
        let owned: HashSet<String> = LocalCatalog::new(self.requests.library.pool().clone())
            .all_owned_release_groups()
            .await
            .map_err(|error| RequestsError::internal(&error))?
            .into_iter()
            .collect();

        let mut mix = self.recommended_tracks(&creds, &owned).await;
        let seeds = pick_seed_artists(&mix);
        if !seeds.is_empty() && mix.len() < TRACK_CAP {
            let needed = TRACK_CAP - mix.len();
            mix.extend(self.similar_artist_tracks(&seeds, &owned, needed).await);
        }
        mix.truncate(TRACK_CAP);
        if mix.is_empty() {
            return Ok(MixOutcome::skipped(SkipReason::NoTracks, existing));
        }
        self.match_library_files(&mut mix).await?;

        let Some(role) = self.role_of(user_id).await? else {
            return Ok(MixOutcome::skipped(SkipReason::UserNotFound, None));
        };
        let playlist_id = self
            .write_playlist(existing, user_id, &source_ref, &mix)
            .await?;
        let requested = if self.prefs.get(user_id).await.auto_request_personal_mix
            && self.granted(user_id, role).await?
        {
            self.request_missing(user_id, role, &mix).await
        } else {
            0
        };
        Ok(MixOutcome {
            playlist_id: Some(playlist_id),
            track_count: u32::try_from(mix.len()).unwrap_or(u32::MAX),
            requested_albums: requested,
            skipped: None,
        })
    }

    /// Weekly-jams then weekly-exploration tracks, resolved to release
    /// groups, one entry per recording.
    async fn recommended_tracks(
        &self,
        creds: &ListenBrainzCredentials,
        owned: &HashSet<String>,
    ) -> Vec<MixTrack> {
        let playlists = match self.sources.recommendation_playlists(creds).await {
            Ok(playlists) => playlists,
            Err(cause) => {
                tracing::warn!(%cause, "personal mix: recommendation playlists unavailable");
                return Vec::new();
            }
        };
        let mut tracks = Vec::new();
        let mut seen_recordings: HashSet<String> = HashSet::new();
        for patch in RECOMMENDATION_PATCHES {
            let Some(playlist) = playlists.iter().find(|p| p.source_patch == patch) else {
                continue;
            };
            let entries = match self
                .sources
                .playlist_tracks(&playlist.playlist_id, creds)
                .await
            {
                Ok(entries) => entries,
                Err(cause) => {
                    tracing::warn!(%cause, patch, "personal mix: playlist unavailable");
                    continue;
                }
            };
            let recordings: Vec<String> = entries
                .iter()
                .filter_map(|track| track.recording_mbid.clone())
                .collect();
            let groups = match self.sources.release_groups(&recordings, creds).await {
                Ok(groups) => groups,
                Err(cause) => {
                    tracing::warn!(%cause, patch, "personal mix: album lookup unavailable");
                    continue;
                }
            };
            for track in entries {
                let Some(recording) = track.recording_mbid.clone() else {
                    continue;
                };
                let Some(group) = groups.get(&recording) else {
                    continue;
                };
                if !seen_recordings.insert(recording.to_ascii_lowercase()) {
                    continue;
                }
                let group = group.to_ascii_lowercase();
                tracks.push(MixTrack {
                    in_library: owned.contains(&group),
                    track_name: track.title,
                    artist_name: track.creator,
                    album_name: track.album,
                    release_group_mbid: group,
                    artist_mbid: track.artist_mbids.first().cloned(),
                    recording_mbid: Some(recording),
                    library_file_id: None,
                    track_number: None,
                    disc_number: 1,
                });
            }
        }
        tracks.truncate(TRACK_CAP);
        tracks
    }

    /// Top-up: albums of artists similar to the seeds, picked round-robin
    /// across seeds, each carrying its artist's most played recording.
    async fn similar_artist_tracks(
        &self,
        seeds: &[(String, String)],
        owned: &HashSet<String>,
        needed: usize,
    ) -> Vec<MixTrack> {
        let mut pools: Vec<Vec<QueueItemLight>> = Vec::with_capacity(seeds.len());
        for (seed_mbid, seed_name) in seeds {
            let mut pool = Vec::new();
            let mut pool_seen: HashSet<String> = HashSet::new();
            let similar = self
                .sources
                .similar_artists(seed_mbid, SIMILAR_LIMIT)
                .await
                .unwrap_or_else(|cause| {
                    tracing::debug!(%cause, "personal mix: similar artists unavailable");
                    Vec::new()
                });
            for artist in similar {
                let artist_mbid = artist.artist_mbid.to_ascii_lowercase();
                if artist_mbid == VARIOUS_ARTISTS_MBID {
                    continue;
                }
                let groups = self
                    .sources
                    .top_release_groups(&artist_mbid, ALBUMS_PER_SEED)
                    .await
                    .unwrap_or_else(|cause| {
                        tracing::debug!(%cause, "personal mix: artist albums unavailable");
                        Vec::new()
                    });
                for group in groups {
                    let group_mbid = group.release_group_mbid.to_ascii_lowercase();
                    if owned.contains(&group_mbid) || !pool_seen.insert(group_mbid.clone()) {
                        continue;
                    }
                    pool.push(QueueItemLight {
                        release_group_mbid: group_mbid,
                        album_name: group.name,
                        artist_name: if group.artist_name.is_empty() {
                            artist.artist_name.clone()
                        } else {
                            group.artist_name
                        },
                        artist_mbid: artist_mbid.clone(),
                        recommendation_reason: format!("Similar to {seed_name}"),
                        cover_url: None,
                        is_wildcard: false,
                        in_library: false,
                    });
                }
            }
            pools.push(pool);
        }
        // Headroom: some artists have no top recording to offer.
        let candidates = select::round_robin(
            pools,
            needed.saturating_mul(2),
            select::MAX_PER_ARTIST,
            &mut Shuffler::random(),
        );
        let mut top: HashMap<String, Option<TopRecording>> = HashMap::new();
        let mut tracks = Vec::new();
        for item in candidates {
            if tracks.len() >= needed {
                break;
            }
            if !top.contains_key(&item.artist_mbid) {
                let recording = self
                    .sources
                    .top_recording(&item.artist_mbid)
                    .await
                    .unwrap_or_else(|cause| {
                        tracing::debug!(%cause, "personal mix: top recording unavailable");
                        None
                    });
                top.insert(item.artist_mbid.clone(), recording);
            }
            let Some(Some(recording)) = top.get(&item.artist_mbid) else {
                continue;
            };
            tracks.push(MixTrack {
                track_name: recording.title.clone(),
                artist_name: item.artist_name,
                album_name: item.album_name,
                release_group_mbid: item.release_group_mbid,
                artist_mbid: Some(item.artist_mbid),
                recording_mbid: recording.recording_mbid.clone(),
                // Owned albums never enter the pools.
                in_library: false,
                library_file_id: None,
                track_number: None,
                disc_number: 1,
            });
        }
        tracks
    }

    /// Point owned tracks at their library file, matched by recording.
    async fn match_library_files(&self, mix: &mut [MixTrack]) -> Result<(), RequestsError> {
        let groups: Vec<String> = mix
            .iter()
            .filter(|track| track.in_library)
            .map(|track| track.release_group_mbid.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        if groups.is_empty() {
            return Ok(());
        }
        let sql = format!(
            "SELECT t.id, lower(ae.release_group_mbid), lower(te.recording_mbid), \
             t.track_number, t.disc_number FROM local_tracks t \
             JOIN local_albums b ON b.id = t.local_album_id \
             JOIN local_album_external_identities ae ON ae.local_album_id = b.id \
             JOIN local_track_external_identities te \
             ON te.local_track_id = t.id AND te.provider = 'musicbrainz' \
             WHERE b.retired_into_album_id IS NULL AND t.availability = 'indexed' \
             AND te.recording_mbid IS NOT NULL AND lower(ae.release_group_mbid) IN ({})",
            vec!["?"; groups.len()].join(", ")
        );
        let mut query =
            sqlx::query_as::<_, (String, String, String, Option<i64>, Option<i64>)>(&sql);
        for group in &groups {
            query = query.bind(group);
        }
        let rows = query
            .fetch_all(self.requests.library.pool())
            .await
            .map_err(|error| RequestsError::internal(&error))?;
        let by_key: HashMap<(String, String), LibraryFile> = rows
            .into_iter()
            .map(|(id, group, recording, number, disc)| ((group, recording), (id, number, disc)))
            .collect();
        for track in mix.iter_mut().filter(|track| track.in_library) {
            let Some(recording) = &track.recording_mbid else {
                continue;
            };
            let key = (
                track.release_group_mbid.clone(),
                recording.to_ascii_lowercase(),
            );
            if let Some((id, number, disc)) = by_key.get(&key) {
                track.library_file_id = Some(id.clone());
                track.track_number = number.and_then(|n| i32::try_from(n).ok());
                track.disc_number = disc.and_then(|d| i32::try_from(d).ok()).unwrap_or(1);
            }
        }
        Ok(())
    }

    /// Create the mix playlist or replace its entries.
    async fn write_playlist(
        &self,
        existing: Option<String>,
        user_id: &str,
        source_ref: &str,
        mix: &[MixTrack],
    ) -> Result<String, RequestsError> {
        let playlist_id = match existing {
            Some(id) => {
                let entries: Vec<String> = self
                    .playlists
                    .tracks(&id)
                    .await
                    .map_err(store_fault)?
                    .into_iter()
                    .map(|track| track.id)
                    .collect();
                if !entries.is_empty() {
                    self.playlists
                        .remove(&id, &entries)
                        .await
                        .map_err(store_fault)?;
                }
                id
            }
            None => match self
                .playlists
                .create(user_id, PLAYLIST_NAME, Some(source_ref))
                .await
                .map_err(store_fault)?
            {
                Some(id) => id,
                None => self
                    .playlists
                    .find_by_source(user_id, source_ref)
                    .await
                    .map_err(store_fault)?
                    .ok_or_else(|| RequestsError::internal(&"mix playlist vanished on create"))?,
            },
        };
        let entries = mix.iter().map(entry_for).collect();
        match self
            .playlists
            .insert(&playlist_id, None, entries)
            .await
            .map_err(store_fault)?
        {
            Written::Done(_) => Ok(playlist_id),
            Written::Missing => Err(RequestsError::internal(&"mix playlist deleted mid-build")),
        }
    }

    /// Admins hold the grant by role (so a demotion drops it); everyone
    /// else needs an approved row.
    async fn granted(&self, user_id: &str, role: Role) -> Result<bool, RequestsError> {
        if role == Role::Admin {
            return Ok(true);
        }
        Ok(self.requests.mixes.state(user_id).await?.as_deref() == Some("approved"))
    }

    /// Request up to [`MAX_AUTO_REQUESTS`] missing albums under the
    /// standing grant. A quota or storage refusal ends the walk: every
    /// further ask would be refused the same way.
    async fn request_missing(&self, user_id: &str, role: Role, mix: &[MixTrack]) -> u32 {
        let service = RequestsService::new(&self.requests);
        let principal = Principal {
            user_id: user_id.to_owned(),
            username: None,
            role,
        };
        let mut requested = 0u32;
        let mut seen: HashSet<&str> = HashSet::new();
        for track in mix {
            if requested as usize >= MAX_AUTO_REQUESTS {
                break;
            }
            if track.in_library || !seen.insert(track.release_group_mbid.as_str()) {
                continue;
            }
            let intake = AlbumIntake {
                musicbrainz_id: track.release_group_mbid.clone(),
                artist: Some(track.artist_name.clone()),
                album: Some(track.album_name.clone()),
                year: None,
                artist_mbid: track.artist_mbid.clone(),
                release_mbid: None,
                monitor_artist: false,
                auto_download_artist: false,
            };
            match service.request_album_granted(&principal, &intake).await {
                Ok(response) if response.task_id.is_some() => requested += 1,
                Ok(_) => {}
                Err(RequestsError::QuotaExceeded { .. } | RequestsError::StorageFull { .. }) => {
                    tracing::info!(user_id, "personal mix: request quota reached; stopping");
                    break;
                }
                Err(error) => {
                    tracing::warn!(?error, album = %track.release_group_mbid,
                        "personal mix: album request failed");
                }
            }
        }
        requested
    }

    async fn role_of(&self, user_id: &str) -> Result<Option<Role>, RequestsError> {
        let role: Option<String> = sqlx::query_scalar("SELECT role FROM auth_users WHERE id = ?")
            .bind(user_id)
            .fetch_optional(self.requests.library.pool())
            .await
            .map_err(|error| RequestsError::internal(&error))?;
        Ok(role.map(|role| Role::parse(&role).unwrap_or(Role::User)))
    }
}

/// The first distinct `(artist_mbid, artist_name)` pairs, in mix order.
fn pick_seed_artists(tracks: &[MixTrack]) -> Vec<(String, String)> {
    let mut seeds: Vec<(String, String)> = Vec::new();
    for track in tracks {
        if seeds.len() >= MAX_SEED_ARTISTS {
            break;
        }
        if let Some(mbid) = &track.artist_mbid
            && !seeds.iter().any(|(seen, _)| seen == mbid)
        {
            seeds.push((mbid.clone(), track.artist_name.clone()));
        }
    }
    seeds
}

/// One mix track as a playlist entry. Owned tracks are local entries; the
/// rest carry the recording id so the player can find another source.
fn entry_for(track: &MixTrack) -> NewEntry {
    NewEntry {
        track_name: track.track_name.clone(),
        artist_name: track.artist_name.clone(),
        album_name: track.album_name.clone(),
        album_id: Some(track.release_group_mbid.clone()),
        artist_id: track.artist_mbid.clone(),
        track_source_id: track
            .library_file_id
            .clone()
            .or_else(|| track.recording_mbid.clone()),
        cover_url: Some(format!(
            "/api/v3/covers/release-group/{}?size=250",
            track.release_group_mbid
        )),
        source_type: if track.library_file_id.is_some() {
            "local".to_owned()
        } else {
            String::new()
        },
        track_number: track.track_number,
        disc_number: Some(track.disc_number),
        library_file_id: track.library_file_id.clone(),
        ..NewEntry::default()
    }
}

fn store_fault(error: crate::reads::collections::db::StoreError) -> RequestsError {
    RequestsError::internal(&error)
}

fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// The scrobble-settings hooks over the mix slot: the toggle feeds the
/// approval queue and the prefs response reads the real grant state.
/// Before boot fills the slot both fall back to the role-only answer.
pub struct MixGrantHooks {
    slot: super::state::MixSlot,
}

impl MixGrantHooks {
    /// Hooks over the requests state's mix slot.
    pub fn new(slot: super::state::MixSlot) -> Self {
        Self { slot }
    }
}

impl crate::plugins::scrobble::MixApprovalHook for MixGrantHooks {
    fn on_auto_request_toggled<'a>(
        &'a self,
        user_id: &'a str,
        role: &'a str,
        enabled: bool,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            if let Some(builder) = self.slot.get() {
                let role = Role::parse(role).unwrap_or(Role::User);
                builder.on_toggle(user_id, role, enabled).await;
            }
        })
    }
}

impl crate::plugins::scrobble::MixStateReader for MixGrantHooks {
    fn auto_request_state<'a>(
        &'a self,
        user_id: &'a str,
        role: &'a str,
        toggle_on: bool,
    ) -> BoxFuture<'a, String> {
        Box::pin(async move {
            let role = Role::parse(role).unwrap_or(Role::User);
            match self.slot.get() {
                Some(builder) => builder.auto_request_state(user_id, role, toggle_on).await,
                None if toggle_on && role == Role::Admin => "approved".to_owned(),
                None => "none".to_owned(),
            }
        })
    }
}

/// The daily sweep drives the builder in the slot; before boot fills it
/// the sweep has nothing to do.
impl crate::jobs::personal_mix::PersonalMixer for MixGrantHooks {
    fn run_for_all_users(&self) -> crate::jobs::registry::BoxFuture<'_, Result<(), String>> {
        Box::pin(async move {
            match self.slot.get() {
                Some(builder) => builder.run_for_all_users().await.map(|_| ()),
                None => Ok(()),
            }
        })
    }
}
