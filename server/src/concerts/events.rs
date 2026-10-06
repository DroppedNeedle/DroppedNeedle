//! The `concerts_new` notification seam.
//!
//! After a sweep stores new gigs for an artist, every follower gets a
//! badge-only `concerts_new` event so the sidebar count refreshes without
//! polling. Boot sends them through the live event hub; without it (tests,
//! unwired setups) the default sink drops them and the badge catches up on
//! its own refetch interval.

use serde::Serialize;

/// Payload of one `concerts_new` event, as v2 sent it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConcertsNew {
    /// Artist MBID as followed.
    pub artist_mbid: String,
    /// Artist name.
    pub artist_name: String,
    /// Listings first seen in this sweep.
    pub new_events: usize,
}

/// Where `concerts_new` events go. Delivery is best-effort: a sink must
/// not block the sweep and has no way to fail it.
pub trait ConcertsEvents: Send + Sync + 'static {
    /// Tell one user about new gigs.
    fn concerts_new(&self, user_id: &str, event: &ConcertsNew);
}

/// Drops every event. The default when no event hub is attached.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoEventStream;

impl ConcertsEvents for NoEventStream {
    fn concerts_new(&self, _user_id: &str, _event: &ConcertsNew) {}
}

impl ConcertsEvents for crate::events::EventHub {
    fn concerts_new(&self, user_id: &str, event: &ConcertsNew) {
        self.notify(
            user_id,
            crate::events::UserNotice::ConcertsNew(crate::events::ConcertsNew {
                artist_mbid: event.artist_mbid.clone(),
                artist_name: event.artist_name.clone(),
                new_events: event.new_events,
            }),
        );
    }
}
