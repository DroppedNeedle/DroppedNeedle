//! The raw outside reads behind [`ListeningData`](super::ListeningData).
//!
//! A trait because a network sits behind it: production reads the live
//! providers ([`LiveNetwork`](super::LiveNetwork)) and the tests script the
//! answers, so the cache, single-flight, source policy and the build cap in
//! front of it run the same way in both. Every read answers plain rows; a
//! listening service the user cannot read answers empty, never an error.

use std::collections::HashMap;

use droppedneedle_providers::RequestPriority;

use super::ListeningResult;
use super::rows::{
    AlbumRow, ArtistFacts, ArtistRow, JellyfinList, LastFmAlbum, LastFmChartAlbum, LastFmFacts,
    ListenedAlbum, LovedTrack, PlayedArtist, RankedAlbum, ReleaseGroupFacts, ScoredArtist,
    SourceContext, StatsRange, TagAlbum, UserMusic, WeeklyPlaylist,
};
use crate::reads::discover::ports::BoxFuture;

/// A read that answers `T` or a typed failure.
pub type Read<'a, T> = BoxFuture<'a, ListeningResult<T>>;

/// Every outside read Discover makes.
pub trait ListeningNetwork: Send + Sync {
    /// The MusicBrainz source in use.
    fn source(&self) -> SourceContext;
    /// The user's listening accounts and preference.
    fn accounts<'a>(&'a self, user_id: &'a str) -> BoxFuture<'a, UserMusic>;
    /// Whether the user can read Last.fm (switched on, with their own key or
    /// the server's).
    fn lastfm_ready<'a>(&'a self, user_id: &'a str) -> BoxFuture<'a, bool> {
        Box::pin(async move { self.accounts(user_id).await.lastfm })
    }

