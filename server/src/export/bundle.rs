//! The export bundle: a SQLite file beside the export JSON that holds every
//! carried user-data section in its v3 shape.
//!
//! Large tables (play history, the download ledger) do not belong in one
//! JSON document the importer has to hold in memory. The bundle keeps them
//! as tables the importer can apply with set-based SQL instead. The export
//! JSON names the bundle with its SHA-256 and size, so the content digest
//! covers it; secrets never go in here.
//!
//! The bundle opens v2 through `ATTACH` with the same immutable, read-only
//! URI the exporter uses, so v2 is never written. Bundle columns carry no
//! declared type: values keep the exact storage class v2 gave them.

use std::collections::{BTreeMap, HashSet};
use std::io::Read as _;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags, params};
use sha2::{Digest as _, Sha256};

use crate::export::envelope::LeftBehind;
use crate::export::error::ExportError;
use crate::export::sections::{ALL, Column, FileSet, LIVE_USER, Source, TableSection, V2Value};

/// What building the bundle produced.
#[derive(Debug, Default)]
pub struct BundleOutcome {
    /// Rows written per section.
    pub sections: BTreeMap<String, u64>,
    /// Rows and files the section filters skipped.
    pub left_behind: Vec<LeftBehind>,
    /// v2 tables the sections read, so the inventory skips them.
    pub read_tables: HashSet<&'static str>,
}

/// Image extensions v2 wrote for avatars and playlist covers, with their
/// content types.
const IMAGE_TYPES: &[(&str, &str)] = &[
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("png", "image/png"),
    ("webp", "image/webp"),
    ("gif", "image/gif"),
];

fn bundle_error(reason: impl std::fmt::Display) -> ExportError {
    ExportError::Bundle {
        reason: reason.to_string(),
    }
}

fn db_error(table: &str, error: rusqlite::Error) -> ExportError {
    ExportError::V2Database {
        table: table.to_owned(),
        detail: error.to_string(),
    }
}

/// Build a fresh bundle at `bundle_path` from the stopped v2 database at
/// `v2_db`, reading cover and avatar files under `v2_cache`. The path must
/// not exist yet.
pub fn write_bundle(
    v2_db: &Path,
    v2_cache: &Path,
    bundle_path: &Path,
) -> Result<BundleOutcome, ExportError> {
    // Created empty and owner-only first: the bundle holds listening
    // history and request records, and SQLite would create it world
    // readable. Refuses an existing file.
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(bundle_path).map_err(|error| {
        bundle_error(format!("cannot create {}: {error}", bundle_path.display()))
    })?;
    let conn = Connection::open_with_flags(
        bundle_path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(bundle_error)?;
    let uri = format!(
        "file:{}?immutable=1&mode=ro",
        crate::export::v2dir::sqlite_uri_path(v2_db)
    );
    conn.execute("ATTACH DATABASE ?1 AS v2", params![uri])
        .map_err(|error| db_error("database", error))?;
    conn.execute_batch("BEGIN").map_err(bundle_error)?;
    let mut outcome = BundleOutcome::default();
    for section in ALL {
        let rows = match section.source {
            Source::Table {
                table,
                filter,
                requires,
            } => {
                outcome.read_tables.insert(table);
                let source = TableSource {
                    table,
                    filter,
                    requires,
                };
                copy_table(&conn, section, &source, &mut outcome.left_behind)?
            }
            Source::Files(set) => {
                copy_files(&conn, section, set, v2_cache, &mut outcome.left_behind)?
            }
            Source::TableWithFiles {
                table,
                filter,
                files,
            } => {
                outcome.read_tables.insert(table);
                let source = TableSource {
                    table,
                    filter,
                    requires: &[],
                };
                let copied = copy_table(&conn, section, &source, &mut outcome.left_behind)?;
                if copied == 0 {
                    0
                } else {
                    attach_files(&conn, section, files, v2_cache, &mut outcome.left_behind)?
                }
            }
        };
        index_section(&conn, section)?;
        outcome.sections.insert(section.name.to_owned(), rows);
    }
    // Album keys in v3's form, now that albums and tracks are both in.
    crate::export::library_keys::rekey_albums(&conn)?;
    conn.execute_batch("COMMIT").map_err(bundle_error)?;
    conn.execute_batch("DETACH DATABASE v2")
        .map_err(bundle_error)?;
    conn.close().map_err(|(_, error)| bundle_error(error))?;
    Ok(outcome)
}

/// Index a filled section on its key and its unique rule, so the
/// importer's look-ups (an earlier row on the same rule, a parent row on a
/// dry run) probe an index instead of scanning the section once per row.
/// Built after the rows go in, which is cheaper than keeping it up to date.
fn index_section(conn: &Connection, section: &TableSection) -> Result<(), ExportError> {
    let indexes = std::iter::once(("key", section.key))
        .chain(section.unique.iter().map(|rule| ("unique", *rule)));
    for (kind, columns) in indexes {
        conn.execute_batch(&format!(
            "CREATE INDEX main.\"{name}__{kind}\" ON \"{name}\" ({cols})",
            name = section.name,
            cols = quoted_list(columns, ""),
        ))
        .map_err(bundle_error)?;
    }
    Ok(())
}

/// Columns of one v2 table; empty when the table does not exist.
fn v2_columns(conn: &Connection, table: &str) -> Result<HashSet<String>, ExportError> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA v2.table_info(\"{table}\")"))
        .map_err(|error| db_error(table, error))?;
    let names = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|error| db_error(table, error))?
        .collect::<Result<HashSet<_>, _>>()
        .map_err(|error| db_error(table, error))?;
    Ok(names)
}

