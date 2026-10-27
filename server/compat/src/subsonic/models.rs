//! Subsonic response objects, ported from v2's Subsonic models.
//!
//! Field names are exact Subsonic/OpenSubsonic camelCase and serialize in
//! declaration order (v2 `msgspec.to_builtins` struct order). Optional
//! fields that hold nothing are left off the wire.

#![allow(non_snake_case)]

use serde::Serialize;
use serde::ser::{SerializeMap, Serializer};

/// True when there is no `averageRating` to send.
fn no_rating(average: &Option<f64>) -> bool {
    !matches!(average, Some(mean) if mean.is_finite())
}

/// `averageRating`, rounded to two places. A whole mean is sent as an
/// integer, as Navidrome 0.62.0 sends it (recorded
/// `navidrome-0.62.0-ratings`). The rounding is ours: that recording
/// holds whole means only.
fn average_rating<S: Serializer>(average: &Option<f64>, serializer: S) -> Result<S::Ok, S::Error> {
    match average {
        Some(mean) => {
            let mean = (mean * 100.0).round() / 100.0;
            if mean.fract() == 0.0 {
                serializer.serialize_i64(mean as i64)
            } else {
                serializer.serialize_f64(mean)
            }
        }
        None => serializer.serialize_none(),
    }
}

/// A release date, or an empty object when it is unknown.
fn date_or_empty<S: Serializer>(
    date: &Option<SItemDate>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match date {
        Some(date) => date.serialize(serializer),
        None => serializer.serialize_map(Some(0))?.end(),
    }
}

/// An object with one key, around a list or a value: `{"<key>": ...}`.
#[derive(Debug, Clone)]
pub struct Wrap<T>(pub &'static str, pub T);

impl<T: Serialize> Serialize for Wrap<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(1))?;
        map.serialize_entry(self.0, &self.1)?;
        map.end()
    }
}

/// ID3 artist (detail contexts).
#[derive(Debug, Clone, Default, Serialize)]
pub struct SArtistID3 {
    /// Prefixed artist id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Cover art id (same as id).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverArt: Option<String>,
    /// Album count.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub albumCount: Option<i64>,
    /// The caller's rating, sent only when set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub userRating: Option<i64>,
    /// Every user's mean rating, sent only when anyone rated.
    #[serde(skip_serializing_if = "no_rating", serialize_with = "average_rating")]
    pub averageRating: Option<f64>,
    /// Open picture link (artist lists and details only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artistImageUrl: Option<String>,
    /// Starred timestamp ISO, if starred.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub starred: Option<String>,
    /// MusicBrainz artist id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub musicBrainzId: Option<String>,
    /// Sort name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sortName: Option<String>,
    /// Credit roles (artist lists and details only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub roles: Option<Vec<String>>,
    /// Populated by getArtist only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub album: Option<Vec<SAlbumID3>>,
}

/// File-structure artist (getIndexes / search2 / getStarred).
#[derive(Debug, Clone, Default, Serialize)]
pub struct SArtist {
    /// Prefixed artist id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Starred timestamp ISO, if starred.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub starred: Option<String>,
    /// Cover art id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverArt: Option<String>,
    /// Open picture link.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artistImageUrl: Option<String>,
}

