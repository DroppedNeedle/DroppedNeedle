//! YouTube daily-quota file store (v2 `YouTubeQuotaStore` semantics).
//!
//! Ported behavior, point for point:
//!
//! - File `<cache_dir>/youtube_quota.json` holding `{date, count}`.
//! - A missing file starts fresh (`{date: "", count: 0}`); a corrupt file
//!   or a negative count fails closed with a typed error.
//! - `count` reads 0 when the stored date is not today (UTC); the next
//!   `reserve` then writes `{today, 1}`, which is the day rollover.
//! - `reserve(limit)` refuses at `count >= limit` with a typed rate-limit,
//!   else durably writes `count + 1` and returns the date it wrote.
//! - `refund(date)` decrements only when the stored date still matches.
//! - Writes are atomic (temp file + rename + fsync + directory fsync).
//! - Reserve/refund serialize on a mutex (the v2 `asyncio.Lock` shape).
//!
//! Intended changes: the API is synchronous (`&self` with interior
//! locking; async callers wrap it in `spawn_blocking`), so the v2
//! cancel-mid-write rollback has no await point to exist at; and stores
//! are constructor-injected (no global per-path registry). The clock is
//! constructor-injected too, so date rollover is testable without time
//! travel.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Name of the quota file inside the cache directory.
pub const QUOTA_FILE_NAME: &str = "youtube_quota.json";

/// Every way quota load, persistence, or reservation can fail.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum QuotaError {
    /// The quota file cannot be read or parsed.
    #[error("could not load YouTube quota: {reason}")]
    Load {
        /// Decoder/OS reason.
        reason: String,
    },
    /// The quota file cannot be persisted.
    #[error("could not persist YouTube quota: {reason}")]
    Persist {
        /// OS reason.
        reason: String,
    },
    /// The daily quota is exhausted.
    #[error("YouTube daily quota exceeded")]
    RateLimited,
    /// The store mutex is poisoned.
    #[error("quota lock unavailable")]
    LockUnavailable,
    /// The system clock is unusable.
    #[error("system clock error: {reason}")]
    Clock {
        /// Clock reason.
        reason: String,
    },
}

/// Quota usage snapshot (the v2 `YouTubeQuotaResponse` shape).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaStatus {
    /// Units consumed today.
    pub used: u32,
    /// Daily limit.
    pub limit: u32,
    /// Units left today (floors at 0).
    pub remaining: u32,
    /// UTC date the counts belong to.
    pub date: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct QuotaState {
    #[serde(default)]
    date: String,
    #[serde(default)]
    count: u32,
}

/// Daily-quota governor backed by one JSON file.
pub struct QuotaStore {
    path: PathBuf,
    state: Mutex<QuotaState>,
    today: Arc<dyn Fn() -> Result<String, QuotaError> + Send + Sync>,
}

