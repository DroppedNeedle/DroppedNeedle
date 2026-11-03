//! The edition acquisition fetches for an album the library already holds.
//!
//! [`chosen_edition`] is the one place acquisition asks "which release of
//! this album does the user want?". Album requests, "acquire this
//! edition", upgrades, the wanted watcher and single-track downloads all
//! go through it, so the release they fetch is the release the library
//! shows. The answer is the album's identity row, the library's one record
//! of its edition: a release a person chose (a v2 pin carries as one), or
//! the release the matcher picked as the best fit for the files. An album
//! the library does not hold has no chosen edition, and neither does a guess
//! the files' tags contradict; the request's own release (if any) stands.

use droppedneedle_kernel::db::{Db, DbError};
use droppedneedle_library::identity::DecisionSource;
use droppedneedle_library::identity::reads::copy_for_group;
use serde::{Deserialize, Serialize};

use crate::requests::sql;

/// Why this edition was chosen. [`chosen_edition`] answers the first two;
/// a single track resolved to an album may also take the release it was
/// requested from or the best official release. A track task's plan
/// stores it by its serde name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EditionBasis {
    /// A person chose this release for the album.
    #[serde(rename = "manual_identity")]
    Manual,
    /// The matcher picked this release as the best fit for the files.
    #[serde(rename = "library_edition")]
    BestFit,
    /// The release the track was requested from.
    #[serde(rename = "requested_release")]
    RequestedRelease,
    /// The best official release carrying the recording.
    #[serde(rename = "best_official")]
    BestOfficial,
}

impl EditionBasis {
    /// Stable label, as the task plan records it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual_identity",
            Self::BestFit => "library_edition",
            Self::RequestedRelease => "requested_release",
            Self::BestOfficial => "best_official",
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

/// The chosen edition of the library's copy of one release group: the copy
/// the album page shows ([`copy_for_group`]), so a download joins the copy a
/// person sees. `None` when the library holds no copy, or that copy names no
/// release (left unmanaged); another copy's release does not stand in.
pub async fn chosen_edition(
    db: &Db,
    release_group_mbid: &str,
) -> Result<Option<ChosenEdition>, DbError> {
    let release_group_mbid = release_group_mbid.trim().to_owned();
    let copy = db
        .read("acquire.edition.chosen", move |conn| {
            copy_for_group(conn, &release_group_mbid).map_err(DbError::from)
        })
        .await?;
    Ok(copy.and_then(|copy| pick(copy.release_mbid.as_deref(), copy.source)))
}

/// The identity row's release, with who chose it.
fn pick(identity: Option<&str>, source: DecisionSource) -> Option<ChosenEdition> {
    let release_mbid = identity
        .map(|mbid| mbid.trim().to_ascii_lowercase())
        .filter(|mbid| !mbid.is_empty())?;
    let basis = if source.is_persons_choice() {
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
    let group = release_group_mbid.to_owned();
    db.read("acquire.edition.library_holds", move |conn| {
        Ok(droppedneedle_library::identity::reads::group_owned(
            conn, &group,
        )?)
    })
    .await
}

/// Artist and title of the library's copy of one release group, for
/// asks that arrive with only the group id.
pub async fn local_names(
    db: &droppedneedle_kernel::db::Db,
    release_group_mbid: &str,
) -> Result<Option<(String, String)>, droppedneedle_catalog::local::stores::StoreError> {
    let album = droppedneedle_catalog::metadata::library::LocalCatalog::new(db.clone())
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
