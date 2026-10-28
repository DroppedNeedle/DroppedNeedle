//! Which listening service answers, per user and per ListenBrainz health.
//!
//! Every Discover feature asks [`SourcePolicy`] instead of deciding on its
//! own, so the queue, the page, home, radio and the charts choose the same
//! way.

use std::collections::{HashMap, HashSet};

use super::ListeningError;
use super::rows::{ListenSource, ScoredArtist, UserMusic};

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

    /// Whether a build reads seed and similar artists from both services
    /// and blends them: the server `allows` it and the user can read
    /// Last.fm and has linked both accounts.
    pub fn blends(&self, allows: bool) -> bool {
        allows
            && self.user.listenbrainz.is_some()
            && self.user.lastfm
            && self.user.lastfm_username.is_some()
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

/// Merge two services' ranked lists, `first` from the user's primary
/// service: rows both lists hold come first, by their combined rank, then
/// the rest alternate between the lists, each in its own order. `key`
/// says when two rows are the same artist (a row without one is never
/// merged), and `join` makes one row of a pair.
pub fn blend<T>(
    first: Vec<T>,
    second: Vec<T>,
    key: impl Fn(&T) -> Option<String>,
    join: impl Fn(T, T) -> T,
) -> Vec<T> {
    let mut second_at: HashMap<String, usize> = HashMap::new();
    for (at, row) in second.iter().enumerate() {
        if let Some(key) = key(row) {
            second_at.entry(key).or_insert(at);
        }
    }
    let mut second: Vec<Option<T>> = second.into_iter().map(Some).collect();
    let mut seen: HashSet<String> = HashSet::new();
    let mut both: Vec<(usize, usize, T)> = Vec::new();
    let mut only_first: Vec<T> = Vec::new();
    for (at, row) in first.into_iter().enumerate() {
        let Some(row_key) = key(&row) else {
            only_first.push(row);
            continue;
        };
        if !seen.insert(row_key.clone()) {
            continue;
        }
        let pair = second_at
            .get(&row_key)
            .and_then(|&other_at| Some((other_at, second.get_mut(other_at)?.take()?)));
        match pair {
            Some((other_at, other)) => both.push((at, other_at, join(row, other))),
            None => only_first.push(row),
        }
    }
    both.sort_by_key(|(at, other_at, _)| (at + other_at, *at));
    let only_second: Vec<T> = second
        .into_iter()
        .flatten()
        .filter(|row| key(row).is_none_or(|key| seen.insert(key)))
        .collect();
    let mut blended: Vec<T> = both.into_iter().map(|(_, _, row)| row).collect();
    let mut first_rest = only_first.into_iter();
    let mut second_rest = only_second.into_iter();
    loop {
        let (a, b) = (first_rest.next(), second_rest.next());
        if a.is_none() && b.is_none() {
            break;
        }
        blended.extend(a);
        blended.extend(b);
    }
    blended
}

/// [`blend`] for two services' similar artists, keyed by MusicBrainz id
/// or else by name. Each list's scores are scaled to its own best first
/// (Last.fm sends a match from 0 to 1, ListenBrainz a raw count), and an
/// artist both suggest carries the sum, so agreement also weighs most
/// where a score is read.
pub fn blend_similar(first: Vec<ScoredArtist>, second: Vec<ScoredArtist>) -> Vec<ScoredArtist> {
    fn scaled(mut rows: Vec<ScoredArtist>) -> Vec<ScoredArtist> {
        let best = rows.iter().map(|row| row.score).fold(0.0_f64, f64::max);
        for row in &mut rows {
            row.score = if best > 0.0 { row.score / best } else { 0.0 };
        }
        rows
    }
    let key = |row: &ScoredArtist| {
        let mbid = row.artist.mbid.as_deref().map(str::trim).unwrap_or("");
        if mbid.is_empty() {
            let name = row.artist.name.trim().to_lowercase();
            (!name.is_empty()).then(|| format!("name:{name}"))
        } else {
            Some(mbid.to_ascii_lowercase())
        }
    };
    blend(scaled(first), scaled(second), key, |mut kept, other| {
        kept.score += other.score;
        kept
    })
}
