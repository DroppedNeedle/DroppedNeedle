//! The in-process event hub.
//!
//! Producers call [`EventHub::publish`] (or [`EventHub::notify`] for one
//! user); every open stream holds a [`Subscription`]. One bounded
//! broadcast channel carries every frame and each subscription keeps only
//! the frames meant for its user, so per-user filtering costs one string
//! compare per frame and a slow stream never holds up a producer.
//!
//! The hub also keeps the latest frame of each event per audience and
//! replays it to a new stream, the way v2's `SSEPublisher` did: a fresh
//! tab sees current presence and library revisions at once, and a tab
//! that was hidden when a notice fired still hears about it. Unlike v2,
//! one-off notices replay only for [`NOTICE_REPLAY_WINDOW`], so opening a
//! new tab no longer re-toasts a download from last week.
//!
//! A stream that falls more than [`CAPACITY`] frames behind skips ahead
//! and gets the retained frames again, so it converges on current state
//! (v2 drained the queue and kept the newest message for the same reason).

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use tokio::sync::{Notify, broadcast, watch};

use super::model::{Audience, Event, Frame, Replay, UserNotice};

/// Frames buffered per stream before it starts skipping (v2 queue size
/// was 200).
pub const CAPACITY: usize = 256;

/// How long a one-off notice keeps replaying to new streams.
pub const NOTICE_REPLAY_WINDOW: Duration = Duration::from_secs(10 * 60);

/// Retained frames, keyed by audience and event name.
type Retained = HashMap<(Audience, &'static str), Arc<Frame>>;

/// The hub. Cheap to clone; clones share everything.
#[derive(Clone)]
pub struct EventHub {
    inner: Arc<Inner>,
}

struct Inner {
    sender: broadcast::Sender<Arc<Frame>>,
    retained: Mutex<Retained>,
    activity_poke: Notify,
    closed: watch::Sender<bool>,
}

impl Default for EventHub {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for EventHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventHub")
            .field("streams", &self.inner.sender.receiver_count())
            .finish()
    }
}

impl EventHub {
    /// An empty hub with no streams.
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(CAPACITY);
        let (closed, _) = watch::channel(false);
        Self {
            inner: Arc::new(Inner {
                sender,
                retained: Mutex::new(HashMap::new()),
                activity_poke: Notify::new(),
                closed,
            }),
        }
    }

    /// Send one event to every stream allowed to see it. Never blocks and
    /// never fails the caller: a payload that cannot encode is logged and
    /// dropped.
    pub fn publish(&self, event: Event) {
        let frame = match Frame::encode(event) {
            Ok(frame) => Arc::new(frame),
            Err(error) => {
                tracing::warn!(%error, "event payload did not encode; dropped");
                return;
            }
        };
        // Retain and send under one lock, and subscribe under the same
        // lock, so a new stream sees each frame exactly once: either in
        // its replay or on its receiver.
        let mut retained = self.lock_retained();
        retained.insert((frame.audience.clone(), frame.name), frame.clone());
        // An error only means no stream is open right now.
        let _ = self.inner.sender.send(frame);
    }

    /// Send one notice to one user's streams.
    pub fn notify(&self, user_id: &str, notice: UserNotice) {
        self.publish(Event::User {
            user_id: user_id.to_owned(),
            notice,
        });
    }

    /// Open a subscription for one user's stream. It replays the retained
    /// frames first, oldest first, then follows live frames.
    pub fn subscribe(&self, user_id: &str) -> Subscription {
        let retained = self.lock_retained();
        let receiver = self.inner.sender.subscribe();
        let replay = replayable(&retained, user_id);
        drop(retained);
        Subscription {
            user_id: user_id.to_owned(),
            replay,
            receiver,
            closed: self.inner.closed.subscribe(),
            hub: self.clone(),
        }
    }

    /// Ask the library revision poller to look now instead of at its next
    /// tick (a scan just changed state).
    pub fn poke_activity(&self) {
        self.inner.activity_poke.notify_one();
    }

    /// Resolves once someone pokes activity. A poke with nobody waiting is
    /// kept for the next wait.
    pub async fn activity_poked(&self) {
        self.inner.activity_poke.notified().await;
    }

    /// End every open stream (server shutdown), so connection draining
    /// does not wait on them.
    pub fn close(&self) {
        self.inner.closed.send_replace(true);
    }

    /// Open streams right now.
    pub fn stream_count(&self) -> usize {
        self.inner.sender.receiver_count()
    }

    fn lock_retained(&self) -> std::sync::MutexGuard<'_, Retained> {
        self.inner
            .retained
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// The retained frames `user_id` should see on connect, oldest first.
fn replayable(retained: &Retained, user_id: &str) -> VecDeque<Arc<Frame>> {
    let mut frames: Vec<Arc<Frame>> = retained
        .values()
        .filter(|frame| frame.visible_to(user_id))
        .filter(|frame| match frame.replay {
            Replay::State => true,
            Replay::Notice => frame.at.elapsed() <= NOTICE_REPLAY_WINDOW,
        })
        .cloned()
        .collect();
    frames.sort_by_key(|frame| frame.at);
    frames.into()
}

