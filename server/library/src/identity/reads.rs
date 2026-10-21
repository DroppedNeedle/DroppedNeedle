//! The library copy of one release group, as the catalog and acquisition
//! read it. Both call [`copy_for_group`] and differ only in the [`Pick`]
//! they pass.

use rusqlite::{Connection, OptionalExtension, params};

use super::sql;
use super::{DecisionSource, PROVIDER};

/// Which copy [`copy_for_group`] returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    /// The copy the library shows, whatever its release. A copy that names
    /// no release (left unmanaged) still counts.
    Shown,
    /// The first copy that names a release: the one acquisition can fetch.
    WithRelease,
}

/// One copy of a release group, as the edition reads see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyEdition {
    /// The release its identity names, as stored. `None` when it names none.
    pub release_mbid: Option<String>,
    /// Who chose the identity.
    pub source: DecisionSource,
    /// Indexed tracks the copy holds.
    pub file_count: i64,
}

/// The copy of `release_group_mbid` that `pick` names, over live albums.
/// The group matches ignoring ASCII case. A copy picked by hand (`manual`)
/// comes first, then the oldest. `None` when no live copy is identified as
/// the group.
pub fn copy_for_group(
    conn: &Connection,
    release_group_mbid: &str,
    pick: Pick,
) -> rusqlite::Result<Option<CopyEdition>> {
    let statement = match pick {
        Pick::Shown => sql::COPY_SHOWN,
        Pick::WithRelease => sql::COPY_WITH_RELEASE,
    };
    conn.prepare_cached(statement.text())?
        .query_row(
            params![release_group_mbid.to_ascii_lowercase(), PROVIDER],
            |row| {
                Ok(CopyEdition {
                    release_mbid: row.get(0)?,
                    source: row.get(1)?,
                    file_count: row.get(2)?,
                })
            },
        )
        .optional()
}
