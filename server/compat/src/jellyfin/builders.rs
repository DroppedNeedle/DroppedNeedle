//! View DTO → Jellyfin `BaseItemDto` shaping, ported from v2
//! `api/compat/jellyfin/builders.py`.

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

use droppedneedle_providers::musicbrainz::is_valid_mbid;

use super::models::{
    BaseItemDto, EmptyList, ExternalUrl, ItemDetail, MediaSourceInfo, MediaStream, NameGuidPair,
    UserItemDataDto,
};
use super::seams::{AlbumView, ArtistView, GenreView, PlaylistView, TICKS_PER_SECOND, TrackView};
use crate::adapters::jellyfin_library::CatalogIds;

/// Internal id of the single music library (v2 `LIBRARY_INTERNAL_ID`).
pub const LIBRARY_INTERNAL_ID: &str = "music";

/// Seconds → Jellyfin ticks (v2 `builders.ticks`).
pub fn ticks(seconds: Option<f64>) -> Option<i64> {
    seconds.map(|s| (s * TICKS_PER_SECOND as f64).round() as i64)
}

/// Genre display name → id slug (v2 `genre_slug`).
pub fn genre_slug(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut dash = false;
    for c in name.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Manet requires `DateCreated` present, so items without a library date get
/// the epoch, never null (v2 `_DEFAULT_DATE`).
pub const DEFAULT_DATE: &str = "1970-01-01T00:00:00.0000000Z";

/// Unix seconds → .NET "O" round-trip format: strict clients (Manet) reject
/// whole-second ISO, the 7-digit fraction is required (v2 `_iso`).
pub fn iso_o(ts: Option<f64>) -> String {
    let Some(ts) = ts else {
        return DEFAULT_DATE.to_owned();
    };
    let whole = ts.max(0.0);
    let secs = whole.floor() as i64;
    // A fraction within half a microsecond of the next second would round
    // to a seventh digit: it stays in this second.
    let micros = (((whole - secs as f64) * 1_000_000.0).round() as u32).min(999_999);
    let (y, m, d, hh, mm, ss) = civil_from_unix(secs);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}.{micros:06}0Z")
}

/// `PremiereDate` in the format of `DateCreated`. Jellyfin 10.10.6 writes
/// midnight on 1 January for an item it only knows the year of
/// (`1997-01-01T00:00:00.0000000Z`, checked against its recorded answers).
/// `release_date` is the catalog's `YYYY[-MM[-DD]]`; its month and day are
/// used only when it is a real date in `year`, so the date never disagrees
/// with `ProductionYear` or with the order a sort by this key gives.
pub fn premiere_date(year: Option<i32>, release_date: Option<&str>) -> Option<String> {
    let year = year.filter(|year| (1..=9999).contains(year))?;
    let (month, day) = release_date
        .and_then(|date| month_and_day(year, date))
        .unwrap_or((1, 1));
    Some(format!("{year:04}-{month:02}-{day:02}T00:00:00.0000000Z"))
}

fn month_and_day(year: i32, date: &str) -> Option<(u8, u8)> {
    let mut parts = date.split('-');
    if parts.next()?.parse::<i32>().ok()? != year {
        return None;
    }
    let month: u8 = parts.next()?.parse().ok()?;
    let day: u8 = match parts.next() {
        Some(day) => day.parse().ok()?,
        None => 1,
    };
    time::Date::from_calendar_date(year, time::Month::try_from(month).ok()?, day).ok()?;
    Some((month, day))
}

/// Unix seconds → (year, month, day, hour, min, sec) UTC (Howard Hinnant's
/// civil-from-days; no date crate needed).
fn civil_from_unix(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let time = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (
        if m <= 2 { y + 1 } else { y },
        m,
        d,
        (time / 3600) as u32,
        ((time % 3600) / 60) as u32,
        (time % 60) as u32,
    )
}