fn create_section_table(
    conn: &Connection,
    name: &str,
    columns: &[&str],
) -> Result<(), ExportError> {
    let list = quoted_list(columns, "");
    conn.execute_batch(&format!("CREATE TABLE main.\"{name}\" ({list})"))
        .map_err(bundle_error)
}

/// `"a", "b"` with an optional alias prefix (`s."a", s."b"`).
fn quoted_list(columns: &[&str], alias: &str) -> String {
    columns
        .iter()
        .map(|column| format!("{alias}\"{column}\""))
        .collect::<Vec<_>>()
        .join(", ")
}

/// One table-backed section's v2 origin.
struct TableSource {
    table: &'static str,
    filter: &'static str,
    requires: &'static [&'static str],
}

/// The SQL reading one column from the v2 row, when v2 has it.
fn v2_sql(column: &Column, available: &HashSet<String>) -> Option<String> {
    match column.v2 {
        V2Value::Column(name) => available.contains(name).then(|| format!("s.\"{name}\"")),
        V2Value::Expr(expr) => Some(format!("({expr})")),
    }
}

/// Copy one v2 table into its bundle section. Columns v2 lacks are left
/// out (v3's defaults fill them at import); a missing key column means the
/// table cannot be carried at all. A section whose tables v2 does not have
/// is empty.
fn copy_table(
    conn: &Connection,
    section: &TableSection,
    source: &TableSource,
    left_behind: &mut Vec<LeftBehind>,
) -> Result<u64, ExportError> {
    let table = source.table;
    let available = v2_columns(conn, table)?;
    let mut has_required = true;
    for required in source.requires {
        has_required &= !v2_columns(conn, required)?.is_empty();
    }
    if available.is_empty() || !has_required {
        let all: Vec<&str> = section.columns.iter().map(|column| column.name).collect();
        create_section_table(conn, section.name, &all)?;
        return Ok(0);
    }
    let present: Vec<(&str, String)> = section
        .columns
        .iter()
        .filter_map(|column| v2_sql(column, &available).map(|sql| (column.name, sql)))
        .collect();
    let rules = section.unique.iter().flat_map(|rule| rule.iter());
    for key in section.key.iter().chain(rules) {
        if !present.iter().any(|(name, _)| name == key) {
            return Err(ExportError::V2Database {
                table: table.to_owned(),
                detail: format!("column for {key} is missing"),
            });
        }
    }
    let names: Vec<&str> = present.iter().map(|(name, _)| *name).collect();
    create_section_table(conn, section.name, &names)?;
    let value_of = |name: &str| {
        present
            .iter()
            .find(|(present, _)| *present == name)
            .map(|(_, sql)| sql.clone())
    };
    let mut predicates = Vec::new();
    if !source.filter.is_empty() {
        predicates.push(format!("({})", source.filter));
    }
    if let Some(user) = section.user_column.and_then(value_of) {
        predicates.push(format!("({user} IS NULL OR {user} {LIVE_USER})"));
    }
    let where_clause = if predicates.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", predicates.join(" AND "))
    };
    let order: Vec<String> = section.key.iter().filter_map(|key| value_of(key)).collect();
    let values: Vec<&str> = present.iter().map(|(_, sql)| sql.as_str()).collect();
    let copied = conn
        .execute(
            &format!(
                "INSERT INTO main.\"{name}\" ({cols}) SELECT {values} FROM v2.\"{table}\" AS s \
                 {where_clause} ORDER BY {order}",
                name = section.name,
                cols = quoted_list(&names, ""),
                values = values.join(", "),
                order = order.join(", "),
            ),
            [],
        )
        .map_err(|error| db_error(table, error))?;
    let copied = u64::try_from(copied).unwrap_or(0);
    if section.left_behind.is_empty() {
        return Ok(copied);
    }
    let total: i64 = conn
        .query_row(&format!("SELECT COUNT(*) FROM v2.\"{table}\""), [], |row| {
            row.get(0)
        })
        .map_err(|error| db_error(table, error))?;
    let skipped = u64::try_from(total).unwrap_or(0).saturating_sub(copied);
    if skipped > 0 {
        left_behind.push(LeftBehind {
            table: table.to_owned(),
            rows: skipped,
            reason: section.left_behind.to_owned(),
        });
    }
    Ok(copied)
}

