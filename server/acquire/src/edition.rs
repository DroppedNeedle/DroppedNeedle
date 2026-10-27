//! The edition acquisition fetches for an album the library already holds.
//!
//! [`chosen_edition`] is the one place acquisition asks "which release of
//! this album does the user want?". Album requests, "acquire this
//! edition", upgrades, the wanted watcher and single-track downloads all
//! go through it, so the release they fetch is the release the library
//! shows. The answer is the album's identity row, the library's one record
//! of its edition: a release a person chose (`manual`), or the release the
//! matcher picked as the best fit for the files. An album the library does
//! not hold has no chosen edition; the request's own release (if any)
//! stands.

use droppedneedle_kernel::db::{Db, DbError};
use droppedneedle_library::identity::DecisionSource;
use droppedneedle_library::identity::reads::{Pick, copy_for_group};

use crate::requests::sql;

/// Why this edition was chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditionBasis {
    /// A person chose this release for the album.
    Manual,
    /// The matcher picked this release as the best fit for the files.
    BestFit,
}

impl EditionBasis {
    /// Stable label, as the task plan records it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual_identity",
            Self::BestFit => "library_edition",
        }
    }
}

/// The release acquisition should fetch for one album.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChosenEdition {
    /// Release MBID, lowercase.
    pub release_mbid: String,
    /// Why this one.
    pub basis: EditionBasis,
}

/// The chosen edition of the library's copy of one release group, or
/// `None` when the library holds no copy with a known release. Several
/// copies of one group read as a chosen copy first, then the oldest one,
/// as the album page does.
pub async fn chosen_edition(
    db: &Db,
    release_group_mbid: &str,
) -> Result<Option<ChosenEdition>, DbError> {
    let release_group_mbid = release_group_mbid.trim().to_owned();
    let copy = db
        .read("acquire.edition.chosen", move |conn| {
            copy_for_group(conn, &release_group_mbid, Pick::WithRelease).map_err(DbError::from)
        })
        .await?;
    Ok(copy.and_then(|copy| pick(copy.release_mbid.as_deref(), copy.source)))
}

/// The identity row's release, with who chose it.
fn pick(identity: Option<&str>, source: DecisionSource) -> Option<ChosenEdition> {
    let release_mbid = identity
        .map(|mbid| mbid.trim().to_ascii_lowercase())
        .filter(|mbid| !mbid.is_empty())?;
    let basis = if source == DecisionSource::Manual {
        EditionBasis::Manual
    } else {
        EditionBasis::BestFit
    };
    Some(ChosenEdition {
        release_mbid,
        basis,
    })
}

/// Whether the app leaves a library album's files as they are: a person
/// chose "leave unmanaged", the app stepped back after outside edits, or a
/// scan followed the album across a retag. None of its files is replaced
/// by another version. Read by "Get missing tracks" and by the landing.
pub async fn copy_is_held(db: &Db, album_id: &str) -> Result<bool, DbError> {
    let album_id = album_id.to_owned();
    db.read("acquire.edition.copy_is_held", move |conn| {
        Ok(conn.query_row(sql::COPY_IS_HELD.text(), [album_id], |row| row.get(0))?)
    })
    .await
}

/// Whether the library holds a copy of one release group with at least one
/// indexed track: the check the landing, the wanted watcher and library
/// presence share.
pub async fn library_holds(db: &Db, release_group_mbid: &str) -> Result<bool, DbError> {
    let group = release_group_mbid.to_ascii_lowercase();
    db.read("acquire.edition.library_holds", move |conn| {
        Ok(conn.query_row(sql::LIBRARY_HOLDS.text(), [group], |row| row.get(0))?)
    })
    .await
}

/// Artist and title of the library's copy of one release group, for
/// asks that arrive with only the group id.
pub async fn local_names(
    db: &droppedneedle_kernel::db::Db,
    release_group_mbid: &str,
) -> Result<Option<(String, String)>, droppedneedle_catalog::reads::library::stores::StoreError> {
    let album = droppedneedle_catalog::reads::catalog::library::LocalCatalog::new(db.clone())
        .album(release_group_mbid)
        .await?;
    Ok(album
        .map(|album| (album.artist_name, album.title))
        .filter(|(artist, title)| !artist.trim().is_empty() && !title.trim().is_empty()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_row_names_the_edition_and_who_chose_it() {
        let manual = pick(Some("A"), DecisionSource::Manual).expect("chosen");
        assert_eq!(
            (manual.release_mbid.as_str(), manual.basis),
            ("a", EditionBasis::Manual)
        );
        let fit = pick(Some("a"), DecisionSource::Automatic).map(|chosen| chosen.basis);
        assert_eq!(fit, Some(EditionBasis::BestFit));
        assert_eq!(pick(None, DecisionSource::Manual), None);
    }
}
