//! Navidrome (Subsonic/OpenSubsonic JSON) wire shapes the adapter reads.
//!
//! Every answer arrives inside `subsonic-response`; the payload sits under
//! one endpoint-specific key, so [`Body`] carries each key the adapter
//! reads as an optional field. Unknown keys and fields are ignored and
//! optional fields default. Identity is required: an album, artist, song,
//! playlist, or folder without an `id` fails decoding and the adapter
//! reports an upstream error instead of rendering an item nobody can open.
//! Folder ids arrive as numbers on some servers and strings on others;
//! both decode to a string.

use serde::{Deserialize, Deserializer};

/// The JSON envelope.
#[derive(Debug, Clone, Deserialize)]
pub struct Envelope {
    /// The response.
    #[serde(rename = "subsonic-response")]
    pub response: Body,
}

/// `subsonic-response`: status plus whichever payload the endpoint sent.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Body {
    /// `ok` or `failed`.
    pub status: String,
    /// API version.
    pub version: Option<String>,
    /// Failure detail when `status` is `failed`.
    pub error: Option<SubsonicError>,
    /// `getMusicFolders`.
    pub music_folders: Option<MusicFolders>,
    /// `getAlbumList2`.
    pub album_list2: Option<AlbumList>,
    /// `getArtists`.
    pub artists: Option<ArtistIndexes>,
    /// `getArtist`.
    pub artist: Option<Artist>,
    /// `getAlbum`.
    pub album: Option<Album>,
    /// `search3`.
    pub search_result3: Option<Buckets>,
    /// `getStarred2`.
    pub starred2: Option<Buckets>,
    /// `getGenres`.
    pub genres: Option<Genres>,
    /// `getSongsByGenre`.
    pub songs_by_genre: Option<Songs>,
    /// `getPlaylists`.
    pub playlists: Option<Playlists>,
    /// `getPlaylist`.
    pub playlist: Option<Playlist>,
    /// `getArtistInfo2`.
    pub artist_info2: Option<Info>,
    /// `getAlbumInfo2`.
    pub album_info: Option<Info>,
    /// `getLyricsBySongId`.
    pub lyrics_list: Option<LyricsList>,
    /// `getLyrics`.
    pub lyrics: Option<ClassicLyrics>,
    /// `getTopSongs`.
    pub top_songs: Option<Songs>,
    /// `getRandomSongs`.
    pub random_songs: Option<Songs>,
    /// `getSimilarSongs2`.
    pub similar_songs2: Option<Songs>,
    /// `getNowPlaying`.
    pub now_playing: Option<NowPlaying>,
}

/// A failed call.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SubsonicError {
    /// Subsonic error code (40/41 are auth failures).
    pub code: i64,
    /// Human message.
    pub message: Option<String>,
}

/// `musicFolders`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MusicFolders {
    /// Folders.
    pub music_folder: Vec<MusicFolder>,
}

/// One music folder (library).
#[derive(Debug, Clone, Deserialize)]
pub struct MusicFolder {
    /// Folder id, number or string upstream.
    #[serde(deserialize_with = "string_or_number")]
    pub id: String,
    /// Folder name.
    #[serde(default)]
    pub name: Option<String>,
}

/// `albumList2`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AlbumList {
    /// Albums.
    pub album: Vec<Album>,
}

/// `artists`: the alphabetic index.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ArtistIndexes {
    /// Buckets.
    pub index: Vec<IndexBucket>,
}

/// One index bucket.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct IndexBucket {
    /// Bucket label.
    pub name: String,
    /// Artists.
    pub artist: Vec<Artist>,
}

/// One artist.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Artist {
    /// Artist id.
    pub id: String,
    /// Name.
    #[serde(default)]
    pub name: Option<String>,
    /// Album count.
    #[serde(default)]
    pub album_count: Option<i64>,
    /// MusicBrainz artist id.
    #[serde(default)]
    pub music_brainz_id: Option<String>,
    /// Cover-art id.
    #[serde(default)]
    pub cover_art: Option<String>,
}

/// One album (`songs` filled by `getAlbum`).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Album {
    /// Album id.
    pub id: String,
    /// Name (`getAlbumList2`, `getAlbum`).
    #[serde(default)]
    pub name: Option<String>,
    /// Title (older servers).
    #[serde(default)]
    pub title: Option<String>,
    /// Album artist.
    #[serde(default)]
    pub artist: Option<String>,
    /// Album artist id.
    #[serde(default)]
    pub artist_id: Option<String>,
    /// Release year.
    #[serde(default)]
    pub year: Option<i64>,
    /// Genre.
    #[serde(default)]
    pub genre: Option<String>,
    /// Track count.
    #[serde(default)]
    pub song_count: Option<i64>,
    /// MusicBrainz release id.
    #[serde(default)]
    pub music_brainz_id: Option<String>,
    /// Cover-art id.
    #[serde(default)]
    pub cover_art: Option<String>,
    /// Tracks.
    #[serde(default)]
    pub song: Vec<Song>,
}

impl Album {
    /// The display name: `name`, else `title`, else "Unknown".
    pub fn display_name(&self) -> &str {
        self.name
            .as_deref()
            .or(self.title.as_deref())
            .unwrap_or("Unknown")
    }
}

