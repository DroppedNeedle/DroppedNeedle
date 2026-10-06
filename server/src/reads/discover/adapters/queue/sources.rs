//! What the queue reads from the outside world, as plain rows.
//!
//! The deck builder and the card details only see [`QueueSources`], so
//! they run the same way over the live providers and over a scripted test
//! double. Every read is fallible with a log-only cause: a failed source
//! shrinks the deck or blanks a field, it never fails the whole request.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::reads::discover::ports::BoxFuture;

/// A read that can fail with a log-only cause.
pub type SourceResult<T> = Result<T, String>;

/// Which listening service a user's discovery follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MusicSource {
    /// ListenBrainz (the default).
    #[default]
    ListenBrainz,
    /// Last.fm.
    LastFm,
}

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
    pub primary: MusicSource,
}

impl UserMusic {
    /// The source discovery follows: the primary one, unless only the
    /// other is usable (v2 `resolve_source_value`).
    pub fn resolved_source(&self) -> MusicSource {
        let listenbrainz = self.listenbrainz.is_some();
        match self.primary {
            MusicSource::ListenBrainz if !listenbrainz && self.lastfm => MusicSource::LastFm,
            MusicSource::LastFm if !self.lastfm && listenbrainz => MusicSource::ListenBrainz,
            other => other,
        }
    }
}

/// ListenBrainz statistics windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatsRange {
    /// The current week.
    ThisWeek,
    /// The current month.
    ThisMonth,
    /// The current year.
    ThisYear,
    /// Everything.
    AllTime,
}

impl StatsRange {
    /// The ListenBrainz spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ThisWeek => "this_week",
            Self::ThisMonth => "this_month",
            Self::ThisYear => "this_year",
            Self::AllTime => "all_time",
        }
    }
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
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LastFmFacts {
    /// MusicBrainz id Last.fm holds, when any.
    pub mbid: Option<String>,
    /// Tag names.
    pub tags: Vec<String>,
    /// Wiki or bio summary, raw.
    pub summary: String,
}

/// Every outside read the queue makes.
pub trait QueueSources: Send + Sync {
    /// The MusicBrainz source in use. Decks and card details built against
    /// one source are never served after a switch to another.
    fn source_key(&self) -> String;
    /// The user's listening services.
    fn user_music<'a>(&'a self, user_id: &'a str) -> BoxFuture<'a, UserMusic>;

