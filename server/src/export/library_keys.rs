//! Album grouping keys in v3's form for the carried catalog.
//!
//! v2 keyed every album by folder and case-folded names; v3 keys an album
//! by its release MBID, else its tagged album artist and title, else its
//! folder (see [`album_grouping_key`]). When v3 re-reads a file it keeps
//! the file in its album while the album answers to the key the file
//! gives, so a carried album must already hold that key or the first
//! changed file would leave it.
//!
//! Each carried album takes the key most of its tracks give (present
//! tracks first; ties go to the smaller key, so the result does not depend
//! on row order). Albums without tracks keep v2's key.

use std::collections::{BTreeMap, HashMap};

use rusqlite::{Connection, params};

use crate::db::fold::fold_text;
use crate::export::error::ExportError;
use crate::export::sections::library::{ALBUMS, TRACKS};
use crate::library::scan::naming::{album_grouping_key, grouping_directory};

/// One carried track's naming facts, as the scan would see them.
struct TrackNames {
    album_id: String,
    present: bool,
    relative_path: String,
    release_mbid: Option<String>,
    from_tags: bool,
    album_title: String,
    album_artist: String,
}

impl TrackNames {
    fn key(&self) -> String {
        album_grouping_key(
            self.release_mbid.as_deref(),
            self.from_tags,
            &grouping_directory(&self.relative_path),
            &fold_text(&self.album_title),
            &fold_text(&self.album_artist),
        )
    }
}

fn bundle_error(error: rusqlite::Error) -> ExportError {
    ExportError::Bundle {
        reason: error.to_string(),
    }
}

/// The key an album should hold: the one most of its present tracks give
/// (all tracks when none is present), ties to the smaller key.
fn chosen_key(tracks: &[TrackNames]) -> Option<String> {
    let any_present = tracks.iter().any(|track| track.present);
    let mut votes: BTreeMap<String, usize> = BTreeMap::new();
    for track in tracks.iter().filter(|track| track.present || !any_present) {
        *votes.entry(track.key()).or_default() += 1;
    }
    let best = votes.values().copied().max()?;
    votes
        .into_iter()
        .find(|(_, count)| *count == best)
        .map(|(key, _)| key)
}

/// Rewrite the grouping key of every carried album in the bundle. Returns
/// how many albums changed.
pub fn rekey_albums(conn: &Connection) -> Result<usize, ExportError> {
    let tracks_table = TRACKS.name;
    let albums_table = ALBUMS.name;
    let has = |table: &str, column: &str| -> Result<bool, ExportError> {
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info(?1, 'main') WHERE name = ?2)",
            params![table, column],
            |row| row.get(0),
        )
        .map_err(bundle_error)
    };
    // A v2 without a catalog (or an older one lacking these columns)
    // leaves the keys as they are.
    for column in [
        "local_album_id",
        "relative_path",
        "embedded_release_mbid",
        "album_title_provenance",
        "album_artist_provenance",
        "album_title",
        "album_artist_name",
        "availability",
    ] {
        if !has(tracks_table, column)? {
            return Ok(0);
        }
    }
    if !has(albums_table, "grouping_key")? {
        return Ok(0);
    }
    let mut by_album: HashMap<String, Vec<TrackNames>> = HashMap::new();
    {
        let mut stmt = conn
            .prepare(&format!(
                "SELECT local_album_id, availability = 'indexed', relative_path, \
                 embedded_release_mbid, \
                 album_title_provenance = 'tag' AND album_artist_provenance = 'tag', \
                 album_title, COALESCE(album_artist_name, '') FROM main.\"{tracks_table}\""
            ))
            .map_err(bundle_error)?;
        let rows = stmt
            .query_map([], |row| {
                let mbid: Option<String> = row.get(3)?;
                Ok(TrackNames {
                    album_id: row.get(0)?,
                    present: row.get(1)?,
                    relative_path: row.get(2)?,
                    release_mbid: mbid
                        .map(|mbid| mbid.trim().to_owned())
                        .filter(|mbid| !mbid.is_empty()),
                    from_tags: row.get(4)?,
                    album_title: row.get(5)?,
                    album_artist: row.get(6)?,
                })
            })
            .map_err(bundle_error)?;
        for row in rows {
            let track = row.map_err(bundle_error)?;
            by_album
                .entry(track.album_id.clone())
                .or_default()
                .push(track);
        }
    }
    let mut update = conn
        .prepare(&format!(
            "UPDATE main.\"{albums_table}\" SET grouping_key = ?2 \
             WHERE id = ?1 AND grouping_key IS NOT ?2"
        ))
        .map_err(bundle_error)?;
    let mut changed = 0;
    for (album_id, tracks) in &by_album {
        if let Some(key) = chosen_key(tracks) {
            changed += update
                .execute(params![album_id, key])
                .map_err(bundle_error)?;
        }
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(path: &str, mbid: Option<&str>, present: bool) -> TrackNames {
        TrackNames {
            album_id: "a".to_owned(),
            present,
            relative_path: path.to_owned(),
            release_mbid: mbid.map(str::to_owned),
            from_tags: true,
            album_title: "Kind of Blue".to_owned(),
            album_artist: "Miles Davis".to_owned(),
        }
    }

    /// The majority of present tracks decides, a missing track does not
    /// vote, and the key matches what the scan computes for the file.
    #[test]
    fn album_takes_the_key_most_present_tracks_give() {
        let mbid = "ABCDEF00-0000-4000-8000-000000000001";
        let tracks = [
            track("Miles/Blue/01.flac", Some(mbid), true),
            track("Miles/Blue/02.flac", Some(mbid), true),
            track("Miles/Blue/03.flac", None, true),
            track("Miles/Blue/04.flac", None, false),
            track("Miles/Blue/05.flac", None, false),
        ];
        assert_eq!(
            chosen_key(&tracks).as_deref(),
            Some("mbid:abcdef00-0000-4000-8000-000000000001")
        );
        let untagged = [track("Miles/Blue/03.flac", None, true)];
        assert_eq!(
            chosen_key(&untagged),
            Some(format!("tag:miles davis{}kind of blue", '\u{1f}'))
        );
    }
}
