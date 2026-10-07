//! What the discover page reads beyond the queue's sources, as plain rows.
//!
//! The page builders see [`PageSources`], which extends the queue's
//! [`QueueSources`] with the reads only the shelves need: charts, the
//! weekly playlist, similar listeners, Last.fm weekly charts and recent
//! scrobbles, tag searches and Jellyfin play history. Like the queue's
//! reads, every one is fallible with a log-only cause: a failed read
//! empties its shelf, it never fails the page.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::reads::discover::adapters::queue::sources::{
    AlbumRow, ArtistRow, QueueSources, SourceResult, StatsRange,
};
use crate::reads::discover::ports::BoxFuture;

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
    /// The key activity rows carry, in v2's spelling so migrated rows
    /// keep working: a compact JSON triple.
    pub fn key(&self) -> String {
        serde_json::to_string(&(&self.mode, &self.id, self.generation))
            .unwrap_or_else(|_| format!("[\"{}\",\"{}\",{}]", self.mode, self.id, self.generation))
    }
}

/// Every outside read the discover page makes beyond the queue's.
pub trait PageSources: QueueSources {
    /// The MusicBrainz source in use.
    fn source_context(&self) -> SourceContext;

    /// This week's most listened artists site-wide.
    fn listenbrainz_sitewide_artists(
        &self,
        count: u32,
    ) -> BoxFuture<'_, SourceResult<Vec<ArtistRow>>>;
    /// Similar artists with their ListenBrainz score.
    fn listenbrainz_similar_scored<'a>(
        &'a self,
        user_id: &'a str,
        artist_mbid: &'a str,
        limit: usize,
    ) -> BoxFuture<'a, SourceResult<Vec<ScoredArtist>>>;
    /// One artist's most played release groups, with listens.
    fn listenbrainz_artist_ranked<'a>(
        &'a self,
        user_id: &'a str,
        artist_mbid: &'a str,
        count: usize,
    ) -> BoxFuture<'a, SourceResult<Vec<RankedAlbum>>>;
    /// This week's most played release groups site-wide, with listens.
    fn listenbrainz_trending_ranked(
        &self,
        count: u32,
    ) -> BoxFuture<'_, SourceResult<Vec<RankedAlbum>>>;
    /// A user's top release groups, with listens.
    fn listenbrainz_user_ranked<'a>(
        &'a self,
        username: &'a str,
        range: StatsRange,
        count: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<RankedAlbum>>>;
    /// The user's genres with their listens, most listened first.
    fn listenbrainz_genre_counts<'a>(
        &'a self,
        username: &'a str,
    ) -> BoxFuture<'a, SourceResult<Vec<(String, i64)>>>;
    /// Users whose listening is like this user's, most similar first.
    fn listenbrainz_similar_users<'a>(
        &'a self,
        username: &'a str,
    ) -> BoxFuture<'a, SourceResult<Vec<String>>>;
    /// The user's newest weekly exploration playlist (else the newest
    /// recommendation playlist), `None` when there is none.
    fn listenbrainz_weekly_playlist<'a>(
        &'a self,
        user_id: &'a str,
        username: &'a str,
    ) -> BoxFuture<'a, SourceResult<Option<WeeklyPlaylist>>>;
    /// Release groups of recordings, keyed by lowercased recording id.
    fn listenbrainz_recording_groups<'a>(
        &'a self,
        user_id: &'a str,
        recording_mbids: &'a [String],
    ) -> BoxFuture<'a, SourceResult<HashMap<String, String>>>;

    /// Similar artists with their Last.fm match.
    fn lastfm_similar_scored<'a>(
        &'a self,
        user_id: &'a str,
        artist: &'a ArtistRow,
        limit: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<ScoredArtist>>>;
    /// The user's Last.fm artists this week.
    fn lastfm_weekly_artists<'a>(
        &'a self,
        user_id: &'a str,
        username: &'a str,
    ) -> BoxFuture<'a, SourceResult<Vec<ArtistRow>>>;
    /// The user's Last.fm albums this week.
    fn lastfm_weekly_albums<'a>(
        &'a self,
        user_id: &'a str,
        username: &'a str,
    ) -> BoxFuture<'a, SourceResult<Vec<LastFmChartAlbum>>>;
    /// The user's latest Last.fm scrobbles, as albums.
    fn lastfm_recent_albums<'a>(
        &'a self,
        user_id: &'a str,
        username: &'a str,
        limit: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<LastFmChartAlbum>>>;
    /// An artist's Last.fm tags, most used first.
    fn lastfm_artist_tags<'a>(
        &'a self,
        user_id: &'a str,
        artist: &'a ArtistRow,
    ) -> BoxFuture<'a, SourceResult<Vec<String>>>;
    /// The most played artists carrying one Last.fm tag.
    fn lastfm_tag_artists<'a>(
        &'a self,
        user_id: &'a str,
        tag: &'a str,
        limit: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<ArtistRow>>>;

    /// Artists carrying one MusicBrainz tag.
    fn musicbrainz_tag_artists<'a>(
        &'a self,
        tag: &'a str,
        limit: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<ArtistRow>>>;

    /// The user's most played Jellyfin artists with their play data.
    fn jellyfin_artist_plays<'a>(
        &'a self,
        user_id: &'a str,
        limit: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<PlayedArtist>>>;
}
