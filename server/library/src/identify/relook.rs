//! The look-again sweep: an album identification found nothing for, or
//! only guessed, is offered to it again by itself, so a release MusicBrainz
//! adds later matches without anyone asking.
//!
//! An album is due 1, 3, 7 and 14 days after its last look, then every
//! 30 days, counted by its finished looks. At most [`PER_DAY`] backlog
//! looks are queued in any 24 hours, the longest waiting album first, as
//! backlog work ([`super::queue::offer_again`]): a pass counts every
//! backlog job queued in the last day, its own and other work's, so a
//! restart never adds a second day's worth. Albums a person decided (a
//! declined match, a custom edition, a person's match) are left alone, as
//! are albums with a job waiting, running or in Attention.

use droppedneedle_kernel::db::{Db, DbError, Lane};
use rusqlite::{Connection, params};

use super::queue::offer_again;

/// Most backlog looks queued in any 24 hours.
pub const PER_DAY: i64 = 20;

const DAY_MS: i64 = 86_400_000;

/// The wait after the first, second, third and fourth finished look, then
/// after every later one.
pub const WAITS_MS: [i64; 5] = [DAY_MS, 3 * DAY_MS, 7 * DAY_MS, 14 * DAY_MS, 30 * DAY_MS];

/// How many more backlog looks [`PER_DAY`] leaves room for at `now_ms`:
/// every backlog job queued in the last 24 hours counts. Runs inside the
/// caller's transaction.
pub(crate) fn backlog_room(conn: &Connection, now_ms: i64) -> rusqlite::Result<i64> {
    let queued: i64 = conn
        .prepare_cached(super::sql::BACKLOG_QUEUED_SINCE.text())?
        .query_row(params![now_ms - DAY_MS], |row| row.get(0))?;
    Ok(PER_DAY - queued)
}

/// Offer the albums due another look at `now_ms` to identification, in one
/// write, as many as the day's [`PER_DAY`] still allows. Answers how many
/// were offered. Blocking.
pub fn look_again(db: &Db, now_ms: i64) -> Result<usize, DbError> {
    db.write_blocking(Lane::Background, "identify.look_again", |tx| {
        let room = backlog_room(tx, now_ms)?;
        if room <= 0 {
            return Ok(0);
        }
        let due: Vec<String> = tx
            .prepare_cached(super::sql::RELOOK_DUE.text())?
            .query_map(
                params![
                    now_ms,
                    room,
                    WAITS_MS[0],
                    WAITS_MS[1],
                    WAITS_MS[2],
                    WAITS_MS[3],
                    WAITS_MS[4]
                ],
                |row| row.get(0),
            )?
            .collect::<Result<_, _>>()?;
        let mut offered = 0;
        for album in &due {
            if offer_again(tx, album, now_ms)? {
                offered += 1;
            }
        }
        Ok::<_, DbError>(offered)
    })
}