/// One stream's view of the hub.
pub struct Subscription {
    user_id: String,
    replay: VecDeque<Arc<Frame>>,
    receiver: broadcast::Receiver<Arc<Frame>>,
    closed: watch::Receiver<bool>,
    hub: EventHub,
}

impl Subscription {
    /// The next frame for this user, or `None` once the hub closes.
    /// Cancel-safe: dropping the future loses nothing.
    pub async fn next(&mut self) -> Option<Arc<Frame>> {
        loop {
            if let Some(frame) = self.replay.pop_front() {
                return Some(frame);
            }
            if *self.closed.borrow() {
                return None;
            }
            tokio::select! {
                received = self.receiver.recv() => match received {
                    Ok(frame) if frame.visible_to(&self.user_id) => return Some(frame),
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::debug!(skipped, "event stream fell behind; resending current state");
                        // Drop the backlog and start again from current
                        // state, under the publish lock like `subscribe`.
                        let retained = self.hub.lock_retained();
                        self.receiver = self.receiver.resubscribe();
                        self.replay = replayable(&retained, &self.user_id);
                    }
                    Err(broadcast::error::RecvError::Closed) => return None,
                },
                changed = self.closed.changed() => {
                    if changed.is_err() {
                        return None;
                    }
                }
            }
        }
    }
}

/// A producer's handle on the hub, attached after the producer is built.
/// Bundles are wired before the application state exists, so they hold
/// one of these; [`crate::AppState::with_events`] points them all at the
/// state's hub. Until then, events are dropped. Clones share the slot.
#[derive(Clone, Default)]
pub struct EventSink {
    slot: Arc<RwLock<Option<EventHub>>>,
}

impl std::fmt::Debug for EventSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventSink")
            .field("attached", &self.is_attached())
            .finish()
    }
}

impl EventSink {
    /// Point this sink (and every clone of it) at `hub`, replacing any
    /// earlier hub.
    pub fn attach(&self, hub: &EventHub) {
        let mut slot = self
            .slot
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *slot = Some(hub.clone());
    }

    /// True once a hub is attached.
    pub fn is_attached(&self) -> bool {
        self.hub().is_some()
    }

    /// Publish through the attached hub, if any.
    pub fn publish(&self, event: Event) {
        if let Some(hub) = self.hub() {
            hub.publish(event);
        }
    }

    /// Notify one user through the attached hub, if any.
    pub fn notify(&self, user_id: &str, notice: UserNotice) {
        if let Some(hub) = self.hub() {
            hub.notify(user_id, notice);
        }
    }

    /// Poke the library revision poller through the attached hub, if any.
    pub fn poke_activity(&self) {
        if let Some(hub) = self.hub() {
            hub.poke_activity();
        }
    }

    fn hub(&self) -> Option<EventHub> {
        self.slot
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}