/// ID3 album.
///
/// The keys after `discTitles`, the ratings and `releaseDate` aside, are
/// ones Navidrome 0.62.0 sends on every album and the app holds nothing
/// for: no label, mood, explicit flag or version is read from the tags.
/// Each carries what Navidrome sends for "none" (checked against its
/// recorded `getAlbum` answer). `userRating` is sent on every album, 0
/// when the caller has not rated it.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SAlbumID3 {
    /// Prefixed album id.
    pub id: String,
    /// Album title.
    pub name: String,
    /// Album artist name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    /// Prefixed album-artist id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artistId: Option<String>,
    /// Cover art id (same as id).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverArt: Option<String>,
    /// Track count.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub songCount: Option<i64>,
    /// Total duration seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<i64>,
    /// Play count.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub playCount: Option<i64>,
    /// Created timestamp ISO.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    /// Starred timestamp ISO.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub starred: Option<String>,
    /// Release year.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<i64>,
    /// Genre name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub genre: Option<String>,
    /// Compilation flag.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub isCompilation: Option<bool>,
    /// MusicBrainz release-group id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub musicBrainzId: Option<String>,
    /// Last-played timestamp ISO.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub played: Option<String>,
    /// Genre list, empty when the album has no genre.
    pub genres: Vec<SItemGenre>,
    /// Contributing artists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artists: Option<Vec<SArtistID3>>,
    /// Display artist.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub displayArtist: Option<String>,
    /// Release types.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub releaseTypes: Option<Vec<String>>,
    /// Sort name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sortName: Option<String>,
    /// Original release date, an empty object when unknown.
    #[serde(serialize_with = "date_or_empty")]
    pub originalReleaseDate: Option<SItemDate>,
    /// Per-disc titles, empty when no disc has one.
    pub discTitles: Vec<SDiscTitle>,
    /// The caller's rating, 0 when none.
    pub userRating: i64,
    /// Every user's mean rating, sent only when anyone rated.
    #[serde(skip_serializing_if = "no_rating", serialize_with = "average_rating")]
    pub averageRating: Option<f64>,
    /// This edition's release date, an empty object when unknown.
    #[serde(serialize_with = "date_or_empty")]
    pub releaseDate: Option<SItemDate>,
    /// The keys for album tags the app does not read.
    #[serde(flatten)]
    pub unreadTags: UnreadAlbumTags,
    /// Populated by getAlbum only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub song: Option<Vec<SChild>>,
}

/// Keys Navidrome 0.62.0 sends on every album for tags the app does not
/// read: label, mood, explicit flag and version, each with what Navidrome
/// sends for "none".
#[derive(Debug, Clone, Default, Serialize)]
pub struct UnreadAlbumTags {
    recordLabels: [String; 0],
    moods: [String; 0],
    explicitStatus: &'static str,
    version: &'static str,
}

/// Genre item.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SItemGenre {
    /// Genre name.
    pub name: String,
}

/// Partial release date.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SItemDate {
    /// Year (required).
    pub year: i64,
    /// Month 1-12.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub month: Option<i64>,
    /// Day 1-31.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub day: Option<i64>,
}

/// Per-disc title.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SDiscTitle {
    /// Disc number.
    pub disc: i64,
    /// Disc title.
    pub title: String,
    /// Cover art id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverArt: Option<String>,
}

/// Replay gain block (always present on songs, fields nullable).
#[derive(Debug, Clone, Default, Serialize)]
pub struct SReplayGain {
    /// Track gain dB.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trackGain: Option<f64>,
    /// Album gain dB.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub albumGain: Option<f64>,
    /// Track peak.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trackPeak: Option<f64>,
    /// Album peak.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub albumPeak: Option<f64>,
}

/// Song (or file-structure directory) entry.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SChild {
    /// Prefixed id.
    pub id: String,
    /// True for directories.
    pub isDir: bool,
    /// Title.
    pub title: String,
    /// Parent id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Album title.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    /// Artist name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    /// Track number.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track: Option<i64>,
    /// Year.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<i64>,
    /// Genre name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub genre: Option<String>,
    /// Cover art id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverArt: Option<String>,
    /// File size bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<i64>,
    /// MIME type.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contentType: Option<String>,
    /// File suffix.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suffix: Option<String>,
    /// Transcode hint MIME (present iff transcoding on + ffmpeg).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcodedContentType: Option<String>,
    /// Transcode hint suffix.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcodedSuffix: Option<String>,
    /// Duration seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<i64>,
    /// Bitrate kbps.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bitRate: Option<i64>,
    /// The file's path under its library folder, never the host's path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Disc number.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discNumber: Option<i64>,
    /// Created timestamp ISO.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    /// Starred timestamp ISO.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub starred: Option<String>,
    /// Prefixed album id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub albumId: Option<String>,
    /// Prefixed artist id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artistId: Option<String>,
    /// Always "music" on songs.
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub type_: Option<String>,
    /// The caller's rating, sent only when set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub userRating: Option<i64>,
    /// Every user's mean rating, sent only when anyone rated.
    #[serde(skip_serializing_if = "no_rating", serialize_with = "average_rating")]
    pub averageRating: Option<f64>,
    /// Play count.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub playCount: Option<i64>,
    /// Always "song" on songs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mediaType: Option<String>,
    /// Bit depth.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bitDepth: Option<i64>,
    /// Sample rate Hz.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub samplingRate: Option<i64>,
    /// Channel count (v2 defaults to 2 when unknown).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channelCount: Option<i64>,
    /// MusicBrainz recording id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub musicBrainzId: Option<String>,
    /// Last-played timestamp ISO.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub played: Option<String>,
    /// Sort name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sortName: Option<String>,
    /// Genre list (split on ";").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub genres: Option<Vec<SItemGenre>>,
    /// Track artists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artists: Option<Vec<SArtistID3>>,
    /// Display artist.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub displayArtist: Option<String>,
    /// Album artists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub albumArtists: Option<Vec<SArtistID3>>,
    /// Display album artist.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub displayAlbumArtist: Option<String>,
    /// Replay gain (always present on songs).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replayGain: Option<SReplayGain>,
    /// Album title again, on an album's folder entry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Track count, on an album's folder entry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub songCount: Option<i64>,
    /// Set on songs and albums: the keys for tags the app does not read.
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub unreadTags: Option<UnreadTags>,
}

