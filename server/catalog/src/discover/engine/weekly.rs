//! The weekly set: up to [`SET_SIZE`] albums chosen for one person once a
//! week from their pool, kept with what they were made from, and from then
//! on a record that changes only by what the person does.
//!
//! - A week starts at 00:00 server time on the person's set day
//!   ([`SetDay`], Friday unless they chose another) and runs seven days. A
//!   new set day counts from the first such day after the current week.
//! - One job kind ([`KIND`], keyed by the user id) builds the set for the
//!   week that is due, from stored rows only: the pool, the taste build and
//!   the library, and only albums found through the person's own artists.
//!   Eight found: the set is written. Fewer while the pool is complete: a
//!   short set with its [`ShortReason`], never padded. Fewer while the pool
//!   still waits for facts: nothing is written, and the job runs again once
//!   the pool is built again ([`schedule`](super::schedule) sees the new
//!   build). A pool missing or older than the taste build is never dealt
//!   from: it is queued first for someone seen in the last seven days.
//!   Someone away longer gets no pool build from here (a pool run may read
//!   ListenBrainz): an out-of-date pool waits for their visit, and an
//!   incomplete one is final for them, giving a short set.
//! - A linked person's first set waits for their ListenBrainz history to be
//!   read, for at most [`HISTORY_WAIT_MS`] from their first visit to
//!   Discover while it was being read; then it is built from what is there.
//! - An album an earlier set held in the last twelve weeks is not dealt
//!   again, and those picks count as shown for the artists' rest.
//!
//! [`WeeklySets`] also answers the routes: the week, "dig further" past the
//! set, and the set day.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::{Db, DbError, writer::Lane};
use droppedneedle_kernel::jobs::BoxFuture;
use droppedneedle_kernel::jobs::queue::{JobCtx, JobError, JobHandler, JobKind, KindSpec, Outcome};
use droppedneedle_kernel::page::{Page, PageRequest};
use droppedneedle_media::history::HistoryImportState;
use rusqlite::Transaction;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use utoipa::ToSchema;

use super::candidate::{Candidate, CandidateKind};
use super::evidence::Evidence;
use super::rank::proposed;
use super::select::{self, Exposure, Floors, Share};
use super::taste_store::Kept;
use super::weekly_store::{self, Context, NewPick, NewSet, StoredSet};
use super::{pool, pool_store, reason};
use crate::discover::cache::ForgetCounts;
use crate::discover::due::Due;
use crate::discover::facts::FactSource;
use crate::discover::facts::plan::SEEN_WINDOW_MS;
use crate::discover::feedback::Answers;
use crate::discover::listening::ListeningData;
use crate::discover::models::{
    AlbumRef, DigFurtherPick, DiscoverPrefs, PickReason, PickReasonKind, SetCounts, SetPick,
    SetSource, SetTaste, SourceFailing, WeekResponse, WeekStatus, WeeklySet,
};
use crate::discover::page::shelves;
use crate::discover::record::Offer;
use crate::metadata::models::ReleaseType;

/// The weekly set job kind.
pub const KIND: JobKind = JobKind::new("discover.weekly_set");

/// How weekly set jobs run: local reads and at most one write a person a
/// week, done rows kept a day since the job is asked again while a set
/// waits for its pool.
pub const SPEC: KindSpec = KindSpec::new(KIND)
    .max_attempts(3)
    .run_timeout(Duration::from_secs(5 * 60))
    .keep_done(Duration::from_secs(24 * 3600))
    .keep_failed(Duration::from_secs(7 * 24 * 3600));

/// The most picks a set holds.
pub const SET_SIZE: usize = 8;
/// A day, ms.
const DAY_MS: i64 = 86_400_000;
/// A week, ms.
pub const WEEK_MS: i64 = 7 * DAY_MS;
/// How long a linked person's first set waits for their history import,
/// from their first visit to Discover while it is being read.
pub const HISTORY_WAIT_MS: i64 = 6 * 3_600_000;
/// The most "dig further" albums one page holds.
pub const MORE_PAGE_MAX: usize = 50;
/// "Dig further" albums a page holds when none is asked for.
pub const MORE_PAGE_DEFAULT: usize = 20;
/// A history shorter than this is short.
const YEAR_MS: i64 = 365 * DAY_MS;

