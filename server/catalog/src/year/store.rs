//! The year in review's reads, on the long read pool.

use std::collections::{BTreeSet, HashMap};

use droppedneedle_kernel::db::time::Millis;
use jiff::tz::TimeZone;
use rusqlite::{Connection, OptionalExtension as _, params};

use super::review::{self, ArtistKey, Lists, NEW_TOP, PlayedArtist, Worked, order};
use super::sql::{self, AlbumDetail, FirstPlay, PlayArtist, YearPlay};

/// A span of time, unix milliseconds: from `start` up to `end`.
#[derive(Debug, Clone, Copy)]
pub struct Span {
    /// The first millisecond in it.
    pub start: i64,
    /// The first millisecond after it.
    pub end: i64,
}

/// The artists a year met for the first time.
#[derive(Debug, Clone)]
pub struct NewArtists {
    /// How many.
    pub count: u64,
    /// The most played of them, at most [`NEW_TOP`].
    pub top: Vec<PlayedArtist>,
    /// The first met, with its first play.
    pub first: Option<(PlayedArtist, i64)>,
}

/// What one year's read found.
pub struct YearRead {
    /// The year worked out from its plays.
    pub worked: Worked,
    /// Its new artists; none in a person's first year or under too few
    /// plays.
    pub new_artists: Option<NewArtists>,
    /// The albums its lists show, by id.
    pub albums: HashMap<String, AlbumDetail>,
}

/// One year in the list: the year, its plays and its distinct artists.
pub type YearCount = (i32, u64, u64);

/// `user_id`'s year in `span`, worked out in `zone`; `current` says it is
/// the year in progress. Run in one closure on the long read pool.
///
/// One closure holds every statement because they must agree on one
/// snapshot: a merge or a play landing between two reads would leave an
/// artist in the year with no first play, or a top album with no name.
pub fn read_year(
    conn: &Connection,
    user_id: &str,
    span: Span,
    zone: &TimeZone,
    current: bool,
) -> rusqlite::Result<YearRead> {
    let worked = review::work_out(&year_plays(conn, user_id, span)?, zone, current);
    let Some(lists) = &worked.lists else {
        return Ok(YearRead {
            worked,
            new_artists: None,
            albums: HashMap::new(),
        });
    };
    let new_artists = new_artists(conn, user_id, span, lists)?;
    let albums = album_details(conn, lists)?;
    Ok(YearRead {
        worked,
        new_artists,
        albums,
    })
}

/// The plays a year counts: every play of `user_id`'s in `span`, oldest
/// first. The one list the review is worked out from.
fn year_plays(conn: &Connection, user_id: &str, span: Span) -> rusqlite::Result<Vec<YearPlay>> {
    conn.prepare(sql::YEAR_PLAYS.text())?
        .query_map(
            params![user_id, Millis(span.start), Millis(span.end)],
            YearPlay::read,
        )?
        .collect()
}

/// The library artists `lists` played whose first play by `user_id` is in
/// `span` and whom no imported listen before it names: none when the
/// person has no history before the year at all, since then every artist
/// would be new.
fn new_artists(
    conn: &Connection,
    user_id: &str,
    span: Span,
    lists: &Lists,
) -> rusqlite::Result<Option<NewArtists>> {
    let earliest = Millis(i64::MIN);
    let before: bool = conn.query_row(
        sql::HISTORY_BEFORE.text(),
        params![user_id, earliest, Millis(span.start)],
        |row| row.get(0),
    )?;
    if !before {
        return Ok(None);
    }
    let firsts: Vec<FirstPlay> = conn
        .prepare(sql::FIRST_PLAYS.text())?
        .query_map(
            params![user_id, earliest, Millis(span.end), Millis(span.start)],
            FirstPlay::read,
        )?
        .collect::<rusqlite::Result<_>>()?;
    let mut heard = conn.prepare(sql::HEARD_BEFORE.text())?;
    let mut met = Vec::new();
    for first in firsts {
        // Placeholder rows are not in the year's artists.
        let Some(artist) = lists.library_artists.get(&first.artist_id) else {
            continue;
        };
        if let Some(mbid) = artist.mbid.as_deref() {
            let listened: bool =
                heard.query_row(params![user_id, mbid, Millis(span.start)], |row| row.get(0))?;
            if listened {
                continue;
            }
        }
        met.push((artist.clone(), first.first_at.0));
    }
    let first = met
        .iter()
        .min_by(|(a, a_at), (b, b_at)| {
            a_at.cmp(b_at)
                .then_with(|| a.tally.name.cmp(&b.tally.name))
                .then_with(|| a.tally.key.cmp(&b.tally.key))
        })
        .cloned();
    let count = met.len() as u64;
    let mut top: Vec<PlayedArtist> = met.into_iter().map(|(artist, _)| artist).collect();
    top.sort_by(|a, b| order(&a.tally, &b.tally));
    top.truncate(NEW_TOP);
    Ok(Some(NewArtists { count, top, first }))
}

/// The artist name, cover and release group of every album `lists` shows:
/// the top albums, the busiest month's and the top songs' albums.
fn album_details(
    conn: &Connection,
    lists: &Lists,
) -> rusqlite::Result<HashMap<String, AlbumDetail>> {
    let ids: BTreeSet<&str> = lists
        .albums
        .iter()
        .map(|album| album.key.as_str())
        .chain(lists.busiest_month.2.iter().map(|album| album.key.as_str()))
        .chain(
            lists
                .songs
                .iter()
                .filter_map(|song| song.album_id.as_deref()),
        )
        .collect();
    let mut detail = conn.prepare(sql::ALBUM_DETAIL.text())?;
    let mut found = HashMap::new();
    for id in ids {
        if let Some(row) = detail
            .query_row(params![id], AlbumDetail::read)
            .optional()?
        {
            found.insert(id.to_owned(), row);
        }
    }
    Ok(found)
}

/// `user_id`'s plays and distinct artists per year in `zone`, newest
/// first, for the years before `end` (the end of the current year) that
/// have a play. `year_start` gives a year's first millisecond. Run on the
/// long read pool.
pub fn read_years(
    conn: &Connection,
    user_id: &str,
    end: i64,
    current: i32,
    year_start: impl Fn(i32) -> Option<i64>,
) -> rusqlite::Result<Vec<YearCount>> {
    let mut statement = conn.prepare(sql::PLAY_ARTISTS.text())?;
    let rows = statement.query_map(
        params![user_id, Millis(i64::MIN), Millis(end)],
        PlayArtist::read,
    )?;
    let mut years: Vec<YearCount> = Vec::new();
    let mut year = current;
    let mut start = year_start(year);
    let mut plays = 0;
    let mut artists: BTreeSet<ArtistKey> = BTreeSet::new();
    for row in rows {
        let row = row?;
        while start.is_some_and(|start| row.played_at.0 < start) {
            if plays > 0 {
                years.push((year, plays, artists.len() as u64));
            }
            (plays, artists) = (0, BTreeSet::new());
            year -= 1;
            start = (year >= 1).then(|| year_start(year)).flatten();
        }
        // A play from before year 1 has no year to count in.
        if start.is_none() {
            break;
        }
        plays += 1;
        if let Some(key) = review::artist_key(
            row.artist_id.as_deref(),
            row.artist_placeholder,
            row.artist_name.as_deref().unwrap_or_default(),
        ) {
            artists.insert(key);
        }
    }
    if plays > 0 {
        years.push((year, plays, artists.len() as u64));
    }
    Ok(years)
}