/// Caller-scoped item state (v2 `_user_data`). `played`, the play count and
/// the last play come from the library's one reading of plays and marks.
pub fn user_data(
    item_id: &str,
    starred: bool,
    played: bool,
    play_count: u64,
    last_played: Option<f64>,
) -> UserItemDataDto {
    UserItemDataDto {
        item_id: item_id.to_owned(),
        key: item_id.to_owned(),
        playback_position_ticks: 0,
        play_count,
        is_favorite: starred,
        played,
        last_played_date: last_played.map(|at| iso_o(Some(at))),
        rating: None,
        played_percentage: None,
    }
}

fn base(id: String, name: String, server_id: &str) -> BaseItemDto {
    BaseItemDto {
        id,
        name,
        item_type: String::new(),
        server_id: server_id.to_owned(),
        is_folder: false,
        media_type: "Unknown".to_owned(),
        run_time_ticks: None,
        production_year: None,
        premiere_date: None,
        index_number: None,
        parent_index_number: None,
        album: None,
        album_id: None,
        album_artist: None,
        album_artists: None,
        artist_items: None,
        artists: None,
        album_primary_image_tag: None,
        image_tags: BTreeMap::new(),
        parent_id: None,
        genres: None,
        container: None,
        child_count: None,
        collection_type: None,
        sort_name: None,
        date_created: None,
        provider_ids: None,
        user_data: None,
        playlist_item_id: None,
        has_lyrics: None,
        location_type: "FileSystem".to_owned(),
        backdrop_image_tags: Vec::new(),
        image_blur_hashes: BTreeMap::new(),
        media_sources: None,
        media_streams: None,
        detail: None,
    }
}

/// Jellyfin's empty id, its "no item". The library view's parent: nothing
/// sits above the library here.
pub const NO_PARENT_ID: &str = "00000000000000000000000000000000";

/// Where `ExternalUrls` point, Jellyfin's default MusicBrainz server.
const MUSICBRAINZ_WEB: &str = "https://musicbrainz.org";

/// First 32 hex characters of the SHA-256 of `text`.
fn hex_id(text: &[u8]) -> String {
    format!("{:x}", Sha256::digest(text))[..32].to_owned()
}

/// MusicBrainz links for the `(entity, id)` pairs that hold a well-formed
/// MBID (a recording's may come from a file tag), each once.
fn musicbrainz_urls(links: &[(&str, Option<&str>)]) -> Vec<ExternalUrl> {
    let mut urls: Vec<ExternalUrl> = Vec::new();
    for (entity, id) in links {
        let Some(id) = id.map(str::trim).filter(|id| is_valid_mbid(id)) else {
            continue;
        };
        let url = format!("{MUSICBRAINZ_WEB}/{entity}/{id}");
        if !urls.iter().any(|known| known.url == url) {
            urls.push(ExternalUrl {
                name: "MusicBrainz".to_owned(),
                url,
            });
        }
    }
    urls
}

/// The long-form keys with what this API never has: no deleting, no
/// trailers, extras, people, studios, tags or locked metadata. Jellyfin
/// 10.10.6 (`BaseItem.DisplayPreferencesId`) gives a collection folder its
/// own id there and every other item one id per item type; the same here.
fn item_detail(display_preferences_id: String) -> ItemDetail {
    ItemDetail {
        etag: String::new(),
        can_delete: false,
        can_download: false,
        path: String::new(),
        external_urls: Vec::new(),
        enable_media_source_display: true,
        taglines: EmptyList,
        play_access: "Full".to_owned(),
        remote_trailers: EmptyList,
        people: EmptyList,
        studios: EmptyList,
        genre_items: Vec::new(),
        local_trailer_count: 0,
        special_feature_count: 0,
        display_preferences_id,
        tags: EmptyList,
        locked_fields: EmptyList,
        lock_data: false,
        cumulative_run_time_ticks: None,
        recursive_item_count: None,
        album_count: None,
        song_count: None,
        music_video_count: None,
    }
}

