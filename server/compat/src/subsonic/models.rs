//! Subsonic response objects, ported from v2's Subsonic models.
//!
//! Field names are exact Subsonic/OpenSubsonic camelCase and render in
//! declaration order (v2 `msgspec.to_builtins` struct order). Optional
//! fields render as null and are stripped from the wire.

#![allow(non_snake_case)]

use super::value::{IntoVal, Val, obj};

/// Render into an ordered value.
pub trait Render {
    /// Convert to a wire value.
    fn render(&self) -> Val;
}

/// ID3 artist (detail contexts).
#[derive(Debug, Clone, Default)]
pub struct SArtistID3 {
    /// Prefixed artist id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Cover art id (same as id).
    pub coverArt: Option<String>,
    /// Album count.
    pub albumCount: Option<i64>,
    /// Open picture link (artist lists and details only).
    pub artistImageUrl: Option<String>,
    /// Starred timestamp ISO, if starred.
    pub starred: Option<String>,
    /// MusicBrainz artist id.
    pub musicBrainzId: Option<String>,
    /// Sort name.
    pub sortName: Option<String>,
    /// Credit roles (artist lists and details only).
    pub roles: Option<Vec<String>>,
    /// Populated by getArtist only.
    pub album: Option<Vec<SAlbumID3>>,
}

impl Render for SArtistID3 {
    fn render(&self) -> Val {
        obj(vec![
            ("id", self.id.as_str().into_val()),
            ("name", self.name.as_str().into_val()),
            ("coverArt", self.coverArt.clone().into_val()),
            ("albumCount", self.albumCount.into_val()),
            ("artistImageUrl", self.artistImageUrl.clone().into_val()),
            ("starred", self.starred.clone().into_val()),
            ("musicBrainzId", self.musicBrainzId.clone().into_val()),
            ("sortName", self.sortName.clone().into_val()),
            (
                "roles",
                self.roles
                    .as_ref()
                    .map(|roles| Val::List(roles.iter().map(|r| r.as_str().into_val()).collect()))
                    .into_val(),
            ),
            (
                "album",
                self.album
                    .as_ref()
                    .map(|albums| Val::List(albums.iter().map(Render::render).collect()))
                    .into_val(),
            ),
        ])
    }
}

/// File-structure artist (getIndexes / search2 / getStarred).
#[derive(Debug, Clone, Default)]
pub struct SArtist {
    /// Prefixed artist id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Starred timestamp ISO, if starred.
    pub starred: Option<String>,
    /// Cover art id.
    pub coverArt: Option<String>,
    /// Open picture link.
    pub artistImageUrl: Option<String>,
}

impl Render for SArtist {
    fn render(&self) -> Val {
        obj(vec![
            ("id", self.id.as_str().into_val()),
            ("name", self.name.as_str().into_val()),
            ("starred", self.starred.clone().into_val()),
            ("coverArt", self.coverArt.clone().into_val()),
            ("artistImageUrl", self.artistImageUrl.clone().into_val()),
        ])
    }
}

/// ID3 album.
///
/// The keys after `discTitles` are ones Navidrome 0.62.0 sends on every
/// album and the app holds nothing for: no rating is stored, and no
/// edition date, label, mood, explicit flag or version is read from the
/// tags. Each carries what Navidrome sends for "none" (checked against its
/// recorded `getAlbum` answer).
#[derive(Debug, Clone, Default)]
pub struct SAlbumID3 {
    /// Prefixed album id.
    pub id: String,
    /// Album title.
    pub name: String,
    /// Album artist name.
    pub artist: Option<String>,
    /// Prefixed album-artist id.
    pub artistId: Option<String>,
    /// Cover art id (same as id).
    pub coverArt: Option<String>,
    /// Track count.
    pub songCount: Option<i64>,
    /// Total duration seconds.
    pub duration: Option<i64>,
    /// Play count.
    pub playCount: Option<i64>,
    /// Created timestamp ISO.
    pub created: Option<String>,
    /// Starred timestamp ISO.
    pub starred: Option<String>,
    /// Release year.
    pub year: Option<i64>,
    /// Genre name.
    pub genre: Option<String>,
    /// Compilation flag.
    pub isCompilation: Option<bool>,
    /// MusicBrainz release-group id.
    pub musicBrainzId: Option<String>,
    /// Last-played timestamp ISO.
    pub played: Option<String>,
    /// Genre list, empty when the album has no genre.
    pub genres: Vec<SItemGenre>,
    /// Contributing artists.
    pub artists: Option<Vec<SArtistID3>>,
    /// Display artist.
    pub displayArtist: Option<String>,
    /// Release types.
    pub releaseTypes: Option<Vec<String>>,
    /// Sort name.
    pub sortName: Option<String>,
    /// Original release date, an empty object when unknown.
    pub originalReleaseDate: Option<SItemDate>,
    /// Per-disc titles, empty when no disc has one.
    pub discTitles: Vec<SDiscTitle>,
    /// Populated by getAlbum only.
    pub song: Option<Vec<SChild>>,
}

