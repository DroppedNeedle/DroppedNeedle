//! The rows of `catalog_cached_pages` and the clear generation in
//! `catalog_page_generations`. Reads use the reader pool; writes go
//! through the writer lane. A save for a user who no longer exists, or
//! under a generation a clear has since passed, does nothing, so a build
//! that ends after the account was deleted or the cache was cleared
//! leaves no row behind.

use super::{PageKind, Variant};
use droppedneedle_kernel::db::{Db, DbError, writer::Lane};

/// One stored page.
pub(super) struct Row {
    pub(super) variant: Variant,
    pub(super) payload: Vec<u8>,
    pub(super) built_at: i64,
}

/// The clear generation now.
pub(super) async fn generation(db: &Db) -> Result<i64, String> {
    sqlx::query_scalar("SELECT generation FROM catalog_page_generations WHERE singleton = 1")
        .fetch_one(db.sqlx())
        .await
        .map_err(|error| format!("page generation read: {error}"))
}

/// The clear generation and the user's row, read together.
pub(super) async fn load(
    db: &Db,
    user_id: &str,
    kind: PageKind,
) -> Result<(i64, Option<Row>), String> {
    type Read = (
        i64,
        Option<String>,
        Option<bool>,
        Option<bool>,
        Option<Vec<u8>>,
        Option<i64>,
    );
    let (generation, source, listenbrainz, lastfm, payload, built_at): Read = sqlx::query_as(
        "SELECT g.generation, p.source, p.listenbrainz, p.lastfm, p.payload, p.built_at \
         FROM catalog_page_generations g \
         LEFT JOIN catalog_cached_pages p ON p.user_id = ?1 AND p.page = ?2 \
         WHERE g.singleton = 1",
    )
    .bind(user_id)
    .bind(kind.as_str())
    .fetch_one(db.sqlx())
    .await
    .map_err(|error| format!("cached page read: {error}"))?;
    let row = match (source, listenbrainz, lastfm, payload, built_at) {
        (Some(source), Some(listenbrainz), Some(lastfm), Some(payload), Some(built_at)) => {
            Some(Row {
                variant: Variant {
                    source,
                    listenbrainz,
                    lastfm,
                },
                payload,
                built_at,
            })
        }
        _ => None,
    };
    Ok((generation, row))
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
            "INSERT INTO catalog_cached_pages \
                 (user_id, page, source, listenbrainz, lastfm, payload, built_at) \
             SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7 \
             WHERE EXISTS (SELECT 1 FROM auth_users WHERE id = ?1) \
               AND (SELECT generation FROM catalog_page_generations WHERE singleton = 1) = ?8 \
             ON CONFLICT(user_id, page) DO UPDATE SET \
                 source = excluded.source, listenbrainz = excluded.listenbrainz, \
                 lastfm = excluded.lastfm, payload = excluded.payload, \
                 built_at = excluded.built_at",
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
            "DELETE FROM catalog_cached_pages WHERE user_id = ?1 AND page = ?2",
            rusqlite::params![user_id, kind.as_str()],
        )?;
        Ok(())
    })
    .await
}

/// Drop every page and move to the next generation, in one transaction.
pub(super) async fn clear_all(db: &Db) -> Result<usize, DbError> {
    db.write(Lane::Foreground, "discover.page_cache.clear", |tx| {
        let cleared = tx.execute("DELETE FROM catalog_cached_pages", [])?;
        tx.execute(
            "UPDATE catalog_page_generations SET generation = generation + 1 \
             WHERE singleton = 1",
            [],
        )?;
        Ok(cleared)
    })
    .await
}
