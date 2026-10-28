//! Listen counts for search results.
//!
//! ListenBrainz answers while it is switched on; otherwise Last.fm answers
//! with the instance API key, while Last.fm is switched on and a key is
//! saved; otherwise the ids echo back with no counts. A person is waiting,
//! so every call goes at user priority. A failed call leaves its rows
//! without counts and adds one degradation note for its source: counts
//! are optional, so they never fail a search.

use futures_util::stream::{self, StreamExt as _};

use droppedneedle_kernel::error::PROVIDER_UNAVAILABLE;
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::secret_sections::{
    LastFmSettings, ListenBrainzConnection,
};
use droppedneedle_providers::lastfm::{LastFmClient, LastFmCredentials};
use droppedneedle_providers::listenbrainz::{ListenBrainzClient, ListenBrainzCredentials};
use droppedneedle_providers::{ProviderError, Providers, RequestPriority};

use super::models::{
    AlbumEnrichment, AlbumEnrichmentRequest, ArtistEnrichment, ArtistEnrichmentRequest,
    Degradation, EnrichmentBatchRequest, EnrichmentResponse,
};
use super::ports::{BoxFuture, EnrichmentPort, EnrichmentPortError, MAX_ENRICHMENT_PER_BUCKET};
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
    /// Counts through the providers' ListenBrainz and Last.fm clients.
    pub fn new(
        providers: &Providers,
        listenbrainz: Live<ListenBrainzConnection>,
        lastfm: Live<LastFmSettings>,
    ) -> Self {
        Self {
            listenbrainz: providers.listenbrainz.clone(),
            listenbrainz_settings: listenbrainz,
            lastfm: providers.lastfm.clone(),
            lastfm_settings: lastfm,
        }
    }

    /// The source for this batch. A section that does not read answers
    /// none rather than standing in its defaults, so an unreadable
    /// ListenBrainz section never sends the batch to Last.fm.
    fn source(&self) -> Source {
        let Some(listenbrainz) = self.listenbrainz_settings.read() else {
            return Source::None;
        };
        if listenbrainz.enabled {
            return Source::ListenBrainz;
        }
        let Some(lastfm) = self.lastfm_settings.read() else {
            return Source::None;
        };
        let api_key = lastfm.api_key.expose().trim();
        if !lastfm.enabled || api_key.is_empty() {
            return Source::None;
        }
        Source::LastFm(LastFmCredentials {
            api_key: api_key.to_owned(),
            ..LastFmCredentials::default()
        })
    }

    /// Counts for one batch. Blank ids are skipped and each bucket keeps
    /// its first [`MAX_ENRICHMENT_PER_BUCKET`] rows.
    pub async fn enrich(&self, request: EnrichmentBatchRequest) -> EnrichmentResponse {
        let (artists, albums) = trimmed(request);
        let mut notes = Notes::default();
        let (source, artists, albums) = match self.source() {
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

    /// Each artist's count is the sum of its top release groups.
    async fn listenbrainz_artists(
        &self,
        artists: Vec<ArtistEnrichmentRequest>,
        notes: &mut Notes,
    ) -> Vec<ArtistEnrichment> {
        let creds = ListenBrainzCredentials::default();
        let creds = &creds;
        let outcomes: Vec<_> = stream::iter(
            artists
                .iter()
                .map(|item| async move {
                    self.listenbrainz
                        .artist_top_release_groups(
                            &item.musicbrainz_id,
                            ARTIST_TOP_RELEASES,
                            creds,
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

    /// One call answers the whole album bucket.
    async fn listenbrainz_albums(
        &self,
        albums: Vec<AlbumEnrichmentRequest>,
        notes: &mut Notes,
    ) -> Vec<AlbumEnrichment> {
        let mbids: Vec<String> = albums
            .iter()
            .map(|item| item.musicbrainz_id.clone())
            .collect();
        let outcome = self
            .listenbrainz
            .release_group_popularity(
                &mbids,
                &ListenBrainzCredentials::default(),
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
                listen_count: notes.keep(outcome).flatten().map(|info| info.listeners),
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
                listen_count: notes.keep(outcome).flatten().map(|info| info.playcount),
                ..bare_album(item)
            })
            .collect()
    }
}

impl EnrichmentPort for SearchCounts {
    fn enrich_batch(
        &self,
        request: EnrichmentBatchRequest,
    ) -> BoxFuture<'_, Result<EnrichmentResponse, EnrichmentPortError>> {
        Box::pin(async move { Ok(self.enrich(request).await) })
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
    request: EnrichmentBatchRequest,
) -> EnrichmentResponse {
    let (artists, albums) = trimmed(request);
    let request = EnrichmentBatchRequest {
        artists: artists.clone(),
        albums: albums.clone(),
    };
    match port.enrich_batch(request).await {
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
