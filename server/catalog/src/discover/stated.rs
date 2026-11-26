//! The artists each person named as ones they love
//! (`catalog_stated_artists`): one row a person and artist, at most
//! [`LIMIT`] a person.
//!
//! A named artist is one more seed ([`crate::discover::queue::seeds`]) and
//! an artist the person knows, so it is never offered to them as a new
//! one. Naming one is not a follow: it starts no watch and no request.
//! Naming or removing one queues the person's taste build in the same
//! write, through [`Due::queue_now`], and the pool and set follow from it.
//! The list is something the person told Discover: Forget leaves it.

use std::sync::Arc;

use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::time::Millis;
use droppedneedle_kernel::db::{Db, DbError, writer::Lane};
use droppedneedle_kernel::jobs::queue::JobError;
use droppedneedle_library::identify::artist_links::placeholder;
use rusqlite::params;

use crate::discover::due::Due;
use crate::discover::engine::jobs;
use crate::discover::models::StatedArtist;
use crate::discover::{read_all, sql};

/// The most artists one person may name (proposed).
pub const LIMIT: u32 = 30;

/// Why a change to the list was refused.
#[derive(Debug)]
pub enum StatedError {
    /// The artist is not a MusicBrainz artist id, or is a special-purpose
    /// one.
    Invalid(String),
    /// The person already named [`LIMIT`] artists.
    Full,
    /// The database or the enqueue failed.
    Store(JobError),
}

impl std::fmt::Display for StatedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(reason) => f.write_str(reason),
            Self::Full => write!(f, "at most {LIMIT} artists"),
            Self::Store(error) => error.fmt(f),
        }
    }
}

impl From<DbError> for StatedError {
    fn from(error: DbError) -> Self {
        Self::Store(JobError::from(error))
    }
}

impl From<JobError> for StatedError {
    fn from(error: JobError) -> Self {
        Self::Store(error)
    }
}

impl From<rusqlite::Error> for StatedError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Store(JobError::from(error))
    }
}

/// An artist from a path, trimmed and lowercased, as it is stored. A
/// special-purpose MusicBrainz id (Various Artists, unknown and the like)
/// is refused: it is never a seed.
fn checked(artist_mbid: &str) -> Result<String, StatedError> {
    let mbid = artist_mbid.trim();
    if !droppedneedle_providers::musicbrainz::is_valid_mbid(mbid) {
        return Err(StatedError::Invalid(
            "artist_mbid must be a MusicBrainz id".to_owned(),
        ));
    }
    let lower = mbid.to_ascii_lowercase();
    if placeholder(&lower) {
        return Err(StatedError::Invalid(
            "artist_mbid names no one artist".to_owned(),
        ));
    }
    Ok(lower)
}

fn entry(row: sql::StatedRow) -> StatedArtist {
    StatedArtist {
        artist_mbid: row.artist_mbid,
        artist_name: row.artist_name,
        stated_at: row.stated_at.0 / 1000,
    }
}

/// Each person's named artists: named, listed and removed, each change
/// queuing their taste build.
#[derive(Clone)]
pub struct StatedArtists {
    db: Db,
    clock: Arc<dyn Clock>,
    due: Arc<Due>,
}

impl StatedArtists {
    /// The lists in `db`, dated by `clock`, queuing taste builds through
    /// `due`.
    pub fn new(db: Db, clock: Arc<dyn Clock>, due: Arc<Due>) -> Self {
        Self { db, clock, due }
    }

    /// Name one artist for `user_id` as `name` and queue their taste build
    /// in the same write. An artist already named keeps its row and queues
    /// nothing. Answers it as stored.
    ///
    /// # Errors
    ///
    /// An id that is not an artist's, a full list, or the write's failure.
    pub async fn name(
        &self,
        user_id: &str,
        artist_mbid: &str,
        name: &str,
    ) -> Result<StatedArtist, StatedError> {
        let lower = checked(artist_mbid)?;
        let (user, name, now_ms) = (
            user_id.to_owned(),
            name.trim().to_owned(),
            self.clock.now_ms(),
        );
        let due = Arc::clone(&self.due);
        self.db
            .write(Lane::Foreground, "discover.stated.name", move |tx| {
                let named = |tx: &rusqlite::Transaction| {
                    tx.prepare_cached(sql::STATED_LIST.text())?
                        .query_map(params![user, lower], sql::StatedRow::read)?
                        .next()
                        .transpose()
                };
                if let Some(row) = named(tx)? {
                    return Ok(entry(row));
                }
                let count: i64 =
                    tx.query_row(sql::STATED_COUNT.text(), [&user], |row| row.get(0))?;
                if count >= i64::from(LIMIT) {
                    return Err(StatedError::Full);
                }
                tx.execute(
                    sql::STATED_ADD.text(),
                    params![user, lower, name, Millis(now_ms)],
                )?;
                let Some(row) = named(tx)? else {
                    // The account is gone: nothing was written.
                    return Err(StatedError::Invalid("no such account".to_owned()));
                };
                due.queue_now(tx, jobs::KIND, &user)?;
                Ok(entry(row))
            })
            .await
    }

    /// Take one artist off `user_id`'s list, queuing their taste build in
    /// the same write when it was there.
    ///
    /// # Errors
    ///
    /// An id that is not an artist's, or the write's failure.
    pub async fn remove(&self, user_id: &str, artist_mbid: &str) -> Result<(), StatedError> {
        let lower = checked(artist_mbid)?;
        let (user, due) = (user_id.to_owned(), Arc::clone(&self.due));
        self.db
            .write(Lane::Foreground, "discover.stated.remove", move |tx| {
                let removed = tx.execute(sql::STATED_REMOVE.text(), params![user, lower])?;
                if removed > 0 {
                    due.queue_now(tx, jobs::KIND, &user)?;
                }
                Ok::<_, StatedError>(())
            })
            .await
    }

    /// `user_id`'s named artists, in the order they named them.
    ///
    /// # Errors
    ///
    /// The read's failure.
    pub async fn list(&self, user_id: &str) -> Result<Vec<StatedArtist>, DbError> {
        let rows = read_all(
            &self.db,
            "discover.stated.list",
            sql::STATED_LIST.text(),
            (user_id.to_owned(), None::<String>),
            sql::StatedRow::read,
        )
        .await?;
        Ok(rows.into_iter().map(entry).collect())
    }
}
