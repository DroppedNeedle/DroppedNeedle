//! The sync rows in `acquire_playlist_syncs`, and the writes that queue or
//! cancel a playlist's pass in the same transaction.

use std::collections::HashSet;
use std::sync::Arc;

use rusqlite::{OptionalExtension, Transaction, params};

use super::models::{SyncFailure, SyncHold};
use super::{KIND, SyncJob, sql, sync_key};
use crate::imports::jobs as imports;
use crate::requests::ledger::ApprovalState;
use droppedneedle_auth::users::roles::Role;
use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::{Db, DbError, Lane};
use droppedneedle_kernel::jobs::queue::{JobCtx, JobError, JobQueue, State};

/// One playlist's sync, as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncRow {
    /// Sync is on.
    pub enabled: bool,
    /// New songs are asked for as whole albums.
    pub whole_albums: bool,
    /// Spotify's version of the playlist at the last full read.
    pub snapshot_id: Option<String>,
    /// When the songs were last read in full, unix ms.
    pub read_at: Option<i64>,
    /// When the last pass finished, unix ms.
    pub checked_at: Option<i64>,
    /// When the next pass is due, unix ms.
    pub next_check_at: i64,
    /// Passes failed in a row.
    pub failures: i64,
    /// Why the last pass failed.
    pub failure: Option<SyncFailure>,
    /// Why requests wait.
    pub hold: Option<SyncHold>,
    /// A plain user's grant.
    pub grant: Option<ApprovalState>,
}

impl From<sql::SyncColumns> for SyncRow {
    fn from(row: sql::SyncColumns) -> Self {
        Self {
            enabled: row.enabled,
            whole_albums: row.whole_albums,
            snapshot_id: row.snapshot_id,
            read_at: row.read_at,
            checked_at: row.checked_at,
            next_check_at: row.next_check_at,
            failures: row.failures,
            failure: row.failure,
            hold: row.hold,
            grant: row.grant_state,
        }
    }
}

/// Queue playlist `playlist_id`'s pass now, on the caller's transaction.
pub(super) fn enqueue(
    queue: &JobQueue,
    tx: &Transaction,
    playlist_id: &str,
) -> Result<(), JobError> {
    let job = SyncJob {
        playlist_id: playlist_id.to_owned(),
    };
    queue.enqueue(tx, KIND, &sync_key(playlist_id), &job)?;
    Ok(())
}

/// The sync rows and the queue their passes run on.
#[derive(Clone)]
pub struct SyncStore {
    db: Db,
    clock: Arc<dyn Clock>,
    queue: JobQueue,
}

impl std::fmt::Debug for SyncStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("SyncStore").finish_non_exhaustive()
    }
}

impl SyncStore {
    /// The rows on `db`, stamped by `clock`, with passes queued on `queue`.
    pub fn new(db: Db, clock: Arc<dyn Clock>, queue: JobQueue) -> Self {
        Self { db, clock, queue }
    }

    /// Wall time, unix ms.
    pub fn now_ms(&self) -> i64 {
        self.clock.now_ms()
    }

    pub(super) fn db(&self) -> &Db {
        &self.db
    }

    pub(super) fn queue(&self) -> &JobQueue {
        &self.queue
    }

    /// Whether user `owner`'s import of Spotify playlist `spotify_id` is
    /// queued or running.
    pub(super) async fn import_waiting(
        &self,
        owner: &str,
        spotify_id: &str,
    ) -> Result<bool, DbError> {
        Ok(self
            .queue
            .latest(imports::KIND, &imports::import_job_key(owner, spotify_id))
            .await?
            .is_some_and(|job| matches!(job.state, State::Queued | State::Running)))
    }

    /// Playlist `playlist_id`'s sync, or none when it never had one.
    pub async fn get(&self, playlist_id: &str) -> Result<Option<SyncRow>, DbError> {
        let playlist_id = playlist_id.to_owned();
        self.db
            .read("spotify_sync.get", move |conn| {
                Ok(conn
                    .query_row(
                        sql::SYNC_ROW.text(),
                        params![playlist_id],
                        sql::SyncColumns::read,
                    )
                    .optional()?
                    .map(SyncRow::from))
            })
            .await
    }

