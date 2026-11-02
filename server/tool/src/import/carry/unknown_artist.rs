//! The fold of v2's own Unknown Artist into v3's built-in one.
//!
//! The export keeps a v2 row named "Unknown Artist" under its v2 id, since
//! the built-in row holds the key that name scans to. Every import with a
//! bundle looks at those rows after the links settle. The library decides
//! and merges ([`fold_v2_placeholder`]); a row that holds a real id or a
//! mixed credit stays, and the summary notes say why. The step is never
//! marked: repeating it writes nothing, and it moves on what a later export
//! landed on a row that already folded, except links and other-name credits,
//! which stay there. A dry run judges the bundle's rows by the same rule.

use droppedneedle_kernel::db::statement::Statement;
use droppedneedle_library::corrections::{
    V2Fold, V2PlaceholderFacts, V2PlaceholderKept, fold_v2_placeholder,
};
use droppedneedle_library::scan::naming::{UNKNOWN_ARTIST, UNKNOWN_ARTIST_ID};
use rusqlite::types::ToSql;
use rusqlite::{Connection, OptionalExtension as _, Transaction, params};

use super::baselines::bundle_has;
use super::sql;
use super::{CarryError, SectionResult};
use crate::export::sections::library::ARTISTS;
use crate::import::report::now_secs;

/// Report entity name.
pub(crate) const ENTITY: &str = "unknown_artist";

const FOLDED: &str = "v2's Unknown Artist is now v3's: its songs, plays and favorites moved \
     there, and its v2 id still opens it";

const WOULD_FOLD: &str = "v2's Unknown Artist would become v3's: its songs, plays and favorites \
     would move there, and its v2 id would still open it";

fn kept_line(id: &str, kept: V2PlaceholderKept) -> String {
    format!(
        "An Unknown Artist ({id}) stays apart from v3's: {}",
        kept.sentence()
    )
}

fn candidates(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    if !bundle_has(conn, ARTISTS.name)? {
        return Ok(Vec::new());
    }
    conn.prepare(sql::UNKNOWN_ARTIST_CANDIDATES.text())?
        .query_map(params![UNKNOWN_ARTIST, UNKNOWN_ARTIST_ID], |row| row.get(0))?
        .collect()
}

/// Fold every candidate in the section's transaction.
pub(crate) fn fold(tx: &Transaction<'_>) -> Result<SectionResult, CarryError> {
    let mut result = SectionResult::default();
    let now = now_secs();
    for id in candidates(tx)? {
        match fold_v2_placeholder(tx, &id, now)? {
            V2Fold::Folded => {
                result.written += 1;
                result.unknown_artists_folded += 1;
                result.summary.push(FOLDED.to_owned());
            }
            V2Fold::AlreadyFolded { moved, left } => {
                result.written += moved;
                result.unknown_artists_folded += 1;
                if moved > 0 {
                    result.summary.push(format!(
                        "{moved} row(s) that landed on the folded Unknown Artist ({id}) moved \
                         to v3's"
                    ));
                }
                if left > 0 {
                    result.summary.push(format!(
                        "{left} link(s) or credit(s) that landed on the folded Unknown Artist \
                         ({id}) stay on that row, where nothing reads them, because they may \
                         be another artist's"
                    ));
                }
            }
            V2Fold::Kept(kept) => result.summary.push(kept_line(&id, kept)),
        }
    }
    Ok(result)
}

/// Say what a real run would do, from the bundle's rows.
pub(crate) fn would_fold(conn: &Connection) -> Result<SectionResult, CarryError> {
    let mut result = SectionResult::default();
    let retired_into = |id: &str| -> rusqlite::Result<Option<Option<String>>> {
        conn.query_row(sql::ARTIST_RETIRED_INTO.text(), params![id], |row| {
            row.get(0)
        })
        .optional()
    };
    let built_in_live = matches!(retired_into(UNKNOWN_ARTIST_ID)?, Some(None));
    for id in candidates(conn)? {
        if retired_into(&id)?.flatten().as_deref() == Some(UNKNOWN_ARTIST_ID) {
            result.unknown_artists_folded += 1;
            continue;
        }
        let verdict = if built_in_live {
            bundle_facts(conn, &id)?.judge()
        } else {
            Err(V2PlaceholderKept::NotPlaceholder)
        };
        match verdict {
            Ok(()) => {
                result.unknown_artists_folded += 1;
                result.summary.push(WOULD_FOLD.to_owned());
            }
            Err(kept) => result.summary.push(kept_line(&id, kept)),
        }
    }
    Ok(result)
}

/// The facts the bundle holds. It carries no credit's MusicBrainz id and no
/// proof, so `named_ids` stays empty.
fn bundle_facts(conn: &Connection, id: &str) -> rusqlite::Result<V2PlaceholderFacts> {
    let linked_ids = if bundle_has(conn, "artist_identity")? {
        conn.prepare(sql::UNKNOWN_ARTIST_LINKS.text())?
            .query_map(params![id], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?
    } else {
        Vec::new()
    };
    let credit = |table, statement| holds(conn, table, statement, params![id, UNKNOWN_ARTIST]);
    Ok(V2PlaceholderFacts {
        linked_ids,
        source_link: holds(
            conn,
            "library_source_link",
            sql::UNKNOWN_ARTIST_SOURCE_LINK,
            params![id],
        )?,
        named_ids: Vec::new(),
        other_credit: credit("library_album_artist", sql::UNKNOWN_ARTIST_ALBUM_CREDIT)?
            || credit("library_track_artist", sql::UNKNOWN_ARTIST_TRACK_CREDIT)?
            || credit("library_album", sql::UNKNOWN_ARTIST_ALBUM_NAME)?,
        marked_distinct: holds(
            conn,
            "artist_dismissal",
            sql::UNKNOWN_ARTIST_DISMISSED,
            params![id, UNKNOWN_ARTIST_ID],
        )?,
    })
}

/// A yes-or-no statement over one bundle table; no when the bundle has none.
fn holds(
    conn: &Connection,
    table: &str,
    statement: Statement,
    values: &[&dyn ToSql],
) -> rusqlite::Result<bool> {
    if !bundle_has(conn, table)? {
        return Ok(false);
    }
    conn.query_row(statement.text(), values, |row| row.get(0))
}
