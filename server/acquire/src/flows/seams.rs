//! Seams behind the acquisition flows: the outside services they reach.
//!
//! The release pages live elsewhere (the MusicBrainz client); the flows
//! reach them only through the traits here. The wanted watcher searches
//! the download sources themselves. Every fallible method returns a plain string
//! cause: provider detail stays in the log, never on the wire. Downloads
//! start through the concrete [`crate::dispatch::DownloadDispatch`].

use futures_util::future::BoxFuture;

/// One release row observed on a provider page for a followed artist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedRelease {
    /// Release-group MBID.
    pub rg_mbid: String,
    /// Release title.
    pub title: String,
    /// First release date (`YYYY-MM-DD` when complete).
    pub first_release_date: Option<String>,
    /// MusicBrainz primary type (Album, Single, EP, ...).
    pub primary_type: Option<String>,
    /// MusicBrainz secondary types (Live, Compilation, ...).
    pub secondary_types: Vec<String>,
}

/// Follow-poll provider seam: one artist's release-group page.
/// Async: production reads the provider page over HTTP.
pub trait ReleasePoll: Send + Sync {
    /// Fetch every release group of the artist, in any order.
    fn poll_releases<'a>(
        &'a self,
        artist_mbid: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ObservedRelease>, String>>;
}

/// Durable tick sink. Each tick is emitted at its durable state
/// transition, beside the store write. Production logs each tick, forwards the
/// outcomes plugins care about and sends user notices to the live event stream
/// (`acquire::plugin_events::FlowTicks`).
pub trait TickSink: Send + Sync {
    /// Record one tick.
    fn emit(&self, kind: &str, detail: &str, at: i64);

    /// Announce one flow outcome. The default drops it; the production
    /// sink forwards it to the plugin host or the event stream.
    fn announce(&self, event: FlowEvent) {
        let _ = event;
    }
}

/// A flow outcome announced to plugin subscribers or to a user's open
/// tabs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlowEvent {
    /// Tell one user's open tabs (`wanted_*`, `auto_download_enqueued`,
    /// `request_imported`).
    Notify {
        /// Who to tell.
        user_id: String,
        /// What happened.
        notice: droppedneedle_kernel::events::UserNotice,
    },
    /// A request reached the library.
    RequestFulfilled {
        /// Request key (the release-group MBID for albums).
        request_id: String,
        /// Requesting user, when the request has one.
        user_id: String,
        /// Release-group MBID.
        release_group_mbid: String,
    },
    /// An import put files in the library.
    ImportFinished {
        /// Release-group MBID, when known.
        release_group_mbid: String,
        /// Files imported.
        track_count: i64,
        /// Where the files came from (`drop_import`).
        source: String,
    },
}
