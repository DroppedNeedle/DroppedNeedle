//! Held downloads: the file lands in `<cache>/held` first, then it joins
//! the held list, in the entry of its song for a track download, else of
//! its album, else of its download when v2 never matched one.
//!
//! The file keeps v2's name (v2 already made it unique). If v3 holds a
//! different file under that name, the carried one is named after its row
//! id instead; the same bytes under the same name are reused. A crash
//! between the file and the row leaves only an unreferenced file, which a
//! rerun reuses.
//!
//! v2's times in seconds become milliseconds. A carried entry expires no
//! sooner than 90 days after the carry, so nothing is removed before it has
//! been shown with its date. v2's library-side holds (`management:*`) read
//! as a taken destination, and a spelling this release does not hold for
//! as held by an earlier version.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension as _, params};

use super::avatars::write_atomically;
use super::{CarryError, SectionResult};
use crate::export::sections::Column;
use super::sql;

/// Why a held file has nowhere to land, as the plain text the report shows.
const NO_USABLE_NAME: &str = "the held file has no usable name; import it by hand from \
                              V2_ROOT/cache/held";
const BOTH_NAMES_TAKEN: &str = "V3_ROOT/cache/held already has different files under both \
                                the v2 name and the v2-held-<id> name, so this one was not \
                                copied; move those files aside and run the import again, or \
                                import it by hand from V2_ROOT/cache/held";

/// Where one held file lands, or why it cannot.
fn destination(
    dir: &Path,
    id: i64,
    stored: &str,
    bytes: &[u8],
) -> std::io::Result<Result<PathBuf, &'static str>> {
    let Some(name) = Path::new(stored)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty() && !name.starts_with('.'))
    else {
        return Ok(Err(NO_USABLE_NAME));
    };
    for candidate in [name.to_owned(), format!("v2-held-{id}-{name}")] {
        let path = dir.join(&candidate);
        match std::fs::read(&path) {
            Ok(existing) if existing == bytes => return Ok(Ok(path)),
            Ok(_) => continue,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Ok(path)),
            Err(error) => return Err(error),
        }
    }
    Ok(Err(BOTH_NAMES_TAKEN))
}

fn io(error: impl std::fmt::Display) -> CarryError {
    CarryError::Io(error.to_string())
}

/// Carry every held row of the bundle with its file.
pub(crate) fn apply(
    conn: &Connection,
    present: &[&'static Column],
    cache_dir: Option<&Path>,
    dry_run: bool,
) -> Result<SectionResult, CarryError> {
    let ids: Vec<i64> = {
        let mut statement = conn.prepare(sql::HELD_IDS.text())?;
        let ids = statement.query_map([], |row| row.get(0))?;
        ids.collect::<rusqlite::Result<_>>()?
    };
    let mut result = SectionResult::default();
    let Some(cache_dir) = cache_dir else {
        // Nothing is read or written without a cache folder, so no rows count.
        if !ids.is_empty() {
            result.counts.dropped_invalid += ids.len() as u64;
            result.note(
                String::new(),
                "dropped_invalid",
                "no v3 cache folder was given, so held files cannot be written; run the \
                 import again with --cache-dir",
            );
        }
        return Ok(result);
    };
    result.rows = ids.len() as u64;
    let dir = cache_dir.join("held");
    let has = |column: &str| present.iter().any(|known| known.name == column);
    for id in ids {
        let sql::HeldRow {
            user_id,
            created_at,
            held_path,
            file,
        } = conn.query_row(sql::HELD_ROW.text(), params![id], sql::HeldRow::read)?;
        let created_ms = (created_at * 1000.0) as i64;
        let Some(bytes) = file else {
            result.counts.dropped_invalid += 1;
            result.note(
                id.to_string(),
                "dropped_invalid",
                "the held file is missing; restore V2_ROOT/cache/held from your backup and \
                 export again, or request the download again in v3",
            );
            continue;
        };
        std::fs::create_dir_all(&dir).map_err(io)?;
        let target = match destination(&dir, id, &held_path, &bytes).map_err(io)? {
            Ok(target) => target,
            Err(reason) => {
                result.counts.dropped_invalid += 1;
                result.note(id.to_string(), "dropped_invalid", reason);
                continue;
            }
        };
        let file_name = target
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        // The same file carried before: it is in the held list already.
        let existing = conn
            .query_row(
                sql::HELD_EXISTING.text(),
                params![file_name],
                sql::HeldExisting::read,
            )
            .optional()?;
        if let Some(held) = existing {
            if held.user_id == user_id && held.created_at == created_ms {
                result.counts.skipped_identical += 1;
            } else {
                result.counts.conflict_kept_existing += 1;
                result.note(
                    id.to_string(),
                    "conflict_kept_existing",
                    "v3 already holds a different download under this file name and keeps \
                     it; the v2 file stays in V2_ROOT/cache/held if you want to import it by \
                     hand",
                );
            }
            continue;
        }
        result.counts.imported += 1;
        if dry_run {
            continue;
        }
        if !target.is_file() {
            write_atomically(&target, &bytes).map_err(io)?;
        }
        // The entry is made when the user has none open. Download tasks are
        // carried before this.
        conn.execute(&sql::held_entry_insert(&has), params![id, created_ms])?;
        conn.execute(
            &sql::held_file_insert(&has),
            params![id, file_name, created_ms],
        )?;
        conn.execute(
            sql::HELD_EXTEND_ENTRY.text(),
            params![file_name, created_ms],
        )?;
        result.written += 1;
    }
    Ok(result)
}
