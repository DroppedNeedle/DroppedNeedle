//! The edition acquisition fetches for an album the library already holds.
//!
//! [`chosen_edition`] is the one place acquisition asks "which release of
//! this album does the user want?". Album requests, "acquire this
//! edition", upgrades, the wanted watcher and single-track downloads all
//! go through it, so the release they fetch is the release the library
//! shows. The order is: an identity a person chose (`manual`), then the
//! album's edition pin, then the identity the matcher picked as the best
//! fit for the files. An album the library does not hold has no chosen
//! edition; the request's own release (if any) stands.

use sqlx::{Row, SqlitePool};

/// Why this edition was chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditionBasis {
    /// A person identified the album as this release.
    Manual,
    /// A curator pinned this release on the album.
    Pin,
    /// The matcher picked this release as the best fit for the files.
    BestFit,
}

impl EditionBasis {
    /// Stable label, as the manifest records it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual_identity",
            Self::Pin => "edition_pin",
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
/// copies of one group read as the oldest one, as the album page does.
pub async fn chosen_edition(
    pool: &SqlitePool,
    release_group_mbid: &str,
) -> Result<Option<ChosenEdition>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT e.release_mbid, e.decision_source, p.release_mbid \
         FROM local_album_external_identities e \
         JOIN local_albums b ON b.id = e.local_album_id \
         LEFT JOIN library_album_release_pins p ON p.local_album_id = b.id \
         WHERE b.retired_into_album_id IS NULL AND e.provider = 'musicbrainz' \
           AND lower(e.release_group_mbid) = ? \
         ORDER BY b.created_at LIMIT 1",
    )
    .bind(release_group_mbid.trim().to_ascii_lowercase())
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let identity: Option<String> = row.try_get(0)?;
    let source: String = row.try_get(1)?;
    let pin: Option<String> = row.try_get(2)?;
    Ok(pick(identity.as_deref(), &source, pin.as_deref()))
}

/// Manual identity, then pin, then the matcher's identity.
fn pick(identity: Option<&str>, source: &str, pin: Option<&str>) -> Option<ChosenEdition> {
    let clean = |value: Option<&str>| {
        value
            .map(|mbid| mbid.trim().to_ascii_lowercase())
            .filter(|mbid| !mbid.is_empty())
    };
    let identity = clean(identity);
    if source == "manual"
        && let Some(release_mbid) = identity.clone()
    {
        return Some(ChosenEdition {
            release_mbid,
            basis: EditionBasis::Manual,
        });
    }
    if let Some(release_mbid) = clean(pin) {
        return Some(ChosenEdition {
            release_mbid,
            basis: EditionBasis::Pin,
        });
    }
    identity.map(|release_mbid| ChosenEdition {
        release_mbid,
        basis: EditionBasis::BestFit,
    })
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
    fn manual_identity_beats_pin_and_pin_beats_matcher() {
        let manual = pick(Some("A"), "manual", Some("b")).map(|chosen| chosen.basis);
        assert_eq!(manual, Some(EditionBasis::Manual));
        let pinned = pick(Some("a"), "automatic", Some("B")).expect("pin chosen");
        assert_eq!(
            (pinned.release_mbid.as_str(), pinned.basis),
            ("b", EditionBasis::Pin)
        );
        let fit = pick(Some("a"), "embedded", None).map(|chosen| chosen.basis);
        assert_eq!(fit, Some(EditionBasis::BestFit));
        assert_eq!(pick(None, "automatic", None), None);
    }
}