/// The day a person's weekly set comes out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SetDay {
    /// Monday.
    Monday,
    /// Tuesday.
    Tuesday,
    /// Wednesday.
    Wednesday,
    /// Thursday.
    Thursday,
    /// Friday, the default.
    Friday,
    /// Saturday.
    Saturday,
    /// Sunday.
    Sunday,
}

droppedneedle_kernel::spelled!(SetDay {
    Monday => "monday",
    Tuesday => "tuesday",
    Wednesday => "wednesday",
    Thursday => "thursday",
    Friday => "friday",
    Saturday => "saturday",
    Sunday => "sunday",
});

impl SetDay {
    /// The day with no choice made.
    pub const DEFAULT: Self = Self::Friday;

    /// Days after Monday.
    fn days_after_monday(self) -> i64 {
        match self {
            Self::Monday => 0,
            Self::Tuesday => 1,
            Self::Wednesday => 2,
            Self::Thursday => 3,
            Self::Friday => 4,
            Self::Saturday => 5,
            Self::Sunday => 6,
        }
    }
}

/// Why a set holds fewer than [`SET_SIZE`] picks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ShortReason {
    /// The person's history is under twelve months long.
    ShortHistory,
    /// The person has no plays at all: the set is made from the shelves.
    ShelvesOnly,
    /// The other candidates are owned, known or already answered.
    RestKnown,
    /// The pool was still waiting for facts: the rest could not be checked.
    NotChecked,
}

droppedneedle_kernel::spelled!(ShortReason {
    ShortHistory => "short_history",
    ShelvesOnly => "shelves_only",
    RestKnown => "rest_known",
    NotChecked => "not_checked",
});

impl ShortReason {
    /// Why a set of fewer than [`SET_SIZE`] holds what it holds, from the
    /// plays it counted, their span, and whether the pool was still waiting
    /// for facts.
    fn of(plays: i64, from: Option<i64>, to: Option<i64>, incomplete: bool) -> Self {
        match (plays, from, to) {
            (0, _, _) => Self::ShelvesOnly,
            (_, Some(from), Some(to)) if to - from >= YEAR_MS => {
                if incomplete {
                    Self::NotChecked
                } else {
                    Self::RestKnown
                }
            }
            _ => Self::ShortHistory,
        }
    }

    /// The line shown after "Only {n} this week.", with the history's span
    /// for a short history.
    pub fn line(self, from: Option<i64>, to: Option<i64>) -> String {
        match self {
            Self::ShortHistory => {
                let days = match (from, to) {
                    (Some(from), Some(to)) => (to - from).max(0) / DAY_MS,
                    _ => 0,
                };
                let months = ((days as f64 / 30.44).round() as i64).max(1);
                let long = if months == 1 {
                    "a month".to_owned()
                } else {
                    format!("{months} months")
                };
                format!(
                    "Your history is {long} long, and nothing from the charts is used to fill a set."
                )
            }
            Self::ShelvesOnly => {
                "Made from your shelves alone, and nothing from the charts is used to fill a set."
                    .to_owned()
            }
            Self::RestKnown => {
                "You own or know the rest, and nothing from the charts is used to fill a set."
                    .to_owned()
            }
            Self::NotChecked => "The rest could not be checked yet, and nothing from the charts is used to fill a set."
                .to_owned(),
        }
    }
}

/// The day holding `at_ms`, counted from 1970-01-01, and its weekday,
/// Monday 0.
fn day_of(at_ms: i64) -> (i64, i64) {
    let day = at_ms.div_euclid(DAY_MS);
    // 1970-01-01 was a Thursday.
    (day, (day + 3).rem_euclid(7))
}

/// 00:00 of the latest `day` at or before `now_ms`.
pub fn day_on_or_before(now_ms: i64, day: SetDay) -> i64 {
    let (today, weekday) = day_of(now_ms);
    (today - (weekday - day.days_after_monday()).rem_euclid(7)) * DAY_MS
}

/// 00:00 of the first `day` at or after `at_ms`.
pub fn day_on_or_after(at_ms: i64, day: SetDay) -> i64 {
    let (mut today, mut weekday) = day_of(at_ms);
    if at_ms.rem_euclid(DAY_MS) != 0 {
        today += 1;
        weekday = (weekday + 1) % 7;
    }
    (today + (day.days_after_monday() - weekday).rem_euclid(7)) * DAY_MS
}