impl Render for SAlbumID3 {
    fn render(&self) -> Val {
        obj(vec![
            ("id", self.id.as_str().into_val()),
            ("name", self.name.as_str().into_val()),
            ("artist", self.artist.clone().into_val()),
            ("artistId", self.artistId.clone().into_val()),
            ("coverArt", self.coverArt.clone().into_val()),
            ("songCount", self.songCount.into_val()),
            ("duration", self.duration.into_val()),
            ("playCount", self.playCount.into_val()),
            ("created", self.created.clone().into_val()),
            ("starred", self.starred.clone().into_val()),
            ("year", self.year.into_val()),
            ("genre", self.genre.clone().into_val()),
            ("isCompilation", self.isCompilation.into_val()),
            ("musicBrainzId", self.musicBrainzId.clone().into_val()),
            ("played", self.played.clone().into_val()),
            (
                "genres",
                Val::List(self.genres.iter().map(Render::render).collect()),
            ),
            (
                "artists",
                self.artists
                    .as_ref()
                    .map(|artists| Val::List(artists.iter().map(Render::render).collect()))
                    .into_val(),
            ),
            ("displayArtist", self.displayArtist.clone().into_val()),
            (
                "releaseTypes",
                self.releaseTypes
                    .as_ref()
                    .map(|types| Val::List(types.iter().map(|t| t.as_str().into_val()).collect()))
                    .into_val(),
            ),
            ("sortName", self.sortName.clone().into_val()),
            (
                "originalReleaseDate",
                self.originalReleaseDate
                    .as_ref()
                    .map_or_else(|| obj(Vec::new()), Render::render),
            ),
            (
                "discTitles",
                Val::List(self.discTitles.iter().map(Render::render).collect()),
            ),
            ("userRating", Val::Int(0)),
            ("releaseDate", obj(Vec::new())),
            ("recordLabels", Val::List(Vec::new())),
            ("moods", Val::List(Vec::new())),
            ("explicitStatus", "".into_val()),
            ("version", "".into_val()),
            (
                "song",
                self.song
                    .as_ref()
                    .map(|songs| Val::List(songs.iter().map(Render::render).collect()))
                    .into_val(),
            ),
        ])
    }
}

/// Genre item.
#[derive(Debug, Clone, Default)]
pub struct SItemGenre {
    /// Genre name.
    pub name: String,
}

impl Render for SItemGenre {
    fn render(&self) -> Val {
        obj(vec![("name", self.name.as_str().into_val())])
    }
}

/// Partial release date.
#[derive(Debug, Clone, Default)]
pub struct SItemDate {
    /// Year (required).
    pub year: i64,
    /// Month 1-12.
    pub month: Option<i64>,
    /// Day 1-31.
    pub day: Option<i64>,
}

impl Render for SItemDate {
    fn render(&self) -> Val {
        obj(vec![
            ("year", self.year.into_val()),
            ("month", self.month.into_val()),
            ("day", self.day.into_val()),
        ])
    }
}

/// Per-disc title.
#[derive(Debug, Clone, Default)]
pub struct SDiscTitle {
    /// Disc number.
    pub disc: i64,
    /// Disc title.
    pub title: String,
    /// Cover art id.
    pub coverArt: Option<String>,
}

impl Render for SDiscTitle {
    fn render(&self) -> Val {
        obj(vec![
            ("disc", self.disc.into_val()),
            ("title", self.title.as_str().into_val()),
            ("coverArt", self.coverArt.clone().into_val()),
        ])
    }
}

