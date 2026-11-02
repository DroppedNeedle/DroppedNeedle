//! The rows of `catalog_cached_pages` and the clear generation in
//! `catalog_page_generations`. Reads use the reader pool; writes go
//! through the writer lane. A save for a user who no longer exists, or
//! under a generation a clear has since passed, does nothing, so a build
//! that ends after the account was deleted or the cache was cleared
//! leaves no row behind.

use super::{PageKind, Variant};
use crate::reads::discover::sql;
use droppedneedle_kernel::db::{Db, DbError, writer::Lane};

/// One stored page.
pub(super) struct Row {
    pub(super) variant: Variant,
    pub(super) payload: Vec<u8>,
    pub(super) built_at: i64,
}

/// The clear generation now.
pub(super) async fn generation(db: &Db) -> Result<i64, String> {
    db.read("discover.page_cache.generation", |conn| {
        let generation = conn.query_row(sql::PAGE_GENERATION.text(), [], |row| row.get(0))?;
        Ok(generation)
    })
    .await
    .map_err(|error: DbError| format!("page generation read: {error}"))
}

/// The clear generation and the user's row, read together.
pub(super) async fn load(
    db: &Db,
    user_id: &str,
    kind: PageKind,
) -> Result<(i64, Option<Row>), String> {
    let user_id = user_id.to_owned();
    let read = db
        .read("discover.page_cache.load", move |conn| {
            let read = conn.query_row(
                sql::CACHED_PAGE.text(),
                rusqlite::params![user_id, kind.as_str()],
                sql::CachedPageColumns::read,
            )?;
            Ok(read)
        })
        .await
        .map_err(|error: DbError| format!("cached page read: {error}"))?;
    let row = match (
        read.source,
        read.listenbrainz,
        read.lastfm,
        read.payload,
        read.built_at,
    ) {
        (Some(source), Some(listenbrainz), Some(lastfm), Some(payload), Some(built_at)) => {
            Some(Row {
                variant: Variant {
                    source,
                    listenbrainz,
                    lastfm,
                },
                payload,
                built_at: built_at.0,
            })
        }
        _ => None,
    };
    Ok((read.generation, row))
}

/// Keep `row` as the user's page when the cache is still at `generation`.
pub(super) async fn save(
    db: &Db,
    user_id: &str,
    kind: PageKind,
    row: Row,
    generation: i64,
) -> Result<(), DbError> {
    let user_id = user_id.to_owned();
    db.write(Lane::Background, "discover.page_cache.save", move |tx| {
        tx.execute(
            sql::SAVE_CACHED_PAGE.text(),
            rusqlite::params![
                user_id,
                kind.as_str(),
                row.variant.source,
                row.variant.listenbrainz,
                row.variant.lastfm,
                row.payload,
                row.built_at,
                generation
            ],
        )?;
        Ok(())
    })
    .await
}

pub(super) async fn forget(db: &Db, user_id: &str, kind: PageKind) -> Result<(), DbError> {
    let user_id = user_id.to_owned();
    db.write(Lane::Background, "discover.page_cache.forget", move |tx| {
        tx.execute(
            sql::FORGET_CACHED_PAGE.text(),
            rusqlite::params![user_id, kind.as_str()],
        )?;
        Ok(())
    })
    .await
}

/// Drop every page and move to the next generation, in one transaction.
pub(super) async fn clear_all(db: &Db) -> Result<usize, DbError> {
    db.write(Lane::Foreground, "discover.page_cache.clear", |tx| {
        let cleared = tx.execute(sql::CLEAR_CACHED_PAGES.text(), [])?;
        tx.execute(sql::NEXT_PAGE_GENERATION.text(), [])?;
        Ok(cleared)
    })
    .await
}