/// The start of the week due a set at `now_ms` for a person whose set day
/// is `day` and whose newest set's week starts at `last`; `None` while
/// that set's week runs, or a new day's first week has not come yet.
pub fn due_week(now_ms: i64, day: SetDay, last: Option<i64>) -> Option<i64> {
    let start = day_on_or_before(now_ms, day);
    match last {
        Some(last) if start < last + WEEK_MS => None,
        _ => Some(start),
    }
}

/// When the set after the one whose week starts at `week_start_at` comes
/// out, on `day`.
pub fn next_set_at(week_start_at: i64, day: SetDay) -> i64 {
    day_on_or_after(week_start_at + WEEK_MS, day)
}

/// The ISO week number of the day holding `at_ms`.
pub fn week_number(at_ms: i64) -> i64 {
    OffsetDateTime::from_unix_timestamp(at_ms.div_euclid(1000))
        .map(|at| i64::from(at.iso_week()))
        .unwrap_or(0)
}

/// The service a failing fact comes from.
fn set_source(source: FactSource) -> SetSource {
    match source {
        FactSource::Labs | FactSource::ListenBrainz => SetSource::Listenbrainz,
        FactSource::MusicBrainz => SetSource::Musicbrainz,
        FactSource::LastFm => SetSource::Lastfm,
    }
}

/// An album row of a candidate or a kept pick.
pub(super) fn album_row(
    group: &str,
    title: &str,
    artist_name: &str,
    artist_mbid: &str,
    primary_type: Option<&str>,
    first_release_date: Option<&str>,
) -> AlbumRef {
    let mut album = shelves::album(
        Some(group),
        title,
        Some(artist_name),
        Some(artist_mbid),
        None,
    );
    album.release_type = ReleaseType::from_primary_type(primary_type);
    album.release_date = first_release_date
        .filter(|date| !date.is_empty())
        .map(str::to_owned);
    album.year = first_release_date
        .and_then(|date| date.get(..4))
        .and_then(|year| year.parse().ok());
    album
}

/// What the selection dealt from a person's pool.
struct Dealt {
    /// The picks, in the pool's order, with their reasons.
    picks: Vec<(Candidate, PickReason)>,
    /// The person's taste groups by number: anchor id, anchor name, share.
    groups: Vec<(i64, String, String, f64)>,
    /// Their seeds now, lowercased.
    seeds: HashSet<String>,
}

/// A person's ranked pool with what selecting from it reads: their tastes'
/// shares, what rests, and the albums their sets held lately.
struct Memory {
    /// The ranked pool, the gates that move applied.
    pool: Vec<Candidate>,
    /// Their taste groups by number: anchor id, anchor name, share.
    groups: Vec<(i64, String, String, f64)>,
    /// The groups' shares.
    shares: Vec<Share>,
    /// What rests: what they were shown and passed over, their sets'
    /// picks included.
    exposure: Exposure,
    /// The albums their sets held in the last twelve weeks.
    held: HashSet<String>,
    /// Their seeds now, lowercased.
    seeds: HashSet<String>,
}

/// What builds and serves each person's weekly set.
pub struct WeeklyInputs {
    /// The database.
    pub db: Db,
    /// The kernel clock.
    pub clock: Arc<dyn Clock>,
    /// The person's accounts, for whether their history is linked.
    pub listening: ListeningData,
    /// The forget counts every page cache shares.
    pub forgets: ForgetCounts,
    /// Where a pool build a set needs is queued.
    pub due: Arc<Due>,
    /// What people told Discover, whose lapsed answers a build drops.
    pub answers: Arc<Answers>,
}

/// Builds each person's weekly set ([`KIND`]) and answers its routes.
#[derive(Clone)]
pub struct WeeklySets {
    db: Db,
    clock: Arc<dyn Clock>,
    listening: ListeningData,
    forgets: ForgetCounts,
    due: Arc<Due>,
    evidence: Evidence,
    answers: Arc<Answers>,
}

