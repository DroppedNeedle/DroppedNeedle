//! Listen counts for search results.
//!
//! Last.fm answers for a user who follows it and has a key to read with
//! (their own, else the instance's). Otherwise ListenBrainz answers while
//! it is switched on, and then Last.fm while it is switched on and a key
//! is saved; otherwise the ids echo back with no counts. ListenBrainz's
//! answers come from the rows Discover keeps, so a search asks only for
//! what no kept row holds. A person is waiting, so every call goes at
//! user priority. A failed call leaves its rows without counts and adds
//! one degradation note for its source: counts are optional, so they
//! never fail a search.

use futures_util::stream::{self, StreamExt as _};

use droppedneedle_auth::users::UsersDeps;
use droppedneedle_kernel::error::PROVIDER_UNAVAILABLE;
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::secret_sections::{
    LastFmSettings, ListenBrainzConnection,
};
use droppedneedle_media::playback::listening::ListeningStore;
use droppedneedle_providers::lastfm::{LastFmClient, LastFmCredentials};
use droppedneedle_providers::listenbrainz::ListenBrainzClient;
use droppedneedle_providers::{ProviderCacheStore, ProviderError, Providers, RequestPriority};

use super::models::{
    AlbumEnrichment, AlbumEnrichmentRequest, ArtistEnrichment, ArtistEnrichmentRequest,
    Degradation, EnrichmentBatchRequest, EnrichmentResponse,
};
use super::ports::{BoxFuture, EnrichmentPort, EnrichmentPortError, MAX_ENRICHMENT_PER_BUCKET};
use crate::reads::catalog::upstream::user_lastfm;
use crate::reads::discover::listening::shared;
use droppedneedle_kernel::runtime_config::sections::ListenSource;

/// Per-row lookups in flight at once, kept from v2: one small batch never
/// presses a provider harder than this.
const IN_FLIGHT: usize = 2;
/// Top release groups summed into one artist's ListenBrainz count, kept
/// from v2.
const ARTIST_TOP_RELEASES: usize = 5;
/// The fixed message of a degraded source.
const DEGRADED_MESSAGE: &str = "Enrichment temporarily unavailable";

/// Search-result counts over the ListenBrainz and Last.fm clients, with
/// both settings read on every call.
#[derive(Clone)]
pub struct SearchCounts {
    listenbrainz: ListenBrainzClient,
    listenbrainz_settings: Live<ListenBrainzConnection>,
    lastfm: LastFmClient,
    lastfm_settings: Live<LastFmSettings>,
    cache: ProviderCacheStore,
    listening: ListeningStore,
    users: UsersDeps,
}

impl std::fmt::Debug for SearchCounts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchCounts").finish_non_exhaustive()
    }
}

/// Which provider answers one batch.
enum Source {
    ListenBrainz,
    LastFm(LastFmCredentials),
    None,
}

impl SearchCounts {
    /// Counts through the providers' ListenBrainz and Last.fm clients, for
    /// the service each user follows in `listening`.
    pub fn new(
        providers: &Providers,
        listenbrainz: Live<ListenBrainzConnection>,
        lastfm: Live<LastFmSettings>,
        listening: ListeningStore,
        users: UsersDeps,
    ) -> Self {
        Self {
            listenbrainz: providers.listenbrainz.clone(),
            listenbrainz_settings: listenbrainz,
            lastfm: providers.lastfm.clone(),
            lastfm_settings: lastfm,
            cache: providers.cache.clone(),
            listening,
            users,
        }
    }

    /// The source for this batch. A section that does not read answers
    /// none rather than standing in its defaults, so an unreadable
    /// ListenBrainz section never sends the batch to Last.fm.
    async fn source(&self, user_id: &str) -> Source {
        let Some(listenbrainz) = self.listenbrainz_settings.read() else {
            return Source::None;
        };
        let instance_key = self.lastfm_settings.read().and_then(|lastfm| {
            let key = lastfm.api_key.expose().trim();
            (lastfm.enabled && !key.is_empty()).then(|| key.to_owned())
        });
        let follows_lastfm = self.listening.prefs(user_id).await.primary_music_source == "lastfm";
        if follows_lastfm && let Some(creds) = self.lastfm_for(user_id, &instance_key).await {
            return Source::LastFm(creds);
        }
        if listenbrainz.enabled {
            return Source::ListenBrainz;
        }
        match instance_key {
            Some(api_key) => Source::LastFm(LastFmCredentials {
                api_key,
                ..LastFmCredentials::default()
            }),
            None => Source::None,
        }
    }

