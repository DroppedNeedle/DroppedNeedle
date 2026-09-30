//! Automatic scan scheduling from durable terminal runs.
//!
//! Port of `backend/services/native/library_scan_scheduler.py` and the
//! terminal-time math in `library_schedule_service.py`. The scheduler never
//! scans on wall-clock guesses: every tick anchors on the latest
//! filesystem terminal run, and only fires when the interval fully elapsed.
//!
//! Timezone note: v2 resolves IANA names through zoneinfo. Without a tz
//! database this port supports `UTC` and numeric offsets (`+HH:MM`,
//! `-HHMM`, `UTC+HH:MM`) for daily schedules; anything else answers "not
//! due" instead of guessing. Interval frequencies are timezone-free and
//! ported exactly, including the absolute-seconds DST note.

use std::collections::HashMap;

use super::models::{Disposition, EffectivePolicy, ScanKind, ScanRequest, ScanScope, ScanTrigger};
use super::roots::RootRegistry;

/// Schedule settings for automatic scans.
#[derive(Debug, Clone)]
pub struct ScheduleSettings {
    pub frequency: String,
    pub daily_time: String,
    pub timezone_name: String,
}

impl ScheduleSettings {
    pub fn new(frequency: &str, daily_time: &str, timezone_name: &str) -> Self {
        Self {
            frequency: frequency.to_owned(),
            daily_time: daily_time.to_owned(),
            timezone_name: timezone_name.to_owned(),
        }
    }

    pub fn manual() -> Self {
        Self::new("manual", "03:00", "UTC")
    }
}

fn interval_seconds(frequency: &str) -> Option<f64> {
    match frequency {
        "5min" => Some(300.0),
        "10min" => Some(600.0),
        "30min" => Some(1_800.0),
        "1hr" => Some(3_600.0),
        "6hr" => Some(21_600.0),
        "12hr" => Some(43_200.0),
        "24hr" => Some(86_400.0),
        "3d" => Some(259_200.0),
        "7d" => Some(604_800.0),
        _ => None,
    }
}

/// Parse a supported timezone name to a fixed UTC offset in seconds.
/// `UTC`/`Z` is zero; numeric forms carry their offset. IANA names are
/// not resolvable here and return `None` (documented limitation above).
pub fn utc_offset_seconds(timezone_name: &str) -> Option<i64> {
    let name = timezone_name.trim();
    if name.eq_ignore_ascii_case("utc")
        || name.eq_ignore_ascii_case("z")
        || name.eq_ignore_ascii_case("etc/utc")
    {
        return Some(0);
    }
    let stripped = name
        .strip_prefix("UTC")
        .or_else(|| name.strip_prefix("utc"))
        .unwrap_or(name);
    let (sign, digits) = match stripped.strip_prefix('+') {
        Some(rest) => (1i64, rest),
        None => match stripped.strip_prefix('-') {
            Some(rest) => (-1i64, rest),
            None => return None,
        },
    };
    let compact: String = digits.chars().filter(|c| *c != ':').collect();
    if compact.len() != 4 || !compact.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let hours: i64 = compact[0..2].parse().ok()?;
    let minutes: i64 = compact[2..4].parse().ok()?;
    if hours > 23 || minutes > 59 {
        return None;
    }
    Some(sign * (hours * 3_600 + minutes * 60))
}

/// Seconds until the next automatic scan is due, anchored on the latest
/// terminal run (v2 `seconds_until_due`). `None` means manual, unknown
/// frequency, or an unresolvable timezone: never due.
pub fn seconds_until_due(
    frequency: &str,
    daily_time: &str,
    terminal_at: Option<f64>,
    now: f64,
    timezone_name: &str,
) -> Option<f64> {
    if frequency == "manual" {
        return None;
    }
    if frequency != "daily" {
        let interval = interval_seconds(frequency)?;
        let Some(terminal) = terminal_at else {
            return Some(0.0);
        };
        // Absolute-seconds arithmetic keeps the rolling gap honest across
        // DST transitions (v2 schedule-service note).
        return Some((terminal + interval - now).max(0.0));
    }
    let offset = utc_offset_seconds(timezone_name)? as f64;
    let (hour, minute) = parse_daily_time(daily_time);
    let local_now = now + offset;
    let day_index = (local_now / 86_400.0).floor();
    let day_start = day_index * 86_400.0;
    match terminal_at {
        None => {
            let today = day_start + (hour * 3_600 + minute * 60) as f64;
            Some(if local_now < today {
                today - local_now
            } else {
                0.0
            })
        }
        Some(terminal) => {
            let terminal_day = ((terminal + offset) / 86_400.0).floor();
            let candidate = (terminal_day + 1.0) * 86_400.0 + (hour * 3_600 + minute * 60) as f64;
            Some((candidate - local_now).max(0.0))
        }
    }
}