impl WeeklySets {
    /// The sets over `inputs`.
    pub fn new(inputs: WeeklyInputs) -> Self {
        Self {
            evidence: Evidence::new(inputs.db.clone()),
            db: inputs.db,
            clock: inputs.clock,
            listening: inputs.listening,
            forgets: inputs.forgets,
            due: inputs.due,
            answers: inputs.answers,
        }
    }

    /// Whether `user_id`'s first set still waits for their history at
    /// `now_ms`: they have no set, their ListenBrainz is linked and read,
    /// their import is not done, and it is under [`HISTORY_WAIT_MS`] since
    /// their first visit while it was being read.
    async fn waits_for_history(&self, user_id: &str, context: &Context, now_ms: i64) -> bool {
        if context.last_week_at.is_some() {
            return false;
        }
        let finished = matches!(
            context.import_state,
            Some(
                HistoryImportState::Current
                    | HistoryImportState::Failed
                    | HistoryImportState::Forgotten
            )
        );
        if finished
            || context
                .wait_from_at
                .is_some_and(|from| now_ms >= from + HISTORY_WAIT_MS)
        {
            return false;
        }
        self.listening.listenbrainz_on()
            && self
                .listening
                .accounts(user_id)
                .await
                .listenbrainz
                .is_some()
    }

    /// Whether `user_id` used the app in the last seven days, the window
    /// background reads keep to.
    async fn seen_lately(&self, user_id: &str, now_ms: i64) -> Result<bool, DbError> {
        let seen =
            droppedneedle_auth::sqlite::seen_since(&self.db, now_ms - SEEN_WINDOW_MS).await?;
        Ok(seen.iter().any(|user| user == user_id))
    }

    /// `user_id`'s ranked pool at `now_ms` and what selecting from it
    /// reads. The picks of their sets of the last twelve weeks count as
    /// shown when their set was built.
    async fn memory(&self, user_id: &str, now_ms: i64) -> Result<Memory, DbError> {
        let db = &self.db;
        let since = now_ms - proposed::REST_MS;
        let earlier = weekly_store::earlier_picks(db, user_id, since).await?;
        let held = earlier
            .iter()
            .map(|pick| pick.release_group.clone())
            .collect();
        let pool = pool_store::ranked(db, user_id, now_ms).await?;
        let groups = weekly_store::taste_groups(db, user_id).await?;
        let seeds = pool_store::seed_ids(db, user_id).await?;
        let shares = groups
            .iter()
            .map(|(group, _, _, share)| Share {
                group: *group,
                share: *share,
            })
            .collect();
        let mut shown = pool_store::shown(db, user_id, since).await?;
        shown.extend(earlier);
        Ok(Memory {
            pool,
            groups,
            shares,
            exposure: Exposure::from_shown(&shown, now_ms),
            held,
            seeds,
        })
    }

    /// Deal up to [`SET_SIZE`] picks from `user_id`'s pool at `now_ms`, as
    /// the engine selects them. Only albums found through the person's own
    /// artists go in a set: a service's picks, and any other kind, are left
    /// to dig further, so they never fill a slot. An album an earlier set
    /// held in the last twelve weeks is left out.
    async fn deal(&self, user_id: &str, now_ms: i64) -> Result<Dealt, DbError> {
        let memory = self.memory(user_id, now_ms).await?;
        let pool: Vec<Candidate> = memory
            .pool
            .into_iter()
            .filter(|candidate| {
                candidate.kind == CandidateKind::NewArtist
                    && !memory.held.contains(&candidate.release_group_mbid)
            })
            .collect();
        let selection = select::select(
            &pool,
            SET_SIZE,
            &memory.shares,
            &memory.exposure,
            Floors::PROPOSED,
        );
        let picks = selection
            .picks
            .into_iter()
            .filter_map(|candidate| {
                Some((candidate.clone(), reason::reason(candidate, &memory.seeds)?))
            })
            .collect();
        Ok(Dealt {
            picks,
            groups: memory.groups,
            seeds: memory.seeds,
        })
    }

    /// Run `complete` in a write of its own, queuing `user_id`'s pool build
    /// first when `pool_first`.
    async fn finish(
        &self,
        user_id: &str,
        pool_first: bool,
        complete: impl FnOnce(&Transaction) -> Result<(), JobError> + Send + 'static,
    ) -> Result<(), JobError> {
        let (due, user) = (Arc::clone(&self.due), user_id.to_owned());
        self.db
            .write(Lane::Background, "discover.weekly.wait", move |tx| {
                if pool_first {
                    due.queue_now(tx, pool::KIND, &user)?;
                }
                complete(tx)
            })
            .await
    }