    /// A ListenBrainz user's top artists.
    fn listenbrainz_top_artists<'a>(
        &'a self,
        username: &'a str,
        range: StatsRange,
        count: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<ArtistRow>>>;
    /// A ListenBrainz user's top release groups.
    fn listenbrainz_top_albums<'a>(
        &'a self,
        username: &'a str,
        range: StatsRange,
        count: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<AlbumRow>>>;
    /// Artists similar to one artist. `user_id` lends its token.
    fn listenbrainz_similar_artists<'a>(
        &'a self,
        user_id: &'a str,
        artist_mbid: &'a str,
        limit: usize,
    ) -> BoxFuture<'a, SourceResult<Vec<ArtistRow>>>;
    /// One artist's most played release groups, credited to that artist.
    fn listenbrainz_artist_albums<'a>(
        &'a self,
        user_id: &'a str,
        artist_mbid: &'a str,
        count: usize,
    ) -> BoxFuture<'a, SourceResult<Vec<AlbumRow>>>;
    /// A user's genres, most listened first.
    fn listenbrainz_genres<'a>(
        &'a self,
        username: &'a str,
    ) -> BoxFuture<'a, SourceResult<Vec<String>>>;
    /// Recent releases by artists the user listens to.
    fn listenbrainz_fresh_releases<'a>(
        &'a self,
        username: &'a str,
    ) -> BoxFuture<'a, SourceResult<Vec<AlbumRow>>>;
    /// The first artist id of each loved recording, in order.
    fn listenbrainz_loved_artists<'a>(
        &'a self,
        username: &'a str,
        count: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<String>>>;
    /// This week's most played release groups site-wide.
    fn listenbrainz_trending(&self, count: u32) -> BoxFuture<'_, SourceResult<Vec<AlbumRow>>>;
    /// Listen counts per release group. `user_id` lends its token.
    fn listenbrainz_listen_counts<'a>(
        &'a self,
        user_id: &'a str,
        release_group_mbids: &'a [String],
    ) -> BoxFuture<'a, SourceResult<HashMap<String, i64>>>;
    /// Whether ListenBrainz popularity reads are refused upstream right now.
    /// While they are, Last.fm stands in when the user has it.
    fn listenbrainz_popularity_down(&self) -> bool;

    /// A Last.fm user's top artists over three months.
    fn lastfm_top_artists<'a>(
        &'a self,
        user_id: &'a str,
        username: &'a str,
        limit: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<ArtistRow>>>;
    /// Artists Last.fm calls similar.
    fn lastfm_similar_artists<'a>(
        &'a self,
        user_id: &'a str,
        artist: &'a ArtistRow,
        limit: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<ArtistRow>>>;
    /// An artist's top albums on Last.fm.
    fn lastfm_artist_albums<'a>(
        &'a self,
        user_id: &'a str,
        artist: &'a ArtistRow,
        limit: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<LastFmAlbum>>>;
    /// The Last.fm site-wide artist chart.
    fn lastfm_chart_artists<'a>(
        &'a self,
        user_id: &'a str,
        limit: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<ArtistRow>>>;
    /// Last.fm album facts, `None` when Last.fm is off or does not know it.
    fn lastfm_album_facts<'a>(
        &'a self,
        user_id: &'a str,
        artist: &'a str,
        album: &'a str,
    ) -> BoxFuture<'a, SourceResult<Option<LastFmFacts>>>;
    /// Last.fm artist facts, `None` when Last.fm is off or does not know it.
    fn lastfm_artist_facts<'a>(
        &'a self,
        user_id: &'a str,
        artist: &'a str,
        mbid: Option<&'a str>,
    ) -> BoxFuture<'a, SourceResult<Option<LastFmFacts>>>;

    /// Artists from the user's Jellyfin account.
    fn jellyfin_artists<'a>(
        &'a self,
        user_id: &'a str,
        list: JellyfinList,
        limit: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<ArtistRow>>>;

    /// Release groups carrying one tag, release-type exclusions applied.
    fn musicbrainz_tag_albums<'a>(
        &'a self,
        tag: &'a str,
        limit: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<AlbumRow>>>;
    /// The release group a release belongs to. `Ok(None)` is a definite
    /// "no such release".
    fn musicbrainz_release_group_of<'a>(
        &'a self,
        release_mbid: &'a str,
    ) -> BoxFuture<'a, SourceResult<Option<String>>>;
    /// Release-group facts for a card. `Ok(None)` is a definite miss.
    fn musicbrainz_release_group<'a>(
        &'a self,
        mbid: &'a str,
    ) -> BoxFuture<'a, SourceResult<Option<ReleaseGroupFacts>>>;
    /// Artist facts for a card.
    fn musicbrainz_artist<'a>(
        &'a self,
        mbid: &'a str,
    ) -> BoxFuture<'a, SourceResult<Option<ArtistFacts>>>;
    /// A YouTube link from a release's URL relationships.
    fn musicbrainz_release_video<'a>(
        &'a self,
        release_mbid: &'a str,
    ) -> BoxFuture<'a, SourceResult<Option<String>>>;
    /// The first recordings on a release, in track order.
    fn musicbrainz_release_recordings<'a>(
        &'a self,
        release_mbid: &'a str,
        limit: usize,
    ) -> BoxFuture<'a, SourceResult<Vec<String>>>;
    /// A YouTube link from a recording's URL relationships.
    fn musicbrainz_recording_video<'a>(
        &'a self,
        recording_mbid: &'a str,
    ) -> BoxFuture<'a, SourceResult<Option<String>>>;
    /// The Wikipedia intro behind a Wikipedia or Wikidata link.
    fn wikipedia_extract<'a>(&'a self, url: &'a str)
    -> BoxFuture<'a, SourceResult<Option<String>>>;
}