/// Keys Navidrome 0.62.0 sends on every song and album entry for tags the
/// app does not read: tempo, comment, ISRC, mood, credits beyond the
/// artists, composer, explicit flag and grouping. Each carries what
/// Navidrome sends for "none" (checked against its recorded `getSong` and
/// `getAlbumList` answers).
#[derive(Debug, Clone, Default, Serialize)]
pub struct UnreadTags {
    bpm: i64,
    comment: &'static str,
    isrc: [String; 0],
    moods: [String; 0],
    contributors: [String; 0],
    displayComposer: &'static str,
    explicitStatus: &'static str,
    groupings: [String; 0],
}

/// getNowPlaying entry: a song child plus session attribution.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SNowPlayingEntry {
    /// The song child fields.
    #[serde(flatten)]
    pub child: SChild,
    /// Listener username.
    pub username: String,
    /// Minutes since the entry updated.
    pub minutesAgo: i64,
    /// Player index.
    pub playerId: i64,
    /// Player name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub playerName: Option<String>,
}

/// Genre with counts (counts are ints on the wire, never bools).
#[derive(Debug, Clone, Default, Serialize)]
pub struct SGenre {
    /// Genre name (XML text).
    pub value: String,
    /// Song count.
    pub songCount: i64,
    /// Album count.
    pub albumCount: i64,
}

/// Playlist (entry list only on getPlaylist/createPlaylist detail).
#[derive(Debug, Clone, Default, Serialize)]
pub struct SPlaylist {
    /// Prefixed playlist id.
    pub id: String,
    /// Playlist name.
    pub name: String,
    /// Comment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    /// Owner username.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// Public flag.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public: Option<bool>,
    /// True when the caller may not change the playlist (OpenSubsonic).
    pub readonly: bool,
    /// Streamable-entry count (#181: matches served entries).
    pub songCount: i64,
    /// Total duration seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<i64>,
    /// Created timestamp ISO.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    /// Changed timestamp ISO.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed: Option<String>,
    /// Cover art id: the playlist's own id, on every playlist.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverArt: Option<String>,
    /// Entries (detail only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry: Option<Vec<SChild>>,
}

/// One A-Z bucket of ID3 artists.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SIndexID3 {
    /// Bucket letter.
    pub name: String,
    /// Artists in the bucket.
    pub artist: Vec<SArtistID3>,
}

/// getArtists payload.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SArtistsID3 {
    /// Library revision, as getIndexes sends it: a counter, not a time.
    pub lastModified: i64,
    /// Articles stripped for bucketing.
    pub ignoredArticles: String,
    /// Buckets.
    pub index: Vec<SIndexID3>,
}

/// One A-Z bucket of file-structure artists.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SIndex {
    /// Bucket letter.
    pub name: String,
    /// Artists in the bucket.
    pub artist: Vec<SArtist>,
}

/// getIndexes payload.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SIndexes {
    /// Library revision.
    pub lastModified: i64,
    /// Articles stripped for bucketing.
    pub ignoredArticles: String,
    /// Buckets (empty when ifModifiedSince is fresh).
    pub index: Vec<SIndex>,
}

