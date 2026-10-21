//! What the recycle bin holds, read from the journal's rows. A copy is in
//! the bin while its row says so (it reached the bin, or its write waits
//! for an admin, and nothing has marked it gone); the disk is only asked
//! how big each copy is, by a look that opens nothing, through the same
//! checks the bin's expiry uses.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use droppedneedle_kernel::db::{CancelFlag, Db};
use rusqlite::{Connection, OptionalExtension, params};

use super::paths::Sandbox;
use super::put_back::RemovalKind;
use super::{PublishError, sql};

/// What put a group of files in the bin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BinKind {
    /// A whole album's removal.
    Album,
    /// One track's removal.
    Track,
    /// Files the album's edition has no place for.
    Extras,
    /// Files an upgrade or swap replaced.
    Replaced,
    /// A removal with no record this version can put back.
    Removed,
}

impl From<RemovalKind> for BinKind {
    fn from(kind: RemovalKind) -> Self {
        match kind {
            RemovalKind::Album => Self::Album,
            RemovalKind::Track => Self::Track,
            RemovalKind::Extras => Self::Extras,
        }
    }
}

/// The album a group of files came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinAlbum {
    /// The live album id (an album merged since resolves to the one it
    /// went into).
    pub id: String,
    pub title: String,
    pub artist: Option<String>,
    /// Its page opens: it has files, or a MusicBrainz release group.
    pub has_page: bool,
    pub release_group_mbid: Option<String>,
    /// It has art of its own.
    pub cover_available: bool,
}

/// One removal or change whose files are in the bin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinGroup {
    /// The bundle id: for a removal, its id, which Put back names.
    pub id: String,
    pub kind: BinKind,
    /// The album that the undo record, or else its first file's track,
    /// names.
    pub album: Option<BinAlbum>,
    /// When it was made, unix milliseconds.
    pub removed_at: i64,
    /// When its first copy reached the bin, unix milliseconds.
    pub first_binned_at: Option<i64>,
    /// Files in the bin.
    pub files: u32,
    /// Bytes of the copies found.
    pub bytes: u64,
    /// Copies not found where their row says.
    pub not_found: u32,
    /// Copies that can't be looked at: not in a bin that may be emptied
    /// (a bin no longer set, a link on the way), no plain file, or while
    /// the bins can't be told.
    pub unchecked: u32,
    /// Files whose write waits for an admin.
    pub held: u32,
    /// Put back is offered: an undo record not spent, nothing held or
    /// still being written.
    pub can_put_back: bool,
}

/// One page of the bin, and where the next starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinPage {
    pub items: Vec<BinGroup>,
    /// The cursor after the last item, when there may be more.
    pub next: Option<(i64, String)>,
}

/// What emptying the bin now would delete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BinSummary {
    /// Copies found that an empty would delete, unless they changed.
    pub files: u32,
    /// Their bytes.
    pub bytes: u64,
    /// Files in the bin it would leave: held, in use by a put-back, not
    /// found, or not in a bin it may empty.
    pub staying: u32,
    /// Old bin folders no row names that it would empty.
    pub older_folders: u32,
}

/// How a copy looks on disk.
enum Look {
    Found(u64),
    NotFound,
    Unchecked,
}

fn look(sandbox: Option<&Sandbox>, copy: &Path) -> Look {
    let Some(sandbox) = sandbox else {
        return Look::Unchecked;
    };
    if sandbox.ensure_under_bin(copy).is_err() {
        return Look::Unchecked;
    }
    match std::fs::symlink_metadata(copy) {
        Ok(meta) if meta.file_type().is_file() => Look::Found(meta.len()),
        Ok(_) => Look::Unchecked,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Look::NotFound,
        Err(_) => Look::Unchecked,
    }
}

type Row = (String, i64, Option<f64>, bool);
type File = (String, String, String, Option<String>);
type Record = (String, Option<String>, Option<String>, bool);

