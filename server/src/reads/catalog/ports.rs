//! Seams the catalog reads through without owning the data behind them.

use std::future::Future;
use std::pin::Pin;

/// Boxed future for dyn-compatible port methods.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// One user's follow state for one artist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FollowState {
    /// Whether the user follows the artist.
    pub followed: bool,
    /// Whether the user wants new releases downloaded on their own.
    pub auto_download: bool,
    /// Approval state of that ask: none, pending, approved, rejected or
    /// revoked.
    pub auto_download_state: String,
}

/// Where the artist header reads the caller's follow state. The follow
/// store lives with the collections; the header only reads it.
pub trait FollowLookup: Send + Sync {
    /// The user's follow state for one artist, or `None` when it cannot be
    /// read (the header then shows "not followed").
    fn status<'a>(
        &'a self,
        user_id: &'a str,
        artist_mbid: &'a str,
    ) -> BoxFuture<'a, Option<FollowState>>;
}

/// No follow store wired: every artist reads as not followed.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoFollows;

impl FollowLookup for NoFollows {
    fn status<'a>(
        &'a self,
        _user_id: &'a str,
        _artist_mbid: &'a str,
    ) -> BoxFuture<'a, Option<FollowState>> {
        Box::pin(async { None })
    }
}