fn parse_daily_time(daily_time: &str) -> (i64, i64) {
    // v2 falls back to 03:00 on any parse failure.
    daily_time
        .split_once(':')
        .and_then(|(hour, minute)| {
            let hour: i64 = hour.parse().ok()?;
            let minute: i64 = minute.parse().ok()?;
            if (0..=23).contains(&hour) && (0..=59).contains(&minute) {
                Some((hour, minute))
            } else {
                None
            }
        })
        .unwrap_or((3, 0))
}

/// One included subpath rule under an excluded root. The settings slice
/// owns rule storage; the scheduler only resolves them. Carried as a
/// parameter until that slice lands.
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

/// Whole-registry scopes for an automatic scan (v2 `scheduled_scopes`):
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
        if selected.iter().any(|parent| {
            rule.relative_path == *parent || rule.relative_path.starts_with(&format!("{parent}/"))
        }) {
            continue;
        }
        selected.push(rule.relative_path.clone());
        scopes.push(ScanScope {
            root_id: rule.root_id.clone(),
            scope_id: Some(rule.rule_id.clone()),
            relative_path: rule.relative_path.clone(),
            root_path: Some(root_path.clone()),
            effective_policy: EffectivePolicy::Automatic,
            policy_revision: registry.policy_revision().to_owned(),
            estimated_count: None,
        });
    }
    scopes
}

/// One scheduler tick (v2 `LibraryAutomaticScanScheduler.tick`).
/// `request` issues the scan request; returns true when work will run.
/// started/queued/coalesced/expanded all mean work runs; only conflict
/// means "leave the queued follow-up alone" (S-05).
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
    if settings.frequency == "manual" {
        return false;
    }
    let remaining = seconds_until_due(
        &settings.frequency,
        &settings.daily_time,
        terminal_at,
        now,
        &settings.timezone_name,
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
        policy_revision: registry.policy_revision().to_owned(),
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

    #[test]
    fn interval_math_anchors_on_terminal() {
        assert_eq!(
            seconds_until_due("manual", "03:00", None, 100.0, "UTC"),
            None
        );
        assert_eq!(
            seconds_until_due("1hr", "03:00", None, 100.0, "UTC"),
            Some(0.0)
        );
        assert_eq!(
            seconds_until_due("1hr", "03:00", Some(0.0), 100.0, "UTC"),
            Some(3_500.0)
        );
        assert_eq!(
            seconds_until_due("1hr", "03:00", Some(0.0), 10_000.0, "UTC"),
            Some(0.0)
        );
        assert_eq!(
            seconds_until_due("bogus", "03:00", None, 100.0, "UTC"),
            None
        );
    }

    #[test]
    fn daily_utc_before_and_after_target() {
        // 2024-01-01T00:00:00Z, target 03:00, no terminal yet.
        let monday = 1_704_067_200.0;
        assert_eq!(
            seconds_until_due("daily", "03:00", None, monday, "UTC"),
            Some(10_800.0)
        );
        // Past today's target with no terminal: due now.
        assert_eq!(
            seconds_until_due("daily", "03:00", None, monday + 20_000.0, "UTC"),
            Some(0.0)
        );
        // Terminal Monday noon: next due Tuesday 03:00.
        assert_eq!(
            seconds_until_due(
                "daily",
                "03:00",
                Some(monday + 43_200.0),
                monday + 43_200.0,
                "UTC"
            ),
            Some(54_000.0)
        );
        // Unknown timezone never fires.
        assert_eq!(
            seconds_until_due("daily", "03:00", None, monday, "America/New_York"),
            None
        );
        // Numeric offsets resolve.
        assert_eq!(utc_offset_seconds("UTC+02:00"), Some(7_200));
        assert_eq!(utc_offset_seconds("-0530"), Some(-19_800));
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
        assert!(scopes.iter().any(|scope| scope.relative_path == "."));
        assert!(scopes.iter().any(|scope| scope.relative_path == "keep"));
        assert!(scopes.iter().any(|scope| scope.relative_path == "other"));
    }

    #[test]
    fn tick_fires_only_when_due() {
        let settings = ScheduleSettings::new("1hr", "03:00", "UTC");
        let fired = tick(
            |_| Ok(Disposition::Started),
            &registry(),
            &[],
            &settings,
            Some(0.0),
            100.0,
        );
        assert!(!fired, "interval has not elapsed");
        let fired = tick(
            |_| Ok(Disposition::Started),
            &registry(),
            &[],
            &settings,
            Some(0.0),
            10_000.0,
        );
        assert!(fired);
        let conflicted = tick(
            |_| Ok(Disposition::Conflict),
            &registry(),
            &[],
            &settings,
            Some(0.0),
            10_000.0,
        );
        assert!(!conflicted);
    }
}
