//! Automatic scan scheduling from durable terminal runs.
//!
//! Library scan scheduler and the terminal-time math. The scheduler never
//! scans on wall-clock guesses: every tick anchors on the latest
//! filesystem terminal run, and only fires when the interval fully elapsed.
//!
//! Daily schedules run at the set time in the server's time zone, read
//! from the system zone database. Interval frequencies are time-zone free
//! and count absolute seconds, so a clock change does not move them.

use std::collections::HashMap;
use std::path::Path;

use droppedneedle_kernel::runtime_config::sections::ScanFrequency;
use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;

use super::models::{Disposition, EffectivePolicy, ScanKind, ScanRequest, ScanScope, ScanTrigger};
use super::roots::{RootRegistry, on_disk_rule_paths, scope_covers};

/// Schedule settings for automatic scans.
#[derive(Debug, Clone)]
pub struct ScheduleSettings {
    pub frequency: ScanFrequency,
    pub daily_time: String,
    /// The zone a daily time is read in.
    pub zone: TimeZone,
}

impl ScheduleSettings {
    pub fn new(frequency: ScanFrequency, daily_time: &str, zone: TimeZone) -> Self {
        Self {
            frequency,
            daily_time: daily_time.to_owned(),
            zone,
        }
    }

    pub fn manual() -> Self {
        Self::new(ScanFrequency::Manual, "03:00", TimeZone::UTC)
    }
}

/// The rolling gap of an interval frequency; `None` for `manual` and
/// `daily`.
fn interval_seconds(frequency: ScanFrequency) -> Option<f64> {
    match frequency {
        ScanFrequency::Min30 => Some(1_800.0),
        ScanFrequency::Hr1 => Some(3_600.0),
        ScanFrequency::Hr6 => Some(21_600.0),
        ScanFrequency::Hr12 => Some(43_200.0),
        ScanFrequency::Hr24 => Some(86_400.0),
        ScanFrequency::Days3 => Some(259_200.0),
        ScanFrequency::Days7 => Some(604_800.0),
        ScanFrequency::Manual | ScanFrequency::Daily => None,
    }
}

/// Seconds until the next automatic scan is due, anchored on the latest
/// terminal run. `None` means manual: never due.
/// A daily time is read in `zone`. On a day a clock change skips it, the
/// scan runs as far after the change as the time was past it (02:30 for a
/// lost 01:30); on a day the time comes twice, it runs the first time.
pub fn seconds_until_due(
    frequency: ScanFrequency,
    daily_time: &str,
    terminal_at: Option<f64>,
    now: f64,
    zone: &TimeZone,
) -> Option<f64> {
    if frequency == ScanFrequency::Manual {
        return None;
    }
    if frequency != ScanFrequency::Daily {
        let interval = interval_seconds(frequency)?;
        let Some(terminal) = terminal_at else {
            return Some(0.0);
        };
        // Absolute-seconds arithmetic keeps the rolling gap exact across
        // DST transitions.
        return Some((terminal + interval - now).max(0.0));
    }
    let (hour, minute) = parse_daily_time(daily_time);
    let local_day = |at: f64| {
        Timestamp::from_millisecond((at * 1_000.0) as i64)
            .ok()
            .map(|instant| instant.to_zoned(zone.clone()).date())
    };
    let run_on = |day: Date| {
        zone.to_zoned(day.at(hour, minute, 0, 0))
            .ok()
            .map(|run| run.timestamp().as_millisecond() as f64 / 1_000.0)
    };
    let due = match terminal_at {
        None => run_on(local_day(now)?)?,
        Some(terminal) => run_on(local_day(terminal)?.tomorrow().ok()?)?,
    };
    Some((due - now).max(0.0))
}

fn parse_daily_time(daily_time: &str) -> (i8, i8) {
    // Quirk (v2): any parse failure falls back to 03:00.
    daily_time
        .split_once(':')
        .and_then(|(hour, minute)| {
            let hour: i8 = hour.parse().ok()?;
            let minute: i8 = minute.parse().ok()?;
            if (0..=23).contains(&hour) && (0..=59).contains(&minute) {
                Some((hour, minute))
            } else {
                None
            }
        })
        .unwrap_or((3, 0))
}

