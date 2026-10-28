//! The rows the listening reads answer with, shared by every feature.

use serde::{Deserialize, Serialize};

pub use crate::reads::discover::models::StatsRange;
pub use droppedneedle_kernel::runtime_config::sections::ListenSource;

/// One user's listening services, resolved once per build.
#[derive(Debug, Clone, Default)]
pub struct UserMusic {
    /// ListenBrainz username, when the user linked an account.
    pub listenbrainz: Option<String>,
    /// Last.fm username, when the user linked an account.
    pub lastfm_username: Option<String>,
    /// Whether Last.fm reads work for this user (switch on and a key).
    pub lastfm: bool,
    /// Whether a Jellyfin connection resolves for this user.
    pub jellyfin: bool,
    /// The user's primary source preference.
    pub primary: ListenSource,
}

/// Which Jellyfin artist list seeds a deck.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JellyfinList {
    /// Most played artists.
    MostPlayed,
    /// Favorite artists.
    Favorites,
}

/// One artist row from any provider.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtistRow {
    /// Artist name.
    pub name: String,
    /// MusicBrainz artist id, when the provider knows it.
    pub mbid: Option<String>,
    /// Listens or plays behind the row, when the provider counts them.
    pub listen_count: i64,
}

/// One album row keyed by its release group.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlbumRow {
    /// MusicBrainz release-group id.
    pub release_group_mbid: String,
    /// Album title.
    pub title: String,
    /// Credited artist name.
    pub artist_name: String,
    /// MusicBrainz id of the first credited artist, when known.
    pub artist_mbid: Option<String>,
}

/// One Last.fm album row. Its MBID, when present, usually names a
/// release, not a release group.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LastFmAlbum {
    /// Album title.
    pub name: String,
    /// Credited artist name.
    pub artist_name: String,
    /// MusicBrainz id Last.fm holds for the album.
    pub mbid: Option<String>,
}

/// The release-group facts behind a card.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReleaseGroupFacts {
    /// Group title.
    pub title: String,
    /// First credited artist id.
    pub artist_mbid: Option<String>,
    /// First credited artist name.
    pub artist_name: Option<String>,
    /// Folksonomy tags, most voted first.
    pub tags: Vec<String>,
    /// A YouTube link from the group's URL relationships.
    pub youtube_url: Option<String>,
    /// The first listed release.
    pub first_release_id: Option<String>,
    /// That release's date.
    pub first_release_date: Option<String>,
}

/// The artist facts behind a card.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ArtistFacts {
    /// Country code, else the home area's name.
    pub country: Option<String>,
    /// A Wikipedia or Wikidata link from the URL relationships.
    pub wiki_url: Option<String>,
}

/// Last.fm album or artist facts used as a fallback.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LastFmFacts {
    /// MusicBrainz id Last.fm holds, when any.
    pub mbid: Option<String>,
    /// Tag names.
    pub tags: Vec<String>,
    /// Wiki or bio summary, raw.
    pub summary: String,
}

/// An album row with the listens that rank it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RankedAlbum {
    /// The album.
    pub album: AlbumRow,
    /// Listens behind the rank (0 when the provider does not count).
    pub listen_count: i64,
}

/// A similar artist with its similarity: ListenBrainz sends a listen
/// count (unbounded), Last.fm a match between 0 and 1.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScoredArtist {
    /// The artist.
    pub artist: ArtistRow,
    /// Raw similarity score.
    pub score: f64,
}

/// One Last.fm chart or scrobble album. Its MBID, when present, usually
/// names a release, not a release group.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LastFmChartAlbum {
    /// Album title.
    pub name: String,
    /// Credited artist name.
    pub artist_name: String,
    /// MusicBrainz id Last.fm holds for the album.
    pub mbid: Option<String>,
    /// Plays behind the row (0 for scrobbles).
    pub playcount: i64,
    /// Artwork URL, empty when none.
    pub image_url: String,
}

/// One artist from the user's Jellyfin play history.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayedArtist {
    /// Artist name.
    pub name: String,
    /// MusicBrainz artist id, when Jellyfin has it.
    pub mbid: Option<String>,
    /// The user's plays.
    pub play_count: i64,
    /// The user's last play (ISO 8601), when known.
    pub last_played: Option<String>,
    /// Artwork URL served through the remotes proxy, when there is art.
    pub image_url: Option<String>,
}