fn type_preferences_id(item_type: &str) -> String {
    hex_id(format!("displaypreferences:{item_type}").as_bytes())
}

/// Set `Etag` from everything the item says apart from the caller's own
/// state, so it changes exactly when the item does.
fn seal(mut dto: BaseItemDto) -> BaseItemDto {
    let mut shared = dto.clone();
    shared.user_data = None;
    let etag = hex_id(&serde_json::to_vec(&shared).unwrap_or_default());
    if let Some(detail) = dto.detail.as_mut() {
        detail.etag = etag;
    }
    dto
}

/// The single "Music" collection-folder view, in the long form Jellyfin
/// sends it. Strict clients (Manet) report "No music libraries found"
/// unless it carries `UserData`, a non-empty `ImageTags.Primary`,
/// `LocationType`, and `SortName` (v2 `_music_view`). `child_count` is the
/// number of album artists, the nearest thing to the top folders Jellyfin
/// counts.
pub fn music_view(library_id: &str, server_id: &str, child_count: usize) -> BaseItemDto {
    let mut dto = base(library_id.to_owned(), "Music".to_owned(), server_id);
    dto.item_type = "CollectionFolder".to_owned();
    dto.sort_name = Some("Music".to_owned());
    dto.is_folder = true;
    dto.collection_type = Some("music".to_owned());
    dto.image_tags
        .insert("Primary".to_owned(), library_id.to_owned());
    dto.user_data = Some(UserItemDataDto::blank(library_id));
    dto.parent_id = Some(NO_PARENT_ID.to_owned());
    dto.child_count = Some(child_count);
    dto.genres = Some(Vec::new());
    dto.date_created = Some(DEFAULT_DATE.to_owned());
    dto.provider_ids = Some(BTreeMap::new());
    dto.detail = Some(item_detail(library_id.to_owned()));
    seal(dto)
}

/// View → DTO builder (v2 `JellyfinBuilder`). Image tags ride the views,
/// so shaping a page never reads the library again.
pub struct Builder<'a> {
    ids: &'a CatalogIds,
    server_id: &'a str,
}

impl<'a> Builder<'a> {
    /// Borrow the id map and server id.
    pub fn new(ids: &'a CatalogIds, server_id: &'a str) -> Self {
        Self { ids, server_id }
    }

