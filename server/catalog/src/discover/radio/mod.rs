//! Radio and playlist
//! suggestions.
//!
//! Three things share one set of building blocks:
//! - a radio shelf: albums seeded by an artist (similar artists' albums),
//!   an album (similar albums, then more by its artist) or a genre (the
//!   library's artists in that genre as seeds, sampled the same way all
//!   day);
//! - a radio plan: the next tracks of a station, which the player asks for
//!   again as it runs low, sending what it played and the person's steering
//!   (the server keeps no station). While sound analysis is on, a station
//!   from a track, an album, an artist or a playlist plays the library
//!   tracks that sound like its seed tracks ([`sound`], [`walk`]); else, and
//!   for a genre or a shelf, owned tracks by the seed artists and their
//!   similar artists. In hybrid mode, with a source an admin set up for
//!   tracks the library lacks, they are mixed about half and half with
//!   popular tracks (ListenBrainz, then Last.fm, then Deezer) of the seed
//!   artists and their neighbours. A shelf of albums plays its owned tracks
//!   and album samples in shelf order. Every track carries its reason
//!   ([`reasons`]);
//! - playlist suggestions: albums by artists similar to the playlist's,
//!   plus albums in the playlist artists' genres.
//!
//! Similar-artist pools come from ListenBrainz, or from Last.fm while
//! ListenBrainz popularity is refused upstream. Every provider read may fail;
//! a failure shrinks the answer, it never fails the request. What the
//! person told Discover to hide ([`Feedback`]) is left out of shelves and
//! suggestions, no artist they hid is added to a plan's seeds, and a plan
//! plays nothing from an album they said no to.

pub mod library;
pub mod plan;
mod reasons;
pub mod sound;
pub mod walk;

use droppedneedle_providers::RequestPriority;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use futures_util::future::join_all;
use futures_util::stream::{self, StreamExt};

use super::engine::pool_store;
use super::feedback::Feedback;
use super::listening::{ArtistRow, Ask, ListenSource, ListeningData, SourcePolicy, UserMusic};
use super::page::build::daily_shuffler;
use super::page::library::{LibraryReads, fold};
use super::page::shelves;
use super::queue::build::{
    AlbumCard, BuildContext, VARIOUS_ARTISTS_MBID, is_various_artists, lastfm_pools, normalize,
    similar_artist_pools, similar_to, timed,
};
use super::queue::select::{MAX_PER_ARTIST, Shuffler, round_robin};
use super::queue::store::QueueDb;
use super::queue::{QueueSettings, SETTINGS_UNREADABLE};
use crate::discover::models::{
    ChartSection, PlaylistProfile, PlaylistSuggestionsRequest, PlaylistSuggestionsResponse,
    RadioMode, RadioPlanRequest, RadioPlanResponse, RadioPlanTrack, RadioReason, RadioRequest,
    RadioSeedItem, RadioSeedKind, SectionItem, SectionKind,
};
use crate::discover::ports::{BoxFuture, ProviderFailure, RadioFailure};
use crate::metadata::Catalog;
use crate::metadata::error::CatalogError;
use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::secret_sections::{AdvancedSettings, YouTubeConnection};
use droppedneedle_library::sound::{SoundError, SoundSearch};
use droppedneedle_providers::error::ProviderError;
use droppedneedle_providers::musicbrainz::is_valid_mbid;
use droppedneedle_providers::preview::{PreviewClient, PreviewTrack};
use droppedneedle_providers::{CacheKey, Fetched, Policy, ProviderCacheStore, Source};
use library::RadioLibrary;
use plan::{LIBRARY_POOL, MAX_SEED_ARTISTS, TRACKS_PER_ARTIST, clean_pool, mix, song_key};
use walk::STRETCH;

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
/// Tracks of an artist or a playlist a station by sound grows from.
const SEED_TRACKS: usize = 20;
/// Most tracks of an album a station by sound grows from: all of them.
const ALBUM_SEED_TRACKS: i64 = 200;
/// A skip this early in a track counts toward resting it.
const EARLY_SKIP_MS: i64 = 30_000;
/// How far back early skips are counted.
const REST_WINDOW_MS: i64 = 30 * 24 * 60 * 60 * 1000;
/// Early skips on a station in that window that rest a track from
/// stations.
const REST_AFTER_SKIPS: i64 = 2;
/// What a station asks while the sound analysis settings cannot be read:
/// it waits rather than play as if analysis were off.
const SOUND_SETTINGS_UNREADABLE: &str =
    "The sound analysis settings cannot be read. An admin needs to save them again.";