/// Fill a file-backed section.
fn copy_files(
    conn: &Connection,
    section: &TableSection,
    set: FileSet,
    v2_cache: &Path,
    left_behind: &mut Vec<LeftBehind>,
) -> Result<u64, ExportError> {
    let names: Vec<&str> = section.columns.iter().map(|column| column.name).collect();
    create_section_table(conn, section.name, &names)?;
    match set {
        FileSet::PlaylistCovers => copy_playlist_covers(conn, v2_cache, left_behind),
        FileSet::Avatars => copy_avatars(conn, v2_cache),
        FileSet::ManagementBlobs => copy_management_blobs(conn, v2_cache, left_behind),
        FileSet::HeldImports => Err(bundle_error("held files come with their rows")),
    }
}

/// Fill the file column of a table section whose rows name files. Rows
/// whose file is gone are taken out of the bundle and listed as left
/// behind. Returns the rows kept.
fn attach_files(
    conn: &Connection,
    section: &TableSection,
    set: FileSet,
    v2_cache: &Path,
    left_behind: &mut Vec<LeftBehind>,
) -> Result<u64, ExportError> {
    match set {
        FileSet::HeldImports => attach_held_files(conn, section, v2_cache, left_behind),
        FileSet::PlaylistCovers | FileSet::Avatars | FileSet::ManagementBlobs => Err(bundle_error(
            format!("{} cannot hang off table rows", section.name),
        )),
    }
}

/// Where v2 kept one held file: its name under `<cache>/held` (v2 wrote
/// absolute paths from inside its own container), else the stored path.
fn held_file(v2_cache: &Path, stored: &str) -> Option<PathBuf> {
    let stored = PathBuf::from(stored);
    stored
        .file_name()
        .map(|name| v2_cache.join("held").join(name))
        .filter(|path| path.is_file())
        .or_else(|| stored.is_file().then_some(stored))
}