/// One song.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Song {
    /// Song id.
    pub id: String,
    /// Title.
    #[serde(default)]
    pub title: Option<String>,
    /// Album title.
    #[serde(default)]
    pub album: Option<String>,
    /// Album id.
    #[serde(default)]
    pub album_id: Option<String>,
    /// Artist name.
    #[serde(default)]
    pub artist: Option<String>,
    /// Artist id.
    #[serde(default)]
    pub artist_id: Option<String>,
    /// Track number.
    #[serde(default)]
    pub track: Option<i64>,
    /// Disc number.
    #[serde(default)]
    pub disc_number: Option<i64>,
    /// Length in seconds.
    #[serde(default)]
    pub duration: Option<i64>,
    /// Release year.
    #[serde(default)]
    pub year: Option<i64>,
    /// MusicBrainz recording id.
    #[serde(default)]
    pub music_brainz_id: Option<String>,
    /// Cover-art id.
    #[serde(default)]
    pub cover_art: Option<String>,
}

/// `search3` and `starred2`: three buckets.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Buckets {
    /// Artists.
    pub artist: Vec<Artist>,
    /// Albums.
    pub album: Vec<Album>,
    /// Songs.
    pub song: Vec<Song>,
}

/// `genres`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Genres {
    /// Genres.
    pub genre: Vec<Genre>,
}

/// One genre: the name rides `value` (older servers: `name`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Genre {
    /// Genre name.
    pub value: Option<String>,
    /// Older spelling.
    pub name: Option<String>,
}

/// A song list (`songsByGenre`, `topSongs`, `randomSongs`,
/// `similarSongs2`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Songs {
    /// Songs.
    pub song: Vec<Song>,
}

/// `playlists`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Playlists {
    /// Playlists.
    pub playlist: Vec<Playlist>,
}

/// One playlist (`entry` filled by `getPlaylist`).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Playlist {
    /// Playlist id.
    pub id: String,
    /// Name.
    #[serde(default)]
    pub name: Option<String>,
    /// Track count.
    #[serde(default)]
    pub song_count: Option<i64>,
    /// Total length in seconds.
    #[serde(default)]
    pub duration: Option<i64>,
    /// Cover-art id.
    #[serde(default)]
    pub cover_art: Option<String>,
    /// Entries.
    #[serde(default)]
    pub entry: Vec<Song>,
}

/// `artistInfo2` / `albumInfo`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Info {
    /// Artist biography.
    pub biography: Option<String>,
    /// Album notes.
    pub notes: Option<String>,
    /// MusicBrainz id.
    pub music_brainz_id: Option<String>,
    /// Small image URL.
    pub small_image_url: Option<String>,
    /// Medium image URL.
    pub medium_image_url: Option<String>,
    /// Large image URL.
    pub large_image_url: Option<String>,
    /// Similar artists.
    pub similar_artist: Vec<Artist>,
}

impl Info {
    /// The largest image offered.
    pub fn best_image(&self) -> String {
        [
            self.large_image_url.as_deref(),
            self.medium_image_url.as_deref(),
            self.small_image_url.as_deref(),
        ]
        .into_iter()
        .flatten()
        .find(|url| !url.is_empty())
        .unwrap_or("")
        .to_owned()
    }
}

/// `lyricsList`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LyricsList {
    /// Candidates, best first.
    pub structured_lyrics: Vec<StructuredLyrics>,
}

/// One structured lyrics candidate.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct StructuredLyrics {
    /// True when lines carry timings.
    pub synced: bool,
    /// Lines.
    pub line: Vec<StructuredLine>,
}

/// One structured line; `start` is milliseconds.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct StructuredLine {
    /// Line text.
    pub value: Option<String>,
    /// Start offset in milliseconds.
    pub start: Option<i64>,
}

/// `lyrics` (classic artist/title lookup).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ClassicLyrics {
    /// Full text.
    pub value: Option<String>,
}

/// `nowPlaying`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct NowPlaying {
    /// Entries.
    pub entry: Vec<NowPlayingEntry>,
}

/// One now-playing entry.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct NowPlayingEntry {
    /// Song id.
    pub id: Option<String>,
    /// Listening user.
    pub username: Option<String>,
    /// Player label.
    pub player_name: Option<String>,
    /// Minutes since the play started.
    pub minutes_ago: Option<i64>,
    /// Title.
    pub title: Option<String>,
    /// Artist.
    pub artist: Option<String>,
    /// Album.
    pub album: Option<String>,
    /// Album id.
    pub album_id: Option<String>,
    /// Cover-art id.
    pub cover_art: Option<String>,
    /// Length in seconds.
    pub duration: Option<i64>,
}

/// Accept a JSON string or number as a string id.
fn string_or_number<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Id {
        Text(String),
        Number(i64),
    }
    Ok(match Id::deserialize(deserializer)? {
        Id::Text(text) => text,
        Id::Number(number) => number.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A song without an id must not decode into an unplayable track, and
    /// numeric folder ids still read as ids.
    #[test]
    fn identity_is_required_and_folder_ids_may_be_numbers() {
        let missing = serde_json::from_str::<Envelope>(
            r#"{"subsonic-response":{"status":"ok","randomSongs":{"song":[{"title":"Ghost"}]}}}"#,
        );
        assert!(missing.is_err());
        let folders = serde_json::from_str::<Envelope>(
            r#"{"subsonic-response":{"status":"ok","musicFolders":{"musicFolder":[{"id":1,"name":"Music"}]}}}"#,
        )
        .expect("numeric folder ids decode");
        let folders = folders.response.music_folders.expect("folders");
        assert_eq!(folders.music_folder[0].id, "1");
    }
}
