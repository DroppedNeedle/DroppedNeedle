//! The raw outside reads behind [`ListeningData`](super::ListeningData).
//!
//! A trait because a network sits behind it: production reads the live
//! providers ([`LiveNetwork`](super::LiveNetwork)) and the tests script the
//! answers, so the cache, single-flight, source policy and the build cap in
//! front of it run the same way in both. Every read answers plain rows; a
//! listening service the user cannot read answers empty, never an error.

use std::collections::HashMap;

use droppedneedle_providers::RequestPriority;
use droppedneedle_providers::listenbrainz::{
    ArtistPopularity, ReleaseGroupCounts, ScoredSimilarArtist,
};

use super::ListeningResult;
use super::band_links::BandLink;
use super::rows::{
    ArtistRow, FreshRow, JellyfinList, LastFmAlbum, LastFmChartAlbum, LastFmFacts, ListenedAlbum,
    LovedTrack, PlayedArtist, ReleaseGroupFacts, ScoredArtist, SourceContext, StatsRange, TagAlbum,
    UserMusic, WeeklyPlaylist,
};
use crate::discover::ports::BoxFuture;
use crate::metadata::artist::ReleaseGroupItem;

/// A read that answers `T` or a typed failure.
pub type Read<'a, T> = BoxFuture<'a, ListeningResult<T>>;

/// Every outside read Discover makes.
pub trait ListeningNetwork: Send + Sync {
    /// The MusicBrainz source in use.
    fn source(&self) -> SourceContext;
    /// The user's listening accounts and preference. While ListenBrainz
    /// reads are off the user reads as having no ListenBrainz link.
    fn accounts<'a>(&'a self, user_id: &'a str) -> BoxFuture<'a, UserMusic>;
    /// Whether ListenBrainz reads are switched on (false while their
    /// setting does not read).
    fn listenbrainz_ready(&self) -> bool;
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
    /// How long labs reads still rest after a failure, if they do: a read
    /// made meanwhile sends nothing.
    fn labs_rest(&self) -> Option<std::time::Duration> {
        None
    }
    /// ListenBrainz labs' scored similar artists for one artist, as
    /// answered. No token is sent.
    fn scored_similar<'a>(
        &'a self,
        artist_mbid: &'a str,
        priority: RequestPriority,
    ) -> Read<'a, Vec<ScoredSimilarArtist>>;
    /// Listen and listener counts for artists (a popularity read), sent
    /// with no token.
    fn artist_listeners<'a>(
        &'a self,
        artist_mbids: &'a [String],
        priority: RequestPriority,
    ) -> Read<'a, HashMap<String, ArtistPopularity>>;
    /// One artist's whole discography from the install's MusicBrainz
    /// source, `None` while a large one is still being fetched.
    fn discography<'a>(
        &'a self,
        artist_mbid: &'a str,
        priority: RequestPriority,
    ) -> Read<'a, Option<Vec<ReleaseGroupItem>>>;
    /// One artist's band links from the install's MusicBrainz source: its
    /// members, bands, founders, subgroups, collaborations and other names.
    /// `None` when MusicBrainz has no such artist; a MusicBrainz that does
    /// not answer is an error, never `None`.
    fn band_links<'a>(
        &'a self,
        artist_mbid: &'a str,
        priority: RequestPriority,
    ) -> Read<'a, Option<Vec<BandLink>>>;
    /// Listen and listener counts for release groups (a popularity read),
    /// sent with no token.
    fn release_group_counts<'a>(
        &'a self,
        release_group_mbids: &'a [String],
        priority: RequestPriority,
    ) -> Read<'a, HashMap<String, ReleaseGroupCounts>>;
    /// A user's genres with their listens, most listened first.
    fn genres<'a>(
        &'a self,
        username: &'a str,
        priority: RequestPriority,
    ) -> Read<'a, Vec<(String, i64)>>;
    /// Every release of the last `days` days, sitewide, oldest first, sent
    /// with no token. A network that does not script it has none.
    fn sitewide_fresh_releases(
        &self,
        days: u32,
        priority: RequestPriority,
    ) -> Read<'_, Vec<FreshRow>> {
        let _ = (days, priority);
        Box::pin(async { Ok(Vec::new()) })
    }
    /// A user's loved recordings, newest first.
    fn loved<'a>(
        &'a self,
        username: &'a str,
        count: u32,
        priority: RequestPriority,
    ) -> Read<'a, Vec<LovedTrack>>;
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