    /// Track → `Audio` DTO (v2 `JellyfinBuilder.audio`).
    pub async fn audio(&self, t: &TrackView) -> BaseItemDto {
        let track_id = self.ids.to_jf("track", &t.file_id).await;
        let album_id = match t.album_id.as_deref() {
            Some(rg) => Some(self.ids.to_jf("album", rg).await),
            None => None,
        };
        let album_artist = t.album_artist_id.as_deref().or(t.artist_id.as_deref());
        let artist_jf = match t.artist_id.as_deref() {
            Some(m) => Some(self.ids.to_jf("artist", m).await),
            None => None,
        };
        let album_artist_jf = match album_artist {
            Some(m) => Some(self.ids.to_jf("artist", m).await),
            None => None,
        };
        let album_tag = t.album_image_tag.clone();
        let mut dto = base(track_id.clone(), t.title.clone(), self.server_id);
        dto.item_type = "Audio".to_owned();
        dto.media_type = "Audio".to_owned();
        dto.sort_name = Some(t.title.clone());
        dto.run_time_ticks = ticks(t.duration_seconds);
        dto.production_year = t.year;
        dto.premiere_date = premiere_date(t.year, None);
        dto.index_number = t.track_number.filter(|n| *n != 0);
        dto.parent_index_number = t.disc_number.filter(|n| *n != 0);
        dto.album = t.album_title.clone();
        dto.album_id = album_id.clone();
        dto.album_artist = Some(
            t.album_artist_name
                .clone()
                .or(t.artist_name.clone())
                .unwrap_or_default(),
        );
        // Jellyfin emits these as a (possibly empty) array, never null;
        // strict clients (Manet) require them present.
        dto.album_artists = Some(
            match (&t.album_artist_name, &t.artist_name, album_artist_jf) {
                (Some(name), _, Some(id)) | (None, Some(name), Some(id)) => {
                    vec![NameGuidPair {
                        name: name.clone(),
                        id,
                    }]
                }
                _ => Vec::new(),
            },
        );
        dto.artist_items = Some(match (&t.artist_name, artist_jf) {
            (Some(name), Some(id)) => vec![NameGuidPair {
                name: name.clone(),
                id,
            }],
            _ => Vec::new(),
        });
        dto.artists = Some(t.artist_name.clone().map(|n| vec![n]).unwrap_or_default());
        // A track has no picture of its own: apps fall back to `AlbumId`
        // with `AlbumPrimaryImageTag`, and the picture address serves only
        // albums, artists and the library.
        dto.album_primary_image_tag = album_tag;
        dto.parent_id = album_id;
        dto.container = t.file_format.clone();
        dto.genres = Some(t.genre.clone().map(|g| vec![g]).unwrap_or_default());
        dto.has_lyrics = Some(t.has_lyrics);
        let provider_ids: BTreeMap<String, String> = [
            ("MusicBrainzTrack", &t.recording_mbid),
            ("MusicBrainzAlbum", &t.release_mbid),
            ("MusicBrainzArtist", &t.artist_musicbrainz_id),
            ("MusicBrainzReleaseGroup", &t.release_group_mbid),
        ]
        .into_iter()
        .filter_map(|(key, id)| Some((key.to_owned(), id.clone()?)))
        .collect();
        if !provider_ids.is_empty() {
            dto.provider_ids = Some(provider_ids);
        }
        dto.date_created = Some(iso_o(t.created_at));
        dto.user_data = Some(user_data(
            &track_id,
            t.starred,
            t.played,
            t.play_count,
            t.last_played,
        ));
        dto
    }

    /// Album → `MusicAlbum` DTO (v2 `JellyfinBuilder.album`).
    pub async fn album(&self, a: &AlbumView) -> BaseItemDto {
        let album_id = self.ids.to_jf("album", &a.id).await;
        let artist_jf = match a.artist_id.as_deref() {
            Some(m) => Some(self.ids.to_jf("artist", m).await),
            None => None,
        };
        let tag = a.image_tag.clone();
        let mut dto = base(album_id.clone(), a.title.clone(), self.server_id);
        dto.item_type = "MusicAlbum".to_owned();
        dto.is_folder = true;
        dto.sort_name = Some(a.title.clone());
        dto.run_time_ticks = ticks(a.total_duration_seconds);
        dto.production_year = a.year;
        dto.premiere_date = premiere_date(a.year, a.release_date.as_deref());
        dto.child_count = Some(a.track_count);
        dto.album_artist = a.artist_name.clone();
        let pair = match (&a.artist_name, artist_jf) {
            (Some(name), Some(id)) => vec![NameGuidPair {
                name: name.clone(),
                id,
            }],
            _ => Vec::new(),
        };
        dto.album_artists = Some(pair.clone());
        dto.artist_items = Some(pair);
        dto.artists = Some(a.artist_name.clone().map(|n| vec![n]).unwrap_or_default());
        dto.genres = Some(a.genre.clone().map(|g| vec![g]).unwrap_or_default());
        if let Some(tag) = tag {
            dto.image_tags.insert("Primary".to_owned(), tag);
        }
        let mut provider_ids = BTreeMap::new();
        if let Some(mbid) = a.release_mbid.as_deref() {
            provider_ids.insert("MusicBrainzAlbum".to_owned(), mbid.to_owned());
        }
        if let Some(mbid) = a.release_group_mbid.as_deref() {
            provider_ids.insert("MusicBrainzReleaseGroup".to_owned(), mbid.to_owned());
        }
        dto.provider_ids = Some(provider_ids);
        dto.date_created = Some(iso_o(a.date_added));
        dto.user_data = Some(user_data(
            &album_id,
            a.starred,
            a.played,
            a.play_count,
            a.last_played,
        ));
        dto
    }

