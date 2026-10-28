//! The acquire flow loops: wanted watcher, follow poll, upgrade sweep,
//! request-status sweep and store prune.
//!
//! Each loop is a `*_tick` function that runs one pass at an explicit
//! `now`, so tests call it directly, plus a `spawn_*` function that runs it
//! on the loop supervisor under its job name. The supervisor owns the
//! waiting, jitter, liveness rows and stop; every wake runs a pass.
//!
//! The wanted and follow loops spread each wait over 0.8 to 1.2 of the
//! interval; the sync and prune loops wait the exact interval.

use std::collections::HashSet;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::jobs::{
    AlreadyRunning, Next, Schedule, Supervisor, Tick, TickCtx, TickError, TickResult,
};

use super::seams::{FlowEvent, ObservedRelease, ReleasePoll, TickSink};
use super::stores::{AdminDirectory, CutoffList, LibraryPresence, UpgradePolicy};
use crate::dispatch::{
    DispatchOrigin, DispatchOutcome, DispatchRequest, DispatchTarget, DownloadDispatch,
};
use crate::downloads::journal::Journal;
use crate::downloads::state::TaskStatus;
use crate::follows::Follows;
use crate::follows::cursors::{FeedRelease, FollowCursor, Follower, PendingRelease};
use crate::requests::error::RequestsError;
use crate::requests::ledger::{RequestStatus, WantedWatch, WatchState};
use crate::requests::models::RequestKind;
use crate::requests::service::{FollowIntake, FollowRelease, RequestsService};
use crate::requests::settle::announce_fulfilled;
use crate::requests::sqlite::{RequestStore, WantedStore, WatchCheck};
use crate::search_jobs::store::JobStore;
use crate::target::Targets;
use droppedneedle_catalog::reads::catalog::mapping::{should_include_release, type_set};
use droppedneedle_kernel::events::{AutoDownloadEnqueued, UserNotice, WantedNotice};
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::secret_sections::AdvancedSettings;
use droppedneedle_kernel::runtime_config::sections::{
    DownloadPolicy, UserPreferences, WantedWatcher,
};

/// Wanted-watcher sweep cadence in seconds.
pub const WANTED_INTERVAL: Duration = Duration::from_secs(900);
/// Wanted-watcher startup delay in seconds.
pub const WANTED_INITIAL_DELAY: Duration = Duration::from_secs(240);
/// Follow poll wake-up cadence in seconds;
/// each wake polls only the artists that are due.
pub const FOLLOW_INTERVAL: Duration = Duration::from_secs(60);
/// Follow poll startup delay in seconds.
pub const FOLLOW_INITIAL_DELAY: Duration = Duration::from_secs(300);
/// Follow poll due-driven cap: provider work happens only for due artists,
/// at most this many per tick.
pub const FOLLOW_MAX_ARTISTS_PER_TICK: usize = 10;
/// How long after a successful poll an artist is due again: a day.
pub const FOLLOW_ARTIST_REPOLL: Duration = Duration::from_secs(86_400);
/// How long after a failed poll an artist is tried again.
pub const FOLLOW_ARTIST_RETRY: Duration = Duration::from_secs(3_600);
/// Upgrade-scan startup delay in seconds.
pub const UPGRADE_INITIAL_DELAY: Duration = Duration::from_secs(900);
/// Request-status-sweep cadence in seconds.
pub const SYNC_INTERVAL: Duration = Duration::from_secs(60);
/// Status-sweep startup delay in seconds.
pub const SYNC_INITIAL_DELAY: Duration = Duration::from_secs(15);

/// Registry name for the wanted-watcher loop.
pub const WANTED_JOB: &str = "wanted-watcher";
/// Registry name for the follow new-release poll.
pub const FOLLOW_JOB: &str = "follow-new-release-poll";
/// Registry name for the background upgrade scan.
pub const UPGRADE_JOB: &str = "background-upgrade-scan";
/// Registry name for the request-status sweep.
pub const SYNC_JOB: &str = "request-status-sync";
/// Registry name for the store prune.
pub const PRUNE_JOB: &str = "store-prune";
/// Store-prune startup delay.
pub const PRUNE_INITIAL_DELAY: Duration = Duration::from_secs(600);
/// How often the prune loop checks whether its configured interval passed.
pub const PRUNE_TICK: Duration = Duration::from_secs(3600);
/// How often the upgrade loop checks whether its configured interval
/// passed; the interval itself is read on every check.
pub const UPGRADE_TICK: Duration = Duration::from_secs(3600);

/// Wanted-watcher settings, re-read every sweep so flipping the toggle
/// needs no restart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WantedSettings {
    /// The watcher runs at all.
    pub enabled: bool,
    /// Also enrol `incomplete` (partial-album) rows, not just `failed`.
    pub watch_partial_albums: bool,
    /// Due watches checked per sweep, at most.
    pub max_checks_per_sweep: usize,
    /// Dispatch on find. Off means badge-only: candidates tick
    /// `wanted.found` and wait for a hand dispatch instead.
    pub auto_download_on_find: bool,
}

impl WantedSettings {
    /// The watcher settings the `wanted` section sets.
    pub fn from_section(section: &WantedWatcher) -> Self {
        Self {
            enabled: section.enabled,
            watch_partial_albums: section.watch_partial_albums,
            max_checks_per_sweep: crate::settings::clamp_usize(section.max_checks_per_sweep).max(1),
            auto_download_on_find: section.auto_download_on_find,
        }
    }
}

/// What one wanted sweep accomplished.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WantedSummary {
    /// Requests newly enrolled as watches.
    pub enrolled: usize,
    /// Due watches checked.
    pub checked: usize,
    /// Watches that auto-dispatched a download.
    pub dispatched: usize,
    /// Watches satisfied from the library.
    pub fulfilled: usize,
    /// Watches that errored (each isolated, the sweep continued).
    pub errors: usize,
}

