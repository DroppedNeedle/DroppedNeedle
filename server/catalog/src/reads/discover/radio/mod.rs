//! Radio (v2 `DiscoverRadioService` and `RadioPlanService`) and playlist
//! suggestions.
//!
//! Three things share one set of building blocks:
//! - a radio shelf: albums seeded by an artist (similar artists' albums),
//!   an album (similar albums, then more by its artist) or a genre (the
//!   library's artists in that genre as seeds, sampled the same way all
//!   day);
//! - a radio plan: a finite track list the player walks. Owned tracks by
//!   the seed artists and their similar artists, mixed about half and half
//!   with their popular tracks (ListenBrainz, then Last.fm, then Deezer)
//!   in hybrid mode. A shelf of albums plays its owned tracks and album
//!   samples in shelf order;
//! - playlist suggestions: albums by artists similar to the playlist's,
//!   plus albums in the playlist artists' genres.
//!
//! Similar-artist pools come from ListenBrainz, or from Last.fm while
//! ListenBrainz popularity is refused upstream (v2
//! `lb_popularity_degraded`). Every provider read may fail; a failure
//! shrinks the answer, it never fails the request.

pub mod library;
pub mod plan;

use droppedneedle_providers::RequestPriority;
use std::collections::HashSet;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use futures_util::future::join_all;
use futures_util::stream::{self, StreamExt};

use super::listening::{ArtistRow, Ask, ListenSource, ListeningData, SourcePolicy, UserMusic};
use super::page::build::daily_shuffler;
use super::page::library::{LibraryReads, fold};
use super::page::shelves;
use super::queue::QueueSettings;
use super::queue::build::{
    BuildContext, VARIOUS_ARTISTS_MBID, cover_url, is_various_artists, lastfm_pools, normalize,
    similar_artist_pools, timed,
};
use super::queue::select::{MAX_PER_ARTIST, Shuffler, round_robin};
use super::queue::store::QueueDb;
use crate::reads::catalog::Catalog;
use crate::reads::catalog::error::CatalogError;
use crate::reads::discover::models::{
    ChartSection, PlaylistProfile, PlaylistSuggestionsRequest, PlaylistSuggestionsResponse,
    QueueItemLight, RadioMode, RadioPlanRequest, RadioPlanResponse, RadioPlanTrack, RadioRequest,
    RadioSeedItem, RadioSeedKind, SectionItem, SectionKind,
};
use crate::reads::discover::ports::{BoxFuture, ProviderFailure, RadioFailure};
use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::secret_sections::AdvancedSettings;
use droppedneedle_providers::error::ProviderError;
use droppedneedle_providers::musicbrainz::is_valid_mbid;
use droppedneedle_providers::preview::{PreviewClient, PreviewTrack};
use droppedneedle_providers::{CacheKey, Fetched, Policy, ProviderCacheStore, Source};
use library::RadioLibrary;
use plan::{LIBRARY_POOL, MAX_SEED_ARTISTS, TRACKS_PER_ARTIST, clean_pool, mix, song_key};

/// Title of a plan built from a shelf of items.
const SHELF_TITLE: &str = "Radio: This Shelf";
/// Provider lookups one plan runs at once.
const LOOKUP_CONCURRENCY: usize = 6;
/// How long an artist's preview tracks or an album's samples are kept.
const PREVIEWS: Policy = Policy {
    fresh: Duration::from_secs(3600),
    miss: Duration::from_secs(3600),
};

/// Title of the playlist suggestions shelf.
const SUGGESTIONS_TITLE: &str = "Suggestions for your playlist";

/// What the radio reads from.
pub struct RadioInputs {
    /// Provider reads (shared with the page and the queue).
    pub listening: ListeningData,
    /// The queue tables (the Last.fm release-group map).
    pub queue_db: QueueDb,
    /// The advanced settings the queue settings come from, read per call.
    pub settings: Live<AdvancedSettings>,
    /// The catalog database.
    pub pool: sqlx::SqlitePool,
    /// Album pages: similar albums, more by the artist, top songs.
    pub catalog: Catalog,
    /// Keyless previews (Deezer, then iTunes).
    pub previews: PreviewClient,
    /// The provider cache, which keeps preview titles.
    pub cache: ProviderCacheStore,
    /// Wall time, which picks the day's shuffle.
    pub clock: Arc<dyn Clock>,
}

