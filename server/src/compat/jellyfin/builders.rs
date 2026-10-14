//! View DTO → Jellyfin `BaseItemDto` shaping, ported from v2
//! `api/compat/jellyfin/builders.py`.

use std::collections::BTreeMap;

use super::models::{BaseItemDto, MediaStream, NameGuidPair, UserItemDataDto};
use super::seams::{
    AlbumView, ArtistView, GenreView, IdMap, PlaylistView, TICKS_PER_SECOND, TrackView,
};

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

/// Caller-scoped item state (v2 `_user_data`). `Played` follows the play
/// history: an item is played once the caller has a play of it.
pub fn user_data(
    item_id: &str,
    starred: bool,
    play_count: u64,
    last_played: Option<f64>,
) -> UserItemDataDto {
    UserItemDataDto {
        item_id: item_id.to_owned(),
        key: item_id.to_owned(),
        playback_position_ticks: 0,
        play_count,
        is_favorite: starred,
        played: play_count > 0,
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
        location_type: "FileSystem".to_owned(),
        backdrop_image_tags: Vec::new(),
        image_blur_hashes: BTreeMap::new(),
    }
}

/// The single "Music" collection-folder view. Strict clients (Manet) report
/// "No music libraries found" unless it carries `UserData`, a non-empty
/// `ImageTags.Primary`, `LocationType`, and `SortName` (v2 `_music_view`).
pub fn music_view(library_id: &str, server_id: &str) -> BaseItemDto {
    let mut dto = base(library_id.to_owned(), "Music".to_owned(), server_id);
    dto.item_type = "CollectionFolder".to_owned();
    dto.sort_name = Some("Music".to_owned());
    dto.is_folder = true;
    dto.collection_type = Some("music".to_owned());
    dto.image_tags
        .insert("Primary".to_owned(), library_id.to_owned());
    dto.user_data = Some(UserItemDataDto::blank(library_id));
    dto
}

/// View → DTO builder (v2 `JellyfinBuilder`). Image tags ride the views,
/// so shaping a page never reads the library again.
pub struct Builder<'a, I> {
    ids: &'a I,
    server_id: &'a str,
}

impl<'a, I: IdMap> Builder<'a, I> {
    /// Borrow the id map and server id.
    pub fn new(ids: &'a I, server_id: &'a str) -> Self {
        Self { ids, server_id }
    }

    /// Track → `Audio` DTO (v2 `JellyfinBuilder.audio`).
    pub async fn audio(&self, t: &TrackView) -> BaseItemDto {
        let track_id = self.ids.to_jf("track", &t.file_id).await;
        let album_id = match t.rg_mbid.as_deref() {
            Some(rg) => Some(self.ids.to_jf("album", rg).await),
            None => None,
        };
        let album_artist_mbid = t.album_artist_mbid.as_deref().or(t.artist_mbid.as_deref());
        let artist_jf = match t.artist_mbid.as_deref() {
            Some(m) => Some(self.ids.to_jf("artist", m).await),
            None => None,
        };
        let album_artist_jf = match album_artist_mbid {
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
        dto.album_primary_image_tag = album_tag.clone();
        if let Some(tag) = album_tag {
            dto.image_tags.insert("Primary".to_owned(), tag);
        }
        dto.parent_id = album_id;
        dto.container = t.file_format.clone();
        dto.genres = Some(t.genre.clone().map(|g| vec![g]).unwrap_or_default());
        if let Some(mbid) = t.recording_mbid.as_deref() {
            dto.provider_ids = Some(BTreeMap::from([(
                "MusicBrainzTrack".to_owned(),
                mbid.to_owned(),
            )]));
        }
        dto.date_created = Some(iso_o(t.created_at));
        dto.user_data = Some(user_data(&track_id, t.starred, t.play_count, t.last_played));
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
        if let Some(mbid) = a.release_group_mbid.as_deref() {
            provider_ids.insert("MusicBrainzReleaseGroup".to_owned(), mbid.to_owned());
        }
        dto.provider_ids = Some(provider_ids);
        dto.date_created = Some(iso_o(a.date_added));
        dto.user_data = Some(user_data(&album_id, a.starred, a.play_count, a.last_played));
        dto
    }

    /// Artist → `MusicArtist` DTO (v2 `JellyfinBuilder.artist`).
    pub async fn artist(&self, ar: &ArtistView) -> BaseItemDto {
        let artist_id = self.ids.to_jf("artist", &ar.id).await;
        let tag = ar.image_tag.clone();
        let mut dto = base(artist_id.clone(), ar.name.clone(), self.server_id);
        dto.item_type = "MusicArtist".to_owned();
        dto.is_folder = true;
        dto.child_count = Some(ar.album_count);
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
        dto.user_data = Some(user_data(&artist_id, ar.starred, 0, None));
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
        dto.user_data = Some(user_data(&pid, false, 0, None));
        dto
    }

    /// Genre → `MusicGenre` DTO (v2 `JellyfinBuilder.genre`).
    pub async fn genre(&self, g: &GenreView) -> BaseItemDto {
        let gid = self.ids.to_jf("genre", &genre_slug(&g.name)).await;
        let mut dto = base(gid.clone(), g.name.clone(), self.server_id);
        dto.item_type = "MusicGenre".to_owned();
        dto.is_folder = true;
        dto.child_count = Some(g.song_count);
        dto.sort_name = Some(g.name.clone());
        dto.user_data = Some(user_data(&gid, false, 0, None));
        dto
    }
}

/// Track → `MediaStream` for PlaybackInfo (v2 `_audio_stream_model`).
pub fn media_stream(track: &TrackView) -> MediaStream {
    MediaStream {
        stream_type: "Audio".to_owned(),
        codec: track.file_format.clone(),
        index: 0,
        bit_rate: track
            .bitrate
            .map(|b| u64::from(b) * 1000)
            .filter(|b| *b != 0),
        channels: track.channels.unwrap_or(2),
        channel_layout: "stereo".to_owned(),
        sample_rate: track.sample_rate,
        bit_depth: track.bit_depth,
        is_default: true,
        is_interlaced: false,
        is_forced: false,
        is_external: false,
        is_text_subtitle_stream: false,
        supports_external_stream: false,
    }
}
