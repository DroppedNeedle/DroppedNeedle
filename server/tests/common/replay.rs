//! The replay's timeline and scoring, as plain functions: a person's
//! listens from a ListenBrainz export (read by the import's own archive
//! reader), what they went on to play after a cut date, and how a set of
//! picks scores against that. `tests/catalog/discover_replay.rs` runs the
//! engine and its baselines over them.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

use droppedneedle_media::history::archive::{ArchiveError, Export};

/// A day, ms.
pub const DAY_MS: i64 = 86_400_000;
/// A week, ms.
pub const WEEK_MS: i64 = 7 * DAY_MS;
/// An in-app play this close to a listen in the timeline is that listen,
/// scrobbled to ListenBrainz: it is counted once.
pub const SAME_PLAY_MS: i64 = 10 * 60_000;
/// The plays after a cut that make a new artist adopted.
pub const ADOPTED: usize = 5;
/// The plays after a cut that make a new artist adopted for certain.
pub const ADOPTED_HEAVILY: usize = 20;

/// One listen or play.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listen {
    /// When, unix ms.
    pub at_ms: i64,
    /// The first credited artist's MusicBrainz id, lowercased.
    pub artist_mbid: Option<String>,
    /// The artist's name.
    pub artist_name: String,
    /// The track's title.
    pub track_name: String,
    /// The recording's MusicBrainz id, lowercased.
    pub recording_mbid: Option<String>,
    /// The release's title.
    pub release_name: Option<String>,
    /// The release's MusicBrainz id, lowercased.
    pub release_mbid: Option<String>,
    /// Whether it is a play in the app rather than an imported listen.
    pub in_app: bool,
}

/// A person's listening, oldest first.
#[derive(Debug, Clone, Default)]
pub struct Timeline {
    /// Every listen and play, oldest first.
    pub listens: Vec<Listen>,
    /// Lines of the export that were not a listen.
    pub skipped: usize,
    /// In-app plays left out as the same as a listen.
    pub same_as_a_listen: usize,
}

impl Timeline {
    /// The listens of the export ZIP at `path`: every `listens/*/*.jsonl`
    /// file, through the archive reader the history import uses.
    pub fn from_export(path: &Path) -> Result<Self, ArchiveError> {
        let mut export = Export::open(path)?;
        let mut timeline = Self::default();
        for position in 0..export.months().len() {
            let Some(month) = export.read(position) else {
                break;
            };
            timeline.skipped += month.skipped;
            if month.passed_over.is_some() {
                timeline.skipped += 1;
            }
            timeline
                .listens
                .extend(month.listens.into_iter().map(|listen| Listen {
                    at_ms: listen.listened_at * 1000,
                    artist_mbid: listen.artist_mbid,
                    artist_name: listen.artist_name,
                    track_name: listen.track_name,
                    recording_mbid: listen.recording_mbid,
                    release_name: listen.release_name,
                    release_mbid: listen.release_mbid,
                    in_app: false,
                }));
        }
        timeline.listens.sort_by_key(|listen| listen.at_ms);
        Ok(timeline)
    }

    /// A timeline of `listens`, in any order.
    pub fn from_listens(mut listens: Vec<Listen>) -> Self {
        listens.sort_by_key(|listen| listen.at_ms);
        Self {
            listens,
            ..Self::default()
        }
    }

    /// Add the person's in-app `plays`, leaving out each one within
    /// [`SAME_PLAY_MS`] of an imported listen: that is the play, scrobbled.
    pub fn add_app_plays(&mut self, plays: Vec<Listen>) {
        let imported: Vec<i64> = self
            .listens
            .iter()
            .filter(|listen| !listen.in_app)
            .map(|listen| listen.at_ms)
            .collect();
        for play in plays {
            let near = imported.partition_point(|at| *at < play.at_ms - SAME_PLAY_MS);
            if imported
                .get(near)
                .is_some_and(|at| *at <= play.at_ms + SAME_PLAY_MS)
            {
                self.same_as_a_listen += 1;
            } else {
                self.listens.push(Listen {
                    in_app: true,
                    ..play
                });
            }
        }
        self.listens.sort_by_key(|listen| listen.at_ms);
    }

    /// The first and last listen, unix ms.
    pub fn span(&self) -> Option<(i64, i64)> {
        Some((self.listens.first()?.at_ms, self.listens.last()?.at_ms))
    }

    /// The listens at or before `cut`.
    pub fn until(&self, cut: i64) -> impl Iterator<Item = &Listen> {
        self.listens
            .iter()
            .take_while(move |listen| listen.at_ms <= cut)
    }