/// Wanted-watcher dependencies.
pub struct WantedDeps {
    /// The watcher settings, read fresh every sweep.
    pub settings: Live<WantedWatcher>,
    /// Watch registry (shared with the wanted view).
    pub watches: WantedStore,
    /// Request ledger for enrolment.
    pub ledger: RequestStore,
    /// The download sources a watch searches: those turned on, plugin
    /// sources included.
    pub sources: crate::downloads::worker::SourceList,
    /// Download dispatch.
    pub downloads: Arc<DownloadDispatch>,
    /// Library presence for satisfaction checks.
    pub library: Arc<LibraryPresence>,
    /// Release-group dates from MusicBrainz, for the recheck cadence.
    pub targets: Arc<Targets>,
    /// Durable ticks.
    pub ticks: Arc<dyn TickSink>,
}

/// Epoch seconds as the watch store keeps them.
fn epoch(now: i64) -> u64 {
    u64::try_from(now).unwrap_or(0)
}

/// Run one wanted sweep: enrol availability-dead requests, then
/// check due watches. Disabled reads as an empty pass. One bad watch never
/// kills the sweep.
pub async fn wanted_tick(now: i64, deps: &WantedDeps) -> WantedSummary {
    let Some(section) = deps.settings.read() else {
        return WantedSummary::default();
    };
    let settings = WantedSettings::from_section(&section);
    if !settings.enabled {
        return WantedSummary::default();
    }
    let mut summary = WantedSummary {
        enrolled: enrol_watches(now, &settings, deps).await,
        ..WantedSummary::default()
    };
    let due = match deps
        .watches
        .list_due(epoch(now), settings.max_checks_per_sweep)
        .await
    {
        Ok(due) => due,
        Err(error) => {
            tracing::warn!(?error, "wanted sweep could not list due watches");
            summary.errors += 1;
            return summary;
        }
    };
    for watch in due {
        summary.checked += 1;
        if let Err(error) = check_watch(now, &settings, deps, &watch, &mut summary).await {
            tracing::warn!(key = %watch.key, ?error, "wanted check failed");
            summary.errors += 1;
        }
    }
    summary
}

/// Check one due watch: satisfied from the library, dispatched on a find,
/// badged on a find when auto-download is off, or rescheduled quietly.
async fn check_watch(
    now: i64,
    settings: &WantedSettings,
    deps: &WantedDeps,
    watch: &WantedWatch,
    summary: &mut WantedSummary,
) -> Result<(), RequestsError> {
    let at = epoch(now);
    if deps.library.contains(&watch.key).await {
        let fulfilled = deps
            .ledger
            .settle_in_library(&watch.key, "in_library", at)
            .await?;
        announce_fulfilled(deps.ticks.as_ref(), fulfilled, now);
        notify_wanted(deps, watch, UserNotice::WantedFulfilled, |_| {});
        summary.fulfilled += 1;
        return Ok(());
    }
    let searched = match search_watch(deps, watch).await {
        Ok(None) => {
            // No source is on, or the download settings do not read: the
            // watch stays due, unrecorded, and is checked once that
            // changes.
            tracing::debug!(key = %watch.key, "wanted check waits for a download source");
            return Ok(());
        }
        Ok(Some(candidates)) => Ok(candidates),
        Err(cause) => Err(cause),
    };
    let date = watch_date(
        &deps.targets,
        &watch.key,
        watch.first_release_date.as_deref(),
        watch.year,
    )
    .await;
    let next = |streak: u32| {
        at + u64::try_from(interval_seconds(date.schedule.as_deref(), streak, now)).unwrap_or(0)
    };
    let found = date.found;
    let check = |quiet_streak: u32, outcome: &'static str| WatchCheck {
        quiet_streak,
        next_check_at: next(quiet_streak),
        now: at,
        outcome,
        first_release_date: found.clone(),
        ..WatchCheck::default()
    };
    match searched {
        Ok(candidates) if !candidates.is_empty() => {
            let seen: Vec<(String, String)> = candidates
                .iter()
                .map(|candidate| (candidate.view.source.clone(), candidate.seen_identity()))
                .collect();
            if !settings.auto_download_on_find {
                // Only copies the watch has not seen before badge it and
                // tell the owner.
                let known = deps.watches.seen(&watch.key).await?;
                let fresh = seen.iter().filter(|pair| !known.contains(*pair)).count();
                if fresh == 0 {
                    let streak = watch.quiet_streak.saturating_add(1);
                    deps.watches
                        .record_check(
                            &watch.key,
                            WatchCheck {
                                seen,
                                ..check(streak, "seen")
                            },
                        )
                        .await?;
                    return Ok(());
                }
                let fresh = u32::try_from(fresh).unwrap_or(u32::MAX);
                deps.watches
                    .record_check(
                        &watch.key,
                        WatchCheck {
                            seen,
                            new_candidates: Some(fresh),
                            ..check(0, "found")
                        },
                    )
                    .await?;
                deps.ticks.emit(
                    "wanted.found",
                    &format!(
                        "wanted {} has {fresh} new candidate(s) (badge-only)",
                        watch.key
                    ),
                    now,
                );
                notify_wanted(deps, watch, UserNotice::WantedNewCandidates, |notice| {
                    notice.new_candidates = Some(fresh);
                });
                return Ok(());
            }
            let dispatch = album_dispatch(
                &watch.user_id,
                &watch.key,
                &watch.artist_name,
                &watch.album_title,
                DispatchOrigin::Wanted,
                // One dispatch per check slot: a crash between the dispatch
                // and the reschedule answers the same task next time.
                Some(format!("wanted:{}:{}", watch.key, watch.check_count)),
            );
            match deps.downloads.dispatch(&dispatch).await {
                Ok(outcome) => {
                    let task_id = outcome.task_id().to_owned();
                    deps.ledger
                        .link_task(RequestKind::Album, &watch.key, &task_id, None)
                        .await?;
                    deps.watches
                        .record_check(
                            &watch.key,
                            WatchCheck {
                                seen,
                                ..check(0, "dispatched")
                            },
                        )
                        .await?;
                    if !outcome.started() {
                        // Another download of the album is already working.
                        return Ok(());
                    }
                    deps.ticks.emit(
                        "wanted.dispatched",
                        &format!("wanted {} auto-dispatched as {task_id}", watch.key),
                        now,
                    );
                    notify_wanted(deps, watch, UserNotice::WantedAutoDispatched, |notice| {
                        notice.task_id = Some(task_id.clone());
                    });
                    summary.dispatched += 1;
                }
                Err(cause) => {
                    tracing::warn!(key = %watch.key, %cause, "wanted dispatch failed");
                    deps.watches
                        .record_check(&watch.key, check(0, "error"))
                        .await?;
                    summary.errors += 1;
                }
            }
        }
        Ok(_) => {
            let streak = watch.quiet_streak.saturating_add(1);
            deps.watches
                .record_check(&watch.key, check(streak, "quiet"))
                .await?;
        }
        Err(cause) => {
            // A per-want failure reschedules normally with a reset streak:
            // one bad want never kills the sweep.
            tracing::warn!(key = %watch.key, %cause, "wanted search failed");
            deps.watches
                .record_check(&watch.key, check(0, "error"))
                .await?;
            summary.errors += 1;
        }
    }
    Ok(())
}

