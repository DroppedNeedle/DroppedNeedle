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
    let micros = ((whole - secs as f64) * 1_000_000.0).round() as u32;
    let (y, m, d, hh, mm, ss) = civil_from_unix(secs);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}.{micros:06}0Z")
}

/// Current UTC time in v2 `datetime.now(timezone.utc).isoformat()` shape
/// (`2026-09-28T12:00:00.123456+00:00`) for `SessionInfo.LastActivityDate`.
pub fn utc_now_iso() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    let whole = now.max(0.0);
    let secs = whole.floor() as i64;
    let micros = ((whole - secs as f64) * 1_000_000.0).round() as u32;
    let (y, m, d, hh, mm, ss) = civil_from_unix(secs);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}.{micros:06}+00:00")
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

/// Caller-scoped item state (v2 `_user_data`).
pub fn user_data(item_id: &str, starred: bool, play_count: u64) -> UserItemDataDto {
    UserItemDataDto {
        item_id: item_id.to_owned(),
        key: item_id.to_owned(),
        playback_position_ticks: 0,
        play_count,
        is_favorite: starred,
        played: play_count > 0,
        last_played_date: None,
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
        dto.user_data = Some(user_data(&track_id, t.starred, t.play_count));
        dto
    }

    /// Album → `MusicAlbum` DTO (v2 `JellyfinBuilder.album`).
    pub async fn album(&self, a: &AlbumView) -> BaseItemDto {
        let album_id = self.ids.to_jf("album", &a.rg_mbid).await;
        let artist_jf = match a.artist_mbid.as_deref() {
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
        dto.provider_ids = Some(BTreeMap::from([(
            "MusicBrainzReleaseGroup".to_owned(),
            a.rg_mbid.clone(),
        )]));
        dto.date_created = Some(iso_o(a.date_added));
        dto.user_data = Some(user_data(&album_id, a.starred, a.play_count));
        dto
    }

    /// Artist → `MusicArtist` DTO (v2 `JellyfinBuilder.artist`).
    pub async fn artist(&self, ar: &ArtistView) -> BaseItemDto {
        let artist_id = self.ids.to_jf("artist", &ar.artist_mbid).await;
        let tag = ar.image_tag.clone();
        let mut dto = base(artist_id.clone(), ar.name.clone(), self.server_id);
        dto.item_type = "MusicArtist".to_owned();
        dto.is_folder = true;
        dto.child_count = Some(ar.album_count);
        if let Some(tag) = tag {
            dto.image_tags.insert("Primary".to_owned(), tag);
        }
        dto.provider_ids = Some(BTreeMap::from([(
            "MusicBrainzArtist".to_owned(),
            ar.artist_mbid.clone(),
        )]));
        dto.sort_name = Some(ar.name.clone());
        dto.genres = Some(Vec::new());
        dto.date_created = Some(iso_o(ar.date_added));
        dto.user_data = Some(user_data(&artist_id, ar.starred, 0));
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
        dto.user_data = Some(user_data(&pid, false, 0));
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
        dto.user_data = Some(user_data(&gid, false, 0));
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