    /// The artists played at or before `cut`.
    pub fn heard_by(&self, cut: i64) -> HashSet<String> {
        self.until(cut)
            .filter_map(|listen| listen.artist_mbid.clone())
            .collect()
    }

    /// The first of each month from the month after the first listen, while
    /// a whole `window` after it is in the timeline.
    pub fn monthly_cuts(&self, window: i64) -> Vec<i64> {
        let Some((first, last)) = self.span() else {
            return Vec::new();
        };
        let mut cuts = Vec::new();
        let mut cut = next_month(first);
        while cut + window <= last {
            cuts.push(cut);
            cut = next_month(cut);
        }
        cuts
    }
}

/// A request for one of an artist's albums.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// The album's artist, lowercased.
    pub artist_mbid: String,
    /// When it was asked for, unix ms.
    pub at_ms: i64,
}

/// One artist the person went on to play.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// Its MusicBrainz id, lowercased.
    pub artist: String,
    /// Its name.
    pub name: String,
    /// The plays in the window.
    pub plays: usize,
    /// Whether a request for one of its albums came before its first play
    /// in the window: found through the app, not elsewhere.
    pub through_app: bool,
}

/// What the person went on to play in the window after a cut.
#[derive(Debug, Clone, Default)]
pub struct Truth {
    /// Every artist with no play up to the cut and one or more after.
    pub new: Vec<Found>,
}

impl Truth {
    /// The new artists with at least `plays` in the window.
    pub fn at_least(&self, plays: usize) -> impl Iterator<Item = &Found> {
        self.new.iter().filter(move |found| found.plays >= plays)
    }

    /// The ids of the new artists with at least `plays` in the window.
    pub fn ids(&self, plays: usize) -> BTreeSet<String> {
        self.at_least(plays)
            .map(|found| found.artist.clone())
            .collect()
    }
}

/// The truth for (`cut`, `cut + window`]: the artists `timeline` never
/// played up to `cut` and played after it, each tagged by `requests`.
pub fn truth(timeline: &Timeline, cut: i64, window: i64, requests: &[Request]) -> Truth {
    let heard = timeline.heard_by(cut);
    let mut after: BTreeMap<&str, (usize, i64, &str)> = BTreeMap::new();
    for listen in &timeline.listens {
        if listen.at_ms <= cut || listen.at_ms > cut + window {
            continue;
        }
        let Some(artist) = listen.artist_mbid.as_deref() else {
            continue;
        };
        if heard.contains(artist) {
            continue;
        }
        let entry = after
            .entry(artist)
            .or_insert((0, listen.at_ms, listen.artist_name.as_str()));
        entry.0 += 1;
    }
    let new = after
        .into_iter()
        .map(|(artist, (plays, first_at, name))| Found {
            artist: artist.to_owned(),
            name: name.to_owned(),
            plays,
            through_app: requests
                .iter()
                .any(|request| request.artist_mbid == artist && request.at_ms <= first_at),
        })
        .collect();
    Truth { new }
}

/// One pick, as scoring reads it.
#[derive(Debug, Clone, PartialEq)]
pub struct Scored {
    /// Its release group, lowercased.
    pub release_group: String,
    /// Its artist, lowercased.
    pub artist: String,
    /// Its artist's listeners, when counted.
    pub artist_listeners: Option<i64>,
    /// The person's artist behind its strongest seed path, if it has one.
    pub strongest_seed: Option<String>,
}

/// How one set scored at one cut.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SetScore {
    /// How many picks.
    pub picks: usize,
    /// The adopted artists ([`ADOPTED`] plays) among the picks.
    pub adopted: BTreeSet<String>,
    /// The new artists played at all among the picks.
    pub new_played: BTreeSet<String>,
    /// The artists adopted heavily ([`ADOPTED_HEAVILY`]) among the picks.
    pub adopted_heavily: usize,
    /// Picks the library held at the cut.
    pub owned: usize,
    /// Picks whose artist the person had played by the cut.
    pub heard: usize,
    /// The counted artist listeners of the picks.
    pub listeners: Vec<i64>,
    /// Picks whose strongest seed is the person's heaviest, when the set
    /// has seeds at all.
    pub top_seed: Option<usize>,
}