    /// Turn sync on (or change whole albums while it is on) and queue a
    /// pass, in one write. Turning it on from off makes it due at once
    /// and, with `ask_grant`, asks an admin again unless the grant waits
    /// or stands.
    pub async fn turn_on(
        &self,
        playlist_id: &str,
        whole_albums: bool,
        ask_grant: bool,
    ) -> Result<(), JobError> {
        let (playlist_id, now, queue) = (playlist_id.to_owned(), self.now_ms(), self.queue.clone());
        self.db
            .write(Lane::Foreground, "spotify_sync.turn_on", move |tx| {
                let row = tx
                    .query_row(
                        sql::SYNC_ROW.text(),
                        params![playlist_id],
                        sql::SyncColumns::read,
                    )
                    .optional()?
                    .map(SyncRow::from);
                match &row {
                    None => {
                        tx.execute(
                            sql::INSERT_SYNC.text(),
                            params![playlist_id, whole_albums, now],
                        )?;
                    }
                    Some(row) if row.enabled => {
                        tx.execute(
                            sql::SET_WHOLE_ALBUMS.text(),
                            params![playlist_id, whole_albums],
                        )?;
                        return Ok(());
                    }
                    Some(_) => {
                        tx.execute(sql::TURN_ON.text(), params![playlist_id, whole_albums, now])?;
                    }
                }
                let standing = row.as_ref().and_then(|row| row.grant).is_some_and(|grant| {
                    matches!(grant, ApprovalState::Pending | ApprovalState::Approved)
                });
                if ask_grant && !standing {
                    tx.execute(sql::ASK_GRANT.text(), params![playlist_id, now])?;
                }
                enqueue(&queue, tx, &playlist_id)
            })
            .await
    }

    /// Queue playlist `playlist_id`'s pass again at `due_at`, unix ms.
    pub(super) async fn queue_at(&self, playlist_id: &str, due_at: i64) -> Result<(), JobError> {
        let (playlist_id, queue) = (playlist_id.to_owned(), self.queue.clone());
        self.db
            .write(Lane::Background, "spotify_sync.queue_at", move |tx| {
                let job = SyncJob {
                    playlist_id: playlist_id.clone(),
                };
                queue.enqueue_at(tx, KIND, &sync_key(&playlist_id), &job, due_at)?;
                Ok(())
            })
            .await
    }

    /// Turn sync off and cancel a queued or running pass, in one write.
    /// The grant stays.
    pub async fn turn_off(&self, playlist_id: &str) -> Result<(), JobError> {
        let (playlist_id, queue) = (playlist_id.to_owned(), self.queue.clone());
        self.db
            .write(Lane::Foreground, "spotify_sync.turn_off", move |tx| {
                tx.execute(sql::TURN_OFF.text(), params![playlist_id])?;
                queue.cancel(tx, KIND, &sync_key(&playlist_id))?;
                Ok(())
            })
            .await
    }

    /// Queue playlist `playlist_id`'s pass now, while its sync is on.
    /// Answers whether it is on.
    pub async fn wake(&self, playlist_id: &str) -> Result<bool, JobError> {
        let (playlist_id, now, queue) = (playlist_id.to_owned(), self.now_ms(), self.queue.clone());
        self.db
            .write(Lane::Foreground, "spotify_sync.wake", move |tx| {
                let on = tx
                    .query_row(
                        sql::SYNC_ROW.text(),
                        params![playlist_id],
                        sql::SyncColumns::read,
                    )
                    .optional()?
                    .is_some_and(|row| row.enabled);
                if on {
                    tx.execute(sql::SET_NEXT_CHECK.text(), params![playlist_id, now])?;
                    enqueue(&queue, tx, &playlist_id)?;
                }
                Ok(on)
            })
            .await
    }

    /// Queue a pass now for each of user `user_id`'s syncs that failed for
    /// want of a working link: they just connected Spotify again.
    pub async fn wake_unlinked(&self, user_id: &str) -> Result<usize, JobError> {
        let (user_id, now, queue) = (user_id.to_owned(), self.now_ms(), self.queue.clone());
        self.db
            .write(Lane::Foreground, "spotify_sync.wake_unlinked", move |tx| {
                let woken: Vec<String> = tx
                    .prepare(sql::WAKE_UNLINKED.text())?
                    .query_map(params![user_id, now], |row| row.get(0))?
                    .collect::<rusqlite::Result<_>>()?;
                for playlist_id in &woken {
                    enqueue(&queue, tx, playlist_id)?;
                }
                Ok(woken.len())
            })
            .await
    }

    /// The synced playlists due by `now`.
    pub async fn due(&self, now: i64) -> Result<Vec<String>, DbError> {
        self.db
            .read("spotify_sync.due", move |conn| {
                Ok(conn
                    .prepare(sql::DUE.text())?
                    .query_map(params![now], |row| row.get(0))?
                    .collect::<rusqlite::Result<_>>()?)
            })
            .await
    }

