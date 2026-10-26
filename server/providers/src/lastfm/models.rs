//! Last.fm wire shapes.
//!
//! Every answer is tolerant of unknown fields and of numbers sent as text
//! (Last.fm sends counts as strings in most answers and as numbers in a
//! few, both seen in the recordings). The fields a caller keys on are
//! required: an answer without them fails decoding. Each read's envelope
//! (`artist`, `toptracks`, ...) is required too, as it is what makes the
//! answer the one asked for; the list inside it may be missing or empty.
//!
//! The read recordings are in `tests/fixtures/lastfm/`, described in its
//! README. A list may arrive as a single object where it holds one item
//! (the bio's `links.link` does so in the artist recording); every list
//! here accepts both forms.

use serde::Deserialize;
use serde::de::IgnoredAny;

/// Last.fm's error envelope, `{"error": n, "message": ...}`, sent in a 200
/// as often as not. Verified against ws.audioscrobbler.com/2.0/ on
/// 2026-10-26: tests/fixtures/lastfm/artist_not_found.json (200, error 6),
/// user_not_found.json (404, error 6) and invalid_key.json (403, error 10).
#[derive(Debug, Deserialize)]
pub(crate) struct WireError {
    /// The error code.
    pub error: WireErrorCode,
}

/// An error code: a number, or anything else (no usable code).
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum WireErrorCode {
    /// A numeric code.
    Code(i64),
    /// Anything else.
    Other(IgnoredAny),
}

/// `auth.getToken`. Not verified: sign-in flow.
#[derive(Debug, Deserialize)]
pub(crate) struct WireToken {
    /// The token to approve.
    pub token: String,
}

/// `auth.getSession`: the session under `session`, or at the top level as
/// v2 also accepted. Not verified: sign-in flow.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum WireSessionReply {
    /// Under `session`, as documented.
    Nested {
        /// The session.
        session: WireSession,
    },
    /// At the top level.
    Flat(WireSession),
}

/// One linked session.
#[derive(Debug, Deserialize)]
pub(crate) struct WireSession {
    /// Linked username.
    pub name: String,
    /// Session key.
    pub key: String,
    /// Subscriber flag.
    #[serde(default)]
    pub subscriber: Option<Lenient>,
}

/// `artist.getInfo`. Verified against ws.audioscrobbler.com/2.0/?method=artist.getInfo
/// on 2026-10-26: tests/fixtures/lastfm/artist_getinfo.json.
#[derive(Debug, Deserialize)]
pub(crate) struct WireArtistInfoReply {
    /// The artist.
    pub artist: WireArtistInfo,
}

/// The artist in `artist.getInfo`.
#[derive(Debug, Deserialize)]
pub(crate) struct WireArtistInfo {
    /// Artist name.
    pub name: String,
    /// MusicBrainz artist id; blank when Last.fm has none.
    #[serde(default)]
    pub mbid: Option<String>,
    /// Last.fm page.
    #[serde(default)]
    pub url: Option<String>,
    /// Listener and play counts.
    #[serde(default)]
    pub stats: Option<Section<WireStats>>,
    /// Biography.
    #[serde(default)]
    pub bio: Option<Section<WireText>>,
    /// Tags.
    #[serde(default)]
    pub tags: Option<Section<WireTagList>>,
    /// Similar artists.
    #[serde(default)]
    pub similar: Option<Section<WireSimilarList>>,
}

/// An artist's counts.
#[derive(Debug, Deserialize)]
pub(crate) struct WireStats {
    /// Listener count.
    #[serde(default)]
    pub listeners: Option<Lenient>,
    /// Play count.
    #[serde(default)]
    pub playcount: Option<Lenient>,
}

/// A biography or album wiki.
#[derive(Debug, Deserialize)]
pub(crate) struct WireText {
    /// Short form.
    #[serde(default)]
    pub summary: Option<String>,
    /// Full text.
    #[serde(default)]
    pub content: Option<String>,
}

/// The `tags` list of an info answer.
#[derive(Debug, Deserialize)]
pub(crate) struct WireTagList {
    /// The tags.
    #[serde(default)]
    pub tag: Option<OneOrMany<WireTag>>,
}

/// One tag of an info answer.
#[derive(Debug, Deserialize)]
pub(crate) struct WireTag {
    /// Tag name.
    #[serde(default)]
    pub name: Option<String>,
    /// Tag page.
    #[serde(default)]
    pub url: Option<String>,
}

/// `artist.getSimilar`. Verified against ws.audioscrobbler.com/2.0/?method=artist.getSimilar
/// on 2026-10-26: tests/fixtures/lastfm/artist_getsimilar.json.
#[derive(Debug, Deserialize)]
pub(crate) struct WireSimilarReply {
    /// The list.
    pub similarartists: WireSimilarList,
}