/// Replay gain block (always present on songs, fields nullable).
#[derive(Debug, Clone, Default)]
pub struct SReplayGain {
    /// Track gain dB.
    pub trackGain: Option<f64>,
    /// Album gain dB.
    pub albumGain: Option<f64>,
    /// Track peak.
    pub trackPeak: Option<f64>,
    /// Album peak.
    pub albumPeak: Option<f64>,
}

impl Render for SReplayGain {
    fn render(&self) -> Val {
        obj(vec![
            ("trackGain", self.trackGain.into_val()),
            ("albumGain", self.albumGain.into_val()),
            ("trackPeak", self.trackPeak.into_val()),
            ("albumPeak", self.albumPeak.into_val()),
        ])
    }
}

/// Song (or file-structure directory) entry.
#[derive(Debug, Clone, Default)]
pub struct SChild {
    /// Prefixed id.
    pub id: String,
    /// True for directories.
    pub isDir: bool,
    /// Title.
    pub title: String,
    /// Parent id.
    pub parent: Option<String>,
    /// Album title.
    pub album: Option<String>,
    /// Artist name.
    pub artist: Option<String>,
    /// Track number.
    pub track: Option<i64>,
    /// Year.
    pub year: Option<i64>,
    /// Genre name.
    pub genre: Option<String>,
    /// Cover art id.
    pub coverArt: Option<String>,
    /// File size bytes.
    pub size: Option<i64>,
    /// MIME type.
    pub contentType: Option<String>,
    /// File suffix.
    pub suffix: Option<String>,
    /// Transcode hint MIME (present iff transcoding on + ffmpeg).
    pub transcodedContentType: Option<String>,
    /// Transcode hint suffix.
    pub transcodedSuffix: Option<String>,
    /// Duration seconds.
    pub duration: Option<i64>,
    /// Bitrate kbps.
    pub bitRate: Option<i64>,
    /// The file's path under its library folder, never the host's path.
    pub path: Option<String>,
    /// Disc number.
    pub discNumber: Option<i64>,
    /// Created timestamp ISO.
    pub created: Option<String>,
    /// Starred timestamp ISO.
    pub starred: Option<String>,
    /// Prefixed album id.
    pub albumId: Option<String>,
    /// Prefixed artist id.
    pub artistId: Option<String>,
    /// Always "music" on songs.
    pub type_: Option<String>,
    /// Play count.
    pub playCount: Option<i64>,
    /// Always "song" on songs.
    pub mediaType: Option<String>,
    /// Bit depth.
    pub bitDepth: Option<i64>,
    /// Sample rate Hz.
    pub samplingRate: Option<i64>,
    /// Channel count (v2 defaults to 2 when unknown).
    pub channelCount: Option<i64>,
    /// MusicBrainz recording id.
    pub musicBrainzId: Option<String>,
    /// Last-played timestamp ISO.
    pub played: Option<String>,
    /// Sort name.
    pub sortName: Option<String>,
    /// Genre list (split on ";").
    pub genres: Option<Vec<SItemGenre>>,
    /// Track artists.
    pub artists: Option<Vec<SArtistID3>>,
    /// Display artist.
    pub displayArtist: Option<String>,
    /// Album artists.
    pub albumArtists: Option<Vec<SArtistID3>>,
    /// Display album artist.
    pub displayAlbumArtist: Option<String>,
    /// Replay gain (always present on songs).
    pub replayGain: Option<SReplayGain>,
    /// Album title again, on an album's folder entry.
    pub name: Option<String>,
    /// Track count, on an album's folder entry.
    pub songCount: Option<i64>,
    /// True on songs and albums: send the keys for tags the app does not
    /// read (see [`unread_tags`]).
    pub unreadTags: bool,
}

/// Keys Navidrome 0.62.0 sends on every song and album entry for tags the
/// app does not read: tempo, comment, ISRC, mood, credits beyond the
/// artists, composer, explicit flag and grouping. Each carries what
/// Navidrome sends for "none" (checked against its recorded `getSong` and
/// `getAlbumList` answers).
fn unread_tags() -> Vec<(&'static str, Val)> {
    vec![
        ("bpm", Val::Int(0)),
        ("comment", "".into_val()),
        ("isrc", Val::List(Vec::new())),
        ("moods", Val::List(Vec::new())),
        ("contributors", Val::List(Vec::new())),
        ("displayComposer", "".into_val()),
        ("explicitStatus", "".into_val()),
        ("groupings", Val::List(Vec::new())),
    ]
}

