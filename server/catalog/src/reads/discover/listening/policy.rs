//! Which listening service answers, per user and per ListenBrainz health.
//!
//! Every Discover feature asks [`SourcePolicy`] instead of deciding on its
//! own, so the queue, the page, home, radio and the charts choose the same
//! way.

use super::ListeningError;
use super::rows::{ListenSource, UserMusic};

/// One user's accounts plus ListenBrainz's health, read once per build or
/// request.
#[derive(Debug, Clone)]
pub struct SourcePolicy {
    user: UserMusic,
    popularity_down: bool,
}

impl SourcePolicy {
    /// The policy for `user` while ListenBrainz popularity reads are down
    /// (`popularity_down`) or up.
    pub fn new(user: UserMusic, popularity_down: bool) -> Self {
        Self {
            user,
            popularity_down,
        }
    }

    /// The user's accounts.
    pub fn user(&self) -> &UserMusic {
        &self.user
    }

    /// Whether the user can read `source`: a linked ListenBrainz account,
    /// or Last.fm switched on with a usable key.
    pub fn ready(&self, source: ListenSource) -> bool {
        match source {
            ListenSource::Listenbrainz => self.user.listenbrainz.is_some(),
            ListenSource::Lastfm => self.user.lastfm,
        }
    }

    /// `asked`, switched to the other service when only that one is set
    /// up for the user (v2 `resolve_source`).
    pub fn asked(&self, asked: ListenSource) -> ListenSource {
        let listenbrainz = self.ready(ListenSource::Listenbrainz);
        let lastfm = self.ready(ListenSource::Lastfm);
        match asked {
            ListenSource::Listenbrainz if !listenbrainz && lastfm => ListenSource::Lastfm,
            ListenSource::Lastfm if !lastfm && listenbrainz => ListenSource::Listenbrainz,
            other => other,
        }
    }

    /// Whose listening a build follows: the user's primary service, unless
    /// only the other is set up.
    pub fn listening(&self) -> ListenSource {
        self.asked(self.user.primary)
    }

    /// Where album popularity comes from: Last.fm when the user follows
    /// Last.fm or ListenBrainz popularity is down, and the user can read
    /// Last.fm; ListenBrainz otherwise.
    pub fn popularity(&self) -> ListenSource {
        if self.user.lastfm && (self.listening() == ListenSource::Lastfm || self.popularity_down) {
            ListenSource::Lastfm
        } else {
            ListenSource::Listenbrainz
        }
    }

    /// Whether ListenBrainz refuses popularity reads right now.
    pub fn popularity_down(&self) -> bool {
        self.popularity_down
    }

    /// Whether Last.fm stands in for ListenBrainz popularity for every
    /// user who can read it, whatever their primary service.
    pub fn lastfm_stands_in(&self) -> bool {
        self.user.lastfm && self.popularity_down
    }

    /// Where sitewide charts come from: Last.fm for a user who follows it
    /// and can read it, ListenBrainz (no account needed) otherwise.
    pub fn sitewide(&self) -> ListenSource {
        if self.user.lastfm && self.listening() == ListenSource::Lastfm {
            ListenSource::Lastfm
        } else {
            ListenSource::Listenbrainz
        }
    }

    /// The service that stands in after a ListenBrainz read failed with
    /// `error`: Last.fm, when ListenBrainz is down or limiting and the
    /// user can read Last.fm.
    pub fn stand_in(&self, error: &ListeningError) -> Option<ListenSource> {
        let outage = matches!(error, ListeningError::Provider(error) if error.trips_breaker());
        (outage && self.user.lastfm).then_some(ListenSource::Lastfm)
    }
}