    /// The key a user who follows Last.fm reads with: their own, else the
    /// instance's. `None` while Last.fm is switched off.
    async fn lastfm_for(
        &self,
        user_id: &str,
        instance_key: &Option<String>,
    ) -> Option<LastFmCredentials> {
        if !self.users.lastfm_switch.enabled() {
            return None;
        }
        let api_key = match user_lastfm(&self.users, user_id).await.api_key {
            Some(key) => key,
            None => instance_key.clone()?,
        };
        Some(LastFmCredentials {
            api_key,
            ..LastFmCredentials::default()
        })
    }

    /// Counts for one batch. Blank ids are skipped and each bucket keeps
    /// its first [`MAX_ENRICHMENT_PER_BUCKET`] rows.
    pub async fn enrich(
        &self,
        user_id: &str,
        request: EnrichmentBatchRequest,
    ) -> EnrichmentResponse {
        let (artists, albums) = trimmed(request);
        let mut notes = Notes::default();
        let (source, artists, albums) = match self.source(user_id).await {
            Source::ListenBrainz => (
                Some(ListenSource::Listenbrainz),
                self.listenbrainz_artists(artists, &mut notes).await,
                self.listenbrainz_albums(albums, &mut notes).await,
            ),
            Source::LastFm(creds) => (
                Some(ListenSource::Lastfm),
                self.lastfm_artists(&creds, artists, &mut notes).await,
                self.lastfm_albums(&creds, albums, &mut notes).await,
            ),
            Source::None => (
                None,
                artists.into_iter().map(bare_artist).collect(),
                albums.into_iter().map(bare_album).collect(),
            ),
        };
        EnrichmentResponse {
            artists,
            albums,
            source,
            degradations: notes.0,
        }
    }

    /// Each artist's count is the sum of its top release groups, from the
    /// artist's kept rows.
    async fn listenbrainz_artists(
        &self,
        artists: Vec<ArtistEnrichmentRequest>,
        notes: &mut Notes,
    ) -> Vec<ArtistEnrichment> {
        let outcomes: Vec<_> = stream::iter(
            artists
                .iter()
                .map(|item| async move {
                    shared::artist_albums(
                        &self.cache,
                        &self.listenbrainz,
                        &item.musicbrainz_id,
                        ARTIST_TOP_RELEASES,
                        RequestPriority::UserInitiated,
                    )
                    .await
                })
                .collect::<Vec<_>>(),
        )
        .buffered(IN_FLIGHT)
        .collect()
        .await;
        artists
            .into_iter()
            .zip(outcomes)
            .map(|(item, outcome)| {
                let count = match notes.keep(outcome) {
                    Some(top) if !top.is_empty() => {
                        Some(top.iter().map(|row| row.listen_count).sum())
                    }
                    _ => None,
                };
                ArtistEnrichment {
                    listen_count: count,
                    ..bare_artist(item)
                }
            })
            .collect()
    }

    /// One call answers whatever of the album bucket no kept row holds.
    async fn listenbrainz_albums(
        &self,
        albums: Vec<AlbumEnrichmentRequest>,
        notes: &mut Notes,
    ) -> Vec<AlbumEnrichment> {
        let mbids: Vec<String> = albums
            .iter()
            .map(|item| item.musicbrainz_id.clone())
            .collect();
        let outcome = shared::listen_counts(
            &self.cache,
            &self.listenbrainz,
            &mbids,
            RequestPriority::UserInitiated,
        )
        .await;
        let counts = notes.keep(outcome).unwrap_or_default();
        albums
            .into_iter()
            .map(|item| AlbumEnrichment {
                listen_count: counts.get(&item.musicbrainz_id).copied(),
                ..bare_album(item)
            })
            .collect()
    }

    /// Each artist's Last.fm listeners, by MusicBrainz id and then, when
    /// Last.fm does not know the id, by name.
    async fn lastfm_artists(
        &self,
        creds: &LastFmCredentials,
        artists: Vec<ArtistEnrichmentRequest>,
        notes: &mut Notes,
    ) -> Vec<ArtistEnrichment> {
        let outcomes: Vec<_> = stream::iter(
            artists
                .iter()
                .map(|item| async move {
                    let priority = RequestPriority::UserInitiated;
                    let name = item.name.trim();
                    let by_id = self
                        .lastfm
                        .artist_info(creds, name, Some(&item.musicbrainz_id), priority)
                        .await?;
                    match by_id {
                        Some(info) => Ok(Some(info)),
                        None if !name.is_empty() => {
                            self.lastfm.artist_info(creds, name, None, priority).await
                        }
                        None => Ok(None),
                    }
                })
                .collect::<Vec<_>>(),
        )
        .buffered(IN_FLIGHT)
        .collect()
        .await;
        artists
            .into_iter()
            .zip(outcomes)
            .map(|(item, outcome)| ArtistEnrichment {
                listen_count: notes
                    .keep(outcome)
                    .flatten()
                    .and_then(|info| info.listeners),
                ..bare_artist(item)
            })
            .collect()
    }