impl Render for SChild {
    fn render(&self) -> Val {
        let mut entries = vec![
            ("id", self.id.as_str().into_val()),
            ("isDir", self.isDir.into_val()),
            ("title", self.title.as_str().into_val()),
            ("parent", self.parent.clone().into_val()),
            ("album", self.album.clone().into_val()),
            ("artist", self.artist.clone().into_val()),
            ("track", self.track.into_val()),
            ("year", self.year.into_val()),
            ("genre", self.genre.clone().into_val()),
            ("coverArt", self.coverArt.clone().into_val()),
            ("size", self.size.into_val()),
            ("contentType", self.contentType.clone().into_val()),
            ("suffix", self.suffix.clone().into_val()),
            (
                "transcodedContentType",
                self.transcodedContentType.clone().into_val(),
            ),
            ("transcodedSuffix", self.transcodedSuffix.clone().into_val()),
            ("duration", self.duration.into_val()),
            ("bitRate", self.bitRate.into_val()),
            ("path", self.path.clone().into_val()),
            ("discNumber", self.discNumber.into_val()),
            ("created", self.created.clone().into_val()),
            ("starred", self.starred.clone().into_val()),
            ("albumId", self.albumId.clone().into_val()),
            ("artistId", self.artistId.clone().into_val()),
            ("type", self.type_.clone().into_val()),
            ("playCount", self.playCount.into_val()),
            ("mediaType", self.mediaType.clone().into_val()),
            ("bitDepth", self.bitDepth.into_val()),
            ("samplingRate", self.samplingRate.into_val()),
            ("channelCount", self.channelCount.into_val()),
            ("musicBrainzId", self.musicBrainzId.clone().into_val()),
            ("played", self.played.clone().into_val()),
            ("sortName", self.sortName.clone().into_val()),
            (
                "genres",
                self.genres
                    .as_ref()
                    .map(|genres| Val::List(genres.iter().map(Render::render).collect()))
                    .into_val(),
            ),
            (
                "artists",
                self.artists
                    .as_ref()
                    .map(|artists| Val::List(artists.iter().map(Render::render).collect()))
                    .into_val(),
            ),
            ("displayArtist", self.displayArtist.clone().into_val()),
            (
                "albumArtists",
                self.albumArtists
                    .as_ref()
                    .map(|artists| Val::List(artists.iter().map(Render::render).collect()))
                    .into_val(),
            ),
            (
                "displayAlbumArtist",
                self.displayAlbumArtist.clone().into_val(),
            ),
            (
                "replayGain",
                self.replayGain.as_ref().map(Render::render).into_val(),
            ),
            ("name", self.name.clone().into_val()),
            ("songCount", self.songCount.into_val()),
        ];
        if self.unreadTags {
            entries.extend(unread_tags());
        }
        obj(entries)
    }
}

/// getNowPlaying entry: a song child plus session attribution.
#[derive(Debug, Clone, Default)]
pub struct SNowPlayingEntry {
    /// The song child fields.
    pub child: SChild,
    /// Listener username.
    pub username: String,
    /// Minutes since the entry updated.
    pub minutesAgo: i64,
    /// Player index.
    pub playerId: i64,
    /// Player name.
    pub playerName: Option<String>,
}

impl Render for SNowPlayingEntry {
    fn render(&self) -> Val {
        // Children always render as objects; a non-object starts from an
        // empty object rather than panicking (panics are denied here).
        let mut entries = match self.child.render() {
            Val::Obj(entries) => entries,
            _ => Vec::new(),
        };
        entries.push(("username".to_owned(), self.username.as_str().into_val()));
        entries.push(("minutesAgo".to_owned(), self.minutesAgo.into_val()));
        entries.push(("playerId".to_owned(), self.playerId.into_val()));
        entries.push(("playerName".to_owned(), self.playerName.clone().into_val()));
        Val::Obj(entries)
    }
}

/// Genre with counts (counts are ints on the wire, never bools).
#[derive(Debug, Clone, Default)]
pub struct SGenre {
    /// Genre name (XML text).
    pub value: String,
    /// Song count.
    pub songCount: i64,
    /// Album count.
    pub albumCount: i64,
}

impl Render for SGenre {
    fn render(&self) -> Val {
        obj(vec![
            ("value", self.value.as_str().into_val()),
            ("songCount", self.songCount.into_val()),
            ("albumCount", self.albumCount.into_val()),
        ])
    }
}

