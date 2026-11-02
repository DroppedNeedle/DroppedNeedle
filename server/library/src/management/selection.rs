//! Turn a preview's selection into albums and their files.
//!
//! The album is the unit of work: every file of a selected album is
//! planned together and applied together, so an album never ends up half
//! moved. A `tracks` selection plans only the named files, still grouped
//! by album. Only indexed files take part; missing and excluded ones are
//! left out.

use rusqlite::{Connection, params};

use super::models::{CatalogFilter, MAX_SELECTION_IDS, ManagementError, Selection, SelectionKind};
use super::sql::{AlbumScopeRow, FilteredAlbum, TrackAlbum, TrackRow};

/// One album in scope and, for a track selection, the files picked in it.
#[derive(Debug, Clone)]
pub struct AlbumScope {
    pub album_id: String,
    pub album_revision: i64,
    /// `None` plans every indexed file of the album.
    pub track_ids: Option<Vec<String>>,
}

/// One indexed file as the catalog knows it.
#[derive(Debug, Clone)]
pub struct CatalogTrack {
    pub id: String,
    pub root_id: String,
    pub relative_path: String,
    pub title: String,
    pub artist_name: Option<String>,
    pub album_title: String,
    pub track_number: i64,
    pub disc_number: i64,
    pub file_format: String,
    pub bit_rate: Option<i64>,
    pub sample_rate: Option<i64>,
    pub bit_depth: Option<i64>,
}

/// Check a selection and drop duplicate ids, keeping their order.
pub fn normalize(mut selection: Selection) -> Result<Selection, ManagementError> {
    let mut seen = std::collections::HashSet::new();
    selection
        .ids
        .retain(|id| !id.trim().is_empty() && seen.insert(id.clone()));
    if selection.ids.len() > MAX_SELECTION_IDS {
        return Err(ManagementError::Invalid(format!(
            "A preview can name at most {MAX_SELECTION_IDS} items; pick fewer or use a filter."
        )));
    }
    match selection.kind {
        SelectionKind::Filter => {
            selection.ids.clear();
            selection.catalog_filter = Some(selection.catalog_filter.unwrap_or_default());
        }
        _ if selection.ids.is_empty() => {
            return Err(ManagementError::Invalid(
                "Pick at least one item to preview.".to_owned(),
            ));
        }
        _ => selection.catalog_filter = None,
    }
    Ok(selection)
}

fn ids_json(ids: &[String]) -> String {
    serde_json::Value::from(ids.to_vec()).to_string()
}

/// Albums a selection covers, in a stable order.
pub fn albums(
    conn: &Connection,
    selection: &Selection,
) -> Result<Vec<AlbumScope>, ManagementError> {
    let ids = ids_json(&selection.ids);
    let whole = |sql: &str, param: &str| -> rusqlite::Result<Vec<AlbumScope>> {
        let mut statement = conn.prepare(sql)?;
        let rows = statement.query_map([param], AlbumScopeRow::read)?;
        rows.map(|row| {
            row.map(|row| AlbumScope {
                album_id: row.id,
                album_revision: row.row_revision,
                track_ids: None,
            })
        })
        .collect()
    };
    let scopes = match selection.kind {
        SelectionKind::Roots => whole(crate::management::sql::ALBUMS_OF_ROOTS.text(), &ids)?,
        SelectionKind::Albums => whole(crate::management::sql::ALBUMS_OF_IDS.text(), &ids)?,
        SelectionKind::Artists => whole(crate::management::sql::ALBUMS_OF_ARTISTS.text(), &ids)?,
        SelectionKind::Tracks => track_albums(conn, &ids)?,
        SelectionKind::Filter => filtered(conn, selection.catalog_filter.as_ref())?,
    };
    Ok(scopes)
}

fn track_albums(conn: &Connection, ids: &str) -> rusqlite::Result<Vec<AlbumScope>> {
    let mut statement = conn.prepare(crate::management::sql::TRACK_ALBUMS.text())?;
    let rows = statement.query_map([ids], TrackAlbum::read)?;
    let mut scopes: Vec<AlbumScope> = Vec::new();
    for row in rows {
        let TrackAlbum {
            album_id,
            album_revision,
            track_id,
        } = row?;
        match scopes.last_mut() {
            Some(scope) if scope.album_id == album_id => {
                scope.track_ids.get_or_insert_with(Vec::new).push(track_id);
            }
            _ => scopes.push(AlbumScope {
                album_id,
                album_revision,
                track_ids: Some(vec![track_id]),
            }),
        }
    }
    Ok(scopes)
}

fn filtered(
    conn: &Connection,
    filter: Option<&CatalogFilter>,
) -> rusqlite::Result<Vec<AlbumScope>> {
    let filter = filter.cloned().unwrap_or_default();
    let search = filter
        .search
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(|text| format!("%{}%", text.to_lowercase()));
    let genre = filter
        .genre
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_lowercase);
    let artists = ids_json(&filter.artist_ids);
    let mut statement = conn.prepare(&crate::management::sql::filtered_albums(
        filter.album_artist_only,
    ))?;
    let rows = statement.query_map(
        params![search, genre, filter.from_year, filter.to_year, artists],
        FilteredAlbum::read,
    )?;
    rows.map(|row| {
        row.map(|row| AlbumScope {
            album_id: row.id,
            album_revision: row.row_revision,
            track_ids: None,
        })
    })
    .collect()
}

/// The indexed files of one album in scope, in disc and track order.
pub fn tracks(conn: &Connection, scope: &AlbumScope) -> Result<Vec<CatalogTrack>, ManagementError> {
    let mut statement = conn.prepare_cached(crate::management::sql::TRACKS.text())?;
    let rows = statement.query_map([&scope.album_id], TrackRow::read)?;
    let mut tracks = rows
        .map(|row| {
            row.map(|row| CatalogTrack {
                id: row.id,
                root_id: row.root_id,
                relative_path: row.relative_path,
                title: row.title,
                artist_name: row.artist_name,
                album_title: row.album_title,
                track_number: row.track_number,
                disc_number: row.disc_number,
                file_format: row.file_format,
                bit_rate: row.bit_rate,
                sample_rate: row.sample_rate,
                bit_depth: row.bit_depth,
            })
        })
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if let Some(picked) = &scope.track_ids {
        tracks.retain(|track| picked.contains(&track.id));
    }
    Ok(tracks)
}
