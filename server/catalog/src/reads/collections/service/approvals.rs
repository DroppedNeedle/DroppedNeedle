//! Auto-download approval reads. Admin only.
//!
//! Approve, reject and revoke live in `acquire::requests`. When wiring
//! connects its approval store, these reads list that store so every card
//! is actionable through those mutations; otherwise they list follows that
//! still wait for a verdict.

use std::collections::BTreeMap;

use super::CollectionsService;
use super::follows::derive_state;
use crate::reads::collections::auth::Principal;
use crate::reads::collections::error::CollectionsError;
use crate::reads::collections::models::{
    ApprovalBatchItem, ApprovalBatchListResponse, AutoDownloadApprovalItem,
    AutoDownloadApprovalListResponse,
};
use crate::reads::collections::state::AutoDownloadState;

/// What produced a batch of follow-sourced asks.
const FOLLOW_SOURCE: &str = "follow_request";
/// What produced an acquire-owned batch card.
const IMPORT_SOURCE: &str = "import";
/// How many artist names a batch card previews.
const BATCH_SAMPLE_COUNT: usize = 3;

impl CollectionsService<'_> {
    /// Follows waiting for an admin verdict, oldest ask first.
    async fn awaiting(&self) -> Result<Vec<AutoDownloadApprovalItem>, CollectionsError> {
        Ok(self
            .state
            .stores
            .follows
            .awaiting_verdict()
            .await?
            .into_iter()
            .filter(|row| derive_state(row) == AutoDownloadState::Pending)
            .map(|row| AutoDownloadApprovalItem {
                requested_at: row.requested_at.unwrap_or(row.updated_at),
                user_id: row.user_id,
                user_name: Some(row.user_name),
                artist_mbid: row.artist_mbid,
                artist_name: row.artist_name,
            })
            .collect())
    }

    /// Pending auto-download asks across users.
    pub async fn list_approvals(
        &self,
        caller: &Principal,
    ) -> Result<AutoDownloadApprovalListResponse, CollectionsError> {
        caller.require_admin()?;
        let items = match &self.state.acquire_approvals {
            Some(source) => source
                .pending_approvals()
                .await
                .map_err(|cause| CollectionsError::internal(&cause))?
                .into_iter()
                .map(|row| AutoDownloadApprovalItem {
                    user_id: row.user_id,
                    user_name: Some(row.user_name),
                    artist_mbid: row.artist_mbid,
                    artist_name: row.artist_name,
                    requested_at: row.requested_at,
                })
                .collect(),
            None => self.awaiting().await?,
        };
        let count = items.len();
        Ok(AutoDownloadApprovalListResponse { items, count })
    }

    /// Pending asks grouped as batch cards.
    pub async fn list_approval_batches(
        &self,
        caller: &Principal,
    ) -> Result<ApprovalBatchListResponse, CollectionsError> {
        caller.require_admin()?;
        let mut batches = match &self.state.acquire_approvals {
            Some(source) => source
                .pending_batches()
                .await
                .map_err(|cause| CollectionsError::internal(&cause))?
                .into_iter()
                .map(|batch| ApprovalBatchItem {
                    batch_id: batch.batch_id,
                    user_id: batch.user_id,
                    user_name: Some(batch.user_name),
                    artist_count: batch.artists.len(),
                    sample_names: batch
                        .artists
                        .iter()
                        .take(BATCH_SAMPLE_COUNT)
                        .map(|(_, name)| name.clone())
                        .collect(),
                    requested_at: batch.requested_at,
                    source: IMPORT_SOURCE.to_owned(),
                })
                .collect::<Vec<_>>(),
            None => {
                let mut by_user: BTreeMap<String, Vec<AutoDownloadApprovalItem>> = BTreeMap::new();
                for item in self.awaiting().await? {
                    by_user.entry(item.user_id.clone()).or_default().push(item);
                }
                by_user
                    .into_iter()
                    .map(|(user_id, items)| ApprovalBatchItem {
                        batch_id: format!("follow:{user_id}"),
                        user_name: items.first().and_then(|item| item.user_name.clone()),
                        artist_count: items.len(),
                        sample_names: items
                            .iter()
                            .take(BATCH_SAMPLE_COUNT)
                            .map(|item| item.artist_name.clone())
                            .collect(),
                        requested_at: items.first().map(|item| item.requested_at).unwrap_or(0),
                        source: FOLLOW_SOURCE.to_owned(),
                        user_id,
                    })
                    .collect()
            }
        };
        batches.sort_by(|a, b| {
            a.requested_at
                .cmp(&b.requested_at)
                .then(a.user_id.cmp(&b.user_id))
        });
        let count = batches.len();
        Ok(ApprovalBatchListResponse { batches, count })
    }
}