/// Playlist (entry list only on getPlaylist/createPlaylist detail).
#[derive(Debug, Clone, Default)]
pub struct SPlaylist {
    /// Prefixed playlist id.
    pub id: String,
    /// Playlist name.
    pub name: String,
    /// Comment.
    pub comment: Option<String>,
    /// Owner username.
    pub owner: Option<String>,
    /// Public flag.
    pub public: Option<bool>,
    /// True when the caller may not change the playlist (OpenSubsonic).
    pub readonly: bool,
    /// Streamable-entry count (#181: matches served entries).
    pub songCount: i64,
    /// Total duration seconds.
    pub duration: Option<i64>,
    /// Created timestamp ISO.
    pub created: Option<String>,
    /// Changed timestamp ISO.
    pub changed: Option<String>,
    /// Cover art id: the playlist's own id, on every playlist.
    pub coverArt: Option<String>,
    /// Entries (detail only).
    pub entry: Option<Vec<SChild>>,
}

impl Render for SPlaylist {
    fn render(&self) -> Val {
        obj(vec![
            ("id", self.id.as_str().into_val()),
            ("name", self.name.as_str().into_val()),
            ("comment", self.comment.clone().into_val()),
            ("owner", self.owner.clone().into_val()),
            ("public", self.public.into_val()),
            ("readonly", self.readonly.into_val()),
            ("songCount", self.songCount.into_val()),
            ("duration", self.duration.into_val()),
            ("created", self.created.clone().into_val()),
            ("changed", self.changed.clone().into_val()),
            ("coverArt", self.coverArt.clone().into_val()),
            (
                "entry",
                self.entry
                    .as_ref()
                    .map(|entries| Val::List(entries.iter().map(Render::render).collect()))
                    .into_val(),
            ),
        ])
    }
}

/// One A-Z bucket of ID3 artists.
#[derive(Debug, Clone, Default)]
pub struct SIndexID3 {
    /// Bucket letter.
    pub name: String,
    /// Artists in the bucket.
    pub artist: Vec<SArtistID3>,
}

impl Render for SIndexID3 {
    fn render(&self) -> Val {
        obj(vec![
            ("name", self.name.as_str().into_val()),
            (
                "artist",
                Val::List(self.artist.iter().map(Render::render).collect()),
            ),
        ])
    }
}

/// getArtists payload.
#[derive(Debug, Clone, Default)]
pub struct SArtistsID3 {
    /// Library revision, as getIndexes sends it: a counter, not a time.
    pub lastModified: i64,
    /// Articles stripped for bucketing.
    pub ignoredArticles: String,
    /// Buckets.
    pub index: Vec<SIndexID3>,
}

impl Render for SArtistsID3 {
    fn render(&self) -> Val {
        obj(vec![
            ("lastModified", self.lastModified.into_val()),
            ("ignoredArticles", self.ignoredArticles.as_str().into_val()),
            (
                "index",
                Val::List(self.index.iter().map(Render::render).collect()),
            ),
        ])
    }
}

/// One A-Z bucket of file-structure artists.
#[derive(Debug, Clone, Default)]
pub struct SIndex {
    /// Bucket letter.
    pub name: String,
    /// Artists in the bucket.
    pub artist: Vec<SArtist>,
}

impl Render for SIndex {
    fn render(&self) -> Val {
        obj(vec![
            ("name", self.name.as_str().into_val()),
            (
                "artist",
                Val::List(self.artist.iter().map(Render::render).collect()),
            ),
        ])
    }
}

/// getIndexes payload.
#[derive(Debug, Clone, Default)]
pub struct SIndexes {
    /// Library revision.
    pub lastModified: i64,
    /// Articles stripped for bucketing.
    pub ignoredArticles: String,
    /// Buckets (empty when ifModifiedSince is fresh).
    pub index: Vec<SIndex>,
}

impl Render for SIndexes {
    fn render(&self) -> Val {
        obj(vec![
            ("lastModified", self.lastModified.into_val()),
            ("ignoredArticles", self.ignoredArticles.as_str().into_val()),
            (
                "index",
                Val::List(self.index.iter().map(Render::render).collect()),
            ),
        ])
    }
}

/// Music folder (exactly one: id 1).
#[derive(Debug, Clone, Default)]
pub struct SMusicFolder {
    /// Folder id (always 1).
    pub id: i64,
    /// Folder name (advertised server name).
    pub name: String,
}