/// What a hybrid station answers while the YouTube settings cannot be
/// read: it waits rather than play as if no source were set up.
const YOUTUBE_SETTINGS_UNREADABLE: &str =
    "The YouTube settings cannot be read. An admin needs to save them again.";

/// The library tracks the station has queued, in the order they play.
fn station_ids(request: &RadioPlanRequest) -> Vec<String> {
    request
        .station_tracks
        .iter()
        .filter_map(|track| track.local_track_id.clone())
        .collect()
}

/// How a station's library pool says where a track came from.
enum Pooled {
    /// From the station's artist and those similar to it.
    Similar,
    /// From the artists on this playlist.
    Playlist(String),
    /// From a genre's artists.
    Genre(String),
    /// From a shelf's artists.
    Shelf,
}

/// What a plan grows from.
struct Station {
    title: String,
    /// The name a seed reason gives it: a track, an album or an artist.
    seed_name: String,
    /// The artist a library pool starts from, with its similar artists.
    base: Option<(String, String)>,
    /// The artists a library pool spreads over when it has no base.
    artists: Vec<(String, String)>,
    pooled: Pooled,
    /// The library tracks a station by sound grows from.
    seed_tracks: Vec<RadioPlanTrack>,
}

impl Station {
    fn of_artists(title: &str, pooled: Pooled, artists: Vec<(String, String)>) -> Self {
        Self {
            title: title.to_owned(),
            seed_name: String::new(),
            base: None,
            artists,
            pooled,
            seed_tracks: Vec::new(),
        }
    }
}

/// An artist a station reads tracks of, and the reason they carry.
struct PoolArtist {
    mbid: String,
    name: String,
    reason: RadioReason,
}