    /// Each album's Last.fm play count, by artist and album name: the
    /// request carries a release-group id, which Last.fm does not key
    /// albums by. A row without both names stays without a count.
    async fn lastfm_albums(
        &self,
        creds: &LastFmCredentials,
        albums: Vec<AlbumEnrichmentRequest>,
        notes: &mut Notes,
    ) -> Vec<AlbumEnrichment> {
        let outcomes: Vec<_> = stream::iter(
            albums
                .iter()
                .map(|item| async move {
                    let (artist, album) = (item.artist_name.trim(), item.album_name.trim());
                    if artist.is_empty() || album.is_empty() {
                        return Ok(None);
                    }
                    self.lastfm
                        .album_info(creds, artist, album, None, RequestPriority::UserInitiated)
                        .await
                })
                .collect::<Vec<_>>(),
        )
        .buffered(IN_FLIGHT)
        .collect()
        .await;
        albums
            .into_iter()
            .zip(outcomes)
            .map(|(item, outcome)| AlbumEnrichment {
                listen_count: notes
                    .keep(outcome)
                    .flatten()
                    .and_then(|info| info.playcount),
                ..bare_album(item)
            })
            .collect()
    }
}

impl EnrichmentPort for SearchCounts {
    fn enrich_batch<'a>(
        &'a self,
        user_id: &'a str,
        request: EnrichmentBatchRequest,
    ) -> BoxFuture<'a, Result<EnrichmentResponse, EnrichmentPortError>> {
        Box::pin(async move { Ok(self.enrich(user_id, request).await) })
    }
}

/// The degradation notes of one batch, one per failed source.
#[derive(Default)]
struct Notes(Vec<Degradation>);

impl Notes {
    /// The answer of a call that succeeded; a failure is logged, noted
    /// once for its source, and reads as no answer.
    fn keep<T>(&mut self, outcome: Result<T, ProviderError>) -> Option<T> {
        match outcome {
            Ok(value) => Some(value),
            Err(error) => {
                let source = error.source();
                tracing::debug!(source, cause = %error, "search counts degraded");
                if !self.0.iter().any(|note| note.source == source) {
                    self.0.push(note(source));
                }
                None
            }
        }
    }
}

/// One fixed degradation note: only the source varies, provider detail
/// never reaches the wire.
fn note(source: &str) -> Degradation {
    Degradation {
        source: source.to_owned(),
        code: PROVIDER_UNAVAILABLE.to_owned(),
        message: DEGRADED_MESSAGE.to_owned(),
    }
}

/// The batch without blank ids, each bucket capped.
fn trimmed(
    request: EnrichmentBatchRequest,
) -> (Vec<ArtistEnrichmentRequest>, Vec<AlbumEnrichmentRequest>) {
    let artists = request
        .artists
        .into_iter()
        .filter(|item| !item.musicbrainz_id.trim().is_empty())
        .take(MAX_ENRICHMENT_PER_BUCKET)
        .collect();
    let albums = request
        .albums
        .into_iter()
        .filter(|item| !item.musicbrainz_id.trim().is_empty())
        .take(MAX_ENRICHMENT_PER_BUCKET)
        .collect();
    (artists, albums)
}

fn bare_artist(item: ArtistEnrichmentRequest) -> ArtistEnrichment {
    ArtistEnrichment {
        musicbrainz_id: item.musicbrainz_id,
        release_group_count: None,
        listen_count: None,
    }
}

fn bare_album(item: AlbumEnrichmentRequest) -> AlbumEnrichment {
    AlbumEnrichment {
        musicbrainz_id: item.musicbrainz_id,
        track_count: None,
        listen_count: None,
    }
}

/// Enrich one batch through the port. A port failure degrades into a note
/// inside a successful response: ids echo back with no counts and source
/// `none`. The cause goes to the log only.
pub async fn enrich_with_degradation(
    port: &dyn EnrichmentPort,
    user_id: &str,
    request: EnrichmentBatchRequest,
) -> EnrichmentResponse {
    let (artists, albums) = trimmed(request);
    let request = EnrichmentBatchRequest {
        artists: artists.clone(),
        albums: albums.clone(),
    };
    match port.enrich_batch(user_id, request).await {
        Ok(response) => response,
        Err(error) => {
            tracing::error!(source = %error.source, cause = %error.message, "search enrichment degraded");
            EnrichmentResponse {
                artists: artists.into_iter().map(bare_artist).collect(),
                albums: albums.into_iter().map(bare_album).collect(),
                source: None,
                degradations: vec![note(&error.source)],
            }
        }
    }
}