/// Ask every source that is on for the watch's album, through the same
/// search (and gates) as a manual search. Candidates outside the download
/// settings do not count as found. `None` when no source is on or the
/// download settings do not read; an error when every source failed.
async fn search_watch(
    deps: &WantedDeps,
    watch: &WantedWatch,
) -> Result<Option<Vec<crate::search_jobs::candidates::Candidate>>, String> {
    let Some(sources) = deps.sources.current().filter(|sources| !sources.is_empty()) else {
        return Ok(None);
    };
    let target = crate::target::SearchTarget {
        artist: watch.artist_name.clone(),
        album_title: watch.album_title.clone(),
        year: None,
        tracklist: Vec::new(),
        wanted: Vec::new(),
        track: None,
    };
    let (mut candidates, failed) =
        crate::search_jobs::search_sources(&sources, &target, &watch.key).await;
    if candidates.is_empty() && failed > 0 && failed == sources.len() {
        return Err("every download source failed".to_owned());
    }
    candidates.retain(|candidate| !candidate.is_refused());
    Ok(Some(candidates))
}

/// Tell the watch's owner what the watcher did.
fn notify_wanted(
    deps: &WantedDeps,
    watch: &WantedWatch,
    kind: fn(WantedNotice) -> UserNotice,
    extra: impl FnOnce(&mut WantedNotice),
) {
    let mut notice = WantedNotice::new(&watch.key, &watch.artist_name, &watch.album_title);
    extra(&mut notice);
    deps.ticks.announce(FlowEvent::Notify {
        user_id: watch.user_id.clone(),
        notice: kind(notice),
    });
}

/// Enrol availability-dead album requests as watches: `failed` rows, plus
/// `incomplete` rows when the partial toggle is on. Never auto-revives a
/// stopped watch, skips rows with no requester, and skips rows whose
/// linked task is still active.
async fn enrol_watches(now: i64, settings: &WantedSettings, deps: &WantedDeps) -> usize {
    let mut statuses = vec![(RequestStatus::Failed, "missing")];
    if settings.watch_partial_albums {
        statuses.push((RequestStatus::Incomplete, "partial"));
    }
    let mut enrolled = 0;
    for (status, kind) in statuses {
        let rows = match deps.ledger.with_status(RequestKind::Album, status).await {
            Ok(rows) => rows,
            Err(error) => {
                tracing::warn!(%status, ?error, "wanted enrolment could not read requests");
                continue;
            }
        };
        for row in rows {
            let Some(user_id) = row.user_id.clone().filter(|user| !user.is_empty()) else {
                continue;
            };
            if deps.library.contains(&row.key).await {
                continue;
            }
            if let Some(task_id) = row.task_id.as_deref() {
                match deps.downloads.task(task_id).await {
                    Ok(Some(task)) if !task.status.is_terminal() => {
                        continue;
                    }
                    Ok(_) => {}
                    Err(cause) => {
                        tracing::warn!(task_id, %cause, "wanted enrolment task read failed");
                        continue;
                    }
                }
            }
            let stored = match deps.watches.get(&row.key).await {
                // Enrolment never touches a live or stopped watch.
                Ok(Some(watch)) if watch.state != WatchState::Fulfilled => continue,
                Ok(existing) => existing.and_then(|watch| watch.first_release_date),
                Err(error) => {
                    tracing::warn!(key = %row.key, ?error, "wanted enrolment watch read failed");
                    continue;
                }
            };
            let date = watch_date(&deps.targets, &row.key, stored.as_deref(), row.year).await;
            let watch = WantedWatch {
                key: row.key.clone(),
                user_id,
                user_name: None,
                artist_name: row.artist_name.clone(),
                album_title: row.album_title.clone(),
                artist_mbid: row.artist_mbid.clone(),
                year: row.year,
                cover_url: None,
                kind: kind.to_owned(),
                state: WatchState::Watching,
                created_at: epoch(now),
                first_release_date: date.found,
                check_count: 0,
                quiet_streak: 0,
                next_check_at: epoch(now + interval_seconds(date.schedule.as_deref(), 0, now)),
                new_candidate_count: 0,
            };
            match deps.watches.enrol(watch).await {
                Ok(true) => {
                    deps.ticks.emit(
                        "wanted.enrolled",
                        &format!("wanted {} enrolled from {status} request", row.key),
                        now,
                    );
                    enrolled += 1;
                }
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(key = %row.key, ?error, "wanted enrolment write failed");
                }
            }
        }
    }
    enrolled
}

/// When a watch's album first came out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WatchDate {
    /// A date MusicBrainz gave on this lookup, for the watch to keep.
    pub found: Option<String>,
    /// The date the recheck cadence reads: the stored or found date, else
    /// the request's year. The year alone is never stored.
    pub schedule: Option<String>,
}

/// A watch's release date: the one it holds, else the release group's
/// first release date from MusicBrainz (one lookup), else the request's
/// year, else none. A failed lookup finds nothing, so the next check
/// tries again.
pub async fn watch_date(
    targets: &Targets,
    release_group_mbid: &str,
    stored: Option<&str>,
    year: Option<i32>,
) -> WatchDate {
    if let Some(date) = stored {
        return WatchDate {
            found: None,
            schedule: Some(date.to_owned()),
        };
    }
    let found = targets.first_release_date(release_group_mbid).await;
    let schedule = found
        .clone()
        .or_else(|| year.map(|year| format!("{year:04}")));
    WatchDate { found, schedule }
}

