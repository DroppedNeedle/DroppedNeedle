//! Bridge from requests to the reads collections, over plain data only.
//!
//! [`FollowDecisionSink`] carries approval verdicts into the collections
//! follow rows, where auto-download intent lives (the approval store keeps
//! only the verdict). It defaults to `None`; without a sink the verdicts
//! still record here.

/// Approval-verdict sink into the collections follow rows.
pub trait FollowDecisionSink: Send + Sync {
    /// Arm auto-download for one follow, creating the follow when the
    /// approval came from intake rather than a follow toggle.
    fn arm_auto_download<'a>(
        &'a self,
        user_id: &'a str,
        artist_mbid: &'a str,
        artist_name: &'a str,
    ) -> futures_util::future::BoxFuture<'a, Result<(), String>>;
    /// Clear auto-download intent, keeping the follow.
    fn clear_auto_download<'a>(
        &'a self,
        user_id: &'a str,
        artist_mbid: &'a str,
    ) -> futures_util::future::BoxFuture<'a, Result<(), String>>;
}
