//! The rows of `catalog_cached_pages` and the clear generation in
//! `catalog_page_generations`. Reads use the reader pool; writes go
//! through the writer lane. A save for a user who no longer exists, under
//! a generation a clear has since passed, or under a forget count the
//! person's forget has since passed, does nothing, so a build that ends
//! after the account was deleted, the cache was cleared or the person
//! forgot their history leaves no row behind.

use super::{ForgetCounts, Generation, PageKind, StoredPage, Variant};
use crate::discover::sql;
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

/// Keep `row` as the user's page when the cache is still at `generation`;
/// whether it was written. The forget count is read on the writer, so a
/// forget's delete and this save are ordered: whichever runs first, a page
/// built before the forget does not stay.
pub(super) async fn save(
    db: &Db,
    user_id: &str,
    kind: PageKind,
    row: Row,
    generation: Generation,
    forgets: ForgetCounts,
) -> Result<bool, DbError> {
    let user_id = user_id.to_owned();
    db.write(Lane::Background, "discover.page_cache.save", move |tx| {
        if !forgets.holds(&user_id, generation) {
            return Ok(false);
        }
        let written = tx.execute(
            sql::SAVE_CACHED_PAGE.text(),
            rusqlite::params![
                user_id,
                kind.as_str(),
                row.variant.source,
                row.variant.listenbrainz,
                row.variant.lastfm,
                row.payload,
                row.built_at,
                generation.clear
            ],
        )?;
        Ok(written > 0)
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

/// Every user's row of kind `kind`.
pub(super) async fn of_kind(db: &Db, kind: PageKind) -> Result<Vec<StoredPage>, DbError> {
    let rows = db
        .read::<_, DbError, _>("discover.page_cache.of_kind", move |conn| {
            let rows = conn
                .prepare(sql::SAVED_PAGES_OF_KIND.text())?
                .query_map(
                    rusqlite::params![kind.as_str()],
                    sql::SavedPageColumns::read,
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await?;
    Ok(rows
        .into_iter()
        .map(|row| StoredPage {
            user_id: row.user_id,
            payload: row.payload,
            built_at: row.built_at.0,
        })
        .collect())
}

/// Drop every page of one user, moving their forget count on in the same
/// write, so no save of theirs ordered after it keeps a page.
pub(super) async fn forget_user(
    db: &Db,
    user_id: &str,
    forgets: ForgetCounts,
) -> Result<usize, DbError> {
    let user_id = user_id.to_owned();
    db.write(
        Lane::Foreground,
        "discover.page_cache.forget_user",
        move |tx| {
            forgets.bump(&user_id);
            Ok(tx.execute(sql::FORGET_USER_PAGES.text(), [user_id])?)
        },
    )
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