/// Quiet streak at which old releases back off from 14 to 28 days.
pub const QUIET_DOUBLING_STREAK: u32 = 10;

/// Age-based recheck cadence in seconds:
/// under 30 days old → 2 days; under 90 → 4; under a year → 7; older or
/// unknown → 14, or 28 past the quiet-doubling streak. Unknown release
/// date reads as old. Day granularity (the table bands are days wide).
pub fn interval_seconds(first_release_date: Option<&str>, quiet_streak: u32, now: i64) -> i64 {
    const DAY: i64 = 86_400;
    let age_days = first_release_date
        .and_then(parse_partial_date)
        .map(|(year, month, day)| now.div_euclid(DAY) - days_from_civil(year, month, day))
        .map(|age| age.max(0));
    let days: i64 = match age_days {
        Some(age) if age < 30 => 2,
        Some(age) if age < 90 => 4,
        Some(age) if age < 365 => 7,
        _ if quiet_streak >= QUIET_DOUBLING_STREAK => 28,
        _ => 14,
    };
    days * DAY
}

/// Parse a partial MusicBrainz date (`YYYY`, `YYYY-MM`, or `YYYY-MM-DD`)
/// to the start of its period.
fn parse_partial_date(value: &str) -> Option<(i64, u32, u32)> {
    let mut parts = value.split('-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: u32 = match parts.next() {
        None => 1,
        Some(part) => part.parse::<u32>().ok()?,
    };
    let day: u32 = match parts.next() {
        None => 1,
        Some(part) => part.parse::<u32>().ok()?,
    };
    if parts.next().is_some() || month == 0 || month > 12 || day == 0 || day > 31 {
        return None;
    }
    Some((year, month, day))
}

/// Days from the unix epoch to a civil date (Hinnant's algorithm).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year.rem_euclid(400) as u64;
    let mp = ((i64::from(month) + 9) % 12) as u64;
    let doy = (153 * mp + 2) / 5 + u64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i64 - 719_468
}

/// What one follow poll accomplished.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FollowSummary {
    /// Artists polled this tick.
    pub artists_polled: usize,
    /// Artists baselined (first poll records, never emits).
    pub baselined: usize,
    /// New releases added to the feed.
    pub new_releases: usize,
    /// Downloads started: new releases, and follow requests that waited
    /// for quota room.
    pub enqueued: usize,
    /// Artists that errored (each isolated).
    pub errors: usize,
}

/// Follow-poll dependencies.
pub struct FollowDeps {
    /// Cursors, auto-download followers and the feed.
    pub follows: Follows,
    /// Release-page provider seam.
    pub poll: Arc<dyn ReleasePoll>,
    /// Download dispatch.
    pub downloads: Arc<DownloadDispatch>,
    /// Request intake: a follow download is a request of the follower's.
    pub requests: Arc<RequestsService>,
    /// Library presence: owned albums never enter the feed or download.
    pub library: Arc<LibraryPresence>,
    /// Durable ticks.
    pub ticks: Arc<dyn TickSink>,
    /// Release-type preferences, read once per tick.
    pub preferences: Live<UserPreferences>,
    /// Today provider (`YYYY-MM-DD`, UTC), so tests pin the cursor date.
    pub today: Arc<dyn Fn() -> String + Send + Sync>,
}

/// Run one follow poll: poll at most
/// [`FOLLOW_MAX_ARTISTS_PER_TICK`] due artists, each at most once a day
/// ([`FOLLOW_ARTIST_REPOLL`]), an hour after a failure. A first poll
/// records every release group as a baseline and emits nothing. Later
/// polls add to the feed the releases that are new to the artist, match
/// the release-type preferences, are not in the library and carry a
/// complete date on or after the prior poll. When an auto-download
/// follower exists, those releases are queued for download once their
/// date arrives.
pub async fn follow_tick(now: i64, deps: &FollowDeps) -> FollowSummary {
    let mut summary = FollowSummary::default();
    let today = (deps.today)();
    // Unreadable release-type preferences skip the pass: cursors stay due
    // and no release is recorded or marked known.
    let Some(preferences) = deps.preferences.read() else {
        return summary;
    };
    let types = ReleaseTypes::from_preferences(&preferences);
    summary.enqueued += start_waiting(deps, now).await;
    let due = match deps
        .follows
        .list_due(now, FOLLOW_MAX_ARTISTS_PER_TICK)
        .await
    {
        Ok(due) => due,
        Err(error) => {
            tracing::warn!(?error, "follow poll could not list due artists");
            summary.errors += 1;
            return summary;
        }
    };
    for mut cursor in due {
        summary.artists_polled += 1;
        let mut feed = Vec::new();
        match deps.poll.poll_releases(&cursor.artist_mbid).await {
            Ok(releases) => {
                if cursor.baselined {
                    let observed =
                        observe_artist(deps, &mut cursor, &releases, &today, &types, now).await;
                    summary.new_releases += observed.feed.len();
                    summary.enqueued += observed.enqueued;
                    feed = observed.feed;
                } else {
                    cursor.baselined = true;
                    summary.baselined += 1;
                }
                cursor
                    .known
                    .extend(releases.iter().map(|row| row.rg_mbid.to_lowercase()));
                cursor.cursor_date = Some(today.clone());
                cursor.next_poll_at = now + FOLLOW_ARTIST_REPOLL.as_secs() as i64;
            }
            Err(cause) => {
                tracing::warn!(artist = %cursor.artist_mbid, %cause, "follow poll failed");
                summary.errors += 1;
                cursor.next_poll_at = now + FOLLOW_ARTIST_RETRY.as_secs() as i64;
            }
        }
        if let Err(error) = deps.follows.record_poll(&cursor, &feed).await {
            tracing::warn!(artist = %cursor.artist_mbid, ?error, "follow cursor write failed");
            summary.errors += 1;
        }
    }
    summary
}

