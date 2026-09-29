//! Priority-queue slots for provider work.
//!
//! Three lanes keep user-facing lookups fast while background jobs still make
//! progress: a wide user lane, a wide image lane (page loads fan out dozens
//! of cover fetches), and a narrow background lane that additionally waits
//! out user activity before admitting anything. Ports v2's
//! `PriorityQueueManager` caps (20 user / 24 image / 5 background) and its
//! 2s user-quiet window.
//!
//! Callers name their lane with [`RequestPriority`] at every call site.
//! Background jobs pass [`RequestPriority::BackgroundSync`] (or
//! `Opportunistic`) explicitly; there is no default, so a background job can
//! never silently consume user slots.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use thiserror::Error;
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};

/// Which lane a provider call runs in. Lower wins ties in the rate limiter;
/// the slot manager maps each level to its lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RequestPriority {
    /// A request the user is waiting on right now.
    UserInitiated = 0,
    /// Cover-art fetch: user-visible but degradable to a placeholder.
    ImageFetch = 1,
    /// Speculative fetch for content about to scroll into view.
    PrefetchVisible = 2,
    /// Explicit background work: syncs, imports, refreshes. Background jobs
    /// pass this level (or `Opportunistic`) at the call site.
    BackgroundSync = 3,
    /// Nice-to-have work that yields to everything else.
    Opportunistic = 4,
}

/// How long the background lane waits for user activity to go quiet.
pub const USER_QUIET_WINDOW: Duration = Duration::from_secs(2);

const USER_SLOTS: usize = 20;
const IMAGE_SLOTS: usize = 24;
const BACKGROUND_SLOTS: usize = 5;

/// Slot acquisition failed because the manager is shutting down. The
/// semaphores are never closed in normal operation, so callers only see this
/// during teardown.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("provider slot manager is shutting down")]
pub struct SlotError;

#[derive(Debug)]
struct ActivityState {
    active: bool,
    at: tokio::time::Instant,
}

/// The three provider lanes plus the user-activity gate for background work.
///
/// Built once inside [`Providers`](super::Providers) and shared by every
/// provider client. `Clone` shares the lanes; permits are per acquisition.
#[derive(Debug)]
pub struct SlotManager {
    user: Arc<Semaphore>,
    image: Arc<Semaphore>,
    background: Arc<Semaphore>,
    activity: Mutex<ActivityState>,
    changed: Notify,
    quiet_window: Duration,
    background_waiters: AtomicUsize,
}

impl SlotManager {
    /// The production lanes: 20 user / 24 image / 5 background with a 2s
    /// user-quiet window.
    #[must_use]
    pub fn new() -> Self {
        Self::with_quiet_window(USER_QUIET_WINDOW)
    }

    /// Lanes with a custom user-quiet window, for fast tests.
    #[must_use]
    pub fn with_quiet_window(quiet_window: Duration) -> Self {
        Self {
            user: Arc::new(Semaphore::new(USER_SLOTS)),
            image: Arc::new(Semaphore::new(IMAGE_SLOTS)),
            background: Arc::new(Semaphore::new(BACKGROUND_SLOTS)),
            activity: Mutex::new(ActivityState {
                active: false,
                at: tokio::time::Instant::now(),
            }),
            changed: Notify::new(),
            quiet_window,
            background_waiters: AtomicUsize::new(0),
        }
    }

    /// Take a lane permit for `priority`, waiting as the lane requires.
    ///
    /// User calls mark activity and take the user lane; image fetches take
    /// the image lane; prefetch, background, and opportunistic work first
    /// wait for user activity to go quiet, then take the background lane.
    ///
    /// # Errors
    ///
    /// Returns [`SlotError`] only while shutting down.
    pub async fn acquire_slot(
        &self,
        priority: RequestPriority,
    ) -> Result<OwnedSemaphorePermit, SlotError> {
        match priority {
            RequestPriority::UserInitiated => {
                self.mark_user_activity();
                Arc::clone(&self.user)
                    .acquire_owned()
                    .await
                    .map_err(|_| SlotError)
            }
            RequestPriority::ImageFetch => Arc::clone(&self.image)
                .acquire_owned()
                .await
                .map_err(|_| SlotError),
            RequestPriority::PrefetchVisible
            | RequestPriority::BackgroundSync
            | RequestPriority::Opportunistic => {
                self.wait_for_quiet().await;
                Arc::clone(&self.background)
                    .acquire_owned()
                    .await
                    .map_err(|_| SlotError)
            }
        }
    }

