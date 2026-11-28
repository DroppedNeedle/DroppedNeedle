//! A person's own shelves turned over: two lists of albums the library
//! holds, live and with files, read from their own rows when the page is
//! built. They are owned albums, so they never go in the pool and are
//! never a pick.
//!
//! - Played a lot, not lately: albums they played [`PLAYED_MIN`] times or
//!   more, last [`IDLE_MS`] ago or longer (`library_user_album_plays`, kept
//!   by triggers from their plays), most played first.
//! - Asked for and never played: release groups they requested that
//!   arrived [`ARRIVED_MS`] ago or longer and that the library owns, with
//!   no play of theirs on any album owning it, the oldest arrival first.
//!   A request row is pruned 180 days after it finishes, and the album
//!   leaves the list with it.
//!
//! Each row's line is built from its own numbers; months are counted in
//! UTC, the server time the weekly set keeps.

use droppedneedle_kernel::db::{Db, DbError};
use time::OffsetDateTime;

use super::super::sql;
use crate::discover::models::{AlbumRef, OwnShelfAlbum, OwnShelves, SectionContent};
use crate::discover::read_all;

/// A day, ms.
const DAY_MS: i64 = 86_400_000;
/// The fewest plays of an album for "played a lot".
pub(crate) const PLAYED_MIN: i64 = 8;
/// How long since an album's last play for "not lately", ms: 90 days.
pub(crate) const IDLE_MS: i64 = 90 * DAY_MS;
/// How long ago an album asked for must have arrived, ms: 14 days.
pub(crate) const ARRIVED_MS: i64 = 14 * DAY_MS;
/// The most albums each list shows.
pub(crate) const PER_LIST: i64 = 8;

/// The UTC moment of `at_ms`.
fn moment(at_ms: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(at_ms.div_euclid(1000))
        .unwrap_or(OffsetDateTime::UNIX_EPOCH)
}

/// The month of `at_ms` as `YYYY-MM`.
fn month_key(at_ms: i64) -> String {
    let at = moment(at_ms);
    format!("{:04}-{:02}", at.year(), u8::from(at.month()))
}

/// The month of `at_ms` as a line says it at `now_ms`: "February", with
/// its year when that is not this year.
fn month_said(at_ms: i64, now_ms: i64) -> String {
    let (at, now) = (moment(at_ms), moment(now_ms));
    if at.year() == now.year() {
        at.month().to_string()
    } else {
        format!("{} {}", at.month(), at.year())
    }
}

/// A played album's line: all its plays in one month ("18 plays in
/// February, none since."), else its count and last month.
fn played_line(plays: i64, first_ms: Option<i64>, last_ms: i64, now_ms: i64) -> String {
    let when = month_said(last_ms, now_ms);
    match first_ms {
        Some(first) if month_key(first) == month_key(last_ms) => {
            format!("{plays} plays in {when}, none since.")
        }
        _ => format!("{plays} plays, last in {when}."),
    }
}

/// A library album as a shelf shows it.
fn album(
    local_id: String,
    title: String,
    artist: (Option<String>, Option<String>),
    release_group_mbid: Option<String>,
    year: Option<i64>,
) -> AlbumRef {
    AlbumRef {
        musicbrainz_id: release_group_mbid,
        local_id: Some(local_id),
        artist_name: artist.0,
        artist_id: artist.1,
        year: year.and_then(|year| i32::try_from(year).ok()),
        in_library: true,
        ..AlbumRef::titled(title)
    }
}

/// The library albums on `user_id`'s shelves at `now_ms`, the played list
/// first, each in its order: what their app playlist plays.
///
/// # Errors
///
/// A read's failure.
pub(crate) async fn albums(db: &Db, user_id: &str, now_ms: i64) -> Result<Vec<String>, DbError> {
    let user = user_id.to_owned();
    let mut albums = read_all(
        db,
        "discover.shelves.played_albums",
        sql::SHELF_PLAYED.text(),
        (user.clone(), PLAYED_MIN, now_ms - IDLE_MS, PER_LIST),
        |row| row.get::<_, String>("local_id"),
    )
    .await?;
    let never = read_all(
        db,
        "discover.shelves.never_played_albums",
        sql::SHELF_NEVER_PLAYED.text(),
        (user, now_ms - ARRIVED_MS, PER_LIST),
        |row| row.get::<_, String>("local_id"),
    )
    .await?;
    for album in never {
        if !albums.contains(&album) {
            albums.push(album);
        }
    }
    Ok(albums)
}

/// `user_id`'s shelves at `now_ms`; `None` when both lists are empty.
///
/// # Errors
///
/// A read's failure.
pub(crate) async fn section(
    db: &Db,
    user_id: &str,
    now_ms: i64,
) -> Result<Option<SectionContent>, DbError> {
    let user = user_id.to_owned();
    let played = read_all(
        db,
        "discover.shelves.played",
        sql::SHELF_PLAYED.text(),
        (user.clone(), PLAYED_MIN, now_ms - IDLE_MS, PER_LIST),
        sql::ShelfPlayedRow::read,
    )
    .await?;
    let never = read_all(
        db,
        "discover.shelves.never_played",
        sql::SHELF_NEVER_PLAYED.text(),
        (user.clone(), now_ms - ARRIVED_MS, PER_LIST),
        sql::ShelfNeverPlayedRow::read,
    )
    .await?;
    if played.is_empty() && never.is_empty() {
        return Ok(None);
    }
    let from: Vec<Option<i64>> = read_all(
        db,
        "discover.shelves.history_from",
        sql::SHELF_HISTORY_FROM.text(),
        [user],
        |row| row.get(0),
    )
    .await?;
    let played_a_lot = played
        .into_iter()
        .map(|row| {
            let last = row.last_played_at.0;
            let first = [row.first_track_at, row.first_album_at]
                .into_iter()
                .flatten()
                .map(|at| at.0)
                .min();
            OwnShelfAlbum {
                line: played_line(row.plays, first, last, now_ms),
                last_played: Some(month_key(last)),
                album: album(
                    row.local_id,
                    row.title,
                    (row.artist_name, row.artist_mbid),
                    row.release_group_mbid,
                    row.year,
                ),
            }
        })
        .collect();
    let never_played = never
        .into_iter()
        .map(|row| OwnShelfAlbum {
            line: format!(
                "Asked for in {}, not played yet.",
                month_said(row.asked_at.0, now_ms)
            ),
            last_played: None,
            album: album(
                row.local_id,
                row.title,
                (row.artist_name, row.artist_mbid),
                Some(row.release_group_mbid),
                row.year,
            ),
        })
        .collect();
    Ok(Some(SectionContent::OwnShelves {
        own_shelves: OwnShelves {
            played_a_lot,
            never_played,
            history_from: from.into_iter().flatten().next().map(month_key),
            month: month_key(now_ms),
        },
    }))
}