    /// Build `user_id`'s set for the week due now, when it can be built;
    /// `complete` runs inside the write. Safe to repeat: a week holds one
    /// set, and a set once written is never touched again.
    ///
    /// # Errors
    ///
    /// A read's or the write's failure.
    pub async fn run_once(
        &self,
        user_id: &str,
        complete: impl FnOnce(&Transaction) -> Result<(), JobError> + Send + 'static,
    ) -> Result<(), JobError> {
        self.answers.prune_if_due().await;
        let forgets_at = self.forgets.of(user_id);
        let now_ms = self.clock.now_ms();
        let Some(context) = weekly_store::context(&self.db, user_id).await? else {
            return self.finish(user_id, false, complete).await;
        };
        let Some(week_start_at) = due_week(now_ms, context.set_day, context.last_week_at) else {
            return self.finish(user_id, false, complete).await;
        };
        if self.waits_for_history(user_id, &context, now_ms).await {
            return self.finish(user_id, false, complete).await;
        }
        let Some(taste) = context.taste.as_ref() else {
            return self.finish(user_id, false, complete).await;
        };
        // Someone not seen in seven days gets no pool build from here: a
        // pool run may read ListenBrainz.
        let lately = self.seen_lately(user_id, now_ms).await?;
        let pool = match context.pool.as_ref() {
            Some(pool) if pool.built_at >= taste.built_at => pool,
            // Never dealt: its groups belong to the taste build before.
            _ => return self.finish(user_id, lately, complete).await,
        };
        let dealt = self.deal(user_id, now_ms).await?;

        let mut picks = Vec::with_capacity(dealt.picks.len());
        for (candidate, reason) in &dealt.picks {
            match self
                .evidence
                .of_candidate(user_id, candidate, &dealt.seeds)
                .await?
            {
                Some(evidence) => picks.push((candidate, reason, evidence)),
                None => tracing::warn!(
                    group = %candidate.release_group_mbid,
                    "a pick with no evidence left out of a weekly set"
                ),
            }
        }
        // A pool still waiting for facts writes a set only once it gives
        // eight, or for someone away, whose facts will not come meanwhile.
        if picks.is_empty() || (picks.len() < SET_SIZE && pool.incomplete && lately) {
            return self.finish(user_id, false, complete).await;
        }
        let plays = taste.plays_counted;
        let listed: HashMap<i64, usize> = dealt
            .groups
            .iter()
            .enumerate()
            .map(|(at, (group, ..))| (*group, at))
            .collect();
        // The groups that got a slot, largest share first.
        let mut slotted: Vec<usize> = picks
            .iter()
            .filter_map(|(candidate, ..)| listed.get(&candidate.group?).copied())
            .collect::<HashSet<usize>>()
            .into_iter()
            .collect();
        slotted.sort_by(|a, b| {
            dealt.groups[*b]
                .3
                .total_cmp(&dealt.groups[*a].3)
                .then(dealt.groups[*a].0.cmp(&dealt.groups[*b].0))
        });
        let taste_of = |candidate: &Candidate| {
            let at = listed.get(&candidate.group?)?;
            slotted.iter().position(|slot| slot == at)
        };
        let tastes: Vec<SetTaste> = slotted
            .iter()
            .enumerate()
            .map(|(no, at)| {
                let (_, anchor_mbid, anchor_name, share) = &dealt.groups[*at];
                SetTaste {
                    name: format!("Around {}", anchor_name.trim()),
                    anchor_artist_mbid: anchor_mbid.clone(),
                    share: (plays > 0).then_some(*share),
                    picks: i64::try_from(
                        picks
                            .iter()
                            .filter(|(candidate, ..)| taste_of(candidate) == Some(no))
                            .count(),
                    )
                    .unwrap_or(0),
                }
            })
            .collect();
        let short_reason = (picks.len() < SET_SIZE).then(|| {
            ShortReason::of(
                plays,
                taste.history_from_at,
                taste.history_to_at,
                pool.incomplete,
            )
        });
        let shelves = weekly_store::shelves(&self.db).await?;
        let counts = SetCounts {
            plays_counted: plays,
            history_from_at: taste.history_from_at,
            history_to_at: taste.history_to_at,
            seeds: taste.seeds,
            neighbours_found: Some(pool.neighbours_found),
            known_left_out: Some(pool.known_left_out),
            hubs_left_out: pool.hubs_left_out,
            shelf_artists: shelves.artists,
            shelf_albums: shelves.albums,
            linked: context.import_state == Some(HistoryImportState::Current),
        };
        let new_picks = picks
            .iter()
            .enumerate()
            .map(|(no, (candidate, reason, evidence))| NewPick {
                pick_no: i64::try_from(no + 1).unwrap_or(i64::MAX),
                taste_no: taste_of(candidate).and_then(|no| i64::try_from(no).ok()),
                candidate: (*candidate).clone(),
                reason: (*reason).clone(),
                evidence: evidence.clone(),
                offer: Offer::from_pick(candidate, reason),
            })
            .collect();
        let set = NewSet {
            week_start_at,
            built_at: now_ms,
            short_reason,
            counts,
            tastes,
            picks: new_picks,
        };
        let kept =
            weekly_store::keep(&self.db, &self.forgets, user_id, forgets_at, set, complete).await?;
        if kept == Kept::Dropped {
            tracing::debug!("a weekly set outlived a forget or its account; nothing kept");
            return Ok(());
        }
        tracing::info!(
            user = %user_id,
            week_start_at,
            picks = picks.len(),
            "discover weekly set built"
        );
        Ok(())
    }