/// Music folder (exactly one: id 1).
#[derive(Debug, Clone, Default, Serialize)]
pub struct SMusicFolder {
    /// Folder id (always 1).
    pub id: i64,
    /// Folder name (advertised server name).
    pub name: String,
}

/// getLicense payload (static valid:true).
#[derive(Debug, Clone, Default, Serialize)]
pub struct SLicense {
    /// Always true.
    pub valid: bool,
}

/// One advertised OpenSubsonic extension.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SOpenSubsonicExtension {
    /// Extension name.
    pub name: String,
    /// Supported versions.
    pub versions: Vec<i64>,
}

/// getPlayQueue payload (current is a song id).
#[derive(Debug, Clone, Default, Serialize)]
pub struct SPlayQueue {
    /// Owner username.
    pub username: String,
    /// Changed timestamp ISO.
    pub changed: String,
    /// Client that last changed it.
    pub changedBy: String,
    /// Current song id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current: Option<String>,
    /// Position ms.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<i64>,
    /// Entries.
    pub entry: Vec<SChild>,
}

/// getPlayQueueByIndex payload (extension indexBasedQueue:1).
#[derive(Debug, Clone, Default, Serialize)]
pub struct SPlayQueueByIndex {
    /// Owner username.
    pub username: String,
    /// Changed timestamp ISO.
    pub changed: String,
    /// Client that last changed it.
    pub changedBy: String,
    /// Current index.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub currentIndex: Option<i64>,
    /// Position ms.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<i64>,
    /// Entries.
    pub entry: Vec<SChild>,
}

/// Bookmark (pinned required: position, username, created, changed, entry).
#[derive(Debug, Clone, Default, Serialize)]
pub struct SBookmark {
    /// Position ms.
    pub position: i64,
    /// Owner username.
    pub username: String,
    /// Created timestamp ISO.
    pub created: String,
    /// Changed timestamp ISO.
    pub changed: String,
    /// The bookmarked song.
    pub entry: SChild,
    /// Comment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

/// Legacy flat lyrics.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SLyrics {
    /// Artist name.
    pub artist: String,
    /// Track title.
    pub title: String,
    /// Full text (empty on miss).
    pub value: String,
}

/// One structured lyric line (pinned required: value on every line).
#[derive(Debug, Clone, Default, Serialize)]
pub struct SLyricsLine {
    /// Line text.
    pub value: String,
    /// Start ms (synced only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start: Option<i64>,
}

/// Structured lyrics (extension songLyrics:1; pinned: lang, synced, line).
#[derive(Debug, Clone, Default, Serialize)]
pub struct SStructuredLyrics {
    /// Language code.
    pub lang: String,
    /// Whether lines carry timestamps.
    pub synced: bool,
    /// Lines.
    pub line: Vec<SLyricsLine>,
    /// Display artist.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub displayArtist: Option<String>,
    /// Display title.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub displayTitle: Option<String>,
    /// Offset seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<f64>,
}

/// Artist info (MBIDs projected only; cover URLs credential-free).
#[derive(Debug, Clone, Default, Serialize)]
pub struct SArtistInfo {
    /// Biography.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub biography: Option<String>,
    /// MusicBrainz id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub musicBrainzId: Option<String>,
    /// Last.fm URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lastFmUrl: Option<String>,
    /// Small cover URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub smallImageUrl: Option<String>,
    /// Medium cover URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mediumImageUrl: Option<String>,
    /// Large cover URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub largeImageUrl: Option<String>,
    /// Similar artists.
    pub similarArtist: Vec<SArtistID3>,
}

/// Album info.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SAlbumInfo {
    /// Notes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// MusicBrainz id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub musicBrainzId: Option<String>,
    /// Last.fm URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lastFmUrl: Option<String>,
    /// Small cover URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub smallImageUrl: Option<String>,
    /// Medium cover URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mediumImageUrl: Option<String>,
    /// Large cover URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub largeImageUrl: Option<String>,
}

