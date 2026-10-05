//! Watchdog and retry timing, ported from the orchestrator.
//!
//! The poll loop watches real byte progress: an actively-transferring
//! batch that stops moving trips the stall timeout, a batch stuck in the
//! peer's remote queue trips the (more generous) queued timeout, and a
//! fresh enqueue that never materializes a transfer fails over fast
//! instead of sitting out the full queued window. The 6-hour ceiling is
//! an absolute backstop the minutes-scale watchdogs normally beat.
//!
//! This module is pure evaluation plus retry math; the sleeping poll loop
//! that drives it belongs to the runtime wiring.

/// One poll's progress sample.
#[derive(Debug, Clone)]
pub struct PollSample {
    /// Seconds since the loop started.
    pub elapsed_seconds: f64,
    /// Seconds since any transfer moved bytes.
    pub idle_seconds: f64,
    /// At least one transfer is actively moving bytes.
    pub has_active_transfer: bool,
    /// Bytes downloaded so far across the batch.
    pub downloaded_bytes: u64,
    /// True once every transfer reached a client-terminal state.
    pub all_terminal: bool,
    /// True when every terminal transfer succeeded.
    pub all_succeeded: bool,
    /// Seconds since enqueue with zero client-side transfer records.
    pub materialize_wait_seconds: f64,
}

/// Watchdog verdict for one sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchdogOutcome {
    /// Keep polling.
    Continue,
    /// Every transfer terminal and succeeded.
    Completed,
    /// Every transfer terminal, at least one failed.
    Terminal,
    /// An active transfer stopped making progress.
    Stalled,
    /// Stuck in the peer's remote upload queue too long.
    QueuedTimeout,
    /// No transfer record materialized after enqueue.
    MaterializeTimeout,
    /// Hit the absolute poll ceiling.
    Deadline,
}

/// Watchdog timing. Defaults are the v2 production values.
#[derive(Debug, Clone)]
pub struct WatchdogConfig {
    /// Poll cadence in seconds.
    pub poll_interval_seconds: f64,
    /// Idle-with-active-transfer limit (default 30 minutes).
    pub stall_timeout_seconds: f64,
    /// Idle-in-remote-queue limit (default 2 hours, more generous than the
    /// stall timeout because remote queues move slowly).
    pub queued_timeout_seconds: f64,
    /// Absolute poll-loop ceiling (default 6 hours).
    pub deadline_seconds: f64,
    /// Fresh-enqueue materialization limit (default 90 seconds).
    pub materialize_seconds: f64,
    /// Reap threshold for tasks with no live poller (default 30 minutes).
    pub reap_threshold_seconds: f64,
}

impl Default for WatchdogConfig {
    fn default() -> Self {
        Self {
            poll_interval_seconds: 2.0,
            stall_timeout_seconds: 30.0 * 60.0,
            queued_timeout_seconds: 120.0 * 60.0,
            deadline_seconds: 6.0 * 3600.0,
            materialize_seconds: 90.0,
            reap_threshold_seconds: 1800.0,
        }
    }
}

/// Byte-progress watchdog evaluator.
#[derive(Debug, Clone)]
pub struct Watchdog {
    config: WatchdogConfig,
}

impl Watchdog {
    /// Build an evaluator from explicit timing.
    pub fn new(config: WatchdogConfig) -> Self {
        Self { config }
    }

    /// Judge one poll sample. The order mirrors `_poll_until_done`:
    /// terminal states first, then the ceiling, then the materialize
    /// fast-fail, then stall versus queued off real byte progress.
    pub fn evaluate(&self, sample: &PollSample) -> WatchdogOutcome {
        if sample.all_terminal {
            return if sample.all_succeeded {
                WatchdogOutcome::Completed
            } else {
                WatchdogOutcome::Terminal
            };
        }
        if sample.elapsed_seconds >= self.config.deadline_seconds {
            return WatchdogOutcome::Deadline;
        }
        if sample.downloaded_bytes == 0
            && !sample.has_active_transfer
            && sample.materialize_wait_seconds >= self.config.materialize_seconds
            && sample.elapsed_seconds < self.config.queued_timeout_seconds
        {
            return WatchdogOutcome::MaterializeTimeout;
        }
        if sample.has_active_transfer {
            if sample.idle_seconds >= self.config.stall_timeout_seconds {
                return WatchdogOutcome::Stalled;
            }
        } else if sample.idle_seconds >= self.config.queued_timeout_seconds {
            return WatchdogOutcome::QueuedTimeout;
        }
        WatchdogOutcome::Continue
    }

