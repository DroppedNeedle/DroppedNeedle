//! The batch rows in `acquire_discovery_batches` and
//! `acquire_discovery_batch_items`. A batch is written only with its
//! requests, in their one transaction ([`NewBatch::insert_tx`]); the store
//! reads batches and forgets them.

use rusqlite::{OptionalExtension, Transaction, params};

use super::models::BatchOutcome;
use crate::db::AcquireDb;
use crate::requests::error::RequestsError;
use crate::requests::ledger::{QuotaRefusal, RequestRecord};
use crate::requests::sql;
use crate::requests::sqlite::{lane_error, read, unknown_value};

/// One stored batch.
#[derive(Debug, Clone)]
pub(crate) struct BatchHead {
    pub(crate) id: String,
    pub(crate) user_id: String,
    pub(crate) name: String,
    pub(crate) source_section: String,
    /// Integer milliseconds.
    pub(crate) created_at: i64,
}

/// One album of a batch, with its request's live state when read back.
#[derive(Debug, Clone)]
pub(crate) struct BatchItem {
    /// Lowercased release-group id.
    pub(crate) release_group_mbid: String,
    pub(crate) artist_mbid: String,
    pub(crate) album_name: String,
    pub(crate) artist_name: String,
    pub(crate) outcome: BatchOutcome,
    /// The album request's status; `None` when writing or when there is
    /// no request.
    pub(crate) request_status: Option<String>,
}

/// A batch about to be filed: its row and its albums. An album marked
/// [`BatchOutcome::Requested`] is one to file; the filing write settles
/// what it actually did before the rows land.
#[derive(Debug, Clone)]
pub(crate) struct NewBatch {
    pub(crate) head: BatchHead,
    pub(crate) items: Vec<BatchItem>,
}

/// What the filing write came to.
#[derive(Debug)]
pub(crate) enum BatchWrite {
    /// The batch and its requests were written; these rows are new.
    Filed { won: Vec<RequestRecord> },
    /// A batch with this id exists already, owned by `owner`. Nothing was
    /// written.
    Taken { owner: String },
    /// The quota gate refused. Nothing was written.
    Refused(QuotaRefusal),
    /// No album could be filed. Nothing was written.
    NothingFiled,
}

impl NewBatch {
    /// Who already owns a batch with this id, inside the filing write.
    pub(crate) fn owner_tx(&self, tx: &Transaction) -> rusqlite::Result<Option<String>> {
        tx.query_row(sql::BATCH_OWNER.text(), params![self.head.id], |row| {
            row.get(0)
        })
        .optional()
    }

    /// Write the batch and its albums.
    pub(crate) fn insert_tx(&self, tx: &Transaction) -> rusqlite::Result<()> {
        let head = &self.head;
        tx.execute(
            sql::INSERT_BATCH.text(),
            params![
                head.id,
                head.user_id,
                head.name,
                head.source_section,
                head.created_at
            ],
        )?;
        for item in &self.items {
            tx.execute(
                sql::INSERT_BATCH_ITEM.text(),
                params![
                    head.id,
                    item.release_group_mbid,
                    item.artist_mbid,
                    item.album_name,
                    item.artist_name,
                    item.outcome.as_str(),
                    head.created_at,
                ],
            )?;
        }
        Ok(())
    }
}

fn head_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<BatchHead> {
    Ok(BatchHead {
        id: row.get(0)?,
        user_id: row.get(1)?,
        name: row.get(2)?,
        source_section: row.get(3)?,
        created_at: row.get(4)?,
    })
}

/// Batch reads and removal over the application database.
#[derive(Clone)]
pub(crate) struct BatchStore {
    db: AcquireDb,
}

impl BatchStore {
    pub(crate) fn new(db: AcquireDb) -> Self {
        Self { db }
    }

    /// One batch by id.
    pub(crate) async fn head(&self, id: &str) -> Result<Option<BatchHead>, RequestsError> {
        let id = id.to_owned();
        read(&self.db, "requests.batches.head", move |conn| {
            conn.prepare_cached(sql::BATCH_HEAD.text())?
                .query_row(params![id], head_from_row)
                .optional()
        })
        .await
    }

    /// A user's batches, newest first.
    pub(crate) async fn heads_of(&self, user_id: &str) -> Result<Vec<BatchHead>, RequestsError> {
        let user_id = user_id.to_owned();
        read(&self.db, "requests.batches.list", move |conn| {
            conn.prepare_cached(sql::BATCHES_OF_USER.text())?
                .query_map(params![user_id], head_from_row)?
                .collect()
        })
        .await
    }

    /// A batch's albums, each with its request's live state.
    pub(crate) async fn items(&self, batch_id: &str) -> Result<Vec<BatchItem>, RequestsError> {
        let batch_id = batch_id.to_owned();
        read(&self.db, "requests.batches.items", move |conn| {
            conn.prepare_cached(sql::BATCH_ITEMS.text())?
                .query_map(params![batch_id], |row| {
                    let outcome: String = row.get(4)?;
                    Ok(BatchItem {
                        release_group_mbid: row.get(0)?,
                        artist_mbid: row.get(1)?,
                        album_name: row.get(2)?,
                        artist_name: row.get(3)?,
                        outcome: BatchOutcome::parse(&outcome)
                            .ok_or_else(|| unknown_value(4, "outcome", &outcome))?,
                        request_status: row.get(5)?,
                    })
                })?
                .collect()
        })
        .await
    }

    /// Whether anyone but `owner` asked for, favourited, rated, played or
    /// marked this album.
    pub(crate) async fn held_by_others(
        &self,
        mbid: &str,
        owner: &str,
    ) -> Result<bool, RequestsError> {
        let (mbid, owner) = (mbid.to_ascii_lowercase(), owner.to_owned());
        read(&self.db, "requests.batches.held", move |conn| {
            conn.prepare_cached(sql::HELD_BY_OTHERS.text())?
                .query_row(params![mbid, owner], |row| row.get(0))
        })
        .await
    }

    /// Forget a batch's albums except `kept`, and the batch once none is
    /// left.
    pub(crate) async fn forget(
        &self,
        batch_id: &str,
        items: Vec<String>,
        kept: Vec<String>,
    ) -> Result<(), RequestsError> {
        let batch_id = batch_id.to_owned();
        self.db
            .write("requests.batches.forget", move |tx| {
                for mbid in items.iter().filter(|mbid| !kept.contains(mbid)) {
                    tx.execute(sql::DELETE_BATCH_ITEM.text(), params![batch_id, mbid])?;
                }
                tx.execute(sql::DELETE_EMPTY_BATCH.text(), params![batch_id])?;
                Ok(())
            })
            .await
            .map_err(|error| lane_error("requests.batches.forget", error))
    }
}