/// One track of the weekly exploration playlist.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlaylistTrack {
    /// Track title.
    pub title: String,
    /// Credited artist.
    pub creator: String,
    /// Album title.
    pub album: String,
    /// Recording MBID, when known.
    pub recording_mbid: Option<String>,
    /// First credited artist MBID, when known.
    pub artist_mbid: Option<String>,
    /// Release the cover comes from, when known.
    pub caa_release_mbid: Option<String>,
    /// Length in milliseconds, when known.
    pub duration_ms: Option<i64>,
}

/// The user's newest weekly exploration playlist.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WeeklyPlaylist {
    /// Playlist title.
    pub title: String,
    /// Playlist date as ListenBrainz sends it.
    pub date: String,
    /// The playlist's page on ListenBrainz.
    pub source_url: String,
    /// Tracks in order.
    pub tracks: Vec<PlaylistTrack>,
}

/// The mode of a source whose settings do not read.
const UNREADABLE_MODE: &str = "unreadable";

/// The MusicBrainz source the page is built against: activity recorded
/// under one source is not serviced after a switch to another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceContext {
    /// Source tier (`official`, `community`, ...).
    pub mode: String,
    /// Source identity.
    pub id: String,
    /// Source generation.
    pub generation: i64,
}

impl SourceContext {
    /// The context while the MusicBrainz settings do not read.
    pub fn unreadable() -> Self {
        Self {
            mode: UNREADABLE_MODE.to_owned(),
            id: crate::reads::catalog::upstream::UNREADABLE_NAMESPACE.to_owned(),
            generation: 0,
        }
    }

    /// The cache namespace of this source: `<id>:g<generation>`, so a
    /// source switch or a new generation starts fresh answers.
    pub fn namespace(&self) -> String {
        if self.mode == UNREADABLE_MODE {
            return self.id.clone();
        }
        format!("{}:g{}", self.id, self.generation)
    }

    /// The key activity rows carry, in v2's spelling so migrated rows
    /// keep working: a compact JSON triple.
    pub fn key(&self) -> String {
        serde_json::to_string(&(&self.mode, &self.id, self.generation))
            .unwrap_or_else(|_| format!("[\"{}\",\"{}\",{}]", self.mode, self.id, self.generation))
    }
}

/// One loved track (ListenBrainz feedback or Last.fm loved tracks).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LovedTrack {
    /// Track title.
    pub name: String,
    /// Credited artist name.
    pub artist_name: String,
    /// First credited artist MBID, when known.
    pub artist_mbid: Option<String>,
    /// Album title, when known.
    pub album_name: Option<String>,
    /// Recording MBID, when known.
    pub recording_mbid: Option<String>,
    /// Cover URL, when known.
    pub image_url: Option<String>,
}

/// One release group carrying a tag.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TagAlbum {
    /// Release-group MBID.
    pub release_group_mbid: String,
    /// Album title.
    pub title: String,
    /// Credited artist name.
    pub artist_name: String,
    /// MusicBrainz id of the first credited artist, when known.
    pub artist_mbid: Option<String>,
    /// First release year, when known.
    pub year: Option<i64>,
}

impl TagAlbum {
    /// The album as a plain row.
    pub fn row(self) -> AlbumRow {
        AlbumRow {
            release_group_mbid: self.release_group_mbid,
            title: self.title,
            artist_name: self.artist_name,
            artist_mbid: self.artist_mbid,
        }
    }
}

/// One album from a ListenBrainz statistics list. Sitewide and user lists
/// keep rows ListenBrainz could not map to a release group.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListenedAlbum {
    /// MusicBrainz release-group id, when mapped.
    pub release_group_mbid: Option<String>,
    /// Album title.
    pub title: String,
    /// Credited artist name.
    pub artist_name: String,
    /// MusicBrainz id of the first credited artist, when known.
    pub artist_mbid: Option<String>,
    /// Listens behind the rank.
    pub listen_count: i64,
}

impl ListenedAlbum {
    /// The album with its listens, when it maps to a release group.
    pub fn ranked(self) -> Option<RankedAlbum> {
        Some(RankedAlbum {
            album: AlbumRow {
                release_group_mbid: self.release_group_mbid?,
                title: self.title,
                artist_name: self.artist_name,
                artist_mbid: self.artist_mbid,
            },
            listen_count: self.listen_count,
        })
    }

    /// The album as a plain row, when it maps to a release group.
    pub fn row(self) -> Option<AlbumRow> {
        self.ranked().map(|ranked| ranked.album)
    }
}