    /// One page of the sitewide ListenBrainz artist chart.
    fn sitewide_artists(
        &self,
        range: StatsRange,
        count: u32,
        offset: u32,
        priority: RequestPriority,
    ) -> Read<'_, Vec<ArtistRow>>;
    /// One page of the sitewide ListenBrainz release-group chart.
    fn sitewide_albums(
        &self,
        range: StatsRange,
        count: u32,
        offset: u32,
        priority: RequestPriority,
    ) -> Read<'_, Vec<ListenedAlbum>>;
    /// One page of a ListenBrainz user's top artists.
    fn user_artists<'a>(
        &'a self,
        username: &'a str,
        range: StatsRange,
        count: u32,
        offset: u32,
        priority: RequestPriority,
    ) -> Read<'a, Vec<ArtistRow>>;
    /// One page of a ListenBrainz user's top release groups.
    fn user_albums<'a>(
        &'a self,
        username: &'a str,
        range: StatsRange,
        count: u32,
        offset: u32,
        priority: RequestPriority,
    ) -> Read<'a, Vec<ListenedAlbum>>;
    /// Artists ListenBrainz finds similar, scored by listens. `user_id`
    /// lends its token.
    fn similar_artists<'a>(
        &'a self,
        user_id: &'a str,
        artist_mbid: &'a str,
        limit: usize,
        priority: RequestPriority,
    ) -> Read<'a, Vec<ScoredArtist>>;
    /// One artist's most played release groups (a popularity read).
    fn artist_albums<'a>(
        &'a self,
        user_id: &'a str,
        artist_mbid: &'a str,
        count: usize,
        priority: RequestPriority,
    ) -> Read<'a, Vec<RankedAlbum>>;
    /// A user's genres with their listens, most listened first.
    fn genres<'a>(
        &'a self,
        username: &'a str,
        priority: RequestPriority,
    ) -> Read<'a, Vec<(String, i64)>>;
    /// Recent releases by artists the user listens to.
    fn fresh_releases<'a>(
        &'a self,
        username: &'a str,
        priority: RequestPriority,
    ) -> Read<'a, Vec<AlbumRow>>;
    /// A user's loved recordings, newest first.
    fn loved<'a>(
        &'a self,
        username: &'a str,
        count: u32,
        priority: RequestPriority,
    ) -> Read<'a, Vec<LovedTrack>>;
    /// Users whose listening is like this user's, most similar first.
    fn similar_users<'a>(
        &'a self,
        username: &'a str,
        priority: RequestPriority,
    ) -> Read<'a, Vec<String>>;
    /// The user's newest weekly exploration playlist (else the newest
    /// recommendation playlist).
    fn weekly_playlist<'a>(
        &'a self,
        user_id: &'a str,
        username: &'a str,
        priority: RequestPriority,
    ) -> Read<'a, Option<WeeklyPlaylist>>;
    /// Release groups of recordings, keyed by lowercased recording id.
    fn recording_groups<'a>(
        &'a self,
        user_id: &'a str,
        recording_mbids: &'a [String],
        priority: RequestPriority,
    ) -> Read<'a, HashMap<String, String>>;
    /// Listen counts per release group (a popularity read).
    fn listen_counts<'a>(
        &'a self,
        user_id: &'a str,
        release_group_mbids: &'a [String],
        priority: RequestPriority,
    ) -> Read<'a, HashMap<String, i64>>;

    /// A Last.fm user's top artists over `period`.
    fn lastfm_user_artists<'a>(
        &'a self,
        user_id: &'a str,
        username: &'a str,
        period: &'a str,
        limit: u32,
        priority: RequestPriority,
    ) -> Read<'a, Vec<ArtistRow>>;
    /// A Last.fm user's top albums over `period`.
    fn lastfm_user_albums<'a>(
        &'a self,
        user_id: &'a str,
        username: &'a str,
        period: &'a str,
        limit: u32,
        priority: RequestPriority,
    ) -> Read<'a, Vec<LastFmChartAlbum>>;
    /// Artists Last.fm calls similar, scored by match.
    fn lastfm_similar<'a>(
        &'a self,
        user_id: &'a str,
        artist: &'a ArtistRow,
        limit: u32,
        priority: RequestPriority,
    ) -> Read<'a, Vec<ScoredArtist>>;
    /// An artist's top albums on Last.fm.
    fn lastfm_artist_albums<'a>(
        &'a self,
        user_id: &'a str,
        artist: &'a ArtistRow,
        limit: u32,
        priority: RequestPriority,
    ) -> Read<'a, Vec<LastFmAlbum>>;
    /// The Last.fm sitewide artist chart.
    fn lastfm_chart_artists<'a>(
        &'a self,
        user_id: &'a str,
        limit: u32,
        priority: RequestPriority,
    ) -> Read<'a, Vec<ArtistRow>>;
    /// Last.fm album facts.
    fn lastfm_album<'a>(
        &'a self,
        user_id: &'a str,
        artist: &'a str,
        album: &'a str,
        priority: RequestPriority,
    ) -> Read<'a, Option<LastFmFacts>>;
    /// Last.fm artist facts.
    fn lastfm_artist<'a>(
        &'a self,
        user_id: &'a str,
        artist: &'a str,
        mbid: Option<&'a str>,
        priority: RequestPriority,
    ) -> Read<'a, Option<LastFmFacts>>;
    /// The user's Last.fm artists this week.
    fn lastfm_weekly_artists<'a>(
        &'a self,
        user_id: &'a str,
        username: &'a str,
        priority: RequestPriority,
    ) -> Read<'a, Vec<ArtistRow>>;
    /// The user's Last.fm albums this week.
    fn lastfm_weekly_albums<'a>(
        &'a self,
        user_id: &'a str,
        username: &'a str,
        priority: RequestPriority,
    ) -> Read<'a, Vec<LastFmChartAlbum>>;
    /// The user's latest Last.fm scrobbles, as albums.
    fn lastfm_recent_albums<'a>(
        &'a self,
        user_id: &'a str,
        username: &'a str,
        limit: u32,
        priority: RequestPriority,
    ) -> Read<'a, Vec<LastFmChartAlbum>>;
    /// The most played artists carrying one Last.fm tag.
    fn lastfm_tag_artists<'a>(
        &'a self,
        user_id: &'a str,
        tag: &'a str,
        limit: u32,
        priority: RequestPriority,
    ) -> Read<'a, Vec<ArtistRow>>;
    /// A Last.fm user's loved tracks.
    fn lastfm_loved<'a>(
        &'a self,
        user_id: &'a str,
        username: &'a str,
        limit: u32,
        priority: RequestPriority,
    ) -> Read<'a, Vec<LovedTrack>>;

    /// One page of release groups carrying a MusicBrainz tag, with the
    /// release types discovery leaves out already dropped.
    fn tag_albums<'a>(
        &'a self,
        tag: &'a str,
        limit: u32,
        offset: u32,
        priority: RequestPriority,
    ) -> Read<'a, Vec<TagAlbum>>;
    /// One page of artists carrying a MusicBrainz tag.
    fn tag_artists<'a>(
        &'a self,
        tag: &'a str,
        limit: u32,
        offset: u32,
        priority: RequestPriority,
    ) -> Read<'a, Vec<ArtistRow>>;
    /// The release group a release belongs to. `None` is a definite miss.
    fn release_group_of<'a>(
        &'a self,
        release_mbid: &'a str,
        priority: RequestPriority,
    ) -> Read<'a, Option<String>>;
    /// Release-group facts for a card. `None` is a definite miss.
    fn release_group<'a>(
        &'a self,
        mbid: &'a str,
        priority: RequestPriority,
    ) -> Read<'a, Option<ReleaseGroupFacts>>;
    /// Artist facts for a card.
    fn artist<'a>(
        &'a self,
        mbid: &'a str,
        priority: RequestPriority,
    ) -> Read<'a, Option<ArtistFacts>>;
    /// A YouTube link from a release's URL relationships.
    fn release_video<'a>(
        &'a self,
        release_mbid: &'a str,
        priority: RequestPriority,
    ) -> Read<'a, Option<String>>;
    /// The first recordings on a release, in track order.
    fn release_recordings<'a>(
        &'a self,
        release_mbid: &'a str,
        limit: usize,
        priority: RequestPriority,
    ) -> Read<'a, Vec<String>>;
    /// A YouTube link from a recording's URL relationships.
    fn recording_video<'a>(
        &'a self,
        recording_mbid: &'a str,
        priority: RequestPriority,
    ) -> Read<'a, Option<String>>;
    /// The Wikipedia intro behind a Wikipedia or Wikidata link.
    fn wikipedia_extract<'a>(
        &'a self,
        url: &'a str,
        priority: RequestPriority,
    ) -> Read<'a, Option<String>>;

    /// Artists from the user's Jellyfin account.
    fn jellyfin_artists<'a>(
        &'a self,
        user_id: &'a str,
        list: JellyfinList,
        limit: u32,
    ) -> Read<'a, Vec<ArtistRow>>;
    /// The user's most played Jellyfin artists with their play data.
    fn jellyfin_plays<'a>(&'a self, user_id: &'a str, limit: u32) -> Read<'a, Vec<PlayedArtist>>;
}