    /// Artist → `MusicArtist` DTO (v2 `JellyfinBuilder.artist`).
    pub async fn artist(&self, ar: &ArtistView) -> BaseItemDto {
        let artist_id = self.ids.to_jf("artist", &ar.id).await;
        let tag = ar.image_tag.clone();
        let mut dto = base(artist_id.clone(), ar.name.clone(), self.server_id);
        dto.item_type = "MusicArtist".to_owned();
        dto.is_folder = true;
        // Jellyfin counts the albums and songs tagged with the artist
        // (`DtoService.SetItemByNameInfo`).
        dto.child_count = Some(ar.album_count + ar.appearance_album_count + ar.track_count);
        dto.run_time_ticks = ticks(Some(ar.total_duration_seconds));
        if let Some(tag) = tag {
            dto.image_tags.insert("Primary".to_owned(), tag);
        }
        let mut provider_ids = BTreeMap::new();
        if let Some(mbid) = ar.artist_mbid.as_deref() {
            provider_ids.insert("MusicBrainzArtist".to_owned(), mbid.to_owned());
        }
        dto.provider_ids = Some(provider_ids);
        dto.sort_name = Some(ar.name.clone());
        dto.genres = Some(Vec::new());
        dto.date_created = Some(iso_o(ar.date_added));
        dto.user_data = Some(user_data(&artist_id, ar.starred, false, 0, None));
        dto
    }

    /// Playlist → `Playlist` DTO (v2 `JellyfinBuilder.playlist`).
    pub async fn playlist(&self, p: &PlaylistView) -> BaseItemDto {
        let pid = self.ids.to_jf("playlist", &p.id).await;
        let mut dto = base(pid.clone(), p.name.clone(), self.server_id);
        dto.item_type = "Playlist".to_owned();
        dto.is_folder = true;
        dto.media_type = "Audio".to_owned();
        dto.child_count = Some(p.track_count);
        dto.sort_name = Some(p.name.clone());
        dto.run_time_ticks = ticks(p.total_duration_seconds);
        dto.user_data = Some(user_data(&pid, false, false, 0, None));
        dto
    }

    /// `GenreItems`: each genre with its id.
    async fn genre_items(&self, dto: &BaseItemDto) -> Vec<NameGuidPair> {
        let mut items = Vec::new();
        for name in dto.genres.iter().flatten() {
            items.push(NameGuidPair {
                name: name.clone(),
                id: self.ids.to_jf("genre", &genre_slug(name)).await,
            });
        }
        items
    }

    /// One track fetched by id, with its file and audio stream as Jellyfin
    /// sends them there: no stream address and no token. `can_download` is
    /// whether the caller may download library files.
    pub async fn audio_full(
        &self,
        t: &TrackView,
        can_download: bool,
        supports_transcoding: bool,
    ) -> BaseItemDto {
        let mut dto = self.audio(t).await;
        let source = media_source(t, &dto.id, supports_transcoding);
        dto.media_streams = Some(source.media_streams.clone());
        dto.media_sources = Some(vec![source]);
        let mut detail = item_detail(type_preferences_id(&dto.item_type));
        detail.can_download = can_download;
        detail.path = t.path.clone().unwrap_or_default();
        detail.external_urls = musicbrainz_urls(&[
            ("release", t.release_mbid.as_deref()),
            ("artist", t.artist_musicbrainz_id.as_deref()),
            ("artist", t.album_artist_musicbrainz_id.as_deref()),
            ("release-group", t.release_group_mbid.as_deref()),
            ("recording", t.recording_mbid.as_deref()),
        ]);
        detail.genre_items = self.genre_items(&dto).await;
        dto.detail = Some(detail);
        seal(dto)
    }