impl Render for SMusicFolder {
    fn render(&self) -> Val {
        obj(vec![
            ("id", self.id.into_val()),
            ("name", self.name.as_str().into_val()),
        ])
    }
}

/// getLicense payload (static valid:true).
#[derive(Debug, Clone, Default)]
pub struct SLicense {
    /// Always true.
    pub valid: bool,
}

impl Render for SLicense {
    fn render(&self) -> Val {
        obj(vec![("valid", self.valid.into_val())])
    }
}

/// One advertised OpenSubsonic extension.
#[derive(Debug, Clone, Default)]
pub struct SOpenSubsonicExtension {
    /// Extension name.
    pub name: String,
    /// Supported versions.
    pub versions: Vec<i64>,
}

impl Render for SOpenSubsonicExtension {
    fn render(&self) -> Val {
        obj(vec![
            ("name", self.name.as_str().into_val()),
            (
                "versions",
                Val::List(self.versions.iter().map(|v| v.into_val()).collect()),
            ),
        ])
    }
}

/// getPlayQueue payload (current is a song id).
#[derive(Debug, Clone, Default)]
pub struct SPlayQueue {
    /// Owner username.
    pub username: String,
    /// Changed timestamp ISO.
    pub changed: String,
    /// Client that last changed it.
    pub changedBy: String,
    /// Current song id.
    pub current: Option<String>,
    /// Position ms.
    pub position: Option<i64>,
    /// Entries.
    pub entry: Vec<SChild>,
}

impl Render for SPlayQueue {
    fn render(&self) -> Val {
        obj(vec![
            ("username", self.username.as_str().into_val()),
            ("changed", self.changed.as_str().into_val()),
            ("changedBy", self.changedBy.as_str().into_val()),
            ("current", self.current.clone().into_val()),
            ("position", self.position.into_val()),
            (
                "entry",
                Val::List(self.entry.iter().map(Render::render).collect()),
            ),
        ])
    }
}

/// getPlayQueueByIndex payload (extension indexBasedQueue:1).
#[derive(Debug, Clone, Default)]
pub struct SPlayQueueByIndex {
    /// Owner username.
    pub username: String,
    /// Changed timestamp ISO.
    pub changed: String,
    /// Client that last changed it.
    pub changedBy: String,
    /// Current index.
    pub currentIndex: Option<i64>,
    /// Position ms.
    pub position: Option<i64>,
    /// Entries.
    pub entry: Vec<SChild>,
}

impl Render for SPlayQueueByIndex {
    fn render(&self) -> Val {
        obj(vec![
            ("username", self.username.as_str().into_val()),
            ("changed", self.changed.as_str().into_val()),
            ("changedBy", self.changedBy.as_str().into_val()),
            ("currentIndex", self.currentIndex.into_val()),
            ("position", self.position.into_val()),
            (
                "entry",
                Val::List(self.entry.iter().map(Render::render).collect()),
            ),
        ])
    }
}

/// Bookmark (pinned required: position, username, created, changed, entry).
#[derive(Debug, Clone, Default)]
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
    pub comment: Option<String>,
}

impl Render for SBookmark {
    fn render(&self) -> Val {
        obj(vec![
            ("position", self.position.into_val()),
            ("username", self.username.as_str().into_val()),
            ("created", self.created.as_str().into_val()),
            ("changed", self.changed.as_str().into_val()),
            ("entry", self.entry.render()),
            ("comment", self.comment.clone().into_val()),
        ])
    }
}

/// Legacy flat lyrics.
#[derive(Debug, Clone, Default)]
pub struct SLyrics {
    /// Artist name.
    pub artist: String,
    /// Track title.
    pub title: String,
    /// Full text (empty on miss).
    pub value: String,
}

impl Render for SLyrics {
    fn render(&self) -> Val {
        obj(vec![
            ("artist", self.artist.as_str().into_val()),
            ("title", self.title.as_str().into_val()),
            ("value", self.value.as_str().into_val()),
        ])
    }
}

/// One structured lyric line (pinned required: value on every line).
#[derive(Debug, Clone, Default)]
pub struct SLyricsLine {
    /// Line text.
    pub value: String,
    /// Start ms (synced only).
    pub start: Option<i64>,
}

impl Render for SLyricsLine {
    fn render(&self) -> Val {
        obj(vec![
            ("value", self.value.as_str().into_val()),
            ("start", self.start.into_val()),
        ])
    }
}