    /// Record user-facing activity, holding the background lane until the
    /// quiet window passes with no further activity.
    pub fn mark_user_activity(&self) {
        let mut activity = self
            .activity
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        activity.active = true;
        activity.at = tokio::time::Instant::now();
    }

    /// Whether the user lane saw activity inside the quiet window.
    #[must_use]
    pub fn is_user_active(&self) -> bool {
        let mut activity = self
            .activity
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if activity.at.elapsed() > self.quiet_window {
            activity.active = false;
        }
        activity.active
    }

    /// Lane snapshot for health endpoints and tests.
    #[must_use]
    pub fn stats(&self) -> SlotStats {
        SlotStats {
            user_slots_available: self.user.available_permits(),
            image_slots_available: self.image.available_permits(),
            background_slots_available: self.background.available_permits(),
            user_active: self.is_user_active(),
            background_waiters: self.background_waiters.load(Ordering::Relaxed),
        }
    }

    async fn wait_for_quiet(&self) {
        self.background_waiters.fetch_add(1, Ordering::Relaxed);
        loop {
            let sleep_for = {
                let mut activity = self
                    .activity
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                if !activity.active || activity.at.elapsed() >= self.quiet_window {
                    activity.active = false;
                    self.changed.notify_waiters();
                    break;
                }
                self.quiet_window.saturating_sub(activity.at.elapsed())
            };
            tokio::select! {
                () = self.changed.notified() => {}
                () = tokio::time::sleep(sleep_for + Duration::from_millis(100)) => {}
            }
        }
        self.background_waiters.fetch_sub(1, Ordering::Relaxed);
    }
}

impl Default for SlotManager {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for SlotManager {
    fn clone(&self) -> Self {
        Self {
            user: Arc::clone(&self.user),
            image: Arc::clone(&self.image),
            background: Arc::clone(&self.background),
            activity: Mutex::new(ActivityState {
                active: false,
                at: tokio::time::Instant::now(),
            }),
            changed: Notify::new(),
            quiet_window: self.quiet_window,
            background_waiters: AtomicUsize::new(0),
        }
    }
}

/// Point-in-time lane counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotStats {
    /// Free user-lane permits.
    pub user_slots_available: usize,
    /// Free image-lane permits.
    pub image_slots_available: usize,
    /// Free background-lane permits.
    pub background_slots_available: usize,
    /// Whether user activity is inside the quiet window.
    pub user_active: bool,
    /// Background callers currently waiting for quiet.
    pub background_waiters: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lanes_start_full_and_idle() {
        let slots = SlotManager::new();
        assert_eq!(
            slots.stats(),
            SlotStats {
                user_slots_available: 20,
                image_slots_available: 24,
                background_slots_available: 5,
                user_active: false,
                background_waiters: 0,
            }
        );
    }

    #[tokio::test]
    async fn user_and_image_take_their_own_lanes() {
        let slots = SlotManager::new();
        let _user = slots
            .acquire_slot(RequestPriority::UserInitiated)
            .await
            .expect("user slot");
        let _image = slots
            .acquire_slot(RequestPriority::ImageFetch)
            .await
            .expect("image slot");
        let stats = slots.stats();
        assert_eq!(stats.user_slots_available, 19);
        assert_eq!(stats.image_slots_available, 23);
        assert_eq!(stats.background_slots_available, 5);
        assert!(stats.user_active);
    }

    #[tokio::test]
    async fn background_waits_out_user_activity() {
        let slots = SlotManager::with_quiet_window(Duration::from_millis(80));
        slots.mark_user_activity();
        let started = tokio::time::Instant::now();
        let _permit = slots
            .acquire_slot(RequestPriority::BackgroundSync)
            .await
            .expect("background slot after quiet");
        assert!(
            started.elapsed() >= Duration::from_millis(80),
            "background admitted during user activity"
        );
        assert!(!slots.is_user_active());
    }

    #[tokio::test]
    async fn background_flows_when_idle() {
        let slots = SlotManager::with_quiet_window(Duration::from_secs(60));
        let started = tokio::time::Instant::now();
        let _permit = slots
            .acquire_slot(RequestPriority::Opportunistic)
            .await
            .expect("idle background admits at once");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "idle background stalled"
        );
    }
}