/// Read every carried held row's file into its `file` column.
fn attach_held_files(
    conn: &Connection,
    section: &TableSection,
    v2_cache: &Path,
    left_behind: &mut Vec<LeftBehind>,
) -> Result<u64, ExportError> {
    let name = section.name;
    let rows: Vec<(i64, String)> = {
        let mut stmt = conn
            .prepare(&format!("SELECT rowid, held_path FROM main.\"{name}\""))
            .map_err(bundle_error)?;
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(bundle_error)?
            .collect::<Result<_, _>>()
            .map_err(bundle_error)?
    };
    let mut kept = 0;
    let mut missing = 0;
    for (rowid, stored) in rows {
        let Some(path) = held_file(v2_cache, &stored) else {
            conn.execute(
                &format!("DELETE FROM main.\"{name}\" WHERE rowid = ?1"),
                params![rowid],
            )
            .map_err(bundle_error)?;
            missing += 1;
            continue;
        };
        let bytes = std::fs::read(&path).map_err(bundle_error)?;
        conn.execute(
            &format!("UPDATE main.\"{name}\" SET file = ?1 WHERE rowid = ?2"),
            params![bytes, rowid],
        )
        .map_err(bundle_error)?;
        kept += 1;
    }
    if missing > 0 {
        left_behind.push(LeftBehind {
            table: "held import files".to_owned(),
            rows: missing,
            reason: "the held file is missing from the v2 cache folder".to_owned(),
        });
    }
    Ok(kept)
}

/// Read the stored bytes of every blob ledger row already in the bundle
/// from v2's blob folder. A file that is missing, or whose bytes do not
/// hash to its name, stays out and is listed: the importer then refuses
/// to manage the track it belonged to rather than trust a bad original.
fn copy_management_blobs(
    conn: &Connection,
    v2_cache: &Path,
    left_behind: &mut Vec<LeftBehind>,
) -> Result<u64, ExportError> {
    let ledger = crate::export::sections::management::BLOBS.name;
    let hashes: Vec<String> = {
        let mut stmt = conn
            .prepare(&format!(
                "SELECT sha256 FROM main.\"{ledger}\" ORDER BY sha256"
            ))
            .map_err(bundle_error)?;
        stmt.query_map([], |row| row.get(0))
            .map_err(bundle_error)?
            .collect::<Result<_, _>>()
            .map_err(bundle_error)?
    };
    let root = v2_cache
        .join("library-management")
        .join("blobs")
        .join("objects");
    let mut copied = 0;
    let mut bad = 0;
    for sha256 in hashes {
        let valid = sha256.len() == 64 && sha256.bytes().all(|byte| byte.is_ascii_hexdigit());
        let path = root
            .join(sha256.get(..2).unwrap_or_default())
            .join(sha256.get(2..4).unwrap_or_default())
            .join(format!("{sha256}.blob"));
        let bytes = match std::fs::read(&path) {
            Ok(bytes) if valid => bytes,
            _ => {
                bad += 1;
                continue;
            }
        };
        if format!("{:x}", Sha256::digest(&bytes)) != sha256 {
            bad += 1;
            continue;
        }
        conn.execute(
            "INSERT INTO main.\"management_blob_bytes\" (sha256, bytes) VALUES (?1, ?2)",
            params![sha256, bytes],
        )
        .map_err(bundle_error)?;
        copied += 1;
    }
    if bad > 0 {
        left_behind.push(LeftBehind {
            table: "Library Management blob files".to_owned(),
            rows: bad,
            reason: "the stored file is missing from v2's blob folder or does not match \
                     its hash"
                .to_owned(),
        });
    }
    Ok(copied)
}

fn content_type_for(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    IMAGE_TYPES
        .iter()
        .find(|(known, _)| *known == ext)
        .map(|(_, content_type)| *content_type)
}

/// Seconds since the epoch of a file's last change, 0 when unknown.
fn modified_secs(path: &Path) -> f64 {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0.0, |elapsed| elapsed.as_secs_f64())
}

