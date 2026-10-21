//! Seams behind the acquisition flows: the boundary to code they do not own.
//!
//! Downloads dispatch and provider/indexer search live elsewhere (the
//! orchestrator and clients, and the search fan-out). The flows reach them
//! only through the traits here, so real implementations plug in without
//! touching flow logic. Every fallible method
//! returns a plain string cause, the playback-ports convention: provider
//! detail stays in the log, never on the wire.

use futures_util::future::BoxFuture;

/// What kind of request a dispatch serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchKind {
    /// Whole album, keyed by release-group MBID.
    Album,
    /// Single track, keyed by recording MBID.
    Track,
}

/// One download dispatch: the orchestrator owns the queue behind it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchRequest {
    /// Owning user id.
    pub user_id: String,
    /// Album or track.
    pub kind: DispatchKind,
    /// Release-group MBID (album) or recording MBID (track).
    pub mbid: String,
    /// Artist name for the task row.
    pub artist: String,
    /// Album or track title for the task row.
    pub title: String,
    /// Origin tag: `user`, `wanted`, `follow`, `upgrade`.
    pub origin: String,
    /// Caller-supplied idempotency key. A repeat dispatch with the same
    /// key answers the original task instead of minting a duplicate;
    /// `None` always mints fresh.
    pub idempotency_key: Option<String>,
}

/// One download task as the status sync sees it. `status` is one of
/// `downloading`, `processing`, `completed`, `partial`, `failed`,
/// `cancelled` (v2 `download_task.status`, `requests_page_service.py`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadTaskView {
    /// Task id.
    pub task_id: String,
    /// Raw task status.
    pub status: String,
    /// Release-group MBID for album tasks, when known.
    pub album_mbid: Option<String>,
}

/// Outcome of an upgrade dispatch. `AlreadyInLibrary` answers the
/// `already_in_library` sentinel from v2 `request_upgrade_album`
/// (`core/tasks.py`): the sweep must not count it as enqueued.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpgradeDispatch {
    /// A new upgrade grab, with its task id.
    Enqueued(String),
    /// The album already meets cutoff; nothing was queued.
    AlreadyInLibrary,
}

/// Downloads dispatch seam, implemented by the orchestrator.
pub trait DownloadDispatch: Send + Sync {
    /// Queue a download; answers the new task id.
    fn dispatch<'a>(
        &'a self,
        request: &'a DispatchRequest,
    ) -> BoxFuture<'a, Result<String, String>>;
    /// Current status of one task, or `None` when unknown.
    fn task_status<'a>(&'a self, task_id: &'a str)
    -> BoxFuture<'a, Result<Option<String>, String>>;
    /// Newest active task for an album across users, for the album-row
    /// fallback in the status sync.
    fn active_task_for_album<'a>(
        &'a self,
        rg_mbid: &'a str,
    ) -> BoxFuture<'a, Result<Option<DownloadTaskView>, String>>;
    /// Queue an origin-`upgrade` grab with active-task dedup.
    fn dispatch_upgrade<'a>(
        &'a self,
        request: &'a DispatchRequest,
    ) -> BoxFuture<'a, Result<UpgradeDispatch, String>>;
}

/// One provider/indexer candidate for a wanted album.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// Release title as the provider reports it.
    pub title: String,
    /// Provider or indexer name.
    pub source: String,
}

/// Provider/indexer search seam, implemented by the search fan-out.
/// Async like the downloads `DownloadSource` seam: production fans out to
/// slskd and the Usenet indexers over HTTP.
pub trait CandidateSearch: Send + Sync {
    /// Search providers for an album's candidates.
    fn search_album<'a>(
        &'a self,
        artist: &'a str,
        title: &'a str,
    ) -> BoxFuture<'a, Result<Vec<Candidate>, String>>;
}

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

/// Durable tick sink. v2 fired plugin ticks as fire-and-forget asyncio
/// tasks; v3 emits each tick at its durable state transition, beside the
/// store write. Production logs each tick, forwards the outcomes plugins
/// care about and sends user notices to the live event stream
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
