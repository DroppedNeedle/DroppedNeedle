//! "Looks wrong": any signed-in person can mark a library album's match as
//! looking wrong, so curators find it in Check albums. Each person's mark
//! is their own. A person choosing, confirming or handing back the album's
//! edition settles it, and every mark on the album goes in that write.

use rusqlite::{Transaction, params};

use crate::clock::LibraryClock as _;
use crate::jobs::models::OperationError;
use crate::jobs::service::Operations;
use crate::reasons;
use droppedneedle_kernel::db::Lane;

/// Every mark on the album goes: a person settled its edition.
pub(crate) fn clear(tx: &Transaction<'_>, album_id: &str) -> rusqlite::Result<()> {
    tx.prepare_cached(super::sql::LOOKS_WRONG_CLEAR.text())?
        .execute(params![album_id])?;
    Ok(())
}

impl Operations {
    /// Set or take back `user_id`'s own mark on a live album. Blocking.
    pub fn mark_looks_wrong(
        &self,
        album_id: &str,
        user_id: &str,
        looks_wrong: bool,
    ) -> Result<(), OperationError> {
        let now = crate::clock::ms(self.clock.now_secs());
        self.db
            .write_blocking(Lane::Foreground, "operations.looks_wrong", |tx| {
                let live: bool =
                    tx.query_row(super::sql::ALBUM_LIVE.text(), params![album_id], |row| {
                        row.get(0)
                    })?;
                if !live {
                    return Err(OperationError::NotFound(reasons::ALBUM_NOT_FOUND));
                }
                if looks_wrong {
                    tx.prepare_cached(super::sql::LOOKS_WRONG_SET.text())?
                        .execute(params![album_id, user_id, now])?;
                } else {
                    tx.prepare_cached(super::sql::LOOKS_WRONG_UNSET.text())?
                        .execute(params![album_id, user_id])?;
                }
                Ok(())
            })
    }

    /// A curator finds nothing wrong: every mark on the album goes.
    /// Blocking.
    pub fn clear_looks_wrong(&self, album_id: &str) -> Result<(), OperationError> {
        self.db
            .write_blocking(Lane::Foreground, "operations.looks_wrong_clear", |tx| {
                let known: bool =
                    tx.query_row(super::sql::ALBUM_EXISTS.text(), params![album_id], |row| {
                        row.get(0)
                    })?;
                if !known {
                    return Err(OperationError::NotFound(reasons::ALBUM_NOT_FOUND));
                }
                Ok(clear(tx, album_id)?)
            })
    }

    /// Whether `user_id` marked the album, and how many people did.
    /// Blocking.
    pub fn looks_wrong_of(
        &self,
        album_id: &str,
        user_id: &str,
    ) -> Result<(bool, u32), OperationError> {
        self.db.read_blocking("operations.looks_wrong_of", |conn| {
            Ok::<_, OperationError>(super::status::looks_wrong_of(conn, album_id, user_id)?)
        })
    }
}