/// A list of similar artists, in `artist.getSimilar` and `artist.getInfo`.
#[derive(Debug, Deserialize)]
pub(crate) struct WireSimilarList {
    /// The artists.
    #[serde(default)]
    pub artist: Option<OneOrMany<WireSimilarArtist>>,
}

/// One similar artist.
#[derive(Debug, Deserialize)]
pub(crate) struct WireSimilarArtist {
    /// Artist name.
    pub name: String,
    /// MusicBrainz artist id; `artist.getInfo` leaves it out.
    #[serde(default)]
    pub mbid: Option<String>,
    /// Similarity, sent as text.
    #[serde(default, rename = "match")]
    pub score: Option<Lenient>,
    /// Last.fm page.
    #[serde(default)]
    pub url: Option<String>,
}

/// `album.getInfo`. Verified against ws.audioscrobbler.com/2.0/?method=album.getInfo
/// on 2026-10-26: tests/fixtures/lastfm/album_getinfo.json.
#[derive(Debug, Deserialize)]
pub(crate) struct WireAlbumInfoReply {
    /// The album.
    pub album: WireAlbumInfo,
}

/// The album in `album.getInfo`.
#[derive(Debug, Deserialize)]
pub(crate) struct WireAlbumInfo {
    /// Album title.
    pub name: String,
    /// Album artist, as text.
    #[serde(default)]
    pub artist: Option<WireNameRef>,
    /// MusicBrainz release id; blank when Last.fm has none.
    #[serde(default)]
    pub mbid: Option<String>,
    /// Listener count.
    #[serde(default)]
    pub listeners: Option<Lenient>,
    /// Play count.
    #[serde(default)]
    pub playcount: Option<Lenient>,
    /// Last.fm page.
    #[serde(default)]
    pub url: Option<String>,
    /// Artwork in several sizes.
    #[serde(default)]
    pub image: Option<OneOrMany<WireImage>>,
    /// Wiki text.
    #[serde(default)]
    pub wiki: Option<Section<WireText>>,
    /// Tags.
    #[serde(default)]
    pub tags: Option<Section<WireTagList>>,
    /// Track list.
    #[serde(default)]
    pub tracks: Option<Section<WireTrackList>>,
}

/// An album's `tracks`.
#[derive(Debug, Deserialize)]
pub(crate) struct WireTrackList {
    /// The tracks.
    #[serde(default)]
    pub track: Option<OneOrMany<WireAlbumTrack>>,
}

/// One album track. Its duration is a number, `null` when Last.fm has none.
#[derive(Debug, Deserialize)]
pub(crate) struct WireAlbumTrack {
    /// Track title.
    #[serde(default)]
    pub name: Option<String>,
    /// Duration in seconds.
    #[serde(default)]
    pub duration: Option<Lenient>,
    /// Position on the album.
    #[serde(default, rename = "@attr")]
    pub attr: Option<WireRank>,
    /// Last.fm page.
    #[serde(default)]
    pub url: Option<String>,
}

/// A row's `@attr`.
#[derive(Debug, Deserialize)]
pub(crate) struct WireRank {
    /// Position, a number on album tracks and text on top lists.
    #[serde(default)]
    pub rank: Option<Lenient>,
}

/// Every list read whose rows become a top item. The envelope and row key
/// differ by method; each read asks for one method, so any of them is
/// accepted here.
///
/// - `artist.getTopTracks` (`toptracks.track`). Verified against
///   ws.audioscrobbler.com/2.0/?method=artist.getTopTracks on 2026-10-26:
///   tests/fixtures/lastfm/artist_gettoptracks.json.
/// - `artist.getTopAlbums` (`topalbums.album`). Verified against
///   ws.audioscrobbler.com/2.0/?method=artist.getTopAlbums on 2026-10-26:
///   tests/fixtures/lastfm/artist_gettopalbums.json.
/// - `chart.getTopArtists` (`artists.artist`). Verified against
///   ws.audioscrobbler.com/2.0/?method=chart.getTopArtists on 2026-10-26:
///   tests/fixtures/lastfm/chart_gettopartists.json.
/// - `tag.getTopArtists` (`topartists.artist`). Verified against
///   ws.audioscrobbler.com/2.0/?method=tag.getTopArtists on 2026-10-26:
///   tests/fixtures/lastfm/tag_gettopartists.json.
/// - `user.getTopAlbums` (`topalbums.album`), `user.getTopArtists`
///   (`topartists.artist`), `user.getWeeklyArtistChart`
///   (`weeklyartistchart.artist`), `user.getWeeklyAlbumChart`
///   (`weeklyalbumchart.album`) and `user.getLovedTracks`
///   (`lovedtracks.track`). Not verified: no Last.fm account was named for
///   recording. An unknown user answers error 6 in a 404, verified against
///   ws.audioscrobbler.com/2.0/?method=user.getTopAlbums on 2026-10-26:
///   tests/fixtures/lastfm/user_not_found.json.
#[derive(Debug, Deserialize)]
pub(crate) struct WireTopReply {
    /// The list.
    #[serde(
        rename = "toptracks",
        alias = "topalbums",
        alias = "artists",
        alias = "topartists",
        alias = "weeklyartistchart",
        alias = "weeklyalbumchart",
        alias = "lovedtracks"
    )]
    pub list: WireTopList,
}