    /// One album fetched by id: its parent is its album artist.
    pub async fn album_full(&self, a: &AlbumView) -> BaseItemDto {
        let mut dto = self.album(a).await;
        let mut detail = item_detail(type_preferences_id(&dto.item_type));
        detail.external_urls = musicbrainz_urls(&[
            ("release", a.release_mbid.as_deref()),
            ("artist", a.artist_musicbrainz_id.as_deref()),
            ("release-group", a.release_group_mbid.as_deref()),
        ]);
        detail.genre_items = self.genre_items(&dto).await;
        detail.cumulative_run_time_ticks = dto.run_time_ticks;
        detail.recursive_item_count = Some(a.track_count);
        dto.parent_id = dto
            .album_artists
            .iter()
            .flatten()
            .next()
            .map(|a| a.id.clone());
        dto.detail = Some(detail);
        seal(dto)
    }

    /// One artist fetched by id: its parent is the library. Jellyfin counts
    /// albums and songs that credit the artist anywhere.
    pub async fn artist_full(&self, ar: &ArtistView, library_id: &str) -> BaseItemDto {
        let mut dto = self.artist(ar).await;
        let mut detail = item_detail(type_preferences_id(&dto.item_type));
        detail.external_urls = musicbrainz_urls(&[("artist", ar.artist_mbid.as_deref())]);
        detail.cumulative_run_time_ticks = dto.run_time_ticks;
        detail.recursive_item_count = Some(ar.track_count);
        detail.album_count = Some(ar.album_count + ar.appearance_album_count);
        detail.song_count = Some(ar.track_count);
        detail.music_video_count = Some(0);
        dto.parent_id = Some(library_id.to_owned());
        dto.detail = Some(detail);
        seal(dto)
    }

    /// One playlist fetched by id.
    pub async fn playlist_full(&self, p: &PlaylistView) -> BaseItemDto {
        let mut dto = self.playlist(p).await;
        dto.detail = Some(item_detail(type_preferences_id(&dto.item_type)));
        seal(dto)
    }

    /// Genre → `MusicGenre` DTO (v2 `JellyfinBuilder.genre`).
    pub async fn genre(&self, g: &GenreView) -> BaseItemDto {
        let gid = self.ids.to_jf("genre", &genre_slug(&g.name)).await;
        let mut dto = base(gid.clone(), g.name.clone(), self.server_id);
        dto.item_type = "MusicGenre".to_owned();
        dto.is_folder = true;
        dto.child_count = Some(g.song_count);
        dto.sort_name = Some(g.name.clone());
        dto.user_data = Some(user_data(&gid, false, false, 0, None));
        dto
    }
}

