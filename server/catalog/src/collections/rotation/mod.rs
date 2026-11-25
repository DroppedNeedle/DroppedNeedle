//! Lists on rotation: up to five playlists the app makes for each person
//! from their own library and keeps fresh.
//!
//! Each list is one [`RotationIdea`], a rule over the person's own rows
//! (their plays here, ratings, hearts, played marks and follows) and the
//! library's facts. An idea goes live with enough songs. The always-on
//! ideas stay live while they qualify; the rest take turns: every Monday at
//! 00:00 UTC the unpinned ones are picked again, those live longest ago
//! first. Shuffled ideas draw the same songs all week from a seed of the
//! person, the idea and the week; ranked ideas follow the data.
//!
//! A live list is an ordinary `catalog_playlists` row, so the web UI and
//! both compat protocols list it; its row in `catalog_rotation_lists` is
//! what makes it a list on rotation. Only [`refresh`] writes its songs, so
//! nobody can edit it, its owner included; its owner may delete it, keep it
//! on rotation or save a copy. A refresh reads nothing outside the
//! database and writes a list's songs only when they changed.

pub mod job;
mod refresh;
pub(crate) mod sql;
pub(crate) mod store;

pub use refresh::refresh;
pub use store::Written;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// A day, ms.
pub const DAY_MS: i64 = 86_400_000;
/// A week, ms.
pub const WEEK_MS: i64 = 7 * DAY_MS;
/// How many lists are live at once.
pub const LIVE_LISTS: usize = 5;

/// One rule a list on rotation is made from: the values of
/// `catalog_rotation_lists.idea`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RotationIdea {
    /// The newest songs in the library.
    AddedRecently,
    /// Songs played twice or more in the last 30 days, most played first.
    OnRepeat,
    /// Songs rated four stars or more or hearted, alone or by album, and
    /// not played in six months.
    ForgottenFavourites,
    /// Songs never played on the 20 albums played most.
    DeepCuts,
    /// Songs rated five stars.
    FiveStars,
    /// Songs never played and never marked played.
    NeverPlayed,
    /// Songs by followed artists not played in 30 days.
    FollowedArtists,
    /// FLAC, WAV and ALAC files.
    Lossless,
    /// What was played in the 30 days up to this day a year ago.
    AYearAgo,
    /// Songs from one decade, the next one each time it comes on.
    Decade,
    /// Songs of seven minutes or more.
    LongOnes,
}

droppedneedle_kernel::spelled!(RotationIdea {
    AddedRecently => "added_recently",
    OnRepeat => "on_repeat",
    ForgottenFavourites => "forgotten_favourites",
    DeepCuts => "deep_cuts",
    FiveStars => "five_stars",
    NeverPlayed => "never_played",
    FollowedArtists => "followed_artists",
    Lossless => "lossless",
    AYearAgo => "a_year_ago",
    Decade => "decade",
    LongOnes => "long_ones",
});

impl RotationIdea {
    /// True for the ideas that are live whenever they have enough songs.
    pub const fn always_on(self) -> bool {
        matches!(self, Self::AddedRecently | Self::OnRepeat)
    }

    /// The fewest songs it goes live with.
    pub const fn bar(self) -> i64 {
        if self.always_on() { 10 } else { 20 }
    }

    /// How many songs its list holds.
    pub const fn size(self) -> i64 {
        match self {
            Self::OnRepeat => 40,
            _ => 50,
        }
    }

    /// True when its songs are drawn by the week's shuffle; false when they
    /// are ranked.
    pub const fn shuffled(self) -> bool {
        !matches!(self, Self::AddedRecently | Self::OnRepeat | Self::AYearAgo)
    }

    /// The playlist's name. The decade idea names its decade.
    pub fn name(self, decade: Option<i32>) -> String {
        match self {
            Self::AddedRecently => "Added recently".to_owned(),
            Self::OnRepeat => "On repeat".to_owned(),
            Self::ForgottenFavourites => "Forgotten favourites".to_owned(),
            Self::DeepCuts => "Deep cuts".to_owned(),
            Self::FiveStars => "Five stars".to_owned(),
            Self::NeverPlayed => "Never played".to_owned(),
            Self::FollowedArtists => "Artists you follow".to_owned(),
            Self::Lossless => "Lossless".to_owned(),
            Self::AYearAgo => "A year ago".to_owned(),
            Self::Decade => match decade {
                Some(decade) => format!("The {decade}s"),
                None => "A decade".to_owned(),
            },
            Self::LongOnes => "Long ones".to_owned(),
        }
    }
}