    /// Reap threshold: active this long with no poller at all means the
    /// loop is dead. Callers must first skip tasks owned by a live loop
    /// on this instance or a pre-rebuild registry entry.
    pub fn reap_threshold_seconds(&self) -> f64 {
        self.config.reap_threshold_seconds
    }
}

/// Auto-retry timing: per-task exponential backoff, base times two to the
/// retry count, capped at 24 hours. Defaults are base 15 minutes, max 6
/// attempts, giving the ladder 15, 30, 60, 120, 240, 480.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// Master switch.
    pub enabled: bool,
    /// Retry ceiling per task.
    pub max_attempts: u32,
    /// Backoff base in minutes.
    pub base_interval_minutes: f64,
    /// Backoff cap in seconds.
    pub cap_seconds: f64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            max_attempts: 6,
            base_interval_minutes: 15.0,
            cap_seconds: 86_400.0,
        }
    }
}

impl RetryPolicy {
    /// Configured max attempts (0 when auto-retry is off).
    pub fn auto_retry_max(&self) -> u32 {
        if self.enabled { self.max_attempts } else { 0 }
    }

    /// Backoff for a task that already retried `retry_count` times.
    pub fn backoff_seconds(&self, retry_count: u32) -> f64 {
        let grown = self.base_interval_minutes * 60.0 * 2_f64.powi(retry_count.min(20) as i32);
        grown.min(self.cap_seconds)
    }

    /// The full backoff ladder in minutes, so the UI's "retry scheduled"
    /// line matches when the sweep actually fires.
    pub fn ladder_minutes(&self) -> Vec<i64> {
        (0..self.auto_retry_max())
            .map(|n| (self.backoff_seconds(n) / 60.0).round() as i64)
            .collect()
    }

    /// Unix time the next auto-retry is due, or `None` when the task will
    /// not auto-retry (off, at ceiling, or already terminal-complete).
    /// `anchor` is the task's terminal time (`completed_at`/`updated_at`).
    pub fn next_retry_at(&self, retry_count: u32, anchor: f64, status: &str) -> Option<f64> {
        if !self.enabled || retry_count >= self.max_attempts {
            return None;
        }
        if !matches!(status, "failed" | "partial") {
            return None;
        }
        Some(anchor + self.backoff_seconds(retry_count))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> PollSample {
        PollSample {
            elapsed_seconds: 10.0,
            idle_seconds: 0.0,
            has_active_transfer: true,
            downloaded_bytes: 100,
            all_terminal: false,
            all_succeeded: false,
            materialize_wait_seconds: 0.0,
        }
    }

    #[test]
    fn stall_trips_on_idle_active_transfer() {
        let watchdog = Watchdog::new(WatchdogConfig::default());
        let mut sample = sample();
        sample.idle_seconds = 31.0 * 60.0;
        assert_eq!(watchdog.evaluate(&sample), WatchdogOutcome::Stalled);
    }

    #[test]
    fn queued_timeout_needs_the_generous_window() {
        let watchdog = Watchdog::new(WatchdogConfig::default());
        let mut sample = sample();
        sample.has_active_transfer = false;
        sample.downloaded_bytes = 0;
        sample.idle_seconds = 31.0 * 60.0;
        // Past the stall mark but with no active transfer, and past the
        // materialize window: the materialize fast-fail fires first.
        sample.materialize_wait_seconds = 120.0;
        assert_eq!(
            watchdog.evaluate(&sample),
            WatchdogOutcome::MaterializeTimeout
        );
    }

    #[test]
    fn default_ladder_matches_v2() {
        assert_eq!(
            RetryPolicy::default().ladder_minutes(),
            vec![15, 30, 60, 120, 240, 480]
        );
    }
}