/// The release-type filter of the user preferences, normalised once.
struct ReleaseTypes {
    primary: HashSet<String>,
    secondary: HashSet<String>,
}

impl ReleaseTypes {
    fn from_preferences(preferences: &UserPreferences) -> Self {
        Self {
            primary: type_set(&preferences.primary_types),
            secondary: type_set(&preferences.secondary_types),
        }
    }

    fn includes(&self, row: &ObservedRelease) -> bool {
        should_include_release(
            row.primary_type.as_deref(),
            &row.secondary_types,
            Some(&self.primary),
            Some(&self.secondary),
            true,
        )
    }
}

/// What one baselined artist's poll found.
struct Observed {
    /// New releases for the feed.
    feed: Vec<FeedRelease>,
    /// Downloads queued.
    enqueued: usize,
}

/// Find the artist's new releases, hold them for auto-download when
/// someone wants that, and queue the held ones whose date has come.
async fn observe_artist(
    deps: &FollowDeps,
    cursor: &mut FollowCursor,
    releases: &[ObservedRelease],
    today: &str,
    types: &ReleaseTypes,
    now: i64,
) -> Observed {
    let mut fresh = Vec::new();
    for row in fresh_releases(cursor, releases, types) {
        if deps.library.contains(&row.rg_mbid).await {
            continue;
        }
        fresh.push(row);
    }
    let followers = if fresh.is_empty() && cursor.pending.is_empty() {
        Vec::new()
    } else {
        match deps.follows.auto_followers(&cursor.artist_mbid).await {
            Ok(followers) => followers,
            Err(error) => {
                tracing::warn!(artist = %cursor.artist_mbid, ?error,
                    "auto-download followers unreadable; holding downloads");
                return Observed {
                    feed: fresh.iter().map(|row| feed_row(row)).collect(),
                    enqueued: 0,
                };
            }
        }
    };
    // With no auto-download follower a release only reaches the feed; it is
    // not replayed if someone turns auto-download on later.
    if !followers.is_empty() {
        for row in &fresh {
            if !cursor
                .pending
                .iter()
                .any(|held| held.rg_mbid == row.rg_mbid)
            {
                cursor.pending.push(PendingRelease {
                    rg_mbid: row.rg_mbid.clone(),
                    title: row.title.clone(),
                    // fresh_releases only passes complete dates.
                    date: row.first_release_date.clone().unwrap_or_default(),
                });
            }
        }
    }
    let observed: HashSet<String> = releases
        .iter()
        .map(|row| row.rg_mbid.to_lowercase())
        .collect();
    let mut enqueued = 0;
    let mut waiting = Vec::new();
    for held in std::mem::take(&mut cursor.pending) {
        if held.date.as_str() > today {
            waiting.push(held);
            continue;
        }
        // A release MusicBrainz no longer lists is dropped, not fetched.
        if !observed.contains(&held.rg_mbid.to_lowercase()) {
            continue;
        }
        match queue_release(deps, cursor, &followers, &held, now).await {
            Queued::Enqueued => enqueued += 1,
            Queued::Settled => {}
            Queued::Retry => waiting.push(held),
        }
    }
    cursor.pending = waiting;
    Observed {
        feed: fresh.iter().map(|row| feed_row(row)).collect(),
        enqueued,
    }
}

/// Page rows worth acting on: unknown to the artist's inventory, within
/// the release-type preferences, with a complete `YYYY-MM-DD` date on or
/// after the prior cursor. Malformed rows never emit.
fn fresh_releases<'a>(
    cursor: &FollowCursor,
    releases: &'a [ObservedRelease],
    types: &ReleaseTypes,
) -> Vec<&'a ObservedRelease> {
    releases
        .iter()
        .filter(|row| !cursor.known.contains(&row.rg_mbid.to_lowercase()))
        .filter(|row| types.includes(row))
        .filter(|row| {
            row.first_release_date.as_deref().is_some_and(|date| {
                is_complete_date(date)
                    && cursor
                        .cursor_date
                        .as_deref()
                        .is_none_or(|cursor| date >= cursor)
            })
        })
        .collect()
}

fn feed_row(row: &ObservedRelease) -> FeedRelease {
    FeedRelease {
        rg_mbid: row.rg_mbid.clone(),
        title: row.title.clone(),
        primary_type: row.primary_type.clone(),
        secondary_types: (!row.secondary_types.is_empty()).then(|| row.secondary_types.join(",")),
        first_release_date: row.first_release_date.clone(),
    }
}

/// How queueing one held release ended.
enum Queued {
    /// A download was queued.
    Enqueued,
    /// Nothing to do any more: owned, already downloading, no follower
    /// wants it, or its request row waits for quota room.
    Settled,
    /// The request failed, or the album's row may never download; keep it
    /// held and try on the next poll.
    Retry,
}

/// Ask for one release as a request of the first auto-download follower
/// (one task per release), unless the library has it or a
/// download for it is already running. The request shows on the Requests
/// page and settles with its download.
async fn queue_release(
    deps: &FollowDeps,
    cursor: &FollowCursor,
    followers: &[Follower],
    release: &PendingRelease,
    now: i64,
) -> Queued {
    let Some(follower) = followers.first() else {
        return Queued::Settled;
    };
    let owner = &follower.user_id;
    if deps.library.contains(&release.rg_mbid).await {
        return Queued::Settled;
    }
    match deps.downloads.active_task_for_album(&release.rg_mbid).await {
        Ok(Some(_)) => return Queued::Settled,
        Ok(None) => {}
        Err(cause) => {
            tracing::warn!(rg_mbid = %release.rg_mbid, %cause,
                "follow poll could not check active downloads; retrying next poll");
            return Queued::Retry;
        }
    }
    let release = FollowRelease {
        follower: follower.clone(),
        artist_mbid: cursor.artist_mbid.clone(),
        artist_name: cursor.artist_name.clone(),
        release_group_mbid: release.rg_mbid.clone(),
        title: release.title.clone(),
    };
    match deps.requests.request_follow(&release, epoch(now)).await {
        Ok(FollowIntake::Started { task_id }) => {
            announce_follow_download(
                deps,
                owner,
                &release.artist_mbid,
                &release.artist_name,
                &release.release_group_mbid,
                &release.title,
                task_id,
                now,
            );
            Queued::Enqueued
        }
        // The request row is durable from here: the poll's start pass
        // takes it up once the follower's quota has room.
        Ok(FollowIntake::Waiting) => {
            tracing::info!(rg_mbid = %release.release_group_mbid, owner,
                "follower's quota is full; the request waits for room");
            Queued::Settled
        }
        Ok(FollowIntake::Joined) => Queued::Settled,
        Ok(FollowIntake::Held) => Queued::Retry,
        Err(error) => {
            tracing::warn!(rg_mbid = %release.release_group_mbid, owner, ?error,
                "follow request failed; retrying next poll");
            Queued::Retry
        }
    }
}