impl std::fmt::Debug for QuotaStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuotaStore")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl QuotaStore {
    /// Open the store at `path`, using the system clock for UTC dates.
    pub fn open(path: &Path) -> Result<Self, QuotaError> {
        Self::open_with_clock(path, Arc::new(|| utc_today_string(SystemTime::now())))
    }

    /// Open the store with an injected UTC-date clock (rollover tests).
    pub fn open_with_clock(
        path: &Path,
        today: Arc<dyn Fn() -> Result<String, QuotaError> + Send + Sync>,
    ) -> Result<Self, QuotaError> {
        let state = match std::fs::read(path) {
            Ok(bytes) => {
                serde_json::from_slice::<QuotaState>(&bytes).map_err(|error| QuotaError::Load {
                    reason: error.to_string(),
                })?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => QuotaState {
                date: String::new(),
                count: 0,
            },
            Err(error) => {
                return Err(QuotaError::Load {
                    reason: error.to_string(),
                });
            }
        };
        Ok(Self {
            path: path.to_path_buf(),
            state: Mutex::new(state),
            today,
        })
    }

    /// Units consumed today (0 when the stored date is stale).
    pub fn count(&self) -> Result<u32, QuotaError> {
        let today = (self.today)()?;
        let state = self.state.lock().map_err(|_| QuotaError::LockUnavailable)?;
        Ok(if state.date == today { state.count } else { 0 })
    }

    /// Usage snapshot against `limit`.
    pub fn status(&self, limit: u32) -> Result<QuotaStatus, QuotaError> {
        let today = (self.today)()?;
        let used = self.count()?;
        Ok(QuotaStatus {
            used,
            limit,
            remaining: limit.saturating_sub(used),
            date: today,
        })
    }

    /// Reserve one unit, durably. Refuses with [`QuotaError::RateLimited`]
    /// at `count >= limit`. Returns the date the reservation landed on;
    /// callers refunding later pass it back to [`QuotaStore::refund`].
    pub fn reserve(&self, limit: u32) -> Result<String, QuotaError> {
        let mut state = self.state.lock().map_err(|_| QuotaError::LockUnavailable)?;
        let today = (self.today)()?;
        let effective = if state.date == today { state.count } else { 0 };
        if effective >= limit {
            return Err(QuotaError::RateLimited);
        }
        let next = QuotaState {
            date: today.clone(),
            count: effective + 1,
        };
        self.write_locked(&next)?;
        *state = next;
        Ok(today)
    }

    /// Refund one unit reserved for `date`. Only applies when the stored
    /// date still matches (a refund landing after rollover is a no-op).
    pub fn refund(&self, date: &str) -> Result<(), QuotaError> {
        let mut state = self.state.lock().map_err(|_| QuotaError::LockUnavailable)?;
        if state.date == date {
            let next = QuotaState {
                date: date.to_owned(),
                count: state.count.saturating_sub(1),
            };
            self.write_locked(&next)?;
            *state = next;
        }
        Ok(())
    }

    fn write_locked(&self, state: &QuotaState) -> Result<(), QuotaError> {
        use std::io::Write as _;

        let failed = |reason: String| QuotaError::Persist { reason };
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| failed(error.to_string()))?;
        }
        let bytes = serde_json::to_vec(state).map_err(|error| failed(error.to_string()))?;
        let mut tmp_name = self
            .path
            .file_name()
            .map(|name| name.to_os_string())
            .unwrap_or_default();
        tmp_name.push(".tmp");
        let tmp_path = self.path.with_file_name(tmp_name);
        let outcome: Result<(), String> = (|| {
            let mut file = std::fs::File::create(&tmp_path).map_err(|error| error.to_string())?;
            file.write_all(&bytes).map_err(|error| error.to_string())?;
            file.sync_all().map_err(|error| error.to_string())?;
            drop(file);
            std::fs::rename(&tmp_path, &self.path).map_err(|error| error.to_string())?;
            #[cfg(unix)]
            std::fs::File::open(self.path.parent().unwrap_or(Path::new(".")))
                .and_then(|dir| dir.sync_all())
                .map_err(|error| error.to_string())?;
            Ok(())
        })();
        if let Err(reason) = outcome {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(failed(reason));
        }
        Ok(())
    }
}

/// UTC `YYYY-MM-DD` for a system time (civil-date math, no date crate).
pub fn utc_today_string(now: SystemTime) -> Result<String, QuotaError> {
    let seconds = now
        .duration_since(UNIX_EPOCH)
        .map_err(|error| QuotaError::Clock {
            reason: error.to_string(),
        })?
        .as_secs();
    let days = seconds.div_euclid(86_400) as i64;
    let (year, month, day) = days_to_civil(days);
    Ok(format!("{year:04}-{month:02}-{day:02}"))
}

/// Days since the Unix epoch to a civil (year, month, day) date.
fn days_to_civil(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u32;
    let month = (if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    }) as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at_date_time(year: i64, month: u32, day: u32, hour: u64, minute: u64) -> SystemTime {
        let y = if month <= 2 { year - 1 } else { year };
        let era = y.div_euclid(400);
        let year_of_era = y.rem_euclid(400);
        let month_prime = ((month + 9) % 12) as i64;
        let day_of_year = (153 * month_prime + 2) / 5 + day as i64 - 1;
        let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
        let days = era * 146_097 + day_of_era - 719_468;
        UNIX_EPOCH
            + std::time::Duration::from_secs(days as u64 * 86_400 + hour * 3600 + minute * 60)
    }

    #[test]
    fn civil_dates_match_known_days() {
        assert_eq!(
            utc_today_string(at_date_time(2026, 9, 28, 0, 0)).unwrap(),
            "2026-09-28"
        );
        assert_eq!(
            utc_today_string(at_date_time(2026, 9, 28, 23, 59)).unwrap(),
            "2026-09-28"
        );
        assert_eq!(
            utc_today_string(at_date_time(2026, 9, 29, 0, 0)).unwrap(),
            "2026-09-29"
        );
        assert_eq!(
            utc_today_string(at_date_time(1970, 1, 1, 0, 0)).unwrap(),
            "1970-01-01"
        );
        assert_eq!(
            utc_today_string(at_date_time(2024, 2, 29, 12, 0)).unwrap(),
            "2024-02-29"
        );
    }
}