/// Read the cover of every carried playlist. v2 stored an absolute path
/// from inside its own container, so the file is looked up by name in
/// `<cache>/covers/playlists` first, and the stored path is the fallback.
fn copy_playlist_covers(
    conn: &Connection,
    v2_cache: &Path,
    left_behind: &mut Vec<LeftBehind>,
) -> Result<u64, ExportError> {
    if v2_columns(conn, "library_playlists")?.is_empty() {
        return Ok(0);
    }
    let mut stmt = conn
        .prepare(
            "SELECT s.id, s.cover_image_path FROM v2.library_playlists AS s \
             WHERE s.cover_image_path IS NOT NULL AND s.cover_image_path <> '' \
             AND s.id IN (SELECT id FROM main.\"playlist\") ORDER BY s.id",
        )
        .map_err(|error| db_error("library_playlists", error))?;
    let covers = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|error| db_error("library_playlists", error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| db_error("library_playlists", error))?;
    let cover_dir = v2_cache.join("covers").join("playlists");
    let mut copied = 0;
    let mut missing = 0;
    for (playlist_id, stored) in covers {
        let stored = PathBuf::from(stored);
        let found = stored
            .file_name()
            .map(|name| cover_dir.join(name))
            .filter(|path| path.is_file())
            .or_else(|| stored.is_file().then_some(stored));
        let Some(path) = found else {
            missing += 1;
            continue;
        };
        let content_type = content_type_for(&path).unwrap_or("image/jpeg");
        let bytes = std::fs::read(&path).map_err(bundle_error)?;
        conn.execute(
            "INSERT INTO main.\"playlist_cover\" (playlist_id, content_type, image, updated_at) \
             VALUES (?1, ?2, ?3, ?4)",
            params![playlist_id, content_type, bytes, modified_secs(&path)],
        )
        .map_err(bundle_error)?;
        copied += 1;
    }
    if missing > 0 {
        left_behind.push(LeftBehind {
            table: "playlist cover files".to_owned(),
            rows: missing,
            reason: "the cover file is missing from the v2 cache folder".to_owned(),
        });
    }
    Ok(copied)
}

/// Read `<cache>/avatars/{user_id}.{ext}` for every carried user. When a
/// user has more than one (an old one v2 failed to delete), the newest
/// wins, as v2 served it.
fn copy_avatars(conn: &Connection, v2_cache: &Path) -> Result<u64, ExportError> {
    let dir = v2_cache.join("avatars");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok(0);
    };
    let mut stmt = conn
        .prepare("SELECT id FROM v2.auth_users")
        .map_err(|error| db_error("auth_users", error))?;
    let users = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| db_error("auth_users", error))?
        .collect::<Result<HashSet<_>, _>>()
        .map_err(|error| db_error("auth_users", error))?;
    let mut newest: BTreeMap<String, (f64, PathBuf)> = BTreeMap::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(user_id) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        if !users.contains(user_id) || content_type_for(&path).is_none() || !path.is_file() {
            continue;
        }
        let changed = modified_secs(&path);
        let keep = newest.get(user_id).is_none_or(|(seen, _)| changed > *seen);
        if keep {
            newest.insert(user_id.to_owned(), (changed, path));
        }
    }
    let mut copied = 0;
    for (user_id, (_, path)) in newest {
        let ext = path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        let bytes = std::fs::read(&path).map_err(bundle_error)?;
        conn.execute(
            "INSERT INTO main.\"avatar\" (user_id, ext, image) VALUES (?1, ?2, ?3)",
            params![user_id, ext, bytes],
        )
        .map_err(bundle_error)?;
        copied += 1;
    }
    Ok(copied)
}

/// Lowercase hex SHA-256 and byte size of a file, read in chunks.
pub fn file_sha256(path: &Path) -> std::io::Result<(String, u64)> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 16];
    let mut total = 0u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        total += read as u64;
    }
    Ok((format!("{:x}", hasher.finalize()), total))
}