/// Start the follow requests waiting for quota room whose owners have
/// room now. Answers how many started.
async fn start_waiting(deps: &FollowDeps, now: i64) -> usize {
    let started = match deps.requests.start_waiting(epoch(now)).await {
        Ok(started) => started,
        Err(error) => {
            tracing::warn!(?error, "waiting requests unreadable; trying next poll");
            return 0;
        }
    };
    for (record, task_id) in &started {
        let owner = record.user_id.clone().unwrap_or_default();
        announce_follow_download(
            deps,
            &owner,
            record.artist_mbid.as_deref().unwrap_or_default(),
            &record.artist_name,
            &record.key,
            &record.album_title,
            task_id.clone(),
            now,
        );
    }
    started.len()
}

/// Tell the follower a new release of an artist they follow is downloading.
#[allow(clippy::too_many_arguments)]
fn announce_follow_download(
    deps: &FollowDeps,
    owner: &str,
    artist_mbid: &str,
    artist_name: &str,
    release_group_mbid: &str,
    title: &str,
    task_id: String,
    now: i64,
) {
    deps.ticks.emit(
        "follow.enqueued",
        &format!("follow {release_group_mbid} enqueued for {owner}"),
        now,
    );
    deps.ticks.announce(FlowEvent::Notify {
        user_id: owner.to_owned(),
        notice: UserNotice::AutoDownloadEnqueued(AutoDownloadEnqueued {
            artist_mbid: artist_mbid.to_owned(),
            artist_name: artist_name.to_owned(),
            release_group_mbid: release_group_mbid.to_owned(),
            title: title.to_owned(),
            task_id,
        }),
    });
}

/// One whole-album download.
fn album_dispatch(
    user_id: &str,
    release_group_mbid: &str,
    artist: &str,
    title: &str,
    origin: DispatchOrigin,
    idempotency_key: Option<String>,
) -> DispatchRequest {
    DispatchRequest {
        user_id: user_id.to_owned(),
        target: DispatchTarget::Album {
            release_group_mbid: release_group_mbid.to_owned(),
        },
        artist_name: artist.to_owned(),
        title: title.to_owned(),
        origin,
        release_mbid: None,
        wanted: Vec::new(),
        planned_album: None,
        idempotency_key,
    }
}

/// True for complete `YYYY-MM-DD` dates.
fn is_complete_date(date: &str) -> bool {
    let bytes = date.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && date[..4].parse::<u32>().is_ok()
        && date[5..7]
            .parse::<u32>()
            .is_ok_and(|month| (1..=12).contains(&month))
        && date[8..10]
            .parse::<u32>()
            .is_ok_and(|day| (1..=31).contains(&day))
}

/// What one upgrade sweep accomplished.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SweepSummary {
    /// Upgrade grabs enqueued.
    pub enqueued: usize,
    /// Items skipped because no admin could own them (whole sweep).
    pub skipped_no_admin: bool,
    /// Items that errored (each isolated from the rest).
    pub errors: usize,
}

/// Upgrade-sweep dependencies.
pub struct SweepDeps {
    /// The download policy, re-read every pass so enabling needs no
    /// restart.
    pub policy: Live<DownloadPolicy>,
    /// Cutoff-unmet worklist.
    pub worklist: Arc<dyn CutoffList>,
    /// Admin directory for sweep ownership.
    pub admins: AdminDirectory,
    /// Download dispatch.
    pub downloads: Arc<DownloadDispatch>,
    /// Durable ticks.
    pub ticks: Arc<dyn TickSink>,
}

/// Run one background-upgrade sweep: while upgrades are allowed
/// and the scan is enabled, walk the cutoff-unmet worklist and enqueue at
/// most `max_per_run` origin-`upgrade` grabs owned by the oldest admin.
/// An album with a live download is not grabbed again and does not count
/// as enqueued. An album with an open held entry waits for its decision,
/// and one whose last upgrade finished inside its back-off waits that
/// out; a poison item never
/// starves its siblings.
pub async fn sweep_tick(now: i64, deps: &SweepDeps) -> SweepSummary {
    let Some(policy) = deps.policy.read() else {
        return SweepSummary::default();
    };
    let policy = UpgradePolicy::from_policy(&policy);
    if !(policy.upgrade_allowed && policy.scan_enabled) {
        return SweepSummary::default();
    }
    let Some(owner) = deps.admins.oldest_admin().await else {
        return SweepSummary {
            skipped_no_admin: true,
            ..SweepSummary::default()
        };
    };
    let mut summary = SweepSummary::default();
    let items = match deps.worklist.cutoff_unmet(&policy.cutoff).await {
        Ok(items) => items,
        Err(error) => {
            tracing::warn!(?error, "upgrade sweep could not read the worklist");
            summary.errors += 1;
            return summary;
        }
    };
    let resting = match deps.worklist.resting(now).await {
        Ok(resting) => resting,
        Err(error) => {
            tracing::warn!(?error, "upgrade sweep could not read its back-off");
            summary.errors += 1;
            return summary;
        }
    };
    for item in items {
        if summary.enqueued >= policy.max_per_run {
            break;
        }
        if resting.contains(&item.rg_mbid) {
            continue;
        }
        let dispatch = album_dispatch(
            &owner,
            &item.rg_mbid,
            &item.artist,
            &item.title,
            DispatchOrigin::Upgrade,
            None,
        );
        match deps.downloads.dispatch(&dispatch).await {
            Ok(DispatchOutcome::Dispatched { task_id }) => {
                deps.ticks.emit(
                    "upgrade.enqueued",
                    &format!("upgrade {} enqueued as {task_id}", item.rg_mbid),
                    now,
                );
                summary.enqueued += 1;
            }
            Ok(DispatchOutcome::Active { .. }) => {}
            Err(cause) => {
                tracing::warn!(rg_mbid = %item.rg_mbid, %cause, "upgrade dispatch failed");
                summary.errors += 1;
            }
        }
    }
    summary
}