/// Radio over the live providers and the library.
pub struct LiveRadio {
    listening: ListeningData,
    queue_db: QueueDb,
    settings: Live<AdvancedSettings>,
    genres: LibraryReads,
    tracks: RadioLibrary,
    catalog: Catalog,
    previews: PreviewClient,
    cache: ProviderCacheStore,
    clock: Arc<dyn Clock>,
}

/// A preview's title and artist: what a plan keeps of it. The preview
/// link itself expires, so it is never cached.
type PreviewTitle = (String, String);

fn titles(tracks: Vec<PreviewTrack>) -> Fetched<Vec<PreviewTitle>> {
    if tracks.is_empty() {
        Fetched::Missing
    } else {
        Fetched::Found(
            tracks
                .into_iter()
                .map(|track| (track.title, track.artist_name))
                .collect(),
        )
    }
}

/// The source a request follows: the one asked for, else the user's
/// primary, switched to the other when only that one is set up.
fn resolve_source(asked: Option<ListenSource>, policy: &SourcePolicy) -> ListenSource {
    match asked {
        Some(asked) => policy.asked(asked),
        None => policy.listening(),
    }
}

/// Python's `str.title()`, which v2 used for genre station names.
fn title_case(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut start = true;
    for ch in text.chars() {
        if ch.is_alphabetic() {
            if start {
                out.extend(ch.to_uppercase());
            } else {
                out.extend(ch.to_lowercase());
            }
            start = false;
        } else {
            out.push(ch);
            start = true;
        }
    }
    out
}

fn today(clock: &dyn Clock) -> String {
    time::OffsetDateTime::from_unix_timestamp(clock.now_unix())
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
        .date()
        .to_string()
}

fn seed(mbid: &str, name: &str) -> ArtistRow {
    ArtistRow {
        name: name.to_owned(),
        mbid: Some(mbid.to_owned()),
        listen_count: 0,
    }
}

fn empty_shelf(title: &str, source: ListenSource, message: Option<String>) -> ChartSection {
    ChartSection {
        title: title.to_owned(),
        kind: SectionKind::Albums,
        items: Vec::new(),
        source: Some(source.into()),
        fallback_message: message,
        connect_service: None,
        radio_seed_type: None,
        radio_seed_id: None,
    }
}

fn catalog_failure(error: CatalogError) -> RadioFailure {
    match error {
        CatalogError::NotFound => RadioFailure::NotFound,
        CatalogError::Invalid(message) => RadioFailure::Invalid(message),
        CatalogError::Unavailable(error) => {
            RadioFailure::Provider(ProviderFailure::Provider(error))
        }
        other => RadioFailure::Provider(ProviderFailure::failed(format!("radio catalog: {other}"))),
    }
}

impl LiveRadio {
    /// Radio over these inputs.
    pub fn new(inputs: RadioInputs) -> Self {
        Self {
            listening: inputs.listening,
            queue_db: inputs.queue_db,
            settings: inputs.settings,
            genres: LibraryReads::new(inputs.pool.clone()),
            tracks: RadioLibrary::new(inputs.pool),
            catalog: inputs.catalog,
            previews: inputs.previews,
            cache: inputs.cache,
            clock: inputs.clock,
        }
    }

    /// One pool of albums per seed from its similar artists.
    async fn similar_pools(
        &self,
        user_id: &str,
        user: &UserMusic,
        seeds: &[ArtistRow],
        similar_limit: usize,
        albums_per: usize,
        use_lastfm: bool,
    ) -> Vec<Vec<QueueItemLight>> {
        let settings = QueueSettings {
            similar_artists_limit: similar_limit,
            albums_per_similar: albums_per,
            ..QueueSettings::read(&self.settings)
        };
        // A radio plan is built while the listener waits.
        let ctx = BuildContext {
            listening: &self.listening,
            db: &self.queue_db,
            settings: &settings,
            ask: Ask::build(RequestPriority::UserInitiated),
        };
        let excluded = HashSet::new();
        if use_lastfm && user.lastfm {
            lastfm_pools(&ctx, user_id, seeds, &excluded, similar_limit * albums_per).await
        } else {
            similar_artist_pools(&ctx, user_id, seeds, &excluded).await
        }
    }

