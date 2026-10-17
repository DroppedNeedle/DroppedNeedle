//! Played and unplayed marks: one per user and track, set from Jellyfin
//! apps. The play history is never touched.

use super::CollectionsService;
use crate::reads::collections::error::CollectionsError;
use crate::reads::collections::store::play_marks::MarkScope;

impl CollectionsService<'_> {
    /// Mark the tracks `scope` covers played or unplayed for the caller.
    pub async fn mark_played(
        &self,
        user_id: &str,
        scope: MarkScope<'_>,
        played: bool,
    ) -> Result<(), CollectionsError> {
        Ok(self
            .state
            .stores
            .play_marks
            .mark(user_id, scope, played)
            .await?)
    }
}
