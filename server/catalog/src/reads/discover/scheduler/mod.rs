//! The one warm scheduler: keeps the pages people actually use fresh.
//!
//! Using a feature records it (`catalog_discover_activity`); a repeat
//! within five minutes is noted in memory and costs no write. The jobs
//! warm-cycle loop calls [`WarmScheduler::tick`] every 90 seconds: it
//! prunes activity older than a day, takes the user whose due activity
//! was serviced longest ago, and warms each of their features through
//! its registration (the discover page, home, the queue deck). A feature
//! that built is due again in six hours, one that did not in 90 seconds,
//! and one that timed out in 15 minutes. Activity recorded through the
//! page also starts a pass for that user at once, so the loop is the
//! backstop.

pub mod activity;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use crate::reads::discover::listening::{ListeningData, SourceContext};
use crate::reads::discover::ports::BoxFuture;
use activity::{Activity, ActivityDb};
use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::DbError;

/// Activity older than this is forgotten, ms.
const WINDOW_MS: i64 = 86_400_000;
/// A repeat of the same activity within this long is not written, ms.
const DEBOUNCE_MS: i64 = 300_000;
/// Bound on warming one feature.
const FEATURE_TIMEOUT: Duration = Duration::from_secs(300);
/// When a warmed feature is due again, ms.
const WARM_AGAIN_MS: i64 = 6 * 3_600_000;
/// Retry after a feature that did not build, ms.
const RETRY_MS: i64 = 90_000;
/// Retry after a feature that timed out, ms.
const TIMED_OUT_RETRY_MS: i64 = 900_000;

/// A discover feature a user can use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Feature {
    /// The home page.
    Home,
    /// The discover page.
    Discover,
    /// The queue deck.
    Queue,
    /// An artist page's discover sections.
    Artist,
}

impl Feature {
    /// The stored name, one of the table's CHECK values.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Home => "home",
            Self::Discover => "discover",
            Self::Queue => "queue",
            Self::Artist => "artist",
        }
    }

    /// The feature a stored or requested name stands for.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "home" => Some(Self::Home),
            "discover" => Some(Self::Discover),
            "queue" => Some(Self::Queue),
            "artist" => Some(Self::Artist),
            _ => None,
        }
    }
}

/// One feature the scheduler warms.
pub trait Warm: Send + Sync {
    /// The feature it warms.
    fn feature(&self) -> Feature;
    /// Build the user's page now; true when a new one was kept.
    fn warm<'a>(&'a self, user_id: &'a str) -> BoxFuture<'a, bool>;
}

/// The warm scheduler.
pub struct WarmScheduler {
    activity: ActivityDb,
    listening: ListeningData,
    clock: Arc<dyn Clock>,
    enabled: bool,
    registrations: Vec<Arc<dyn Warm>>,
    warming: Mutex<HashSet<String>>,
    recorded: Mutex<HashMap<Activity, i64>>,
}

/// Clears a user's warming mark however the pass ends.
struct WarmingGuard<'a> {
    scheduler: &'a WarmScheduler,
    user_id: String,
}

impl Drop for WarmingGuard<'_> {
    fn drop(&mut self) {
        lock(&self.scheduler.warming).remove(&self.user_id);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // The sets stay whole whatever panicked while the lock was held.
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// What one activity names beyond its feature (artist activity only).
#[derive(Debug, Clone, Copy, Default)]
pub struct ArtistDetail<'a> {
    /// The artist's MBID.
    pub artist_mbid: Option<&'a str>,
    /// The artist-page section.
    pub section: Option<&'a str>,
    /// The provider behind the section.
    pub provider: Option<&'a str>,
}

impl WarmScheduler {
    /// A scheduler over the activity in `activity` that warms through
    /// `registrations`. `enabled` is the deployment's kill switch.
    pub fn new(
        activity: ActivityDb,
        listening: ListeningData,
        clock: Arc<dyn Clock>,
        enabled: bool,
        registrations: Vec<Arc<dyn Warm>>,
    ) -> Self {
        Self {
            activity,
            listening,
            clock,
            enabled,
            registrations,
            warming: Mutex::default(),
            recorded: Mutex::default(),
        }
    }

    /// Record that the user used `feature`, unless the same was recorded
    /// in the last five minutes. Returns the MusicBrainz source it was
    /// recorded under, and whether a row was written.
    pub async fn record(
        &self,
        user_id: &str,
        feature: Feature,
        detail: ArtistDetail<'_>,
    ) -> Result<(SourceContext, bool), DbError> {
        let source = self.listening.source();
        let row = Activity {
            user_id: user_id.to_owned(),
            feature,
            artist_mbid: detail.artist_mbid.unwrap_or_default().to_owned(),
            section: detail.section.unwrap_or_default().to_owned(),
            provider: detail.provider.unwrap_or_default().to_owned(),
            source: source.namespace(),
        };
        let now = self.clock.now_ms();
        {
            let mut recorded = lock(&self.recorded);
            recorded.retain(|_, at| now - *at < DEBOUNCE_MS);
            if recorded.contains_key(&row) {
                return Ok((source, false));
            }
            recorded.insert(row.clone(), now);
        }
        if let Err(error) = self.activity.record(row.clone(), now).await {
            lock(&self.recorded).remove(&row);
            return Err(error);
        }
        Ok((source, true))
    }

    /// One pass of the warm cycle: prune old activity, then warm what is
    /// due.
    pub async fn tick(&self) {
        if let Err(error) = self.activity.prune(self.clock.now_ms() - WINDOW_MS).await {
            tracing::warn!(%error, "discover activity prune failed; warming anyway");
        }
        self.run_due(None).await;
    }

    /// Take the first user with activity due under the current source
    /// (or `only_user`) and warm their features.
    pub async fn run_due(&self, only_user: Option<&str>) {
        if !self.enabled {
            return;
        }
        let source = self.listening.namespace();
        let now = self.clock.now_ms();
        let rows = match self
            .activity
            .due(&source, now - WINDOW_MS, now, only_user)
            .await
        {
            Ok(rows) => rows,
            Err(cause) => {
                tracing::warn!(%cause, "discover activity unreadable; skipping this warm pass");
                return;
            }
        };
        let Some(user_id) = rows.first().map(|row| row.user_id.clone()) else {
            return;
        };
        if !lock(&self.warming).insert(user_id.clone()) {
            return;
        }
        let _guard = WarmingGuard {
            scheduler: self,
            user_id: user_id.clone(),
        };
        for row in rows.iter().filter(|row| row.user_id == user_id) {
            if self.listening.namespace() != source {
                break;
            }
            let wait = self.warm_one(row).await;
            let now = self.clock.now_ms();
            if let Err(error) = self.activity.finish(row, now, now + wait).await {
                tracing::warn!(%error, "could not reschedule discover activity");
            }
        }
    }

    /// Warm one feature; returns how long until it is due again, ms.
    async fn warm_one(&self, row: &Activity) -> i64 {
        let Some(registration) = self
            .registrations
            .iter()
            .find(|registration| registration.feature() == row.feature)
        else {
            // Artist pages have no background warmer; check back later.
            return WARM_AGAIN_MS;
        };
        match tokio::time::timeout(FEATURE_TIMEOUT, registration.warm(&row.user_id)).await {
            Ok(true) => WARM_AGAIN_MS,
            Ok(false) => RETRY_MS,
            Err(_) => {
                tracing::warn!(feature = row.feature.as_str(), "discover warm timed out");
                TIMED_OUT_RETRY_MS
            }
        }
    }
}