/// A track's file as a Jellyfin media source, with no stream address.
/// The playback information answer and one track fetched by id both
/// describe the file with it.
pub fn media_source(
    track: &TrackView,
    item_id: &str,
    supports_transcoding: bool,
) -> MediaSourceInfo {
    let path = track.path.clone().unwrap_or_default();
    let name = std::path::Path::new(&path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map_or_else(|| track.title.clone(), str::to_owned);
    let etag = hex_id(
        &serde_json::to_vec(&(
            &track.file_id,
            &path,
            &track.file_format,
            track.file_size_bytes,
            track.duration_seconds,
            track.bitrate,
            track.channels,
            track.sample_rate,
            track.bit_depth,
        ))
        .unwrap_or_default(),
    );
    MediaSourceInfo {
        id: item_id.to_owned(),
        protocol: "File".to_owned(),
        container: track.file_format.clone(),
        size: track.file_size_bytes,
        bitrate: track
            .bitrate
            .map(|b| u64::from(b) * 1000)
            .filter(|b| *b != 0),
        run_time_ticks: ticks(track.duration_seconds),
        supports_direct_play: true,
        supports_direct_stream: true,
        supports_transcoding,
        default_audio_stream_index: 0,
        media_streams: if track.has_lyrics {
            vec![media_stream(track), lyric_stream()]
        } else {
            vec![media_stream(track)]
        },
        name,
        path,
        etag,
        is_remote: false,
        direct_stream_url: None,
        transcoding_url: None,
        transcoding_sub_protocol: "http".to_owned(),
        transcoding_container: None,
        source_type: "Default".to_owned(),
        is_infinite_stream: false,
        requires_opening: false,
        requires_closing: false,
        requires_looping: false,
        supports_probing: false,
        read_at_native_framerate: false,
        ignore_dts: false,
        ignore_index: false,
        gen_pts_input: false,
        formats: EmptyList,
        media_attachments: EmptyList,
        required_http_headers: BTreeMap::new(),
        use_most_compatible_transcoding_profile: false,
        has_segments: false,
    }
}

/// Track → `MediaStream` (v2 `_audio_stream_model`).
pub fn media_stream(track: &TrackView) -> MediaStream {
    // An MP4 file holds AAC or ALAC, and the scan reads a bit depth only
    // from ALAC.
    let codec = match (track.file_format.as_deref(), track.bit_depth) {
        (Some("m4a"), None) => Some("aac".to_owned()),
        (Some("m4a"), Some(_)) => Some("alac".to_owned()),
        _ => track.file_format.clone(),
    };
    let channels = track.channels.unwrap_or(2);
    let layout = match channels {
        1 => "Mono".to_owned(),
        2 => "Stereo".to_owned(),
        n => format!("{n} ch"),
    };
    let display_title = match codec.as_deref() {
        Some(codec) => format!("{} - {layout}", codec.to_uppercase()),
        None => layout,
    };
    MediaStream {
        stream_type: "Audio".to_owned(),
        codec,
        index: 0,
        bit_rate: track
            .bitrate
            .map(|b| u64::from(b) * 1000)
            .filter(|b| *b != 0),
        channels: Some(channels),
        channel_layout: Some("stereo".to_owned()),
        sample_rate: track.sample_rate,
        bit_depth: track.bit_depth,
        is_default: true,
        is_interlaced: false,
        is_forced: false,
        is_external: false,
        is_text_subtitle_stream: false,
        supports_external_stream: false,
        time_base: track.sample_rate.map(|rate| format!("1/{rate}")),
        display_title: Some(display_title),
        video_range: "Unknown".to_owned(),
        video_range_type: "Unknown".to_owned(),
        audio_spatial_format: "None".to_owned(),
        localized_default: Some("Default".to_owned()),
        localized_external: Some("External".to_owned()),
        is_avc: Some(false),
        is_hearing_impaired: false,
        level: Some(0),
    }
}

/// The `Lyric` entry of a file that carries lyrics, after its audio
/// stream. Finamp asks for lyrics only when PlaybackInfo lists one.
/// Jellyfin 10.10.6 also sends the lyrics file's `Path`; a host path is
/// never sent here.
pub fn lyric_stream() -> MediaStream {
    MediaStream {
        stream_type: "Lyric".to_owned(),
        codec: None,
        index: 1,
        bit_rate: None,
        channels: None,
        channel_layout: None,
        sample_rate: None,
        bit_depth: None,
        is_default: false,
        is_interlaced: false,
        is_forced: false,
        is_external: false,
        is_text_subtitle_stream: false,
        supports_external_stream: false,
        time_base: None,
        display_title: None,
        video_range: "Unknown".to_owned(),
        video_range_type: "Unknown".to_owned(),
        audio_spatial_format: "None".to_owned(),
        localized_default: None,
        localized_external: None,
        is_avc: None,
        is_hearing_impaired: false,
        level: None,
    }
}
