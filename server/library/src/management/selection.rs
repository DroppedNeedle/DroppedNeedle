//! Turn a preview's selection into albums and their files.
//!
//! The album is the unit of work: every file of a selected album is
//! planned together and applied together, so an album never ends up half
//! moved. A `tracks` selection plans only the named files, still grouped
//! by album. Only indexed files take part; missing and excluded ones are
//! left out.

use rusqlite::{Connection, params};

use super::models::{CatalogFilter, MAX_SELECTION_IDS, ManagementError, Selection, SelectionKind};

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

const ALBUM_ORDER: &str = "ORDER BY COALESCE(a.album_artist_name_folded, ''), a.title_folded, a.id";

/// Albums a selection covers, in a stable order.
pub fn albums(
    conn: &Connection,
    selection: &Selection,
) -> Result<Vec<AlbumScope>, ManagementError> {
    let ids = ids_json(&selection.ids);
    let whole = |sql: &str, param: &str| -> rusqlite::Result<Vec<AlbumScope>> {
        let mut statement = conn.prepare(sql)?;
        let rows = statement.query_map([param], |row| {
            Ok(AlbumScope {
                album_id: row.get(0)?,
                album_revision: row.get(1)?,
                track_ids: None,
            })
        })?;
        rows.collect()
    };
    let indexed = "EXISTS (SELECT 1 FROM local_tracks t WHERE t.local_album_id = a.id \
                   AND t.availability = 'indexed')";
    let scopes = match selection.kind {
        SelectionKind::Roots => whole(
            &format!(
                "SELECT a.id, a.row_revision FROM local_albums a \
                 WHERE a.root_id IN (SELECT value FROM json_each(?1)) AND {indexed} {ALBUM_ORDER}"
            ),
            &ids,
        )?,
        SelectionKind::Albums => whole(
            &format!(
                "SELECT a.id, a.row_revision FROM local_albums a \
                 WHERE a.id IN (SELECT value FROM json_each(?1)) AND {indexed} {ALBUM_ORDER}"
            ),
            &ids,
        )?,
        SelectionKind::Artists => whole(
            &format!(
                "SELECT a.id, a.row_revision FROM local_albums a \
                 WHERE (a.album_artist_id IN (SELECT value FROM json_each(?1)) \
                 OR EXISTS (SELECT 1 FROM local_album_artists x WHERE x.local_album_id = a.id \
                 AND x.local_artist_id IN (SELECT value FROM json_each(?1)))) \
                 AND {indexed} {ALBUM_ORDER}"
            ),
            &ids,
        )?,
        SelectionKind::Tracks => track_albums(conn, &ids)?,
        SelectionKind::Filter => filtered(conn, selection.catalog_filter.as_ref())?,
    };
    Ok(scopes)
}

fn track_albums(conn: &Connection, ids: &str) -> rusqlite::Result<Vec<AlbumScope>> {
    let mut statement = conn.prepare(&format!(
        "SELECT a.id, a.row_revision, t.id FROM local_tracks t \
         JOIN local_albums a ON a.id = t.local_album_id \
         WHERE t.id IN (SELECT value FROM json_each(?1)) AND t.availability = 'indexed' \
         {ALBUM_ORDER}, t.disc_number, t.track_number, t.relative_path"
    ))?;
    let rows = statement.query_map([ids], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut scopes: Vec<AlbumScope> = Vec::new();
    for row in rows {
        let (album_id, album_revision, track_id) = row?;
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
    let artist_clause = if filter.album_artist_only {
        "a.album_artist_id IN (SELECT value FROM json_each(?5))"
    } else {
        "(a.album_artist_id IN (SELECT value FROM json_each(?5)) \
         OR EXISTS (SELECT 1 FROM local_album_artists x WHERE x.local_album_id = a.id \
         AND x.local_artist_id IN (SELECT value FROM json_each(?5))))"
    };
    let mut statement = conn.prepare(&format!(
        "SELECT a.id, a.row_revision FROM local_albums a \
         WHERE EXISTS (SELECT 1 FROM local_tracks t WHERE t.local_album_id = a.id \
         AND t.availability = 'indexed') \
         AND (?1 IS NULL OR a.title_folded LIKE ?1 OR COALESCE(a.album_artist_name_folded, '') LIKE ?1) \
         AND (?2 IS NULL OR lower(COALESCE(a.primary_genre, '')) = ?2) \
         AND (?3 IS NULL OR a.year >= ?3) AND (?4 IS NULL OR a.year <= ?4) \
         AND (json_array_length(?5) = 0 OR {artist_clause}) {ALBUM_ORDER}"
    ))?;
    let rows = statement.query_map(
        params![search, genre, filter.from_year, filter.to_year, artists],
        |row| {
            Ok(AlbumScope {
                album_id: row.get(0)?,
                album_revision: row.get(1)?,
                track_ids: None,
            })
        },
    )?;
    rows.collect()
}

/// The indexed files of one album in scope, in disc and track order.
pub fn tracks(conn: &Connection, scope: &AlbumScope) -> Result<Vec<CatalogTrack>, ManagementError> {
    let mut statement = conn.prepare_cached(
        "SELECT id, root_id, relative_path, title, artist_name, album_title, track_number, \
         disc_number, file_format, bit_rate, sample_rate, bit_depth FROM local_tracks \
         WHERE local_album_id = ?1 AND availability = 'indexed' \
         ORDER BY disc_number, track_number, relative_path",
    )?;
    let rows = statement.query_map([&scope.album_id], |row| {
        Ok(CatalogTrack {
            id: row.get(0)?,
            root_id: row.get(1)?,
            relative_path: row.get(2)?,
            title: row.get(3)?,
            artist_name: row.get(4)?,
            album_title: row.get(5)?,
            track_number: row.get(6)?,
            disc_number: row.get(7)?,
            file_format: row.get(8)?,
            bit_rate: row.get(9)?,
            sample_rate: row.get(10)?,
            bit_depth: row.get(11)?,
        })
    })?;
    let mut tracks = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    if let Some(picked) = &scope.track_ids {
        tracks.retain(|track| picked.contains(&track.id));
    }
    Ok(tracks)
}
