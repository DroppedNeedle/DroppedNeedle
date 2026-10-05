//! Bridges from requests to other modules, over plain data only.
//!
//! The bridges are local traits over plain structs, implemented in the
//! acquisition wiring. Both default to `None`:
//!
//! - [`FollowDecisionSink`] carries approval verdicts into the reads
//!   collections, where auto-download intent lives (the ledger stores only the
//!   verdict). Without a sink the verdicts still record here.
//! - [`WatchView`] feeds the wanted view with watches the flows watcher
//!   loop owns. Without it the view shows the requests rows only.

/// One watched album as the wanted view renders it.
#[derive(Debug, Clone)]
pub struct WatchedAlbum {
    /// Lowercased release-group MBID.
    pub key: String,
    /// Watching user id.
    pub user_id: String,
    /// Artist name.
    pub artist_name: String,
    /// Album title.
    pub album_title: String,
    /// Epoch seconds when the watch started.
    pub created_at: u64,
    /// Epoch seconds when the next check is due.
    pub next_check_at: u64,
}

/// Approval-verdict sink into the collections follow rows.
pub trait FollowDecisionSink: Send + Sync {
    /// Arm auto-download for one follow, creating the follow when the
    /// approval came from intake rather than a follow toggle.
    fn arm_auto_download(
        &self,
        user_id: &str,
        user_name: &str,
        artist_mbid: &str,
        artist_name: &str,
    );
    /// Clear auto-download intent, keeping the follow.
    fn clear_auto_download(&self, user_id: &str, artist_mbid: &str);
}

/// Watches owned by the flows watcher loop.
pub trait WatchView: Send + Sync {
    /// Currently watching rows.
    fn watching(&self) -> Vec<WatchedAlbum>;
}