    /// The person's set as stored, with each pick's state now.
    async fn whole(
        &self,
        user_id: &str,
        stored: StoredSet,
        day: SetDay,
        now_ms: i64,
    ) -> Result<WeeklySet, DbError> {
        let rows = weekly_store::set_picks(&self.db, user_id, stored.week_start_at, now_ms).await?;
        let picks = rows
            .into_iter()
            .map(|row| {
                let reason = match serde_json::from_str::<crate::discover::models::PickEvidence>(
                    &row.evidence,
                ) {
                    Ok(evidence) => evidence.reason,
                    // A set holds only picks through the person's artists,
                    // so its stored line is that kind of reason.
                    Err(error) => {
                        tracing::warn!(%error, "a kept pick's evidence does not read; its line stands");
                        PickReason {
                            kind: PickReasonKind::YourArtists,
                            line: row.reason_line.clone(),
                            artists: Vec::new(),
                        }
                    }
                };
                SetPick {
                    pick_no: row.pick_no,
                    album: album_row(
                        &row.release_group_mbid,
                        &row.title,
                        &row.artist_name,
                        &row.artist_mbid,
                        row.primary_type.as_deref(),
                        row.first_release_date.as_deref(),
                    ),
                    reason,
                    taste_no: row.taste_no,
                    state: row.state,
                    heard: row.heard,
                }
            })
            .collect();
        Ok(WeeklySet {
            week_start_at: stored.week_start_at,
            week_number: week_number(stored.week_start_at),
            built_at: stored.built_at,
            next_set_at: next_set_at(stored.week_start_at, day),
            short_line: stored.short_reason.map(|reason| {
                reason.line(stored.counts.history_from_at, stored.counts.history_to_at)
            }),
            short_reason: stored.short_reason,
            counts: stored.counts,
            tastes: stored.tastes,
            picks,
        })
    }