/// The rows of a top list.
#[derive(Debug, Deserialize)]
pub(crate) struct WireTopList {
    /// The rows.
    #[serde(default, rename = "track", alias = "album", alias = "artist")]
    pub rows: Option<OneOrMany<WireTopItem>>,
}

/// One row of a top list.
#[derive(Debug, Deserialize)]
pub(crate) struct WireTopItem {
    /// Track, album or artist name.
    pub name: String,
    /// Credited artist, on track and album rows.
    #[serde(default)]
    pub artist: Option<WireNameRef>,
    /// MusicBrainz id; blank or missing when Last.fm has none.
    #[serde(default)]
    pub mbid: Option<String>,
    /// Play count: text on most lists, a number on `artist.getTopAlbums`,
    /// missing on `tag.getTopArtists`.
    #[serde(default)]
    pub playcount: Option<Lenient>,
    /// Artwork in several sizes.
    #[serde(default)]
    pub image: Option<OneOrMany<WireImage>>,
}

/// `user.getRecentTracks` (`recenttracks.track`). Not verified: no Last.fm
/// account was named for recording.
#[derive(Debug, Deserialize)]
pub(crate) struct WireRecentReply {
    /// The list.
    pub recenttracks: WireRecentList,
}

/// The rows of the recent tracks.
#[derive(Debug, Deserialize)]
pub(crate) struct WireRecentList {
    /// The scrobbles.
    #[serde(default)]
    pub track: Option<OneOrMany<WireRecentTrack>>,
}

/// One scrobble.
#[derive(Debug, Deserialize)]
pub(crate) struct WireRecentTrack {
    /// Track title.
    pub name: String,
    /// Credited artist.
    #[serde(default)]
    pub artist: Option<WireNameRef>,
    /// Album.
    #[serde(default)]
    pub album: Option<WireNameRef>,
    /// Artwork in several sizes.
    #[serde(default)]
    pub image: Option<OneOrMany<WireImage>>,
}

/// `artist.getTopTags` and `album.getTopTags` with `autocorrect=0`.
/// Verified against ws.audioscrobbler.com/2.0/?method=artist.getTopTags on
/// 2026-10-26: tests/fixtures/lastfm/artist_gettoptags.json. Verified
/// against ws.audioscrobbler.com/2.0/?method=album.getTopTags on
/// 2026-10-26: tests/fixtures/lastfm/album_gettoptags.json.
#[derive(Debug, Deserialize)]
pub(crate) struct WireTopTagsReply {
    /// The list.
    pub toptags: WireTopTags,
}

/// The weighted tags.
#[derive(Debug, Deserialize)]
pub(crate) struct WireTopTags {
    /// The tags.
    #[serde(default)]
    pub tag: Option<OneOrMany<WireWeightedTag>>,
}

/// One weighted tag.
#[derive(Debug, Deserialize)]
pub(crate) struct WireWeightedTag {
    /// Tag name.
    pub name: String,
    /// Weight, 0 through 100, sent as a number.
    #[serde(default)]
    pub count: Option<Lenient>,
}

/// A name sent as plain text (`album.getInfo`'s artist) or as an object
/// with `name` (top lists) or `#text` (charts and recent tracks).
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum WireNameRef {
    /// Plain text.
    Text(String),
    /// An object.
    Object(WireNamed),
}

/// The object form of a name.
#[derive(Debug, Deserialize)]
pub(crate) struct WireNamed {
    /// Name, on top lists.
    #[serde(default)]
    pub name: Option<String>,
    /// Name, on charts and recent tracks.
    #[serde(default, rename = "#text")]
    pub text: Option<String>,
    /// MusicBrainz id.
    #[serde(default)]
    pub mbid: Option<String>,
}

impl WireNameRef {
    /// The name, `name` before `#text`.
    pub fn name(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            Self::Object(named) => named.name.as_deref().or(named.text.as_deref()),
        }
    }

    /// The name, `#text` before `name`.
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            Self::Object(named) => named.text.as_deref().or(named.name.as_deref()),
        }
    }

    /// The MusicBrainz id the object form carries.
    pub fn mbid(&self) -> Option<&str> {
        match self {
            Self::Text(_) => None,
            Self::Object(named) => named.mbid.as_deref(),
        }
    }
}

