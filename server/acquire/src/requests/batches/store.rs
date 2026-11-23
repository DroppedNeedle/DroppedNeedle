//! The batch rows in `acquire_discovery_batches` and
//! `acquire_discovery_batch_items`. A batch is written only with its
//! requests, in their one transaction ([`NewBatch::insert_tx`]); the store
//! reads batches and forgets them.

use std::collections::HashSet;

use rusqlite::{OptionalExtension, Transaction, params};

use super::models::{BatchOutcome, BatchSource};
use crate::db::AcquireDb;
use crate::requests::error::RequestsError;
use crate::requests::ledger::{QuotaRefusal, RequestRecord};
use crate::requests::sql;
use crate::requests::sqlite::{lane_error, read, unknown_value};
use droppedneedle_catalog::discover::record::MadeFrom;

/// One stored batch.
#[derive(Debug, Clone)]
pub(crate) struct BatchHead {
    pub(crate) id: String,
    pub(crate) user_id: String,
    pub(crate) name: String,
    pub(crate) source_section: String,
    /// Integer milliseconds.
    pub(crate) created_at: i64,
    /// Where it came from.
    pub(crate) source: BatchSource,
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
    /// How many of a playlist's songs the album carries; `None` for
    /// Discover's albums.
    pub(crate) song_count: Option<u32>,
    /// The generation of the request the batch made for the album, set by
    /// the filing write for a request it won; `None` otherwise.
    pub(crate) request_generation: Option<u64>,
}

/// A batch about to be filed: its row and its albums. An album marked
/// [`BatchOutcome::Requested`] is one to file; the filing write settles
/// what it actually did before the rows land.
#[derive(Debug, Clone)]
pub(crate) struct NewBatch {
    pub(crate) head: BatchHead,
    pub(crate) items: Vec<BatchItem>,
    /// Where it was made, when the caller said.
    pub(crate) made_from: Option<MadeFrom>,
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
                head.created_at,
                self.made_from,
                head.source.as_str()
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
                    item.song_count,
                    item.request_generation.map(crate::db::to_i64),
                ],
            )?;
        }
        Ok(())
    }
}

fn head_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<BatchHead> {
    let source: String = row.get(5)?;
    Ok(BatchHead {
        id: row.get(0)?,
        user_id: row.get(1)?,
        name: row.get(2)?,
        source_section: row.get(3)?,
        created_at: row.get(4)?,
        source: BatchSource::parse(&source).ok_or_else(|| unknown_value(5, "source", &source))?,
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

    /// A user's batches from `source`, newest first.
    pub(crate) async fn heads_of(
        &self,
        user_id: &str,
        source: BatchSource,
    ) -> Result<Vec<BatchHead>, RequestsError> {
        let user_id = user_id.to_owned();
        read(&self.db, "requests.batches.list", move |conn| {
            conn.prepare_cached(sql::BATCHES_OF_USER.text())?
                .query_map(params![user_id, source.as_str()], head_from_row)?
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
                        song_count: row.get(6)?,
                        request_generation: None,
                    })
                })?
                .collect()
        })
        .await
    }

    /// The approval cards, oldest first.
    pub(crate) async fn waiting_cards(&self) -> Result<Vec<sql::WaitingCard>, RequestsError> {
        read(&self.db, "requests.batches.cards", move |conn| {
            conn.prepare_cached(sql::WAITING_CARDS.text())?
                .query_map([], sql::WaitingCard::read)?
                .collect()
        })
        .await
    }

    /// The albums waiting for approval on card `batch_id`.
    pub(crate) async fn card_items(
        &self,
        batch_id: &str,
    ) -> Result<Vec<sql::CardItem>, RequestsError> {
        let batch_id = batch_id.to_owned();
        read(&self.db, "requests.batches.card_items", move |conn| {
            conn.prepare_cached(sql::CARD_ITEMS.text())?
                .query_map(params![batch_id], sql::CardItem::read)?
                .collect()
        })
        .await
    }

    /// The album keys waiting for approval on any card.
    pub(crate) async fn card_keys(&self) -> Result<HashSet<String>, RequestsError> {
        read(&self.db, "requests.batches.card_keys", move |conn| {
            conn.prepare_cached(sql::CARD_KEYS.text())?
                .query_map([], |row| row.get(0))?
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