    /// `user_id`'s week now: where it stands and the set to show.
    ///
    /// # Errors
    ///
    /// A read's failure.
    pub async fn week(&self, user_id: &str) -> Result<WeekResponse, DbError> {
        let now_ms = self.clock.now_ms();
        let answer = |status, set| WeekResponse {
            status,
            found_so_far: None,
            failing: None,
            set,
        };
        let Some(context) = weekly_store::context(&self.db, user_id).await? else {
            return Ok(answer(WeekStatus::NothingYet, None));
        };
        let latest = match weekly_store::latest(&self.db, user_id).await? {
            Some(stored) => Some(self.whole(user_id, stored, context.set_day, now_ms).await?),
            None => None,
        };
        if due_week(now_ms, context.set_day, context.last_week_at).is_none() {
            return Ok(answer(WeekStatus::Ready, latest));
        }
        if self.waits_for_history(user_id, &context, now_ms).await {
            return Ok(answer(WeekStatus::ReadingHistory, latest));
        }
        let Some(taste) = context.taste.as_ref() else {
            return Ok(answer(WeekStatus::NothingYet, latest));
        };
        let lately = self.seen_lately(user_id, now_ms).await?;
        let current = context
            .pool
            .as_ref()
            .filter(|pool| pool.built_at >= taste.built_at);
        if current.is_none() && !lately {
            // Away a while: the set waits for their next visit.
            let status = if latest.is_some() {
                WeekStatus::KeptLastWeek
            } else {
                WeekStatus::NothingYet
            };
            return Ok(answer(status, latest));
        }
        let waiting = current.is_none_or(|pool| pool.incomplete && lately);
        // A pool older than the taste build is never dealt from.
        let found = match current {
            Some(_) => self.deal(user_id, now_ms).await?.picks.len(),
            None => 0,
        };
        if waiting {
            if latest.is_some() {
                let mut asked = context
                    .pool
                    .as_ref()
                    .map(|pool| pool.waiting_for.clone())
                    .unwrap_or_default();
                asked.extend(pool_store::seed_ids(&self.db, user_id).await?);
                if let Some((source, since)) = weekly_store::failing(&self.db, &asked).await? {
                    return Ok(WeekResponse {
                        status: WeekStatus::KeptLastWeek,
                        found_so_far: None,
                        failing: Some(SourceFailing {
                            source: set_source(source),
                            since,
                        }),
                        set: latest,
                    });
                }
            }
        } else if found == 0 {
            return Ok(answer(WeekStatus::NothingYet, latest));
        }
        Ok(WeekResponse {
            status: WeekStatus::BeingSetOut,
            found_so_far: Some(i64::try_from(found).unwrap_or(0)),
            failing: None,
            set: latest,
        })
    }

    /// One page of "dig further": `user_id`'s ranked pool past their newest
    /// set, in the pool's order: the floors, one album an artist and none by
    /// the set's artists, and what they were shown and passed over resting.
    /// Every kind of pick is included, and no seed is held to two albums.
    ///
    /// # Errors
    ///
    /// A read's failure.
    pub async fn more(
        &self,
        user_id: &str,
        paging: PageRequest<u64>,
    ) -> Result<Page<DigFurtherPick>, DbError> {
        let now_ms = self.clock.now_ms();
        let in_set = weekly_store::latest_picks(&self.db, user_id).await?;
        let memory = self.memory(user_id, now_ms).await?;
        let groups: HashSet<&str> = in_set.iter().map(|(group, _)| group.as_str()).collect();
        let mut artists: HashSet<&str> = in_set.iter().map(|(_, artist)| artist.as_str()).collect();
        let (exposure, seeds) = (&memory.exposure, &memory.seeds);
        let past: Vec<DigFurtherPick> = memory
            .pool
            .iter()
            .filter(|candidate| {
                !groups.contains(candidate.release_group_mbid.as_str())
                    && Floors::PROPOSED.clears(candidate)
                    && !exposure.rests(candidate)
            })
            .filter_map(|candidate| {
                let reason = reason::reason(candidate, seeds)?;
                if !artists.insert(candidate.artist_mbid.as_str()) {
                    return None;
                }
                Some(DigFurtherPick {
                    album: album_row(
                        &candidate.release_group_mbid,
                        &candidate.title,
                        &candidate.artist_name,
                        &candidate.artist_mbid,
                        candidate.primary_type.as_deref(),
                        candidate.first_release_date.as_deref(),
                    ),
                    reason,
                })
            })
            .collect();
        let total = u64::try_from(past.len()).unwrap_or(u64::MAX);
        let offset = paging.after.unwrap_or(0);
        let limit = u64::try_from(paging.limit).unwrap_or(u64::MAX);
        let items = past
            .into_iter()
            .skip(usize::try_from(offset).unwrap_or(usize::MAX))
            .take(paging.limit)
            .collect();
        Ok(Page::at_offset(items, offset, limit, total))
    }