/// Structured lyrics (extension songLyrics:1; pinned: lang, synced, line).
#[derive(Debug, Clone, Default)]
pub struct SStructuredLyrics {
    /// Language code.
    pub lang: String,
    /// Whether lines carry timestamps.
    pub synced: bool,
    /// Lines.
    pub line: Vec<SLyricsLine>,
    /// Display artist.
    pub displayArtist: Option<String>,
    /// Display title.
    pub displayTitle: Option<String>,
    /// Offset seconds.
    pub offset: Option<f64>,
}

impl Render for SStructuredLyrics {
    fn render(&self) -> Val {
        obj(vec![
            ("lang", self.lang.as_str().into_val()),
            ("synced", self.synced.into_val()),
            (
                "line",
                Val::List(self.line.iter().map(Render::render).collect()),
            ),
            ("displayArtist", self.displayArtist.clone().into_val()),
            ("displayTitle", self.displayTitle.clone().into_val()),
            ("offset", self.offset.into_val()),
        ])
    }
}

/// Artist info (MBIDs projected only; cover URLs credential-free).
#[derive(Debug, Clone, Default)]
pub struct SArtistInfo {
    /// Biography.
    pub biography: Option<String>,
    /// MusicBrainz id.
    pub musicBrainzId: Option<String>,
    /// Last.fm URL.
    pub lastFmUrl: Option<String>,
    /// Small cover URL.
    pub smallImageUrl: Option<String>,
    /// Medium cover URL.
    pub mediumImageUrl: Option<String>,
    /// Large cover URL.
    pub largeImageUrl: Option<String>,
    /// Similar artists.
    pub similarArtist: Vec<SArtistID3>,
}

impl Render for SArtistInfo {
    fn render(&self) -> Val {
        obj(vec![
            ("biography", self.biography.clone().into_val()),
            ("musicBrainzId", self.musicBrainzId.clone().into_val()),
            ("lastFmUrl", self.lastFmUrl.clone().into_val()),
            ("smallImageUrl", self.smallImageUrl.clone().into_val()),
            ("mediumImageUrl", self.mediumImageUrl.clone().into_val()),
            ("largeImageUrl", self.largeImageUrl.clone().into_val()),
            (
                "similarArtist",
                Val::List(self.similarArtist.iter().map(Render::render).collect()),
            ),
        ])
    }
}

/// Album info.
#[derive(Debug, Clone, Default)]
pub struct SAlbumInfo {
    /// Notes.
    pub notes: Option<String>,
    /// MusicBrainz id.
    pub musicBrainzId: Option<String>,
    /// Last.fm URL.
    pub lastFmUrl: Option<String>,
    /// Small cover URL.
    pub smallImageUrl: Option<String>,
    /// Medium cover URL.
    pub mediumImageUrl: Option<String>,
    /// Large cover URL.
    pub largeImageUrl: Option<String>,
}

impl Render for SAlbumInfo {
    fn render(&self) -> Val {
        obj(vec![
            ("notes", self.notes.clone().into_val()),
            ("musicBrainzId", self.musicBrainzId.clone().into_val()),
            ("lastFmUrl", self.lastFmUrl.clone().into_val()),
            ("smallImageUrl", self.smallImageUrl.clone().into_val()),
            ("mediumImageUrl", self.mediumImageUrl.clone().into_val()),
            ("largeImageUrl", self.largeImageUrl.clone().into_val()),
        ])
    }
}

/// Scan status (pinned required: scanning).
#[derive(Debug, Clone, Default)]
pub struct SScanStatus {
    /// Whether a scan is running.
    pub scanning: bool,
    /// Files looked at so far during a scan; playable tracks at rest.
    pub count: Option<i64>,
    /// Folders holding at least one playable track.
    pub folderCount: Option<i64>,
    /// End of the last finished scan, ISO.
    pub lastScan: Option<String>,
    /// Kind of the last finished scan.
    pub scanType: Option<String>,
    /// Length of the last finished scan in nanoseconds: Navidrome 0.62.0
    /// sends `int64(status.ElapsedTime)`, a Go `time.Duration`
    /// (`server/subsonic/library_scanning.go`, read 2026-10-14).
    pub elapsedTime: Option<i64>,
}