/// One included subpath rule under an excluded root. Settings own rule
/// storage; the scheduler only resolves them, carried as a parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InclusionRule {
    pub root_id: String,
    pub rule_id: String,
    pub relative_path: String,
}

impl InclusionRule {
    pub fn new(root_id: &str, rule_id: &str, relative_path: &str) -> Self {
        Self {
            root_id: root_id.to_owned(),
            rule_id: rule_id.to_owned(),
            relative_path: relative_path.to_owned(),
        }
    }
}

/// Whole-registry scopes for an automatic scan:
/// every non-excluded root walks whole; excluded roots contribute only
/// their included subpaths, skipping rules covered by an already-selected
/// ancestor.
pub fn scheduled_scopes(registry: &RootRegistry, rules: &[InclusionRule]) -> Vec<ScanScope> {
    let mut scopes = registry.scheduled_root_scopes();
    let excluded_paths: HashMap<&str, String> = registry
        .roots()
        .iter()
        .filter(|root| root.policy == EffectivePolicy::Excluded)
        .map(|root| (root.id.as_str(), root.path.to_string_lossy().into_owned()))
        .collect();
    let mut selected: Vec<String> = Vec::new();
    for rule in rules {
        let Some(root_path) = excluded_paths.get(rule.root_id.as_str()) else {
            continue;
        };
        if selected
            .iter()
            .any(|parent| scope_covers(parent, &rule.relative_path))
        {
            continue;
        }
        selected.push(rule.relative_path.clone());
        for relative_path in on_disk_rule_paths(Path::new(root_path), &rule.relative_path) {
            scopes.push(ScanScope {
                root_id: rule.root_id.clone(),
                scope_id: Some(rule.rule_id.clone()),
                relative_path,
                root_path: Some(root_path.clone()),
                effective_policy: EffectivePolicy::Automatic,
                policy_revision: registry.fingerprint().to_owned(),
                estimated_count: None,
            });
        }
    }
    scopes
}

