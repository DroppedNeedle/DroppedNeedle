//! Process-wide registry of degraded external services.
//!
//! Ports v2's `ServiceHealthRegistry`: one answer to "is this service
//! having trouble right now?", read by `GET /api/v3/system/health` to drive
//! the header status dot. Entries expire on their own after a TTL that each
//! fresh signal slides forward, so a service heals without a reset or a
//! sweeper once the failures stop.
//!
//! v2 fed it from circuit breakers that opened after several consecutive
//! failures. The provider clients here report failures only (through the
//! [`DegradationSink`](super::degradation::DegradationSink)), so the same
//! rule becomes "N failures inside a short window": one blip never flags a
//! service, a run of them does.

use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    sync::Mutex,
    time::{Duration, Instant},
};

/// How long a degraded entry lives after its latest signal (v2 default).
pub const ENTRY_TTL: Duration = Duration::from_secs(300);
/// The window the failure threshold counts inside (v2 breaker timeout).
pub const FAILURE_WINDOW: Duration = Duration::from_secs(60);

/// A source the provider clients report on, with what a run of failures
/// means for the user. Capabilities, messages and thresholds are v2's.
struct Watched {
    source: &'static str,
    capability: &'static str,
    message: &'static str,
    threshold: usize,
}

const WATCHED: &[Watched] = &[
    Watched {
        source: "musicbrainz",
        capability: "metadata",
        message: "MusicBrainz, our main source for music data, is having trouble - \
                  search and album or artist details may be incomplete for now.",
        threshold: 5,
    },
    Watched {
        source: "listenbrainz",
        capability: "music data",
        message: "ListenBrainz music data is temporarily unavailable.",
        threshold: 10,
    },
    Watched {
        source: "lastfm",
        capability: "music data",
        message: "Last.fm is temporarily unavailable - some listening stats and \
                  recommendations may be missing.",
        threshold: 5,
    },
    Watched {
        source: "audiodb",
        capability: "artist info",
        message: "Extra artist artwork (TheAudioDB) is temporarily unavailable.",
        threshold: 5,
    },
    Watched {
        source: "acoustid",
        capability: "metadata",
        message: "AcoustID, our fingerprint identification source, is having trouble - \
                  identification falls back to review for now.",
        threshold: 5,
    },
];

/// How bad a degraded entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Working with gaps or a fallback.
    Degraded,
    /// Not answering at all.
    Down,
}

impl Severity {
    /// The wire spelling (v2 strings).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Degraded => "degraded",
            Self::Down => "down",
        }
    }
}

/// One signal that a service capability is degraded.
#[derive(Debug, Clone)]
pub struct Degradation {
    /// Lowercase service key, e.g. `listenbrainz`.
    pub service: String,
    /// What is affected, e.g. `music data`.
    pub capability: String,
    /// One user-facing line.
    pub message: String,
    /// What is used instead, when anything is.
    pub fallback: Option<String>,
    /// How bad it is.
    pub severity: Severity,
    /// How long the entry lives without a fresh signal.
    pub ttl: Duration,
}

/// A live degraded entry, as the health route reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DegradedService {
    /// Lowercase service key.
    pub service: String,
    /// What is affected.
    pub capability: String,
    /// How bad it is.
    pub severity: Severity,
    /// One user-facing line.
    pub message: String,
    /// What is used instead, when anything is.
    pub fallback: Option<String>,
    /// How long it has been degraded.
    pub degraded_for: Duration,
}

#[derive(Debug)]
struct Entry {
    severity: Severity,
    message: String,
    fallback: Option<String>,
    since: Instant,
    until: Instant,
}

#[derive(Debug, Default)]
struct State {
    degraded: BTreeMap<(String, String), Entry>,
    failures: HashMap<&'static str, VecDeque<Instant>>,
}

/// The registry. One per process, held by the shared provider deps.
#[derive(Debug, Default)]
pub struct ServiceHealth {
    state: Mutex<State>,
}

impl ServiceHealth {
    /// An empty registry: every service healthy.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record or refresh a degraded capability. The first signal sets the
    /// start time; every signal slides the expiry forward.
    pub fn mark_degraded(&self, signal: Degradation, now: Instant) {
        let mut state = self.lock();
        let key = (signal.service, signal.capability);
        let since = state.degraded.get(&key).map_or(now, |entry| entry.since);
        state.degraded.insert(
            key,
            Entry {
                severity: signal.severity,
                message: signal.message,
                fallback: signal.fallback,
                since,
                until: now + signal.ttl,
            },
        );
    }

    /// Count one failed call to `source`. Once the source's threshold is
    /// reached inside [`FAILURE_WINDOW`], it is marked degraded, and each
    /// further failure keeps it so. Sources nobody watches are ignored.
    pub fn record_failure(&self, source: &str, now: Instant) {
        let Some(watched) = WATCHED.iter().find(|watched| watched.source == source) else {
            return;
        };
        let sustained = {
            let mut state = self.lock();
            let recent = state.failures.entry(watched.source).or_default();
            recent.push_back(now);
            while recent.len() > watched.threshold {
                recent.pop_front();
            }
            recent.len() == watched.threshold
                && recent
                    .front()
                    .is_some_and(|first| now.saturating_duration_since(*first) <= FAILURE_WINDOW)
        };
        if sustained {
            self.mark_degraded(
                Degradation {
                    service: watched.source.to_owned(),
                    capability: watched.capability.to_owned(),
                    message: watched.message.to_owned(),
                    fallback: None,
                    severity: Severity::Degraded,
                    ttl: ENTRY_TTL,
                },
                now,
            );
        }
    }

    /// Live degraded entries, sorted by service then capability. Expired
    /// entries are dropped as a side effect.
    #[must_use]
    pub fn current(&self, now: Instant) -> Vec<DegradedService> {
        let mut state = self.lock();
        state.degraded.retain(|_, entry| entry.until >= now);
        state
            .degraded
            .iter()
            .map(|((service, capability), entry)| DegradedService {
                service: service.clone(),
                capability: capability.clone(),
                severity: entry.severity,
                message: entry.message.clone(),
                fallback: entry.fallback.clone(),
                degraded_for: now.saturating_duration_since(entry.since),
            })
            .collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_of_failures_degrades_and_the_entry_expires() {
        let health = ServiceHealth::new();
        let start = Instant::now();
        for second in 0..4 {
            health.record_failure("lastfm", start + Duration::from_secs(second));
        }
        assert!(
            health.current(start).is_empty(),
            "four blips are not an outage"
        );

        health.record_failure("lastfm", start + Duration::from_secs(4));
        let live = health.current(start + Duration::from_secs(5));
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].service, "lastfm");
        assert_eq!(live[0].capability, "music data");
        assert_eq!(live[0].degraded_for, Duration::from_secs(1));

        let later = start + Duration::from_secs(4) + ENTRY_TTL + Duration::from_secs(1);
        assert!(health.current(later).is_empty(), "heals once signals stop");
    }

    #[test]
    fn spread_out_failures_never_degrade() {
        let health = ServiceHealth::new();
        let start = Instant::now();
        for minute in 0..10 {
            health.record_failure("musicbrainz", start + Duration::from_secs(minute * 30));
        }
        assert!(health.current(start + Duration::from_secs(300)).is_empty());
    }
}
