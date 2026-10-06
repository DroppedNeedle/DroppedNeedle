//! Process-wide registry of degraded external services.
//!
//! Ports v2's `ServiceHealthRegistry`: one answer to "is this service
//! having trouble right now?", read by `GET /api/v3/system/health` to drive
//! the header status dot. Entries expire on their own after a TTL that each
//! fresh failure slides forward, so a service heals without a reset or a
//! sweeper once the failures stop.
//!
//! v2 fed it from circuit breakers that opened after several consecutive
//! failures. Here the provider clients report through the
//! [`DegradationSink`](super::degradation::DegradationSink): each wire
//! failure counts, each answered request clears the count, and a run of
//! failures inside a short window marks the service. One blip never flags
//! a service; a sustained outage does.

use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    sync::Mutex,
    time::{Duration, Instant},
};

/// How long a degraded entry lives after its latest failure (v2 default).
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
        source: "musicbrainz-brainzmash",
        capability: "metadata",
        message: "BrainzMash, our community source for music data, is having trouble - \
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

/// A live degraded entry, as the health route reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DegradedService {
    /// Lowercase service key.
    pub service: &'static str,
    /// What is affected.
    pub capability: &'static str,
    /// One user-facing line.
    pub message: &'static str,
    /// How long it has been degraded.
    pub degraded_for: Duration,
}

#[derive(Debug)]
struct Entry {
    watched: &'static Watched,
    since: Instant,
    until: Instant,
}

#[derive(Debug, Default)]
struct State {
    degraded: BTreeMap<&'static str, Entry>,
    failures: HashMap<&'static str, VecDeque<Instant>>,
}

impl std::fmt::Debug for Watched {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.source)
    }
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

    /// Count one failed call to `source`. Once the source's threshold is
    /// reached inside [`FAILURE_WINDOW`], it is marked degraded for
    /// [`ENTRY_TTL`], and each further failure keeps it so. Sources nobody
    /// watches are ignored.
    pub fn record_failure(&self, source: &str, now: Instant) {
        let Some(watched) = WATCHED.iter().find(|watched| watched.source == source) else {
            return;
        };
        let mut state = self.lock();
        let recent = state.failures.entry(watched.source).or_default();
        recent.push_back(now);
        while recent.len() > watched.threshold {
            recent.pop_front();
        }
        let sustained = recent.len() == watched.threshold
            && recent
                .front()
                .is_some_and(|first| now.saturating_duration_since(*first) <= FAILURE_WINDOW);
        if sustained {
            let since = state
                .degraded
                .get(watched.source)
                .map_or(now, |entry| entry.since);
            state.degraded.insert(
                watched.source,
                Entry {
                    watched,
                    since,
                    until: now + ENTRY_TTL,
                },
            );
        }
    }

    /// `source` answered: its run of failures starts over. A live entry
    /// stays until its TTL runs out, so one lucky call does not hide an
    /// outage.
    pub fn record_success(&self, source: &str) {
        let mut state = self.lock();
        if let Some(recent) = state.failures.get_mut(source) {
            recent.clear();
        }
    }

    /// Live degraded entries, sorted by service. Expired entries are
    /// dropped as a side effect.
    #[must_use]
    pub fn current(&self, now: Instant) -> Vec<DegradedService> {
        let mut state = self.lock();
        state.degraded.retain(|_, entry| entry.until >= now);
        state
            .degraded
            .values()
            .map(|entry| DegradedService {
                service: entry.watched.source,
                capability: entry.watched.capability,
                message: entry.watched.message,
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
        assert!(health.current(later).is_empty(), "heals once failures stop");
    }

    #[test]
    fn spread_out_or_interrupted_failures_never_degrade() {
        let health = ServiceHealth::new();
        let start = Instant::now();
        for step in 0..10 {
            health.record_failure("musicbrainz", start + Duration::from_secs(step * 30));
        }
        for second in 0..8 {
            if second == 4 {
                health.record_success("audiodb");
            }
            health.record_failure("audiodb", start + Duration::from_secs(second));
        }
        assert!(health.current(start + Duration::from_secs(300)).is_empty());
    }
}