/// What the radio reads from.
pub struct RadioInputs {
    /// Provider reads (shared with the page and the queue).
    pub listening: ListeningData,
    /// The queue tables (the Last.fm release-group map).
    pub queue_db: QueueDb,
    /// The advanced settings the queue settings come from, read per call.
    pub settings: Live<AdvancedSettings>,
    /// The catalog database.
    pub db: droppedneedle_kernel::db::Db,
    /// Album pages: similar albums, more by the artist, top songs.
    pub catalog: Catalog,
    /// Keyless previews (Deezer, then iTunes).
    pub previews: PreviewClient,
    /// The provider cache, which keeps preview titles.
    pub cache: ProviderCacheStore,
    /// Wall time, which picks the day's shuffle.
    pub clock: Arc<dyn Clock>,
    /// The search over how the library's tracks sound.
    pub sound: SoundSearch,
    /// The YouTube settings, which say whether tracks the library lacks
    /// can play.
    pub youtube: Live<YouTubeConnection>,
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
    sound: SoundSearch,
    youtube: Live<YouTubeConnection>,
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

/// Title case by Python's `str.title()` rules. Quirk (v2): genre station
/// names are cased this way.
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
        listen_count: None,
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

/// A failed read of this server's own store: an honest failure, never an
/// empty answer.
fn store(cause: impl std::fmt::Display) -> RadioFailure {
    RadioFailure::Store(cause.to_string())
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
            genres: LibraryReads::new(inputs.db.clone()),
            tracks: RadioLibrary::new(inputs.db),
            catalog: inputs.catalog,
            previews: inputs.previews,
            cache: inputs.cache,
            clock: inputs.clock,
            sound: inputs.sound,
            youtube: inputs.youtube,
        }
    }

    /// The queue settings now, or an honest refusal while they cannot be
    /// read.
    fn queue_settings(&self) -> Result<QueueSettings, RadioFailure> {
        QueueSettings::read(&self.settings).ok_or_else(|| {
            RadioFailure::Provider(ProviderFailure::not_configured(SETTINGS_UNREADABLE))
        })
    }

    /// What the person told Discover, as it stands now.
    async fn feedback(&self, user_id: &str) -> Feedback {
        Feedback::load_or_none(self.queue_db.db(), user_id, self.clock.now_ms(), "radio").await
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
    ) -> Result<Vec<Vec<AlbumCard>>, RadioFailure> {
        let settings = self.queue_settings()?;
        // A radio plan is built while the listener waits.
        let ctx = BuildContext {
            listening: &self.listening,
            db: &self.queue_db,
            settings: &settings,
            ask: Ask::build(RequestPriority::UserInitiated),
            now_ms: self.clock.now_ms(),
        };
        let excluded = HashSet::new();
        Ok(if use_lastfm && user.lastfm {
            lastfm_pools(
                &ctx,
                user_id,
                seeds,
                &excluded,
                similar_limit,
                albums_per,
                similar_limit * albums_per,
            )
            .await
        } else {
            similar_artist_pools(&ctx, user_id, seeds, &excluded, similar_limit, albums_per).await
        })
    }

    /// Cards as album rows, marked owned or requested.
    async fn album_rows(&self, cards: Vec<AlbumCard>) -> Vec<SectionItem> {
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

    /// The artist's name from the library, else from their most played
    /// album; `None` when neither knows it.
    async fn artist_name(&self, user_id: &str, mbid: &str) -> Option<String> {
        match self.genres.artist_names(&[mbid.to_owned()]).await {
            Ok(names) => {
                if let Some(name) = names.into_values().find(|name| !name.is_empty()) {
                    return Some(name);
                }
            }
            Err(cause) => tracing::warn!(%cause, "radio artist name read failed"),
        }
        timed(
            "artist albums",
            self.listening
                .artist_albums(user_id, mbid, 1, &Ask::person()),
        )
        .await
        .into_iter()
        .next()
        .map(|ranked| ranked.album.artist_name)
        .filter(|name| !name.is_empty())
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
        let lastfm = source == ListenSource::Lastfm || policy.lastfm_stands_in();
        let (title, seed_id, mut items) = match request.seed_type {
            RadioSeedKind::Artist => {
                let mbid = normalize(Some(&request.seed_id))
                    .filter(|id| is_valid_mbid(id))
                    .ok_or(RadioFailure::NotFound)?;
                let name = self.artist_name(user_id, &mbid).await;
                let pools = self
                    .similar_pools(
                        user_id,
                        &user,
                        &[seed(&mbid, name.as_deref().unwrap_or(""))],
                        15,
                        3,
                        lastfm,
                    )
                    .await?;
                let picked = round_robin(pools, count, MAX_PER_ARTIST, &mut Shuffler::random());
                (
                    name.map_or_else(|| "Radio".to_owned(), |name| format!("Radio: {name}")),
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
            RadioSeedKind::Track | RadioSeedKind::Playlist => {
                return Err(RadioFailure::Invalid(
                    "A radio shelf starts from an artist, an album or a genre".to_owned(),
                ));
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
                    .await?;
                let picked = round_robin(pools, count, MAX_PER_ARTIST, &mut Shuffler::random());
                (
                    format!("Radio: {}", title_case(&genre)),
                    request.seed_id.clone(),
                    self.album_rows(picked).await,
                )
            }
        };
        self.feedback(user_id).await.keep_rows(&mut items);
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

    /// What a plan grows from: its title, its library seed tracks, the
    /// name its reasons give the seed and the artists a library pool
    /// starts from.
    async fn station(
        &self,
        user_id: &str,
        request: &RadioPlanRequest,
    ) -> Result<Station, RadioFailure> {
        let seed_id = request.seed_id.as_deref().unwrap_or("").trim();
        match request.seed_type {
            RadioSeedKind::Items => {
                let mut seen = HashSet::new();
                let artists = request
                    .items
                    .iter()
                    .filter_map(|item| {
                        let mbid =
                            normalize(Some(&item.artist_mbid)).filter(|id| is_valid_mbid(id))?;
                        (mbid != VARIOUS_ARTISTS_MBID && seen.insert(mbid.clone()))
                            .then(|| (mbid, item.artist_name.clone()))
                    })
                    .take(MAX_SEED_ARTISTS)
                    .collect();
                Ok(Station::of_artists(SHELF_TITLE, Pooled::Shelf, artists))
            }
            RadioSeedKind::Genre => {
                let (artists, title) = self.genre_plan_seeds(user_id, seed_id).await;
                Ok(Station::of_artists(
                    &title,
                    Pooled::Genre(seed_id.to_owned()),
                    artists,
                ))
            }
            RadioSeedKind::Playlist => {
                let (found, artists) = match self
                    .tracks
                    .playlist_seeds(user_id, seed_id)
                    .await
                    .map_err(store)?
                {
                    Some(found) => {
                        let (artists, _) = self
                            .tracks
                            .playlist_artists(user_id, seed_id)
                            .await
                            .map_err(store)?
                            .unwrap_or_default();
                        (found, artists)
                    }
                    // One of the person's Daily Mixes plays as a
                    // playlist of its stored library tracks.
                    None => self
                        .tracks
                        .mix_seeds(user_id, seed_id)
                        .await
                        .map_err(store)?
                        .ok_or(RadioFailure::NotFound)?,
                };
                let mut ids = found.track_ids;
                Shuffler::random().shuffle(&mut ids);
                ids.truncate(SEED_TRACKS);
                let artists = artists
                    .into_iter()
                    .take(MAX_SEED_ARTISTS)
                    .map(|mbid| (mbid, String::new()))
                    .collect();
                let mut station = Station::of_artists(
                    &format!("Radio: {}", found.name),
                    Pooled::Playlist(found.name),
                    artists,
                );
                station.seed_tracks = self.library_tracks(&ids).await?;
                Ok(station)
            }
            RadioSeedKind::Track => {
                let track = self
                    .library_tracks(&[seed_id.to_owned()])
                    .await?
                    .into_iter()
                    .next()
                    .ok_or(RadioFailure::NotFound)?;
                let base = normalize(Some(&track.artist_mbid))
                    .filter(|id| is_valid_mbid(id) && !is_various_artists(Some(id)))
                    .map(|mbid| (mbid, track.artist_name.clone()));
                Ok(Station {
                    title: format!("Radio: {}", track.track_name),
                    seed_name: track.track_name.clone(),
                    base,
                    artists: Vec::new(),
                    pooled: Pooled::Similar,
                    seed_tracks: vec![track],
                })
            }
            RadioSeedKind::Album | RadioSeedKind::Artist => {
                let mbid = normalize(Some(seed_id))
                    .filter(|id| is_valid_mbid(id))
                    .ok_or_else(|| {
                        RadioFailure::Invalid(format!("Unknown seed MBID: {seed_id}"))
                    })?;
                if request.seed_type == RadioSeedKind::Artist {
                    let name = self.artist_name(user_id, &mbid).await;
                    let seed_tracks = self
                        .tracks
                        .tracks_by_artists(std::slice::from_ref(&mbid), SEED_TRACKS as i64)
                        .await
                        .map_err(store)?;
                    return Ok(Station {
                        title: name
                            .as_deref()
                            .map_or_else(|| "Radio".to_owned(), |name| format!("Radio: {name}")),
                        seed_name: name.clone().unwrap_or_default(),
                        base: Some((mbid, name.unwrap_or_default())),
                        artists: Vec::new(),
                        pooled: Pooled::Similar,
                        seed_tracks,
                    });
                }
                let basic = match self.catalog.album_basic(&mbid).await {
                    Ok(basic) => basic,
                    Err(CatalogError::NotFound) => {
                        return Ok(Station::of_artists("Radio", Pooled::Similar, Vec::new()));
                    }
                    Err(error) => return Err(catalog_failure(error)),
                };
                let seed_tracks = self
                    .tracks
                    .tracks_of_albums(std::slice::from_ref(&mbid), ALBUM_SEED_TRACKS)
                    .await
                    .map_err(store)?;
                Ok(Station {
                    title: format!("Radio: {}", basic.title),
                    seed_name: basic.title.clone(),
                    base: normalize(Some(&basic.artist_id))
                        .map(|artist| (artist, basic.artist_name.clone())),
                    artists: Vec::new(),
                    pooled: Pooled::Similar,
                    seed_tracks,
                })
            }
        }
    }

    /// The library tracks among `ids` that are in the library now, in the
    /// order asked.
    async fn library_tracks(&self, ids: &[String]) -> Result<Vec<RadioPlanTrack>, RadioFailure> {
        let found = self.tracks.tracks_by_ids(ids).await.map_err(store)?;
        let mut by_id: HashMap<String, RadioPlanTrack> = found
            .into_iter()
            .filter_map(|track| Some((track.local_file_id.clone()?, track)))
            .collect();
        Ok(ids.iter().filter_map(|id| by_id.remove(id)).collect())
    }

    /// The artists a library pool spreads over: the station's artist, the
    /// artists of the tracks the person asked for more like and the
    /// station artist's similar artists; or the station's own artists.
    async fn pool_artists(
        &self,
        user_id: &str,
        station: &Station,
        more_like: &[RadioPlanTrack],
    ) -> Result<Vec<PoolArtist>, RadioFailure> {
        let feedback = self.feedback(user_id).await;
        let mut seen = HashSet::new();
        let mut artists = Vec::new();
        let seed_reason = |name: &str| match &station.pooled {
            Pooled::Shelf => reasons::shelf(),
            Pooled::Genre(genre) => reasons::genre(genre),
            Pooled::Playlist(playlist) => reasons::on_playlist(playlist),
            Pooled::Similar => reasons::seed(if name.is_empty() {
                &station.seed_name
            } else {
                name
            }),
        };
        for (mbid, name) in station.base.iter().chain(&station.artists) {
            if seen.insert(mbid.clone()) {
                artists.push(PoolArtist {
                    mbid: mbid.clone(),
                    name: name.clone(),
                    reason: seed_reason(name),
                });
            }
        }
        for track in more_like {
            if let Some(mbid) = normalize(Some(&track.artist_mbid)).filter(|id| is_valid_mbid(id))
                && !is_various_artists(Some(&mbid))
                && seen.insert(mbid.clone())
            {
                artists.push(PoolArtist {
                    mbid,
                    name: track.artist_name.clone(),
                    reason: reasons::more_like(&track.track_name, None),
                });
            }
        }
        let Some((base, base_name)) = station.base.as_ref() else {
            return Ok(artists);
        };
        let settings = self.queue_settings()?;
        let ctx = BuildContext {
            listening: &self.listening,
            db: &self.queue_db,
            settings: &settings,
            ask: Ask::person(),
            now_ms: self.clock.now_ms(),
        };
        let similar = similar_to(&ctx, user_id, &seed(base, base_name), MAX_SEED_ARTISTS + 1).await;
        for artist in similar.into_iter().map(|scored| scored.artist) {
            if artists.len() >= MAX_SEED_ARTISTS {
                break;
            }
            if let Some(mbid) = normalize(artist.mbid.as_deref())
                && !is_various_artists(Some(&mbid))
                && !feedback.hides_artist(Some(mbid.as_str()))
                && seen.insert(mbid.clone())
            {
                artists.push(PoolArtist {
                    reason: reasons::close_to_artist(&artist.name, base_name),
                    mbid,
                    name: artist.name,
                });
            }
        }
        Ok(artists)
    }

    /// The artists whose popular tracks join a station by sound: the seed
    /// tracks' artists, then the artists the person's Discover pool reaches
    /// from them.
    async fn sound_pool_artists(
        &self,
        user_id: &str,
        station: &Station,
        seeds: &[(&RadioPlanTrack, bool)],
    ) -> Result<Vec<PoolArtist>, RadioFailure> {
        let feedback = self.feedback(user_id).await;
        let mut seen = HashSet::new();
        let mut artists: Vec<PoolArtist> = Vec::new();
        for (track, more_like) in seeds {
            if artists.len() >= MAX_SEED_ARTISTS / 2 {
                break;
            }
            if let Some(mbid) = normalize(Some(&track.artist_mbid)).filter(|id| is_valid_mbid(id))
                && !is_various_artists(Some(&mbid))
                && !feedback.hides_artist(Some(mbid.as_str()))
                && seen.insert(mbid.clone())
            {
                let reason = match (&station.pooled, more_like) {
                    (_, true) => reasons::more_like(&track.track_name, None),
                    (Pooled::Playlist(playlist), false) => reasons::on_playlist(playlist),
                    _ => reasons::seed(&track.artist_name),
                };
                artists.push(PoolArtist {
                    reason,
                    mbid,
                    name: track.artist_name.clone(),
                });
            }
        }
        let seed_names: HashMap<String, String> = artists
            .iter()
            .map(|artist| (artist.mbid.clone(), artist.name.clone()))
            .collect();
        let pool = pool_store::ranked(self.queue_db.db(), user_id, self.clock.now_ms())
            .await
            .map_err(store)?;
        for candidate in pool {
            if artists.len() >= MAX_SEED_ARTISTS {
                break;
            }
            let Some(via) = candidate.paths.iter().find_map(|path| {
                let artist = path.artist.as_ref()?;
                seed_names.get(&artist.mbid)
            }) else {
                continue;
            };
            if seen.insert(candidate.artist_mbid.clone()) {
                artists.push(PoolArtist {
                    reason: reasons::close_to_artist(&candidate.artist_name, via),
                    mbid: candidate.artist_mbid,
                    name: candidate.artist_name,
                });
            }
        }
        Ok(artists)
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
        let feedback = self.feedback(user_id).await;
        let mut seen = HashSet::new();
        let mut seeds: Vec<(String, String)> = self
            .genre_seeds(genre, &[user_id, genre], want)
            .await
            .into_iter()
            .filter(|mbid| !feedback.hides_artist(Some(mbid.as_str())) && seen.insert(mbid.clone()))
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
                    && !feedback.hides_artist(Some(mbid.as_str()))
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
            reason: None,
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

    /// Whether a source an admin set up plays tracks the library lacks:
    /// the YouTube player with its search key. Settings that cannot be read
    /// are refused, never read as no source.
    fn plays_unowned(&self) -> Result<bool, RadioFailure> {
        let youtube = self.youtube.read().ok_or_else(|| {
            RadioFailure::Provider(ProviderFailure::not_configured(YOUTUBE_SETTINGS_UNREADABLE))
        })?;
        Ok(youtube.enabled && youtube.api_enabled && !youtube.api_key.expose().trim().is_empty())
    }

    /// The library tracks `user_id` skipped early on a station often
    /// enough lately to rest them from stations.
    async fn rested(&self, user_id: &str) -> Result<HashSet<String>, RadioFailure> {
        let since = self.clock.now_ms() - REST_WINDOW_MS;
        Ok(self
            .tracks
            .rested(user_id, EARLY_SKIP_MS, since, REST_AFTER_SKIPS)
            .await
            .map_err(store)?
            .into_iter()
            .collect())
    }

    /// The next library tracks of a station grown from `seeds` by how they
    /// sound, each with its reason; `None` while sound analysis is off or
    /// holds none of the seeds.
    async fn by_sound(
        &self,
        user_id: &str,
        request: &RadioPlanRequest,
        seeds: &[sound::Seed],
        names: &HashMap<String, (String, String)>,
        excluded: &HashSet<String>,
        count: usize,
    ) -> Result<Option<Vec<RadioPlanTrack>>, RadioFailure> {
        if seeds.is_empty() {
            return Ok(None);
        }
        match self.sound.switch() {
            Some(true) => {}
            Some(false) => {
                // Turned off: the loaded sounds are let go.
                self.sound.release();
                return Ok(None);
            }
            None => {
                return Err(RadioFailure::Provider(ProviderFailure::not_configured(
                    SOUND_SETTINGS_UNREADABLE,
                )));
            }
        }
        let feedback = self.feedback(user_id).await;
        let groups = if request.sound_only {
            HashMap::new()
        } else {
            self.tracks.taste_groups(user_id).await.map_err(store)?
        };
        let search = self.sound.clone();
        let seeds = seeds.to_vec();
        let excluded = excluded.clone();
        let not_this = request.not_this.clone();
        let played = station_ids(request);
        let sound_only = request.sound_only;
        // The load, when the analysis changed, and the scan of every vector
        // are blocking work.
        let searched = tokio::task::spawn_blocking(move || {
            let Some(index) = search.index()? else {
                return Ok(None);
            };
            if !seeds
                .iter()
                .any(|seed| index.track(&seed.track_id).is_some())
            {
                return Ok(None);
            }
            // "Not this artist" holds on a station too, except for the
            // artists the station itself grows from.
            let own: HashSet<String> = seeds
                .iter()
                .filter_map(|seed| index.track(&seed.track_id)?.artist_mbid.clone())
                .collect();
            let hidden_artist =
                |artist: &str| !own.contains(artist) && feedback.hides_artist(Some(artist));
            let steering = sound::Steering {
                excluded: &excluded,
                hidden_artist: &hidden_artist,
                not_this: &not_this,
                played: &played,
                groups: &groups,
                sound_only,
            };
            Ok::<_, SoundError>(Some(sound::station(&index, &seeds, &steering, count)))
        })
        .await;
        let found = match searched {
            Ok(Ok(Some(found))) => found,
            Ok(Ok(None)) => return Ok(None),
            Ok(Err(error)) => return Err(store(error)),
            Err(error) => return Err(store(error)),
        };
        let ids: Vec<String> = found.iter().map(|found| found.track_id.clone()).collect();
        let mut tracks: HashMap<String, RadioPlanTrack> = self
            .library_tracks(&ids)
            .await?
            .into_iter()
            .filter_map(|track| Some((track.local_file_id.clone()?, track)))
            .collect();
        Ok(Some(
            found
                .into_iter()
                .filter_map(|found| {
                    let mut track = tracks.remove(&found.track_id)?;
                    let (title, artist) = names.get(&found.seed.track_id)?;
                    track.reason = Some(if found.seed.more_like {
                        reasons::more_like(title, Some(&found.seed.track_id))
                    } else {
                        reasons::sounds_like(title, artist, &found.seed.track_id, found.distance)
                    });
                    Some(track)
                })
                .collect(),
        ))
    }

    async fn build_plan(
        &self,
        user_id: &str,
        request: &RadioPlanRequest,
    ) -> Result<RadioPlanResponse, RadioFailure> {
        let count = usize::try_from(request.count).unwrap_or(30);
        let exclude = plan::station_keys(&request.station_tracks);
        let hybrid = request.mode == RadioMode::Hybrid;
        let unowned = hybrid && self.plays_unowned()?;
        let library_only = hybrid && !unowned;
        let mut excluded: HashSet<String> = self.rested(user_id).await?;
        excluded.extend(station_ids(request));
        excluded.extend(request.not_this.iter().cloned());
        if request.seed_type == RadioSeedKind::Items
            && request
                .items
                .iter()
                .any(|item| item.album_mbid.is_some() && !item.album_name.is_empty())
        {
            let mut plan = self
                .shelf_plan(request, count, &exclude, &excluded, unowned)
                .await?;
            plan.library_only = library_only;
            return Ok(plan);
        }
        let station = self.station(user_id, request).await?;
        let more_like = self.library_tracks(&request.more_like).await?;
        let stations = &request.station_tracks;
        let recent: Vec<String> = stations[stations.len().saturating_sub(STRETCH)..]
            .iter()
            .map(plan::station_artist_key)
            .collect();
        let seed_tracks: Vec<(&RadioPlanTrack, bool)> = station
            .seed_tracks
            .iter()
            .map(|track| (track, false))
            .chain(more_like.iter().map(|track| (track, true)))
            .collect();
        let seeds: Vec<sound::Seed> = seed_tracks
            .iter()
            .filter_map(|(track, more_like)| {
                Some(sound::Seed {
                    track_id: track.local_file_id.clone()?,
                    more_like: *more_like,
                })
            })
            .collect();
        let names: HashMap<String, (String, String)> = seed_tracks
            .iter()
            .filter_map(|(track, _)| {
                Some((
                    track.local_file_id.clone()?,
                    (track.track_name.clone(), track.artist_name.clone()),
                ))
            })
            .collect();
        let (library, artists) = match self
            .by_sound(user_id, request, &seeds, &names, &excluded, count)
            .await?
        {
            Some(library) => {
                let artists = if unowned {
                    let tracks: Vec<(&RadioPlanTrack, bool)> = more_like
                        .iter()
                        .map(|track| (track, true))
                        .chain(station.seed_tracks.iter().map(|track| (track, false)))
                        .collect();
                    self.sound_pool_artists(user_id, &station, &tracks).await?
                } else {
                    Vec::new()
                };
                (library, artists)
            }
            None => {
                let artists = self.pool_artists(user_id, &station, &more_like).await?;
                if artists.is_empty() {
                    return Ok(RadioPlanResponse {
                        title: station.title,
                        tracks: Vec::new(),
                        library_only,
                    });
                }
                let ids: Vec<String> = artists.iter().map(|artist| artist.mbid.clone()).collect();
                let pool = self
                    .tracks
                    .pool_tracks(&ids, LIBRARY_POOL)
                    .await
                    .map_err(store)?;
                // A track goes by the artist that brought it: its own, else
                // its album's.
                let library = pool
                    .into_iter()
                    .map(|(mut track, album_artist)| {
                        let by = artists
                            .iter()
                            .find(|artist| artist.mbid == track.artist_mbid)
                            .or_else(|| artists.iter().find(|artist| artist.mbid == album_artist));
                        track.reason = by.map(|artist| artist.reason.clone());
                        track
                    })
                    .collect();
                let artists = if unowned { artists } else { Vec::new() };
                (library, artists)
            }
        };
        let external = if unowned {
            let lookups: Vec<_> = artists
                .iter()
                .map(|artist| async move {
                    let mut tracks = self.top_tracks(user_id, &artist.mbid, &artist.name).await;
                    for track in &mut tracks {
                        track.reason = Some(artist.reason.clone());
                    }
                    tracks
                })
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
            title: station.title,
            tracks: mix(
                clean_pool(library, &exclude, &excluded),
                clean_pool(external, &exclude, &excluded),
                count,
                unowned,
                &recent,
            ),
            library_only,
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
        excluded_tracks: &HashSet<String>,
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
            .map_err(store)?;
        let mut by_album: std::collections::HashMap<String, Vec<RadioPlanTrack>> =
            std::collections::HashMap::new();
        for track in clean_pool(local, exclude, excluded_tracks) {
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
            by_album.insert(album.clone(), clean_pool(found, exclude, excluded_tracks));
        }
        let mut seen = HashSet::new();
        let mut tracks = Vec::new();
        'albums: for album in &album_ids {
            for track in by_album.remove(album).unwrap_or_default() {
                if tracks.len() >= count {
                    break 'albums;
                }
                if seen.insert(song_key(&track.artist_name, &track.track_name)) {
                    tracks.push(RadioPlanTrack {
                        reason: Some(reasons::shelf()),
                        ..track
                    });
                }
            }
        }
        Ok(RadioPlanResponse {
            title: SHELF_TITLE.to_owned(),
            tracks,
            library_only: false,
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
                reason: None,
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
            .await?;
        let genres = self.genre_albums(&profile).await;
        if !genres.is_empty() {
            pools.push(genres);
        }
        // Leave out the playlist's own artists, matched on artist id, and
        // what the person hid.
        let own: HashSet<&str> = profile.artist_mbids.iter().map(String::as_str).collect();
        let feedback = self.feedback(user_id).await;
        for pool in &mut pools {
            pool.retain(|card| {
                !own.contains(card.artist_id.to_ascii_lowercase().as_str())
                    && !feedback.hides_album(Some(&card.musicbrainz_id), Some(&card.artist_id))
            });
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

    /// Albums tagged with up to four of the playlist artists' genres.
    async fn genre_albums(&self, profile: &PlaylistProfile) -> Vec<AlbumCard> {
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
        for albums in found {
            for album in albums {
                let Some(group) = normalize(Some(&album.release_group_mbid)) else {
                    continue;
                };
                if !taken.insert(group.clone()) {
                    continue;
                }
                cards.push(AlbumCard {
                    musicbrainz_id: group,
                    title: album.title,
                    artist_name: album.artist_name,
                    artist_id: album.artist_mbid.unwrap_or_default(),
                });
            }
        }
        cards
    }
}

impl LiveRadio {
    /// The next tracks of a station for the user.
    pub fn plan<'a>(
        &'a self,
        user_id: &'a str,
        request: &'a RadioPlanRequest,
    ) -> BoxFuture<'a, Result<RadioPlanResponse, RadioFailure>> {
        Box::pin(async move {
            let mut plan = self.build_plan(user_id, request).await?;
            self.feedback(user_id).await.keep_tracks(&mut plan.tracks);
            Ok(plan)
        })
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