/// Scan status (pinned required: scanning).
#[derive(Debug, Clone, Default, Serialize)]
pub struct SScanStatus {
    /// Whether a scan is running.
    pub scanning: bool,
    /// Files looked at so far during a scan; playable tracks at rest.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<i64>,
    /// Folders holding at least one playable track.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folderCount: Option<i64>,
    /// End of the last finished scan, ISO.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lastScan: Option<String>,
    /// Kind of the last finished scan.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scanType: Option<String>,
    /// Length of the last finished scan in nanoseconds: Navidrome 0.62.0
    /// sends `int64(status.ElapsedTime)`, a Go `time.Duration`
    /// (`server/subsonic/library_scanning.go`, read 2026-10-14).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsedTime: Option<i64>,
}

/// Stream details inside a transcode decision.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SStreamDetails {
    /// Protocol.
    pub protocol: String,
    /// Container.
    pub container: String,
    /// Codec.
    pub codec: String,
    /// Audio channels.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audioChannels: Option<i64>,
    /// Audio bitrate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audioBitrate: Option<i64>,
    /// Audio profile.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audioProfile: Option<String>,
    /// Sample rate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audioSamplerate: Option<i64>,
    /// Bit depth.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audioBitdepth: Option<i64>,
}

/// getTranscodeDecision payload (signed transcodeParams inside).
#[derive(Debug, Clone, Default, Serialize)]
pub struct STranscodeDecision {
    /// Direct play possible.
    pub canDirectPlay: bool,
    /// Transcode possible.
    pub canTranscode: bool,
    /// Why a transcode is needed.
    pub transcodeReason: Vec<String>,
    /// Why nothing is possible.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub errorReason: Option<String>,
    /// Signed params for getTranscodeStream.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcodeParams: Option<String>,
    /// Source stream facts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sourceStream: Option<SStreamDetails>,
    /// Target stream facts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcodeStream: Option<SStreamDetails>,
}

/// getUser payload (always the caller; the username param is ignored).
/// Roles: admin implies admin+settings; everyone gets playlist, cover and
/// stream roles; download follows the admin's download setting.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SUser {
    /// Caller username.
    pub username: String,
    /// Always true.
    pub scrobblingEnabled: bool,
    /// Admin role.
    pub adminRole: bool,
    /// Settings role (admin only).
    pub settingsRole: bool,
    /// True for a user the admin's download setting admits.
    pub downloadRole: bool,
    /// Always false.
    pub uploadRole: bool,
    /// Always true.
    pub playlistRole: bool,
    /// Always true.
    pub coverArtRole: bool,
    /// Always false.
    pub commentRole: bool,
    /// Always false.
    pub podcastRole: bool,
    /// Always true.
    pub streamRole: bool,
    /// Always false.
    pub jukeboxRole: bool,
    /// Always false.
    pub shareRole: bool,
    /// Always false.
    pub videoConversionRole: bool,
    /// Max bitrate kbps from settings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maxBitRate: Option<i64>,
    /// Visible folders (always [1]).
    pub folder: Vec<i64>,
}

impl SUser {
    /// Caller user object: admin flips admin+settings roles.
    pub fn caller(username: String, is_admin: bool, may_download: bool, max_bit_rate: i64) -> Self {
        Self {
            username,
            scrobblingEnabled: true,
            adminRole: is_admin,
            settingsRole: is_admin,
            downloadRole: may_download,
            uploadRole: false,
            playlistRole: true,
            coverArtRole: true,
            commentRole: false,
            podcastRole: false,
            streamRole: true,
            jukeboxRole: false,
            shareRole: false,
            videoConversionRole: false,
            maxBitRate: Some(max_bit_rate),
            folder: vec![1],
        }
    }
}

/// Search and starred payload: ID3 (`SArtistID3`, `SAlbumID3`) or
/// file-structure (`SArtist`, album `SChild`) artists and albums, then
/// songs.
#[derive(Debug, Clone, Serialize)]
pub struct SSearchResult<A, B> {
    /// Artists.
    pub artist: Vec<A>,
    /// Albums.
    pub album: Vec<B>,
    /// Songs.
    pub song: Vec<SChild>,
}

/// getMusicDirectory payload: the root folder, an artist or an album.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SDirectory {
    /// Prefixed id (`1` for the root).
    pub id: String,
    /// The album's artist id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Folder name.
    pub name: String,
    /// The album's cover art id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverArt: Option<String>,
    /// The artist's album count.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub albumCount: Option<usize>,
    /// The album's track count.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub songCount: Option<usize>,
    /// Entries.
    pub child: Vec<SChild>,
}
