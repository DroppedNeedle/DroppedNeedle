//! Avatar files: bundle bytes land as `<cache>/avatars/{user_id}.{ext}`,
//! the name v3's avatar store reads.
//!
//! A user who already has an avatar in v3 keeps it. Writes go through a
//! temporary file and a rename, so a crash never leaves half an image.

use std::path::{Path, PathBuf};

use rusqlite::Connection;

use super::{CarryError, SectionResult};
use crate::import::sql;

/// Extensions v3's avatar store serves.
const EXTENSIONS: &[&str] = &["jpg", "png", "webp", "gif"];

/// What to do with one avatar.
enum Decision {
    Write(PathBuf),
    Identical,
    Conflict,
    Invalid(&'static str),
}

/// User ids become file names: keep them to plain id characters.
fn plain_id(user_id: &str) -> bool {
    !user_id.is_empty()
        && user_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn decide(dir: &Path, user_id: &str, ext: &str, image: &[u8]) -> std::io::Result<Decision> {
    if !plain_id(user_id) {
        return Ok(Decision::Invalid(
            "the user id cannot be a file name; upload the picture again from the profile page",
        ));
    }
    if !EXTENSIONS.contains(&ext) {
        return Ok(Decision::Invalid(
            "v3 does not serve this image type; upload a PNG, JPEG, GIF or WebP picture \
             from the profile page",
        ));
    }
    let mut existing = None;
    for known in EXTENSIONS {
        let path = dir.join(format!("{user_id}.{known}"));
        if path.is_file() {
            existing = Some(path);
            break;
        }
    }
    match existing {
        None => Ok(Decision::Write(dir.join(format!("{user_id}.{ext}")))),
        Some(path) => {
            let same_name = path.extension().and_then(|found| found.to_str()) == Some(ext);
            if same_name && std::fs::read(&path)? == image {
                Ok(Decision::Identical)
            } else {
                Ok(Decision::Conflict)
            }
        }
    }
}

pub(crate) fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut tmp_name = path
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_default();
    tmp_name.push(".import-tmp");
    let tmp = path.with_file_name(tmp_name);
    let outcome = (|| {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if outcome.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    outcome
}

/// Decide and (on a real run) write every avatar in the bundle.
pub(crate) fn apply(
    conn: &Connection,
    cache_dir: Option<&Path>,
    dry_run: bool,
) -> Result<SectionResult, CarryError> {
    let rows = sql::all_rows(conn, sql::AVATARS, sql::AvatarRow::read)?;
    let mut result = SectionResult {
        rows: rows.len() as u64,
        ..SectionResult::default()
    };
    let Some(cache_dir) = cache_dir else {
        if !rows.is_empty() {
            result.counts.dropped_invalid += rows.len() as u64;
            result.note(
                String::new(),
                "dropped_invalid",
                "no v3 cache folder was given, so avatars cannot be written; run the import \
                 again with --cache-dir",
            );
        }
        return Ok(result);
    };
    let dir = cache_dir.join("avatars");
    for sql::AvatarRow {
        user_id,
        ext,
        image,
    } in rows
    {
        // v3 serves JPEG avatars under `.jpg`.
        let ext = if ext.eq_ignore_ascii_case("jpeg") {
            "jpg".to_owned()
        } else {
            ext.to_ascii_lowercase()
        };
        let decision = decide(&dir, &user_id, &ext, &image)
            .map_err(|error| CarryError::Io(error.to_string()))?;
        match decision {
            Decision::Identical => result.counts.skipped_identical += 1,
            Decision::Conflict => {
                result.counts.conflict_kept_existing += 1;
                result.note(
                    user_id,
                    "conflict_kept_existing",
                    "the user already has an avatar in v3; it is kept. Upload the v2 picture \
                     again from the profile page if you prefer it",
                );
            }
            Decision::Invalid(reason) => {
                result.counts.dropped_invalid += 1;
                result.note(user_id, "dropped_invalid", reason);
            }
            Decision::Write(path) => {
                result.counts.imported += 1;
                if dry_run {
                    continue;
                }
                write_atomically(&path, &image)
                    .map_err(|error| CarryError::Io(error.to_string()))?;
                result.written += 1;
            }
        }
    }
    Ok(result)
}