    /// Cards as album rows, marked owned or requested.
    async fn album_rows(&self, cards: Vec<QueueItemLight>) -> Vec<SectionItem> {
        let ids: Vec<String> = cards
            .iter()
            .map(|card| card.musicbrainz_id.to_ascii_lowercase())
            .collect();
        let local = self.catalog.local();
        let owned = local.owned_albums(&ids).await.unwrap_or_else(|error| {
            tracing::warn!(%error, "radio ownership read failed; rows show as not owned");
            HashSet::new()
        });
        let requested = local.requested_albums(&ids).await.unwrap_or_else(|error| {
            tracing::warn!(%error, "radio request read failed; rows show as not requested");
            HashSet::new()
        });
        cards
            .into_iter()
            .map(|card| {
                let key = card.musicbrainz_id.to_ascii_lowercase();
                let mut album = shelves::album(
                    Some(&card.musicbrainz_id),
                    &card.title,
                    Some(&card.artist_name),
                    Some(&card.artist_id),
                    None,
                );
                album.in_library = owned.contains(&key);
                album.requested = !album.in_library && requested.contains(&key);
                SectionItem::Album(album)
            })
            .collect()
    }

    /// The artist's name from their most played album, else the id.
    async fn artist_name(&self, user_id: &str, mbid: &str) -> String {
        timed(
            "listenbrainz artist albums",
            self.listening
                .artist_albums(user_id, mbid, 1, &Ask::person()),
        )
        .await
        .into_iter()
        .next()
        .map(|ranked| ranked.album.artist_name)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| mbid.to_owned())
    }

    /// Up to `want` library artists in one genre, the same ones all day.
    async fn genre_seeds(&self, genre: &str, parts: &[&str], want: usize) -> Vec<String> {
        let mut artists = match self.genres.artists_for_genres(&[genre.to_owned()]).await {
            Ok(mut by_genre) => by_genre.remove(&fold(genre)).unwrap_or_default(),
            Err(cause) => {
                tracing::warn!(%cause, "radio genre index read failed");
                Vec::new()
            }
        };
        daily_shuffler(&today(self.clock.as_ref()), parts).shuffle(&mut artists);
        artists.truncate(want);
        artists
    }

    async fn shelf_for(
        &self,
        user_id: &str,
        request: &RadioRequest,
    ) -> Result<ChartSection, RadioFailure> {
        let policy = self.listening.policy(user_id).await;
        let user = policy.user().clone();
        let source = resolve_source(request.source, &policy);
        if !policy.ready(source) {
            return Ok(empty_shelf(
                "Radio",
                source,
                Some(format!(
                    "{} is not connected for your account.",
                    match source {
                        ListenSource::Listenbrainz => "ListenBrainz",
                        ListenSource::Lastfm => "Last.fm",
                    }
                )),
            ));
        }
        let count = usize::try_from(request.count).unwrap_or(10);
        let lastfm = policy.lastfm_stands_in();
        let (title, seed_id, items) = match request.seed_type {
            RadioSeedKind::Artist => {
                let mbid = normalize(Some(&request.seed_id))
                    .filter(|id| is_valid_mbid(id))
                    .ok_or(RadioFailure::NotFound)?;
                let name = self.artist_name(user_id, &mbid).await;
                let pools = self
                    .similar_pools(user_id, &user, &[seed(&mbid, &name)], 15, 3, lastfm)
                    .await;
                let picked = round_robin(pools, count, MAX_PER_ARTIST, &mut Shuffler::random());
                (
                    format!("Radio: {name}"),
                    mbid,
                    self.album_rows(picked).await,
                )
            }
            RadioSeedKind::Album => {
                let mbid = normalize(Some(&request.seed_id))
                    .filter(|id| is_valid_mbid(id))
                    .ok_or(RadioFailure::NotFound)?;
                let (title, items) = self.album_station(user_id, &mbid, count).await?;
                (title, mbid, items)
            }
            RadioSeedKind::Genre | RadioSeedKind::Items => {
                let genre = request.seed_id.trim().to_owned();
                let artists = self.genre_seeds(&genre, &[&genre], 5).await;
                if artists.is_empty() {
                    return Err(RadioFailure::Invalid(format!(
                        "No artists in your library are tagged '{genre}', so there is no \
                         station for it yet."
                    )));
                }
                let seeds: Vec<ArtistRow> = artists.iter().map(|id| seed(id, id)).collect();
                let pools = self
                    .similar_pools(user_id, &user, &seeds, 10, 3, lastfm)
                    .await;
                let picked = round_robin(pools, count, MAX_PER_ARTIST, &mut Shuffler::random());
                (
                    format!("Radio: {}", title_case(&genre)),
                    request.seed_id.clone(),
                    self.album_rows(picked).await,
                )
            }
        };
        Ok(ChartSection {
            title,
            kind: SectionKind::Albums,
            items,
            source: Some(source.into()),
            fallback_message: None,
            connect_service: None,
            radio_seed_type: Some(request.seed_type),
            radio_seed_id: Some(seed_id),
        })
    }

    /// Similar albums, then more by the album's artist, without repeats.
    async fn album_station(
        &self,
        user_id: &str,
        mbid: &str,
        count: usize,
    ) -> Result<(String, Vec<SectionItem>), RadioFailure> {
        let basic = self
            .catalog
            .album_basic(mbid)
            .await
            .map_err(catalog_failure)?;
        let title = format!("Radio: {}", basic.title);
        if basic.artist_id.is_empty() {
            return Ok((title, Vec::new()));
        }
        let limit = u32::try_from(count).unwrap_or(10);
        let (similar, more) = tokio::join!(
            self.catalog
                .similar_albums(user_id, mbid, &basic.artist_id, limit),
            self.catalog.more_by_artist(mbid, &basic.artist_id, limit),
        );
        let similar = similar.map(|found| found.albums).unwrap_or_else(|error| {
            tracing::warn!(%error, "album radio: similar albums unavailable");
            Vec::new()
        });
        let more = more.map(|found| found.albums).unwrap_or_else(|error| {
            tracing::warn!(%error, "album radio: more by the artist unavailable");
            Vec::new()
        });
        let mut seen = HashSet::from([mbid.to_ascii_lowercase()]);
        let mut items = Vec::new();
        for album in similar.into_iter().chain(more) {
            if items.len() >= count {
                break;
            }
            let Some(id) = album.musicbrainz_id.as_deref() else {
                continue;
            };
            if !seen.insert(id.to_ascii_lowercase()) {
                continue;
            }
            let mut row = shelves::album(
                Some(id),
                &album.title,
                album.artist_name.as_deref(),
                album.artist_id.as_deref(),
                None,
            );
            row.in_library = album.in_library;
            row.requested = album.requested;
            items.push(SectionItem::Album(row));
        }
        Ok((title, items))
    }

    /// The seed artists a plan spreads over, and its title.
    async fn plan_seeds(
        &self,
        user_id: &str,
        request: &RadioPlanRequest,
    ) -> Result<(Vec<(String, String)>, String), RadioFailure> {
        if request.seed_type == RadioSeedKind::Items {
            let mut seen = HashSet::new();
            let seeds = request
                .items
                .iter()
                .filter_map(|item| {
                    let mbid = normalize(Some(&item.artist_mbid)).filter(|id| is_valid_mbid(id))?;
                    (mbid != VARIOUS_ARTISTS_MBID && seen.insert(mbid.clone()))
                        .then(|| (mbid, item.artist_name.clone()))
                })
                .take(MAX_SEED_ARTISTS)
                .collect();
            return Ok((seeds, SHELF_TITLE.to_owned()));
        }
        let seed_id = request.seed_id.as_deref().unwrap_or("").trim();
        if request.seed_type == RadioSeedKind::Genre {
            return Ok(self.genre_plan_seeds(user_id, seed_id).await);
        }
        let mbid = normalize(Some(seed_id))
            .filter(|id| is_valid_mbid(id))
            .ok_or_else(|| RadioFailure::Invalid(format!("Unknown seed MBID: {seed_id}")))?;
        let (base, title) = if request.seed_type == RadioSeedKind::Album {
            match self.catalog.album_basic(&mbid).await {
                Ok(basic) => {
                    let title = format!("Radio: {}", basic.title);
                    match normalize(Some(&basic.artist_id)) {
                        Some(artist) => ((artist, basic.artist_name), title),
                        None => return Ok((Vec::new(), title)),
                    }
                }
                Err(CatalogError::NotFound) => return Ok((Vec::new(), "Radio".to_owned())),
                Err(error) => return Err(catalog_failure(error)),
            }
        } else {
            let name = self.artist_name(user_id, &mbid).await;
            ((mbid.clone(), name.clone()), format!("Radio: {name}"))
        };
        let similar = timed(
            "listenbrainz similar artists",
            self.listening
                .similar_artists(user_id, &base.0, MAX_SEED_ARTISTS + 1, &Ask::person()),
        )
        .await;
        let mut seeds = vec![base];
        for artist in similar.into_iter().map(|scored| scored.artist) {
            if seeds.len() >= MAX_SEED_ARTISTS {
                break;
            }
            if let Some(mbid) = normalize(artist.mbid.as_deref())
                && !is_various_artists(Some(&mbid))
            {
                seeds.push((mbid, artist.name));
            }
        }
        Ok((seeds, title))
    }

    /// Genre seeds: library artists in the genre (the same ones all day
    /// for this user), topped up with Last.fm's top artists for the tag.
    async fn genre_plan_seeds(
        &self,
        user_id: &str,
        genre: &str,
    ) -> (Vec<(String, String)>, String) {
        let title = format!("Radio: {}", title_case(genre));
        let want = (MAX_SEED_ARTISTS / 2).max(2);
        let mut seen = HashSet::new();
        let mut seeds: Vec<(String, String)> = self
            .genre_seeds(genre, &[user_id, genre], want)
            .await
            .into_iter()
            .filter(|mbid| seen.insert(mbid.clone()))
            .map(|mbid| (mbid, String::new()))
            .collect();
        if seeds.len() < MAX_SEED_ARTISTS {
            let tagged = timed(
                "lastfm tag artists",
                self.listening
                    .lastfm_tag_artists(user_id, genre, 10, &Ask::person()),
            )
            .await;
            for artist in tagged {
                if seeds.len() >= MAX_SEED_ARTISTS {
                    break;
                }
                if let Some(mbid) = normalize(artist.mbid.as_deref())
                    && !is_various_artists(Some(&mbid))
                    && seen.insert(mbid.clone())
                {
                    seeds.push((mbid, artist.name));
                }
            }
        }
        (seeds, title)
    }

    /// Preview titles under `key`, through the provider cache. A failed
    /// fetch is an empty answer and keeps nothing.
    async fn preview_titles<F, Fut>(&self, key: CacheKey, fetch: F) -> Vec<PreviewTitle>
    where
        F: FnOnce(PreviewClient, RequestPriority) -> Fut + Clone + Send + 'static,
        Fut: Future<Output = Result<Fetched<Vec<PreviewTitle>>, ProviderError>> + Send + 'static,
    {
        let previews = self.previews.clone();
        let found = self
            .cache
            .get_or_fetch(
                &key,
                PREVIEWS,
                RequestPriority::UserInitiated,
                move |priority| fetch(previews, priority),
            )
            .await;
        match found {
            Ok(found) => found.unwrap_or_default(),
            Err(error) => {
                tracing::debug!(%error, "radio previews unavailable");
                Vec::new()
            }
        }
    }

    /// One artist's popular tracks: ListenBrainz, then Last.fm (both
    /// through the catalog), then Deezer by name.
    async fn top_tracks(&self, user_id: &str, mbid: &str, name: &str) -> Vec<RadioPlanTrack> {
        timed("radio top tracks", async {
            Ok::<_, std::convert::Infallible>(self.fetch_top_tracks(user_id, mbid, name).await)
        })
        .await
    }

    async fn fetch_top_tracks(&self, user_id: &str, mbid: &str, name: &str) -> Vec<RadioPlanTrack> {
        let track = |title: String, artist: String, recording: Option<String>| RadioPlanTrack {
            track_name: title,
            artist_name: if artist.is_empty() {
                name.to_owned()
            } else {
                artist
            },
            artist_mbid: mbid.to_owned(),
            recording_mbid: recording,
            album_mbid: None,
            album_name: None,
            in_library: false,
            local_file_id: None,
            file_format: None,
            duration_s: None,
        };
        match self
            .catalog
            .top_songs(
                user_id,
                mbid,
                TRACKS_PER_ARTIST as u32,
                Some(ListenSource::Listenbrainz),
                RequestPriority::UserInitiated,
            )
            .await
        {
            Ok(found) if !found.songs.is_empty() => {
                return found
                    .songs
                    .into_iter()
                    .map(|song| track(song.title, song.artist_name, song.recording_mbid))
                    .collect();
            }
            Ok(_) => {}
            Err(error) => tracing::debug!(%error, "radio top songs unavailable"),
        }
        if name.is_empty() {
            return Vec::new();
        }
        let key = CacheKey::hashed(
            Source::Previews,
            "artist_top_tracks",
            &[name, &TRACKS_PER_ARTIST.to_string()],
        );
        let name = name.to_owned();
        self.preview_titles(key, move |previews, priority| async move {
            previews
                .artist_top_tracks_leg(&name, TRACKS_PER_ARTIST, priority)
                .await
                .map(titles)
        })
        .await
        .into_iter()
        .map(|(title, artist)| track(title, artist, None))
        .collect()
    }

    async fn build_plan(
        &self,
        user_id: &str,
        request: &RadioPlanRequest,
    ) -> Result<RadioPlanResponse, RadioFailure> {
        let count = usize::try_from(request.count).unwrap_or(30);
        let exclude: HashSet<String> = request
            .exclude_recording_mbids
            .iter()
            .map(|mbid| mbid.to_lowercase())
            .collect();
        let hybrid = request.mode == RadioMode::Hybrid;
        if request.seed_type == RadioSeedKind::Items
            && request
                .items
                .iter()
                .any(|item| item.album_mbid.is_some() && !item.album_name.is_empty())
        {
            return self.shelf_plan(request, count, &exclude, hybrid).await;
        }
        let (seeds, title) = self.plan_seeds(user_id, request).await?;
        if seeds.is_empty() {
            return Ok(RadioPlanResponse {
                title,
                tracks: Vec::new(),
            });
        }
        let artist_ids: Vec<String> = seeds.iter().map(|(mbid, _)| mbid.clone()).collect();
        let library = self
            .tracks
            .tracks_by_artists(&artist_ids, LIBRARY_POOL)
            .await
            .unwrap_or_else(|cause| {
                tracing::warn!(%cause, "radio library pool failed; playing without it");
                Vec::new()
            });
        let external = if hybrid {
            let lookups: Vec<_> = seeds
                .iter()
                .map(|(mbid, name)| self.top_tracks(user_id, mbid, name))
                .collect();
            stream::iter(lookups)
                .buffered(LOOKUP_CONCURRENCY)
                .collect::<Vec<_>>()
                .await
                .into_iter()
                .flatten()
                .collect()
        } else {
            Vec::new()
        };
        Ok(RadioPlanResponse {
            title,
            tracks: mix(
                clean_pool(library, &exclude),
                clean_pool(external, &exclude),
                count,
                hybrid,
            ),
        })
    }

    /// A shelf of albums as a station: each album's owned tracks, or in
    /// hybrid mode its 30-second samples when the library lacks it, album
    /// by album in shelf order.
    async fn shelf_plan(
        &self,
        request: &RadioPlanRequest,
        count: usize,
        exclude: &HashSet<String>,
        hybrid: bool,
    ) -> Result<RadioPlanResponse, RadioFailure> {
        let items: Vec<_> = request
            .items
            .iter()
            .filter_map(|item| {
                let album = normalize(item.album_mbid.as_deref()).filter(|id| is_valid_mbid(id))?;
                Some((album, item))
            })
            .collect();
        let album_ids: Vec<String> = items.iter().map(|(album, _)| album.clone()).collect();
        let local = self
            .tracks
            .tracks_of_albums(&album_ids, count as i64)
            .await
            .unwrap_or_else(|cause| {
                tracing::warn!(%cause, "shelf radio library read failed");
                Vec::new()
            });
        let mut by_album: std::collections::HashMap<String, Vec<RadioPlanTrack>> =
            std::collections::HashMap::new();
        for track in clean_pool(local, exclude) {
            let album = track.album_mbid.clone().unwrap_or_default();
            by_album.entry(album).or_default().push(track);
        }
        let per_album = (count / items.len().max(1)).clamp(1, 5);
        let missing: Vec<_> = items
            .iter()
            .filter(|(album, item)| {
                hybrid
                    && !by_album.contains_key(album)
                    && !item.artist_name.is_empty()
                    && !item.album_name.is_empty()
            })
            .collect();
        let lookups: Vec<_> = missing
            .iter()
            .map(|(album, item)| self.album_samples(album, item, per_album))
            .collect();
        let samples: Vec<Vec<RadioPlanTrack>> = stream::iter(lookups)
            .buffered(LOOKUP_CONCURRENCY)
            .collect()
            .await;
        for ((album, _), found) in missing.into_iter().zip(samples) {
            by_album.insert(album.clone(), found);
        }
        let mut seen = HashSet::new();
        let mut tracks = Vec::new();
        'albums: for album in &album_ids {
            for track in by_album.remove(album).unwrap_or_default() {
                if tracks.len() >= count {
                    break 'albums;
                }
                if seen.insert(song_key(&track.artist_name, &track.track_name)) {
                    tracks.push(track);
                }
            }
        }
        Ok(RadioPlanResponse {
            title: SHELF_TITLE.to_owned(),
            tracks,
        })
    }

    /// The first few songs of an album the library lacks, from the
    /// keyless preview services, cached and under the read timeout.
    async fn album_samples(
        &self,
        album: &str,
        item: &RadioSeedItem,
        per_album: usize,
    ) -> Vec<RadioPlanTrack> {
        let key = CacheKey::hashed(
            Source::Previews,
            "album_samples",
            &[&item.artist_name, &item.album_name, &per_album.to_string()],
        );
        let (artist, title) = (item.artist_name.clone(), item.album_name.clone());
        let found = timed("radio album samples", async {
            Ok::<_, std::convert::Infallible>(
                self.preview_titles(key, move |previews, priority| async move {
                    let deezer = previews
                        .deezer_album_tracks(&artist, &title, per_album, priority)
                        .await;
                    if let Ok(tracks) = &deezer
                        && !tracks.is_empty()
                    {
                        return deezer.map(titles);
                    }
                    let itunes = previews
                        .itunes_album_tracks(&artist, &title, per_album, priority)
                        .await;
                    match (deezer, itunes) {
                        (_, Ok(tracks)) if !tracks.is_empty() => Ok(titles(tracks)),
                        (Err(error), _) | (_, Err(error)) => Err(error),
                        _ => Ok(Fetched::Missing),
                    }
                })
                .await,
            )
        })
        .await;
        let artist_mbid = normalize(Some(&item.artist_mbid)).unwrap_or_default();
        found
            .into_iter()
            .map(|(track_name, artist_name)| RadioPlanTrack {
                track_name,
                artist_name: if artist_name.is_empty() {
                    item.artist_name.clone()
                } else {
                    artist_name
                },
                artist_mbid: artist_mbid.clone(),
                recording_mbid: None,
                album_mbid: Some(album.to_owned()),
                album_name: Some(item.album_name.clone()),
                in_library: false,
                local_file_id: None,
                file_format: None,
                duration_s: None,
            })
            .collect()
    }

    async fn suggestions(
        &self,
        user_id: &str,
        request: &PlaylistSuggestionsRequest,
    ) -> Result<PlaylistSuggestionsResponse, RadioFailure> {
        let (artist_mbids, track_count) = self
            .tracks
            .playlist_artists(user_id, request.playlist_id.trim())
            .await
            .map_err(|cause| RadioFailure::Provider(ProviderFailure::failed(cause)))?
            .ok_or(RadioFailure::NotFound)?;
        if artist_mbids.is_empty() {
            return Err(RadioFailure::Invalid(
                "This playlist has no artist data to base suggestions on".to_owned(),
            ));
        }
        let genre_distribution = self
            .genres
            .genres_for_artists(&artist_mbids)
            .await
            .unwrap_or_else(|cause| {
                tracing::warn!(%cause, "playlist genre read failed; suggesting without genres");
                Default::default()
            });
        let profile = PlaylistProfile {
            artist_mbids,
            genre_distribution,
            track_count,
        };
        let policy = self.listening.policy(user_id).await;
        let user = policy.user().clone();
        let source = resolve_source(request.source, &policy);
        let respond = |suggestions: ChartSection| PlaylistSuggestionsResponse {
            suggestions,
            playlist_id: request.playlist_id.clone(),
            profile: profile.clone(),
        };
        if !policy.ready(source) {
            return Ok(respond(empty_shelf(
                SUGGESTIONS_TITLE,
                source,
                Some("The music source you selected isn't set up yet.".to_owned()),
            )));
        }
        let mut sample = profile.artist_mbids.clone();
        Shuffler::random().shuffle(&mut sample);
        sample.truncate(3);
        let seeds: Vec<ArtistRow> = sample.iter().map(|id| seed(id, id)).collect();
        let mut pools = self
            .similar_pools(
                user_id,
                &user,
                &seeds,
                15,
                3,
                source == ListenSource::Lastfm,
            )
            .await;
        let genres = self.genre_albums(&profile).await;
        if !genres.is_empty() {
            pools.push(genres);
        }
        // v2 meant to leave out the playlist's own artists but compared
        // their ids with album ids, so nothing was ever left out.
        let own: HashSet<&str> = profile.artist_mbids.iter().map(String::as_str).collect();
        for pool in &mut pools {
            pool.retain(|card| !own.contains(card.artist_id.to_ascii_lowercase().as_str()));
        }
        let count = usize::try_from(request.count).unwrap_or(10);
        let picked = round_robin(pools, count, MAX_PER_ARTIST, &mut Shuffler::random());
        if picked.is_empty() {
            return Ok(respond(empty_shelf(
                SUGGESTIONS_TITLE,
                source,
                Some(
                    "Not enough suggestions for this playlist yet. Try adding more tracks."
                        .to_owned(),
                ),
            )));
        }
        let mut section = empty_shelf(SUGGESTIONS_TITLE, source, None);
        section.items = self.album_rows(picked).await;
        Ok(respond(section))
    }

    /// Albums tagged with up to four of the playlist artists' genres (v2
    /// `discover_by_genres`).
    async fn genre_albums(&self, profile: &PlaylistProfile) -> Vec<QueueItemLight> {
        let mut genres: Vec<String> = Vec::new();
        let mut seen = HashSet::new();
        'artists: for artist in &profile.artist_mbids {
            for genre in profile.genre_distribution.get(artist).into_iter().flatten() {
                if seen.insert(genre.to_lowercase()) {
                    genres.push(genre.clone());
                }
                if genres.len() >= 4 {
                    break 'artists;
                }
            }
        }
        let ask = Ask::build(RequestPriority::UserInitiated);
        let ask = &ask;
        let found = join_all(genres.iter().map(|genre| {
            timed(
                "musicbrainz tag albums",
                self.listening.tag_albums(genre, 8, 0, ask),
            )
        }))
        .await;
        let mut taken = HashSet::new();
        let mut cards = Vec::new();
        for (genre, albums) in genres.iter().zip(found) {
            for album in albums {
                let Some(group) = normalize(Some(&album.release_group_mbid)) else {
                    continue;
                };
                if !taken.insert(group.clone()) {
                    continue;
                }
                cards.push(QueueItemLight {
                    cover_url: Some(cover_url(&group)),
                    musicbrainz_id: group,
                    title: album.title,
                    artist_name: album.artist_name,
                    artist_id: album.artist_mbid.unwrap_or_default(),
                    recommendation_reason: format!("Because the playlist has {genre}"),
                    is_wildcard: false,
                    in_library: false,
                });
            }
        }
        cards
    }
}

impl LiveRadio {
    /// A complete, finite track plan for the user.
    pub fn plan<'a>(
        &'a self,
        user_id: &'a str,
        request: &'a RadioPlanRequest,
    ) -> BoxFuture<'a, Result<RadioPlanResponse, RadioFailure>> {
        Box::pin(self.build_plan(user_id, request))
    }

    /// One album shelf seeded by an artist, album or genre.
    pub fn shelf<'a>(
        &'a self,
        user_id: &'a str,
        request: &'a RadioRequest,
    ) -> BoxFuture<'a, Result<ChartSection, RadioFailure>> {
        Box::pin(self.shelf_for(user_id, request))
    }

    /// Albums that extend one playlist.
    pub fn playlist_suggestions<'a>(
        &'a self,
        user_id: &'a str,
        request: &'a PlaylistSuggestionsRequest,
    ) -> BoxFuture<'a, Result<PlaylistSuggestionsResponse, RadioFailure>> {
        Box::pin(self.suggestions(user_id, request))
    }
}