/// One page of the bin, newest first, after `cursor`, at most `limit`.
/// `sandbox` is the expiry's (`None` while the bins can't be told).
pub fn page(
    db: &Db,
    sandbox: Option<&Sandbox>,
    cursor: Option<(i64, String)>,
    limit: usize,
) -> Result<BinPage, PublishError> {
    let (at, after) = cursor.unwrap_or((i64::MAX, String::new()));
    let limit = limit.max(1);
    let (rows, more, files, records, spent, albums) =
        db.read_long("publish.bin_page", &CancelFlag::never(), |conn| {
            let mut rows: Vec<Row> = conn
                .prepare_cached(sql::BIN_PAGE.text())?
                .query_map(params![at, after, limit as i64 + 1], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                })?
                .collect::<Result<_, _>>()?;
            let more = rows.len() > limit;
            rows.truncate(limit);
            let ids =
                serde_json::Value::from(rows.iter().map(|row| row.0.clone()).collect::<Vec<_>>())
                    .to_string();
            let files: Vec<File> = conn
                .prepare_cached(sql::BIN_FILES.text())?
                .query_map([&ids], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                })?
                .collect::<Result<_, _>>()?;
            let records: Vec<Record> = conn
                .prepare_cached(sql::BIN_RECORDS.text())?
                .query_map([&ids], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                })?
                .collect::<Result<_, _>>()?;
            let spent: HashSet<String> = conn
                .prepare_cached(sql::BIN_SPENT.text())?
                .query_map([&ids], |row| row.get(0))?
                .collect::<Result<_, _>>()?;
            let mut wanted: Vec<&str> = records
                .iter()
                .filter_map(|record| record.2.as_deref())
                .collect();
            wanted.extend(files.iter().filter_map(|file| file.3.as_deref()));
            let mut albums = HashMap::new();
            for id in wanted {
                if !albums.contains_key(id) {
                    albums.insert(id.to_owned(), album(conn, id)?);
                }
            }
            Ok::<_, PublishError>((rows, more, files, records, spent, albums))
        })?;
    let mut by_bundle: BTreeMap<&str, Vec<&File>> = BTreeMap::new();
    for file in &files {
        by_bundle.entry(file.0.as_str()).or_default().push(file);
    }
    let records: HashMap<&str, &Record> = records
        .iter()
        .map(|record| (record.0.as_str(), record))
        .collect();
    let next = more
        .then(|| rows.last().map(|row| (row.1, row.0.clone())))
        .flatten();
    let items = rows
        .iter()
        .map(|(id, removed_at, first_binned, removal)| {
            let record = records.get(id.as_str()).copied();
            let kind = match record {
                Some((_, reason, _, _)) => reason
                    .as_deref()
                    .and_then(RemovalKind::of)
                    .map_or(BinKind::Removed, BinKind::from),
                None if *removal => BinKind::Removed,
                None => BinKind::Replaced,
            };
            let mut group = BinGroup {
                id: id.clone(),
                kind,
                album: None,
                removed_at: *removed_at,
                first_binned_at: first_binned.map(|secs| (secs * 1000.0) as i64),
                files: 0,
                bytes: 0,
                not_found: 0,
                unchecked: 0,
                held: 0,
                can_put_back: false,
            };
            let rows_of = by_bundle.get(id.as_str()).map_or(&[][..], Vec::as_slice);
            for (_, copy, state, _) in rows_of {
                group.files += 1;
                if state == "needs_attention" {
                    group.held += 1;
                }
                match look(sandbox, Path::new(copy)) {
                    Look::Found(size) => group.bytes += size,
                    Look::NotFound => group.not_found += 1,
                    Look::Unchecked => group.unchecked += 1,
                }
            }
            let album_id = record
                .and_then(|record| record.2.as_deref())
                .or_else(|| rows_of.iter().find_map(|file| file.3.as_deref()));
            group.album = album_id.and_then(|id| albums.get(id).cloned().flatten());
            group.can_put_back = record.is_some_and(|(_, _, album, busy)| {
                album.is_some() && !busy && !matches!(kind, BinKind::Removed)
            }) && !spent.contains(id)
                && group.held == 0;
            group
        })
        .collect();
    Ok(BinPage { items, next })
}

fn album(conn: &Connection, id: &str) -> Result<Option<BinAlbum>, PublishError> {
    Ok(conn
        .prepare_cached(sql::BIN_ALBUM.text())?
        .query_row([id], |row| {
            let release_group_mbid: Option<String> = row.get(4)?;
            Ok(BinAlbum {
                id: row.get(0)?,
                title: row.get(1)?,
                artist: row.get(2)?,
                has_page: row.get::<_, bool>(3)? || release_group_mbid.is_some(),
                release_group_mbid,
                cover_available: row.get(5)?,
            })
        })
        .optional()?)
}

/// What emptying the bin at `as_of_ms` would delete: the copies the
/// expiry with no retention would try, found by a look that opens nothing
/// (a changed copy is counted and then kept), and the old folders no row
/// names.
pub fn summary(
    db: &Db,
    sandbox: Option<&Sandbox>,
    as_of_ms: i64,
) -> Result<BinSummary, PublishError> {
    let cutoff = as_of_ms as f64 / 1000.0;
    let held = super::expiry::held_tracks(db)?;
    let (count, due) = db.read_long("publish.bin_summary", &CancelFlag::never(), |conn| {
        let count: u32 = conn
            .prepare_cached(sql::BIN_COUNT.text())?
            .query_row([], |row| row.get(0))?;
        let due: Vec<(String, Option<String>)> = conn
            .prepare_cached(sql::BIN_DUE.text())?
            .query_map([cutoff], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<_, _>>()?;
        Ok::<_, PublishError>((count, due))
    })?;
    let mut summary = BinSummary {
        files: 0,
        bytes: 0,
        staying: 0,
        older_folders: 0,
    };
    for (copy, track) in &due {
        if track.as_ref().is_some_and(|track| held.contains(track)) {
            continue;
        }
        if let Look::Found(size) = look(sandbox, Path::new(copy)) {
            summary.files += 1;
            summary.bytes += size;
        }
    }
    summary.staying = count.saturating_sub(summary.files);
    if let Some(sandbox) = sandbox {
        summary.older_folders =
            u32::try_from(super::expiry::legacy_entries(db, sandbox, cutoff)?.len())
                .unwrap_or(u32::MAX);
    }
    Ok(summary)
}