    /// `user_id`'s set day.
    ///
    /// # Errors
    ///
    /// The read's failure.
    pub async fn prefs(&self, user_id: &str) -> Result<DiscoverPrefs, DbError> {
        Ok(DiscoverPrefs {
            set_day: weekly_store::set_day(&self.db, user_id)
                .await?
                .unwrap_or(SetDay::DEFAULT),
        })
    }

    /// Keep `user_id`'s set day. The set of the week running stays; the new
    /// day counts from the first such day after it.
    ///
    /// # Errors
    ///
    /// The write's failure.
    pub async fn save_prefs(
        &self,
        user_id: &str,
        prefs: DiscoverPrefs,
    ) -> Result<DiscoverPrefs, DbError> {
        weekly_store::save_set_day(&self.db, user_id, prefs.set_day, self.clock.now_ms()).await?;
        Ok(prefs)
    }

    /// The first pick of `user_id`'s set for the week running that they
    /// have not answered, for Home's teaser; `None` when there is none.
    ///
    /// # Errors
    ///
    /// A read's failure.
    pub async fn next_open_pick(&self, user_id: &str) -> Result<Option<SetPick>, DbError> {
        let now_ms = self.clock.now_ms();
        let Some(context) = weekly_store::context(&self.db, user_id).await? else {
            return Ok(None);
        };
        if due_week(now_ms, context.set_day, context.last_week_at).is_some() {
            return Ok(None);
        }
        let Some(stored) = weekly_store::latest(&self.db, user_id).await? else {
            return Ok(None);
        };
        let set = self.whole(user_id, stored, context.set_day, now_ms).await?;
        Ok(set
            .picks
            .into_iter()
            .find(|pick| pick.state == crate::discover::models::PickState::Open))
    }
}

impl JobHandler for WeeklySets {
    fn run(&self, job: JobCtx) -> BoxFuture<'static, Outcome> {
        let runner = self.clone();
        Box::pin(async move {
            let user_id = job.key().to_owned();
            match runner.run_once(&user_id, move |tx| job.complete(tx)).await {
                Ok(()) => Outcome::Done,
                Err(error) if error.is_busy() => Outcome::Busy,
                // Another worker holds the job: its run keeps the set.
                Err(JobError::LeaseLost) => Outcome::Done,
                Err(error) => {
                    tracing::warn!(%error, "a weekly set not kept; trying again");
                    Outcome::Retry {
                        code: "DISCOVER_WEEKLY_SET_FAILED",
                        at: None,
                    }
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 2026-11-20 was a Friday.
    const FRIDAY_20_NOV: i64 = 1_795_132_800_000;

    #[test]
    fn a_week_starts_at_midnight_on_the_set_day_and_a_new_day_waits_for_the_next() {
        let thursday_noon = FRIDAY_20_NOV + 6 * DAY_MS + 12 * 3_600_000;
        assert_eq!(
            day_on_or_before(thursday_noon, SetDay::Friday),
            FRIDAY_20_NOV
        );
        assert_eq!(
            day_on_or_before(FRIDAY_20_NOV, SetDay::Friday),
            FRIDAY_20_NOV
        );
        assert_eq!(week_number(FRIDAY_20_NOV), 47);
        assert_eq!(
            due_week(thursday_noon, SetDay::Friday, Some(FRIDAY_20_NOV)),
            None
        );
        assert_eq!(
            due_week(FRIDAY_20_NOV + WEEK_MS, SetDay::Friday, Some(FRIDAY_20_NOV)),
            Some(FRIDAY_20_NOV + WEEK_MS)
        );
        // Moved to Monday on the Sunday: this week's set holds, and the
        // first Monday after it starts the next.
        let sunday = FRIDAY_20_NOV + 2 * DAY_MS;
        assert_eq!(
            due_week(sunday + DAY_MS, SetDay::Monday, Some(FRIDAY_20_NOV)),
            None
        );
        let monday_after = FRIDAY_20_NOV + 10 * DAY_MS;
        assert_eq!(
            due_week(monday_after, SetDay::Monday, Some(FRIDAY_20_NOV)),
            Some(monday_after)
        );
        assert_eq!(next_set_at(FRIDAY_20_NOV, SetDay::Monday), monday_after);
        assert_eq!(
            next_set_at(FRIDAY_20_NOV, SetDay::Friday),
            FRIDAY_20_NOV + WEEK_MS
        );
    }
}
