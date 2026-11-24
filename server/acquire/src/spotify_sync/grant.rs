//! A plain user's standing approval for one synced playlist: turning sync
//! on asks an admin once; after a yes the sync requests that playlist's
//! songs by itself, inside the owner's quota. Admins and trusted users hold
//! it by role. A no, or an approval taken back, stops the requests; songs
//! are still added.

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::store::SyncStore;
use super::{SpotifySync, sql};
use crate::requests::ledger::{ApprovalState, can_approval_transition};
use droppedneedle_auth::Admin;
use droppedneedle_kernel::db::{DbError, Lane};
use droppedneedle_kernel::jobs::queue::JobError;

/// One playlist's sync asking for, or holding, an admin's approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PlaylistSyncApprovalItem {
    /// The synced playlist.
    pub playlist_id: String,
    /// Its name.
    pub playlist_name: String,
    /// Its owner.
    pub user_id: String,
    /// The owner's display name.
    pub user_name: String,
    /// When the owner turned sync on, epoch seconds.
    pub requested_at: u64,
    /// Songs on the playlist.
    pub song_count: u32,
    /// New songs are asked for as whole albums.
    pub whole_albums: bool,
}

/// The sync asks waiting for an admin, and the approvals that stand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PlaylistSyncApprovalsResponse {
    /// How many wait.
    pub count: u32,
    /// The asks that wait, oldest first.
    pub items: Vec<PlaylistSyncApprovalItem>,
    /// The approvals that stand.
    pub granted: Vec<PlaylistSyncApprovalItem>,
}

/// How an admin's decision on a sync's grant ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantDecided {
    /// No ask or approval matched.
    NoMatch,
    /// Approved: the sync's pass is queued.
    Approved,
    /// Turned down.
    Rejected,
    /// Taken back.
    Revoked,
}

impl From<sql::GrantColumns> for PlaylistSyncApprovalItem {
    fn from(row: sql::GrantColumns) -> Self {
        Self {
            playlist_id: row.playlist_id,
            playlist_name: row.playlist_name,
            user_id: row.user_id,
            user_name: row.user_name.unwrap_or_default(),
            requested_at: u64::try_from(row.grant_requested_at.unwrap_or(0) / 1000).unwrap_or(0),
            song_count: u32::try_from(row.song_count).unwrap_or(u32::MAX),
            whole_albums: row.whole_albums,
        }
    }
}

impl SyncStore {
    /// The grants in `state`, oldest ask first.
    async fn grants(&self, state: ApprovalState) -> Result<Vec<PlaylistSyncApprovalItem>, DbError> {
        self.db()
            .read("spotify_sync.grants", move |conn| {
                Ok(conn
                    .prepare(sql::GRANTS.text())?
                    .query_map(params![state], sql::GrantColumns::read)?
                    .map(|row| row.map(PlaylistSyncApprovalItem::from))
                    .collect::<rusqlite::Result<_>>()?)
            })
            .await
    }

    /// Move playlist `playlist_id`'s grant to `to`, stamped with the
    /// reviewer, when the move is allowed; an approval queues its pass in
    /// the same write. Answers whether it moved.
    async fn decide(
        &self,
        playlist_id: &str,
        to: ApprovalState,
        reviewer: (String, Option<String>),
    ) -> Result<bool, JobError> {
        let (playlist_id, now, queue) =
            (playlist_id.to_owned(), self.now_ms(), self.queue().clone());
        self.db()
            .write(Lane::Foreground, "spotify_sync.decide", move |tx| {
                let from: Option<ApprovalState> = tx
                    .query_row(sql::GRANT_STATE.text(), params![playlist_id], |row| {
                        row.get(0)
                    })
                    .optional()?
                    .flatten();
                let Some(from) = from.filter(|from| can_approval_transition(*from, to)) else {
                    return Ok(false);
                };
                tx.execute(
                    sql::DECIDE_GRANT.text(),
                    params![playlist_id, to, reviewer.0, reviewer.1, now, from],
                )?;
                if to == ApprovalState::Approved {
                    super::store::enqueue(&queue, tx, &playlist_id)?;
                }
                Ok(true)
            })
            .await
    }

    /// Ask an admin for playlist `playlist_id`'s grant when it has none: a
    /// plain user's sync whose owner held it by role before.
    pub async fn ask_grant_if_none(&self, playlist_id: &str) -> Result<(), DbError> {
        let (playlist_id, now) = (playlist_id.to_owned(), self.now_ms());
        self.db()
            .write(Lane::Background, "spotify_sync.ask_grant", move |tx| {
                tx.execute(sql::ASK_GRANT_IF_NONE.text(), params![playlist_id, now])?;
                Ok(())
            })
            .await
    }
}

impl SpotifySync {
    /// The sync asks waiting for an admin, and the approvals that stand.
    pub async fn approvals(
        &self,
        _admin: &Admin,
    ) -> Result<PlaylistSyncApprovalsResponse, super::PlaylistSyncError> {
        let items = self.store.grants(ApprovalState::Pending).await?;
        let granted = self.store.grants(ApprovalState::Approved).await?;
        Ok(PlaylistSyncApprovalsResponse {
            count: u32::try_from(items.len()).unwrap_or(u32::MAX),
            items,
            granted,
        })
    }

    /// Approve, turn down or take back playlist `playlist_id`'s grant.
    pub async fn decide(
        &self,
        admin: &Admin,
        playlist_id: &str,
        to: ApprovalState,
    ) -> Result<GrantDecided, super::PlaylistSyncError> {
        let reviewer = (
            admin.user_id().to_owned(),
            Some(admin.username_display().to_owned()),
        );
        if !self.store.decide(playlist_id, to, reviewer).await? {
            return Ok(GrantDecided::NoMatch);
        }
        self.announce(playlist_id, 0, 0).await;
        Ok(match to {
            ApprovalState::Approved => GrantDecided::Approved,
            ApprovalState::Rejected => GrantDecided::Rejected,
            ApprovalState::Revoked | ApprovalState::Pending => GrantDecided::Revoked,
        })
    }
}
