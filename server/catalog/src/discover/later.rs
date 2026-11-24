//! Each person's Later pile (`catalog_later_albums`): albums they want to
//! keep for another day, one row a person and release group.
//!
//! The pile lists only albums the library does not own: one it comes to own
//! leaves the list at once, and its row goes at the person's next change to
//! the pile. Nothing on the pile is ever requested by itself; "Keep all"
//! goes through the batch route like any other.
//!
//! The pile is something the person told Discover. Its albums are not dealt
//! again as picks while they are on it (the pool's live gates,
//! [`pool_store::ranked`](crate::discover::engine::pool_store::ranked), read
//! [`albums`]), and each album's artist counts for them as a follow does in
//! their seeds ([`crate::discover::queue::seeds`]).

use std::collections::HashSet;
use std::sync::Arc;

use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::time::Millis;
use droppedneedle_kernel::db::{Db, DbError, writer::Lane};
use droppedneedle_kernel::page::{Page, PageRequest};
use rusqlite::params;

use crate::discover::models::{LaterAlbum, LaterRequest};
use crate::discover::page::shelves;
use crate::discover::{read_all, sql};

/// Rows on a page of the pile when the caller names no limit.
pub const PAGE_DEFAULT: usize = 50;
/// The most rows on one page of the pile.
pub const PAGE_MAX: usize = 100;

/// Where a page of the pile starts: the last row's time added (unix ms) and
/// its release group, lowercased.
pub type LaterKey = (i64, String);

/// Why a change to the pile was refused.
#[derive(Debug)]
pub enum LaterError {
    /// The request does not say which album, or names it badly.
    Invalid(String),
    /// The database failed.
    Store(DbError),
}

impl From<DbError> for LaterError {
    fn from(error: DbError) -> Self {
        Self::Store(error)
    }
}

/// A release group from a path, trimmed: as sent and lowercased.
fn checked_group(release_group_mbid: &str) -> Result<(String, String), LaterError> {
    let mbid = release_group_mbid.trim();
    if !droppedneedle_providers::musicbrainz::is_valid_mbid(mbid) {
        return Err(LaterError::Invalid(
            "release_group_mbid must be a MusicBrainz id".to_owned(),
        ));
    }
    Ok((mbid.to_owned(), mbid.to_ascii_lowercase()))
}

/// The albums on `user_id`'s pile, lowercased, owned ones included.
///
/// # Errors
///
/// The read's failure.
pub(crate) async fn albums(db: &Db, user_id: &str) -> Result<HashSet<String>, DbError> {
    let rows = read_all(
        db,
        "discover.later.albums",
        sql::LATER_GROUPS.text(),
        [user_id.to_owned()],
        |row| row.get::<_, String>(0),
    )
    .await?;
    Ok(rows.into_iter().collect())
}

/// Each person's Later pile: marked, listed and taken off.
#[derive(Clone)]
pub struct LaterPile {
    db: Db,
    clock: Arc<dyn Clock>,
}

impl LaterPile {
    /// The piles in `db`, dated by `clock`.
    pub fn new(db: Db, clock: Arc<dyn Clock>) -> Self {
        Self { db, clock }
    }

    /// Put one album on `user_id`'s pile. An album already there keeps its
    /// place; albums the library now owns go in the same write.
    ///
    /// # Errors
    ///
    /// A release group that is not an id, or the write's failure.
    pub async fn mark(
        &self,
        user_id: &str,
        release_group_mbid: &str,
        request: &LaterRequest,
    ) -> Result<(), LaterError> {
        let (mbid, _) = checked_group(release_group_mbid)?;
        // The artist is kept for the list and the seeds, so one that is not
        // an id is left out rather than refusing the album.
        let artist = request
            .artist_mbid
            .as_deref()
            .map(str::trim)
            .filter(|id| droppedneedle_providers::musicbrainz::is_valid_mbid(id))
            .map(str::to_ascii_lowercase);
        let (title, name) = (
            request.title.trim().to_owned(),
            request.artist_name.trim().to_owned(),
        );
        let (user, now_ms) = (user_id.to_owned(), self.clock.now_ms());
        self.db
            .write(Lane::Foreground, "discover.later.mark", move |tx| {
                tx.execute(
                    sql::LATER_MARK.text(),
                    params![user, mbid, title, artist, name, Millis(now_ms)],
                )?;
                tx.execute(sql::LATER_DROP_OWNED.text(), [&user])?;
                Ok::<_, DbError>(())
            })
            .await?;
        Ok(())
    }

    /// Take one album off `user_id`'s pile, with the albums the library now
    /// owns. Nothing else happens when it was not there.
    ///
    /// # Errors
    ///
    /// A release group that is not an id, or the write's failure.
    pub async fn remove(&self, user_id: &str, release_group_mbid: &str) -> Result<(), LaterError> {
        let (_, mbid_lower) = checked_group(release_group_mbid)?;
        let user = user_id.to_owned();
        self.db
            .write(Lane::Foreground, "discover.later.remove", move |tx| {
                tx.execute(sql::LATER_REMOVE.text(), params![user, mbid_lower])?;
                tx.execute(sql::LATER_DROP_OWNED.text(), [&user])?;
                Ok::<_, DbError>(())
            })
            .await?;
        Ok(())
    }

    /// One page of `user_id`'s pile, newest first, with how many albums it
    /// lists; only the album `only` names when it names one. Albums the
    /// library owns are left out.
    ///
    /// # Errors
    ///
    /// An `only` that is not a release group id, or the read's failure.
    pub async fn page(
        &self,
        user_id: &str,
        paging: PageRequest<LaterKey>,
        only: Option<&str>,
    ) -> Result<Page<LaterAlbum>, LaterError> {
        let only = only.map(checked_group).transpose()?.map(|(_, lower)| lower);
        let user = user_id.to_owned();
        let limit = paging.limit;
        let read = i64::try_from(limit + 1).unwrap_or(i64::MAX);
        let (after_at, after_key) = match paging.after {
            Some((at, key)) => (Some(Millis(at)), key),
            None => (None, String::new()),
        };
        let (rows, total) = self
            .db
            .read("discover.later.page", move |conn| {
                // The page and the count are read in one snapshot, so the
                // count is the list the page was cut from.
                let rows = conn
                    .prepare_cached(sql::LATER_PAGE.text())?
                    .query_map(
                        params![user, after_at, after_key, read, only],
                        sql::LaterRow::read,
                    )?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                let total: i64 =
                    conn.query_row(sql::LATER_COUNT.text(), params![user, only], |row| {
                        row.get(0)
                    })?;
                Ok::<_, DbError>((rows, total))
            })
            .await?;
        let page = Page::cut(rows, limit, |row| {
            (row.added_at.0, row.release_group_mbid_lower.clone())
        });
        Ok(page
            .map(|row| {
                let mut album = shelves::album(
                    Some(&row.release_group_mbid),
                    &row.title,
                    Some(&row.artist_name),
                    row.artist_mbid.as_deref(),
                    None,
                );
                album.requested = row.requested;
                LaterAlbum {
                    album,
                    added_at: row.added_at.0 / 1000,
                }
            })
            .with_total(u64::try_from(total).unwrap_or(0)))
    }
}