impl Render for SScanStatus {
    fn render(&self) -> Val {
        obj(vec![
            ("scanning", self.scanning.into_val()),
            ("count", self.count.into_val()),
            ("folderCount", self.folderCount.into_val()),
            ("lastScan", self.lastScan.clone().into_val()),
            ("scanType", self.scanType.clone().into_val()),
            ("elapsedTime", self.elapsedTime.into_val()),
        ])
    }
}

/// Stream details inside a transcode decision.
#[derive(Debug, Clone, Default)]
pub struct SStreamDetails {
    /// Protocol.
    pub protocol: String,
    /// Container.
    pub container: String,
    /// Codec.
    pub codec: String,
    /// Audio channels.
    pub audioChannels: Option<i64>,
    /// Audio bitrate.
    pub audioBitrate: Option<i64>,
    /// Audio profile.
    pub audioProfile: Option<String>,
    /// Sample rate.
    pub audioSamplerate: Option<i64>,
    /// Bit depth.
    pub audioBitdepth: Option<i64>,
}

impl Render for SStreamDetails {
    fn render(&self) -> Val {
        obj(vec![
            ("protocol", self.protocol.as_str().into_val()),
            ("container", self.container.as_str().into_val()),
            ("codec", self.codec.as_str().into_val()),
            ("audioChannels", self.audioChannels.into_val()),
            ("audioBitrate", self.audioBitrate.into_val()),
            ("audioProfile", self.audioProfile.clone().into_val()),
            ("audioSamplerate", self.audioSamplerate.into_val()),
            ("audioBitdepth", self.audioBitdepth.into_val()),
        ])
    }
}

/// getTranscodeDecision payload (signed transcodeParams inside).
#[derive(Debug, Clone, Default)]
pub struct STranscodeDecision {
    /// Direct play possible.
    pub canDirectPlay: bool,
    /// Transcode possible.
    pub canTranscode: bool,
    /// Why a transcode is needed.
    pub transcodeReason: Vec<String>,
    /// Why nothing is possible.
    pub errorReason: Option<String>,
    /// Signed params for getTranscodeStream.
    pub transcodeParams: Option<String>,
    /// Source stream facts.
    pub sourceStream: Option<SStreamDetails>,
    /// Target stream facts.
    pub transcodeStream: Option<SStreamDetails>,
}

impl Render for STranscodeDecision {
    fn render(&self) -> Val {
        obj(vec![
            ("canDirectPlay", self.canDirectPlay.into_val()),
            ("canTranscode", self.canTranscode.into_val()),
            (
                "transcodeReason",
                Val::List(
                    self.transcodeReason
                        .iter()
                        .map(|r| r.as_str().into_val())
                        .collect(),
                ),
            ),
            ("errorReason", self.errorReason.clone().into_val()),
            ("transcodeParams", self.transcodeParams.clone().into_val()),
            (
                "sourceStream",
                self.sourceStream.as_ref().map(Render::render).into_val(),
            ),
            (
                "transcodeStream",
                self.transcodeStream.as_ref().map(Render::render).into_val(),
            ),
        ])
    }
}

/// getUser payload (always the caller; the username param is ignored).
/// Roles: admin implies admin+settings; everyone gets playlist, cover and
/// stream roles; download follows the admin's download setting.
#[derive(Debug, Clone, Default)]
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

impl Render for SUser {
    fn render(&self) -> Val {
        obj(vec![
            ("username", self.username.as_str().into_val()),
            ("scrobblingEnabled", self.scrobblingEnabled.into_val()),
            ("adminRole", self.adminRole.into_val()),
            ("settingsRole", self.settingsRole.into_val()),
            ("downloadRole", self.downloadRole.into_val()),
            ("uploadRole", self.uploadRole.into_val()),
            ("playlistRole", self.playlistRole.into_val()),
            ("coverArtRole", self.coverArtRole.into_val()),
            ("commentRole", self.commentRole.into_val()),
            ("podcastRole", self.podcastRole.into_val()),
            ("streamRole", self.streamRole.into_val()),
            ("jukeboxRole", self.jukeboxRole.into_val()),
            ("shareRole", self.shareRole.into_val()),
            ("videoConversionRole", self.videoConversionRole.into_val()),
            ("maxBitRate", self.maxBitRate.into_val()),
            (
                "folder",
                Val::List(self.folder.iter().map(|f| f.into_val()).collect()),
            ),
        ])
    }
}