/// What one status-sweep pass accomplished.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncSummary {
    /// Finished tasks whose requests were settled.
    pub settled: usize,
    /// Album requests imported because the library holds the album.
    pub imported: usize,
    /// Rows that errored (each isolated).
    pub errors: usize,
}

/// Retention policy for the store prune, read on every pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PruneSettings {
    /// Terminal requests and finished watches older than this go.
    pub retention_days: u64,
    /// Hours between prunes.
    pub interval_hours: u64,
}

impl PruneSettings {
    /// The retention policy the advanced section sets.
    pub fn from_section(section: &AdvancedSettings) -> Self {
        Self {
            retention_days: crate::settings::clamp_u64(section.request_history_retention_days),
            interval_hours: crate::settings::clamp_u64(section.store_prune_interval_hours),
        }
    }
}

/// Store-prune dependencies.
pub struct PruneDeps {
    /// The section the retention policy comes from.
    pub settings: Live<AdvancedSettings>,
    /// Request ledger.
    pub ledger: RequestStore,
    /// Wanted watches.
    pub watches: WantedStore,
    /// Manual album searches.
    pub search_jobs: JobStore,
}

/// What one prune removed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PruneSummary {
    /// Terminal requests deleted.
    pub requests: u64,
    /// Finished watches deleted.
    pub watches: u64,
    /// Manual search jobs deleted.
    pub search_jobs: u64,
}

/// One prune at `now`: terminal requests and stopped or fulfilled watches
/// past the retention window go. The
/// window is clamped to the settings' 30..3650 days. Manual search jobs
/// keep their own, shorter window (see [`JobStore::prune`]).
pub async fn prune_tick(now: i64, deps: &PruneDeps) -> Result<PruneSummary, RequestsError> {
    // Unreadable retention prunes nothing: a default window could delete
    // history the admin chose to keep.
    let Some(section) = deps.settings.read() else {
        return Ok(PruneSummary::default());
    };
    let days = PruneSettings::from_section(&section)
        .retention_days
        .clamp(30, 3650);
    let cutoff = u64::try_from(now)
        .unwrap_or(0)
        .saturating_sub(days * 86_400);
    let requests = deps.ledger.prune_terminal(cutoff).await?;
    let watches = deps.watches.prune(cutoff).await?;
    let search_jobs = deps
        .search_jobs
        .prune(now as f64)
        .await
        .map_err(|cause| RequestsError::internal(&cause))?;
    Ok(PruneSummary {
        requests,
        watches,
        search_jobs,
    })
}

/// Status-sweep dependencies.
pub struct SyncDeps {
    /// Request ledger.
    pub ledger: RequestStore,
    /// The download journal: a missed settle is written through it.
    pub journal: Arc<Journal>,
    /// Library presence for album rows no task can settle.
    pub library: Arc<LibraryPresence>,
    /// Durable ticks.
    pub ticks: Arc<dyn TickSink>,
}

/// Run one status sweep, the safety net behind the journal: requests
/// normally follow their task in the transaction that settles it. A
/// finished task whose linked requests did not follow (a link made after
/// the settle, a write outside the journal) settles them now through the
/// journal; an album request with no task left to follow, or a failed one
/// whose task is gone, is imported when the library holds the album.
/// With nothing to do the sweep writes nothing.
pub async fn sync_tick(now: i64, deps: &SyncDeps) -> SyncSummary {
    let mut summary = SyncSummary::default();
    match deps.ledger.unsettled_tasks().await {
        Ok(tasks) => {
            let tasks: Vec<(String, TaskStatus)> = tasks
                .into_iter()
                .filter_map(|(id, status)| TaskStatus::parse(&status).map(|status| (id, status)))
                .collect();
            if !tasks.is_empty() {
                let count = tasks.len();
                let at = now as f64;
                let settled = deps
                    .journal
                    .run("requests.sweep", move |store| {
                        for (task_id, status) in &tasks {
                            store.settle_requests(task_id, *status, at)?;
                        }
                        Ok(())
                    })
                    .await;
                match settled {
                    Ok(()) => summary.settled += count,
                    Err(error) => {
                        tracing::warn!(%error, "status sweep could not settle finished tasks");
                        summary.errors += 1;
                    }
                }
            }
        }
        Err(error) => {
            tracing::warn!(?error, "status sweep could not read finished tasks");
            summary.errors += 1;
        }
    }
    let albums = match deps.ledger.albums_without_task().await {
        Ok(albums) => albums,
        Err(error) => {
            tracing::warn!(?error, "status sweep could not read taskless requests");
            summary.errors += 1;
            return summary;
        }
    };
    for key in albums {
        if !deps.library.contains(&key).await {
            continue;
        }
        match deps
            .ledger
            .settle_in_library(&key, "in_library", epoch(now))
            .await
        {
            Ok(fulfilled) => {
                summary.imported += fulfilled.len();
                announce_fulfilled(deps.ticks.as_ref(), fulfilled, now);
            }
            Err(error) => {
                tracing::warn!(%key, ?error, "status sweep could not settle a held album");
                summary.errors += 1;
            }
        }
    }
    summary
}

/// Waits of 0.8 to 1.2 times `base`, drawn fresh each time.
fn spread(base: Duration) -> Schedule {
    Schedule::new(base.mul_f64(0.8)).with_jitter(base.mul_f64(0.4))
}

/// One pass per tick, at the supervisor clock's `now`.
struct PassTick<D, F> {
    clock: Arc<dyn Clock>,
    deps: Arc<D>,
    pass: F,
}