/// One scheduler tick.
/// `request` issues the scan request; returns true when work will run.
/// started/queued/coalesced/expanded all mean work runs; only conflict
/// means "leave the queued follow-up alone".
pub fn tick<F>(
    request: F,
    registry: &RootRegistry,
    rules: &[InclusionRule],
    settings: &ScheduleSettings,
    terminal_at: Option<f64>,
    now: f64,
) -> bool
where
    F: FnOnce(ScanRequest) -> Result<Disposition, String>,
{
    if settings.frequency == ScanFrequency::Manual {
        return false;
    }
    let remaining = seconds_until_due(
        settings.frequency,
        &settings.daily_time,
        terminal_at,
        now,
        &settings.zone,
    );
    match remaining {
        None => return false,
        Some(left) if left > 0.0 => return false,
        _ => {}
    }
    let scopes = scheduled_scopes(registry, rules);
    if scopes.is_empty() {
        return false;
    }
    match request(ScanRequest {
        kind: ScanKind::Incremental,
        trigger: ScanTrigger::Automatic,
        scopes,
        requested_by_user_id: None,
        policy_revision: registry.fingerprint().to_owned(),
    }) {
        Ok(disposition) => disposition != Disposition::Conflict,
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::super::roots::LibraryRoot;
    use super::*;
    use std::path::PathBuf;

    fn registry() -> RootRegistry {
        RootRegistry::new(
            vec![
                LibraryRoot::new("a", PathBuf::from("/a"), EffectivePolicy::Automatic),
                LibraryRoot::new("x", PathBuf::from("/x"), EffectivePolicy::Excluded),
            ],
            true,
            "rev-1",
        )
    }

    /// When a scan is due, anchored on the latest terminal run: manual is
    /// never due, an interval counts from the terminal run, a daily time
    /// fires today once passed and tomorrow after a run. A tick fires only
    /// when due and reads a conflict as not fired.
    #[test]
    fn the_schedule_fires_when_due() {
        // 2024-01-01T00:00:00Z.
        let monday = 1_704_067_200.0;
        for (label, frequency, terminal_at, now, expected) in [
            ("manual", ScanFrequency::Manual, None, 100.0, None),
            (
                "interval, no run yet",
                ScanFrequency::Hr1,
                None,
                100.0,
                Some(0.0),
            ),
            (
                "interval, running",
                ScanFrequency::Hr1,
                Some(0.0),
                100.0,
                Some(3_500.0),
            ),
            (
                "interval, elapsed",
                ScanFrequency::Hr1,
                Some(0.0),
                10_000.0,
                Some(0.0),
            ),
            (
                "daily, before 03:00",
                ScanFrequency::Daily,
                None,
                monday,
                Some(10_800.0),
            ),
            (
                "daily, past 03:00 with no run",
                ScanFrequency::Daily,
                None,
                monday + 20_000.0,
                Some(0.0),
            ),
            (
                "daily, run Monday noon: Tuesday 03:00",
                ScanFrequency::Daily,
                Some(monday + 43_200.0),
                monday + 43_200.0,
                Some(54_000.0),
            ),
        ] {
            assert_eq!(
                seconds_until_due(frequency, "03:00", terminal_at, now, &TimeZone::UTC),
                expected,
                "{label}"
            );
        }

        let settings = ScheduleSettings::new(ScanFrequency::Hr1, "03:00", TimeZone::UTC);
        for (label, disposition, now, fired) in [
            ("not due", Disposition::Started, 100.0, false),
            ("due", Disposition::Started, 10_000.0, true),
            ("due, conflict", Disposition::Conflict, 10_000.0, false),
        ] {
            let answer = tick(
                |_| Ok(disposition),
                &registry(),
                &[],
                &settings,
                Some(0.0),
                now,
            );
            assert_eq!(answer, fired, "{label}");
        }
    }

    #[test]
    fn scheduled_scopes_cover_excluded_root_rules() {
        let rules = vec![
            InclusionRule::new("x", "rule-1", "keep"),
            InclusionRule::new("x", "rule-2", "keep/deeper"),
            InclusionRule::new("x", "rule-3", "other"),
        ];
        let scopes = scheduled_scopes(&registry(), &rules);
        // Whole root a, plus keep and other (keep/deeper is covered).
        assert_eq!(scopes.len(), 3);
        assert!(
            scopes
                .iter()
                .any(|scope| scope.root_id == "a" && scope.relative_path == ".")
        );
        assert!(scopes.iter().any(|scope| scope.relative_path == "keep"));
        assert!(scopes.iter().any(|scope| scope.relative_path == "other"));
    }

    /// An included rule walks every folder the disk has that differs only in
    /// case, keeps its own spelling for rows a rename left there, and a rule
    /// its folded spelling covers adds nothing.
    #[test]
    fn included_rules_walk_the_folder_on_disk() {
        let dir =
            droppedneedle_kernel::db::scratch::ScratchDir::new("scheduler-rules").expect("scratch");
        std::fs::create_dir_all(dir.path().join("live").join("90s")).unwrap();
        std::fs::create_dir_all(dir.path().join("LIVE")).unwrap();
        let registry = RootRegistry::new(
            vec![LibraryRoot::new(
                "x",
                dir.path().to_path_buf(),
                EffectivePolicy::Excluded,
            )],
            true,
            "rev-1",
        );
        let rules = vec![
            InclusionRule::new("x", "rule-1", "Live"),
            InclusionRule::new("x", "rule-2", "live/90s"),
        ];
        let paths: Vec<String> = scheduled_scopes(&registry, &rules)
            .into_iter()
            .map(|scope| scope.relative_path)
            .collect();
        assert_eq!(paths, ["LIVE", "live", "Live"]);
    }
}