/// Score `set` against `truth`; `heard` are the artists played by the cut,
/// `owned` the release groups the library held then, `top_seed` the
/// person's heaviest seed.
pub fn score(
    set: &[Scored],
    truth: &Truth,
    heard: &HashSet<String>,
    owned: &HashSet<String>,
    top_seed: Option<&str>,
) -> SetScore {
    let adopted = truth.ids(ADOPTED);
    let heavily = truth.ids(ADOPTED_HEAVILY);
    let played = truth.ids(1);
    let artists: BTreeSet<&str> = set.iter().map(|pick| pick.artist.as_str()).collect();
    let seeded = set.iter().any(|pick| pick.strongest_seed.is_some());
    SetScore {
        picks: set.len(),
        adopted: adopted
            .iter()
            .filter(|artist| artists.contains(artist.as_str()))
            .cloned()
            .collect(),
        new_played: played
            .iter()
            .filter(|artist| artists.contains(artist.as_str()))
            .cloned()
            .collect(),
        adopted_heavily: heavily
            .iter()
            .filter(|artist| artists.contains(artist.as_str()))
            .count(),
        owned: set
            .iter()
            .filter(|pick| owned.contains(&pick.release_group))
            .count(),
        heard: set
            .iter()
            .filter(|pick| heard.contains(&pick.artist))
            .count(),
        listeners: set
            .iter()
            .filter_map(|pick| pick.artist_listeners)
            .collect(),
        top_seed: (seeded && top_seed.is_some()).then(|| {
            set.iter()
                .filter(|pick| pick.strongest_seed.as_deref() == top_seed)
                .count()
        }),
    }
}

/// The share of `a`'s picks `b` repeats, by release group.
pub fn repeat(a: &[Scored], b: &[Scored]) -> Option<f64> {
    if a.is_empty() {
        return None;
    }
    let later: HashSet<&str> = b.iter().map(|pick| pick.release_group.as_str()).collect();
    let kept = a
        .iter()
        .filter(|pick| later.contains(pick.release_group.as_str()))
        .count();
    Some(kept as f64 / a.len() as f64)
}

/// The exact two-sided sign test of `a_only` against `b_only`: the chance
/// of a split at least this uneven if neither side is better.
pub fn sign_test(a_only: usize, b_only: usize) -> f64 {
    let n = a_only + b_only;
    if n == 0 {
        return 1.0;
    }
    let low = a_only.min(b_only);
    let mut choose = 1.0_f64;
    let mut tail = 0.0_f64;
    for k in 0..=low {
        if k > 0 {
            choose = choose * (n - k + 1) as f64 / k as f64;
        }
        tail += choose;
    }
    (2.0 * tail / 2.0_f64.powi(n as i32)).min(1.0)
}

/// The median of `values`, the lower of the middle two for an even count.
pub fn median(values: &[i64]) -> Option<i64> {
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    sorted.get(sorted.len().saturating_sub(1) / 2).copied()
}

/// `at` (unix ms) as a UTC date, `YYYY-MM-DD`.
pub fn day(at: i64) -> String {
    match time::OffsetDateTime::from_unix_timestamp(at.div_euclid(1000)) {
        Ok(moment) => moment.date().to_string(),
        Err(_) => at.to_string(),
    }
}

/// The start of the UTC day `text` (`YYYY-MM-DD`), unix ms.
pub fn parse_day(text: &str) -> Option<i64> {
    let format = time::macros::format_description!("[year]-[month]-[day]");
    let date = time::Date::parse(text.trim(), &format).ok()?;
    Some(date.midnight().assume_utc().unix_timestamp() * 1000)
}

/// Whether a release first out on `date` (`YYYY`, `YYYY-MM` or
/// `YYYY-MM-DD`) came out after `cut`. A date given only to the year or
/// month is read as its first day; an unknown date is not after.
pub fn released_after(date: Option<&str>, cut: i64) -> bool {
    let Some(date) = date.map(str::trim).filter(|date| date.len() >= 4) else {
        return false;
    };
    let padded = match date.len() {
        4 => format!("{date}-01-01"),
        7 => format!("{date}-01"),
        _ => date.chars().take(10).collect(),
    };
    parse_day(&padded).is_some_and(|out| out > cut)
}

/// Whether `date` gives only a year or a month, so [`released_after`]
/// cannot tell within it.
pub fn coarse_date(date: Option<&str>) -> bool {
    date.map(str::trim).is_some_and(|date| date.len() < 10)
}

/// The first of the month after the one `at` (unix ms) falls in, unix ms.
fn next_month(at: i64) -> i64 {
    let Ok(moment) = time::OffsetDateTime::from_unix_timestamp(at.div_euclid(1000)) else {
        return at + 31 * DAY_MS;
    };
    let date = moment.date();
    let (year, month) = match date.month() {
        time::Month::December => (date.year() + 1, time::Month::January),
        month => (date.year(), month.next()),
    };
    time::Date::from_calendar_date(year, month, 1)
        .map(|first| first.midnight().assume_utc().unix_timestamp() * 1000)
        .unwrap_or(at + 31 * DAY_MS)
}