impl<D, F, Fut> Tick for PassTick<D, F>
where
    D: Send + Sync + 'static,
    F: Fn(i64, Arc<D>) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send,
{
    async fn tick(&mut self, _ctx: &TickCtx) -> TickResult {
        (self.pass)(self.clock.now_unix(), Arc::clone(&self.deps)).await;
        Ok(Next::Continue)
    }
}

/// A pass that runs once its configured interval, read on every check, has
/// passed since the last one that finished.
struct IntervalTick<D> {
    clock: Arc<dyn Clock>,
    deps: Arc<D>,
    last: Option<i64>,
}

impl<D> IntervalTick<D> {
    fn new(supervisor: &Supervisor, deps: Arc<D>) -> Self {
        Self {
            clock: Arc::clone(supervisor.clock()),
            deps,
            last: None,
        }
    }

    /// `now` when a pass is due after `hours`, else `None`.
    fn due(&self, hours: u64) -> Option<i64> {
        let now = self.clock.now_unix();
        let interval = i64::try_from(hours.saturating_mul(3600)).unwrap_or(i64::MAX);
        self.last
            .is_none_or(|last| now - last >= interval)
            .then_some(now)
    }
}

impl Tick for IntervalTick<PruneDeps> {
    async fn tick(&mut self, _ctx: &TickCtx) -> TickResult {
        let Some(section) = self.deps.settings.read() else {
            return Ok(Next::Continue);
        };
        let hours = PruneSettings::from_section(&section)
            .interval_hours
            .clamp(1, 168);
        let Some(now) = self.due(hours) else {
            return Ok(Next::Continue);
        };
        let summary = prune_tick(now, &self.deps)
            .await
            .map_err(|error| TickError::new("STORE_PRUNE_FAILED", format!("{error:?}")))?;
        self.last = Some(now);
        tracing::info!(?summary, "store prune finished");
        Ok(Next::Continue)
    }
}

impl Tick for IntervalTick<SweepDeps> {
    async fn tick(&mut self, _ctx: &TickCtx) -> TickResult {
        let Some(policy) = self.deps.policy.read() else {
            return Ok(Next::Continue);
        };
        let hours = UpgradePolicy::from_policy(&policy).interval_hours.max(1);
        if self.last.is_none() {
            match self.deps.worklist.last_upgrade_at().await {
                Ok(last) => self.last = last,
                Err(error) => {
                    tracing::warn!(?error, "upgrade sweep could not read its last run");
                    return Ok(Next::Continue);
                }
            }
        }
        let Some(now) = self.due(hours) else {
            return Ok(Next::Continue);
        };
        self.last = Some(now);
        let summary = sweep_tick(now, &self.deps).await;
        if summary.errors > 0 {
            tracing::warn!(?summary, "upgrade sweep finished with errors");
        }
        Ok(Next::Continue)
    }
}

fn pass_tick<D, F, Fut>(supervisor: &Supervisor, deps: Arc<D>, pass: F) -> PassTick<D, F>
where
    F: Fn(i64, Arc<D>) -> Fut,
{
    PassTick {
        clock: Arc::clone(supervisor.clock()),
        deps,
        pass,
    }
}

/// Run the wanted watcher: a startup delay, then a jittered 15-minute
/// cadence.
pub fn spawn_wanted_loop(
    supervisor: &Supervisor,
    deps: Arc<WantedDeps>,
) -> Result<(), AlreadyRunning> {
    supervisor.periodic(
        WANTED_JOB,
        spread(WANTED_INTERVAL).with_initial_delay(WANTED_INITIAL_DELAY),
        pass_tick(supervisor, deps, |now, deps| async move {
            let summary = wanted_tick(now, &deps).await;
            if summary.errors > 0 {
                tracing::warn!(?summary, "wanted sweep finished with errors");
            }
        }),
    )
}

/// Run the follow new-release poll: a startup delay, then a jittered
/// 1-minute due-driven cadence.
pub fn spawn_follow_loop(
    supervisor: &Supervisor,
    deps: Arc<FollowDeps>,
) -> Result<(), AlreadyRunning> {
    supervisor.periodic(
        FOLLOW_JOB,
        spread(FOLLOW_INTERVAL).with_initial_delay(FOLLOW_INITIAL_DELAY),
        pass_tick(supervisor, deps, |now, deps| async move {
            let summary = follow_tick(now, &deps).await;
            if summary.errors > 0 {
                tracing::warn!(?summary, "follow poll finished with errors");
            }
        }),
    )
}

/// Run the background upgrade sweep: a startup delay, then an hourly check
/// that sweeps once the policy's interval (default 12h) has passed since
/// the last sweep, which a restart reads from the newest upgrade task.
pub fn spawn_sweep_loop(
    supervisor: &Supervisor,
    deps: Arc<SweepDeps>,
) -> Result<(), AlreadyRunning> {
    supervisor.periodic(
        UPGRADE_JOB,
        Schedule::new(UPGRADE_TICK).with_initial_delay(UPGRADE_INITIAL_DELAY),
        IntervalTick::new(supervisor, deps),
    )
}

/// Run the request-status sweep: a startup delay, then a plain 1-minute
/// cadence.
pub fn spawn_sync_loop(supervisor: &Supervisor, deps: Arc<SyncDeps>) -> Result<(), AlreadyRunning> {
    supervisor.periodic(
        SYNC_JOB,
        Schedule::new(SYNC_INTERVAL).with_initial_delay(SYNC_INITIAL_DELAY),
        pass_tick(supervisor, deps, |now, deps| async move {
            let summary = sync_tick(now, &deps).await;
            if summary.errors > 0 {
                tracing::warn!(?summary, "status sweep finished with errors");
            }
        }),
    )
}

/// Run the store prune: a startup delay, then an hourly check that prunes
/// once the configured interval has passed.
pub fn spawn_prune_loop(
    supervisor: &Supervisor,
    deps: Arc<PruneDeps>,
) -> Result<(), AlreadyRunning> {
    supervisor.periodic(
        PRUNE_JOB,
        Schedule::new(PRUNE_TICK).with_initial_delay(PRUNE_INITIAL_DELAY),
        IntervalTick::new(supervisor, deps),
    )
}