/// The start of the week `now_ms` falls in: Monday 00:00 UTC, ms. The
/// epoch was a Thursday.
pub fn week_start(now_ms: i64) -> i64 {
    let day = now_ms.div_euclid(DAY_MS);
    (day - (day + 3).rem_euclid(7)) * DAY_MS
}

/// When the lists next change after `now_ms`: the next Monday 00:00 UTC,
/// epoch seconds.
pub fn next_change_at(now_ms: i64) -> u64 {
    u64::try_from((week_start(now_ms) + WEEK_MS) / 1000).unwrap_or(0)
}

/// A stable hash of a person, an idea and a week, for the week's shuffle
/// and for breaking ties in a pick: FNV-1a, so it reads the same on every
/// build.
pub(crate) fn week_hash(user_id: &str, idea: RotationIdea, week_start: i64) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in user_id
        .bytes()
        .chain([0])
        .chain(idea.as_str().bytes())
        .chain([0])
        .chain(week_start.to_le_bytes())
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// What one idea looked like at a refresh, and where it stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Standing {
    /// The idea.
    pub idea: RotationIdea,
    /// How many songs match it.
    pub matches: i64,
    /// True while its list is live.
    pub live: bool,
    /// True when its owner keeps it on rotation.
    pub pinned: bool,
    /// When it was last live, ms; none when it never was.
    pub last_live_at: Option<i64>,
}

/// The ideas live after a refresh at `now_ms`, in idea order. On a new
/// week (`repick`: nothing was looked at since this Monday) the always-on
/// ideas that qualify go first, then the pinned lists still live, then the
/// rest to [`LIVE_LISTS`]: ideas not live now first, the one live longest
/// ago first (never live before all), ties broken by the week's hash. In
/// the week, the live lists stay while they have songs, and an always-on
/// idea joins when a list is free, unless the person deleted its list this
/// week: a list deleted in the week stays out until Monday.
pub(crate) fn pick(
    user_id: &str,
    ideas: &[Standing],
    now_ms: i64,
    repick: bool,
) -> Vec<RotationIdea> {
    let week = week_start(now_ms);
    let qualifies = |standing: &Standing| standing.matches >= standing.idea.bar();
    let mut live: Vec<RotationIdea> = Vec::new();
    if repick {
        live.extend(
            ideas
                .iter()
                .filter(|standing| standing.idea.always_on() && qualifies(standing))
                .map(|standing| standing.idea),
        );
        live.extend(
            ideas
                .iter()
                .filter(|standing| {
                    !standing.idea.always_on()
                        && standing.live
                        && standing.pinned
                        && qualifies(standing)
                })
                .map(|standing| standing.idea),
        );
        let mut rest: Vec<&Standing> = ideas
            .iter()
            .filter(|standing| {
                !standing.idea.always_on() && qualifies(standing) && !live.contains(&standing.idea)
            })
            .collect();
        rest.sort_by_key(|standing| {
            (
                standing.live,
                standing.last_live_at.unwrap_or(i64::MIN),
                week_hash(user_id, standing.idea, week),
            )
        });
        for standing in rest {
            if live.len() >= LIVE_LISTS {
                break;
            }
            live.push(standing.idea);
        }
    } else {
        live.extend(
            ideas
                .iter()
                .filter(|standing| standing.live && standing.matches > 0)
                .map(|standing| standing.idea),
        );
        for standing in ideas {
            let deleted_this_week =
                !standing.live && standing.last_live_at.is_some_and(|at| at >= week);
            if live.len() < LIVE_LISTS
                && standing.idea.always_on()
                && qualifies(standing)
                && !standing.live
                && !deleted_this_week
            {
                live.push(standing.idea);
            }
        }
    }
    live.truncate(LIVE_LISTS);
    let order = |idea: &RotationIdea| RotationIdea::ALL.iter().position(|one| one == idea);
    live.sort_by_key(order);
    live
}