    /// Queue a pass of each of `playlist_ids`, in one write. A key whose
    /// pass is already queued joins it.
    pub async fn enqueue_all(&self, playlist_ids: Vec<String>) -> Result<(), JobError> {
        let queue = self.queue.clone();
        self.db
            .write(Lane::Background, "spotify_sync.enqueue_due", move |tx| {
                for playlist_id in &playlist_ids {
                    enqueue(&queue, tx, playlist_id)?;
                }
                Ok(())
            })
            .await
    }

    /// The next pass of `playlist_id` is due at `at`.
    pub async fn set_next_check(&self, playlist_id: &str, at: i64) -> Result<(), DbError> {
        let playlist_id = playlist_id.to_owned();
        self.db
            .write(Lane::Background, "spotify_sync.next_check", move |tx| {
                tx.execute(sql::SET_NEXT_CHECK.text(), params![playlist_id, at])?;
                Ok(())
            })
            .await
    }

    /// The songs of `playlist_id` were read in full now, at Spotify's
    /// version `snapshot` (none for Liked Songs).
    pub async fn set_read(
        &self,
        playlist_id: &str,
        snapshot: Option<String>,
    ) -> Result<(), DbError> {
        let (playlist_id, now) = (playlist_id.to_owned(), self.now_ms());
        self.db
            .write(Lane::Background, "spotify_sync.read", move |tx| {
                tx.execute(sql::SET_READ.text(), params![playlist_id, snapshot, now])?;
                Ok(())
            })
            .await
    }

    /// The pass of `playlist_id` finished, its requests waiting for `hold`
    /// when set; the job completes in the same write.
    pub async fn finish(
        &self,
        job: &JobCtx,
        playlist_id: &str,
        hold: Option<SyncHold>,
    ) -> Result<(), JobError> {
        let (job, playlist_id, now) = (job.clone(), playlist_id.to_owned(), self.now_ms());
        self.db
            .write(Lane::Background, "spotify_sync.finish", move |tx| {
                tx.execute(sql::FINISH_PASS.text(), params![playlist_id, now, hold])?;
                job.complete(tx)
            })
            .await
    }

    /// The pass of `playlist_id` failed with `failure`; the next is due at
    /// `next_at`, and the job completes in the same write.
    pub async fn fail(
        &self,
        job: &JobCtx,
        playlist_id: &str,
        failure: SyncFailure,
        next_at: i64,
    ) -> Result<(), JobError> {
        let (job, playlist_id) = (job.clone(), playlist_id.to_owned());
        self.db
            .write(Lane::Background, "spotify_sync.fail", move |tx| {
                tx.execute(
                    sql::FAIL_PASS.text(),
                    params![playlist_id, failure, next_at],
                )?;
                job.complete(tx)
            })
            .await
    }

    /// The Spotify sources (`spotify:{id}`, `spotify:liked`) of user
    /// `user_id`'s synced playlists.
    pub async fn synced_sources(&self, user_id: &str) -> Result<HashSet<String>, DbError> {
        let user_id = user_id.to_owned();
        self.db
            .read("spotify_sync.synced_sources", move |conn| {
                Ok(conn
                    .prepare(sql::SYNCED_SOURCES.text())?
                    .query_map(params![user_id], |row| row.get(0))?
                    .collect::<rusqlite::Result<_>>()?)
            })
            .await
    }

    /// User `user_id`'s role and the name their requests show, read fresh
    /// so a promotion or demotion counts at once; none when the account
    /// is gone.
    pub async fn owner(&self, user_id: &str) -> Result<Option<(Role, Option<String>)>, DbError> {
        let user_id = user_id.to_owned();
        self.db
            .read("spotify_sync.owner", move |conn| {
                Ok(conn
                    .query_row(sql::OWNER.text(), params![user_id], sql::OwnerColumns::read)
                    .optional()?
                    .map(|row| (Role::from_stored(&row.role), row.name)))
            })
            .await
    }
}

/// How many sync asks wait for an admin, for the approvals badge.
pub(crate) async fn pending_grant_count(db: &Db) -> Result<u32, DbError> {
    db.read("spotify_sync.pending_grant_count", move |conn| {
        let count: i64 = conn.query_row(sql::PENDING_GRANT_COUNT.text(), [], |row| row.get(0))?;
        Ok(u32::try_from(count).unwrap_or(u32::MAX))
    })
    .await
}
