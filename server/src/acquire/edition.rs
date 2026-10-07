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

use sqlx::{Row, SqlitePool};

/// Why this edition was chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditionBasis {
    /// A person chose this release for the album.
    Manual,
    /// The matcher picked this release as the best fit for the files.
    BestFit,
}

impl EditionBasis {
    /// Stable label, as the manifest records it.
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
    pool: &SqlitePool,
    release_group_mbid: &str,
) -> Result<Option<ChosenEdition>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT e.release_mbid, e.decision_source \
         FROM local_album_external_identities e \
         JOIN local_albums b ON b.id = e.local_album_id \
         WHERE b.retired_into_album_id IS NULL AND e.provider = 'musicbrainz' \
           AND lower(e.release_group_mbid) = ? AND e.release_mbid IS NOT NULL \
         ORDER BY e.decision_source <> 'manual', b.created_at LIMIT 1",
    )
    .bind(release_group_mbid.trim().to_ascii_lowercase())
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let identity: Option<String> = row.try_get(0)?;
    let source: String = row.try_get(1)?;
    Ok(pick(identity.as_deref(), &source))
}

/// The identity row's release, with who chose it.
fn pick(identity: Option<&str>, source: &str) -> Option<ChosenEdition> {
    let release_mbid = identity
        .map(|mbid| mbid.trim().to_ascii_lowercase())
        .filter(|mbid| !mbid.is_empty())?;
    let basis = if source == "manual" {
        EditionBasis::Manual
    } else {
        EditionBasis::BestFit
    };
    Some(ChosenEdition {
        release_mbid,
        basis,
    })
}

/// Whether the library holds a copy of one release group.
pub async fn library_holds(
    pool: &SqlitePool,
    release_group_mbid: &str,
) -> Result<bool, sqlx::Error> {
    Ok(
        crate::reads::catalog::library::LocalCatalog::new(pool.clone())
            .album(release_group_mbid)
            .await?
            .is_some(),
    )
}

/// Artist and title of the library's copy of one release group, for
/// asks that arrive with only the group id.
pub async fn local_names(
    pool: &SqlitePool,
    release_group_mbid: &str,
) -> Result<Option<(String, String)>, sqlx::Error> {
    let album = crate::reads::catalog::library::LocalCatalog::new(pool.clone())
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
        let manual = pick(Some("A"), "manual").expect("chosen");
        assert_eq!(
            (manual.release_mbid.as_str(), manual.basis),
            ("a", EditionBasis::Manual)
        );
        let fit = pick(Some("a"), "automatic").map(|chosen| chosen.basis);
        assert_eq!(fit, Some(EditionBasis::BestFit));
        assert_eq!(pick(None, "manual"), None);
    }
}