/// One artwork size.
#[derive(Debug, Deserialize)]
pub(crate) struct WireImage {
    /// Image address; blank when there is none.
    #[serde(default, rename = "#text")]
    pub url: Option<String>,
    /// `small`, `medium`, `large`, `extralarge`, `mega` or blank.
    #[serde(default)]
    pub size: Option<String>,
}

/// A part of an info answer that is read only when it has the expected
/// shape. None of the recordings sends another shape; the old reader
/// accepted any here, so a stray one reads as absent rather than failing
/// the artist or album.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum Section<T> {
    /// The expected shape.
    Present(T),
    /// Anything else.
    Other(IgnoredAny),
}

impl<T> Section<T> {
    /// The part, when it had the expected shape.
    pub fn get(&self) -> Option<&T> {
        match self {
            Self::Present(value) => Some(value),
            Self::Other(_) => None,
        }
    }
}

/// The whole `track.scrobble` answer.
///
/// Checked 2026-10-21 against https://www.last.fm/api/show/track.scrobble
/// and https://www.last.fm/api/scrobbling: a scrobble Last.fm turns away
/// still comes back as a success, with `accepted` and `ignored` counts on
/// `scrobbles` and an `ignoredMessage` code on each `scrobble` (0 none,
/// 1 artist ignored, 2 track ignored, 3 timestamp too old, 4 timestamp too
/// new, 5 daily scrobble limit exceeded). The docs show the XML form; the
/// JSON form carries the counts under `@attr` and the message text under
/// `#text`, as every other Last.fm JSON answer does. No live answer was
/// read: a scrobble needs a user's session. The `scrobbles` element and its
/// `accepted` count are required: an answer without them says nothing
/// about the play and fails decoding.
#[derive(Debug, Deserialize)]
pub(crate) struct WireScrobbleReply {
    /// The batch's result.
    pub scrobbles: WireScrobbles,
}

/// The batch's counts and its scrobbles.
#[derive(Debug, Deserialize)]
pub(crate) struct WireScrobbles {
    /// How many were accepted and ignored.
    #[serde(rename = "@attr")]
    pub counts: WireCounts,
    /// One scrobble, or a list of them.
    #[serde(default)]
    pub scrobble: Option<OneOrMany<WireScrobble>>,
}

/// The batch's counts.
#[derive(Debug, Deserialize)]
pub(crate) struct WireCounts {
    /// Scrobbles Last.fm kept.
    pub accepted: Lenient,
}

/// A single element or a list of them; Last.fm sends a lone item bare.
/// The list is tried first: a struct also decodes from a JSON array.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum OneOrMany<T> {
    /// Several items.
    Many(Vec<T>),
    /// One item.
    One(T),
}

impl<T> OneOrMany<T> {
    /// The first item, if any.
    pub fn first(&self) -> Option<&T> {
        match self {
            Self::Many(items) => items.first(),
            Self::One(item) => Some(item),
        }
    }

    /// Every item, in order.
    pub fn iter(&self) -> std::slice::Iter<'_, T> {
        match self {
            Self::Many(items) => items.iter(),
            Self::One(item) => std::slice::from_ref(item).iter(),
        }
    }
}

/// One scrobble's outcome.
#[derive(Debug, Deserialize)]
pub(crate) struct WireScrobble {
    /// Why it was turned away, when it was.
    #[serde(rename = "ignoredMessage", default)]
    pub ignored_message: Option<WireIgnored>,
}

/// The reason a scrobble was turned away.
#[derive(Debug, Deserialize)]
pub(crate) struct WireIgnored {
    /// The ignored code; 0 means it was not.
    #[serde(default)]
    pub code: Option<Lenient>,
}

/// A number sent as a number or as text.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum Lenient {
    /// A whole number.
    Number(i64),
    /// A fraction.
    Fraction(f64),
    /// Sent as text.
    Text(String),
}

impl Lenient {
    /// The whole number, when there is one; a fraction is cut short.
    pub fn value(&self) -> Option<i64> {
        match self {
            Self::Number(number) => Some(*number),
            #[allow(clippy::cast_possible_truncation)]
            Self::Fraction(fraction) => Some(*fraction as i64),
            Self::Text(text) => text.trim().parse().ok(),
        }
    }

    /// The number as a fraction, when there is one.
    pub fn fraction(&self) -> Option<f64> {
        match self {
            #[allow(clippy::cast_precision_loss)]
            Self::Number(number) => Some(*number as f64),
            Self::Fraction(fraction) => Some(*fraction),
            Self::Text(text) => text.trim().parse().ok(),
        }
    }
}
