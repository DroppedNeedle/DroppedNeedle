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

use sqlx::SqliteConnection;

use super::avatars::write_atomically;
use super::bundle::SCHEMA;
use super::{CarryError, SectionResult};
use crate::export::sections::{Column, TableSection};

/// The bundle column holding the file's bytes.
const FILE_COLUMN: &str = "file";

/// The reasons a v3 file is held for, kept as they are; the same closed
/// list the database upgrade (0085) maps by.
const KEPT_REASONS: &str = "('weak_match', 'no_artist_evidence', 'mostly_unmatched', \
     'tag_mismatch', 'fingerprint_mismatch', 'fingerprint_unconfirmed', 'album_version_only', \
     'no_release', 'no_tracks', 'version_lower_quality', 'no_better_than_yours', \
     'no_album_match', 'album_not_found', 'tags_disagree', 'not_chosen_edition', 'samples', \
     'too_many_files', 'interrupted', 'import_failed', 'held_by_earlier_version', \
     'target_occupied', 'upgrade_blocked', 'library_changed', 'library_unavailable', \
     'naming_script_failed', 'library_folder_missing', 'local_fault')";

/// Now, and 90 days, in milliseconds, as SQL.
const NOW_MS: &str = "CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)";
const KEEP_MS: i64 = 90 * 86_400_000;

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
pub(crate) async fn apply(
    conn: &mut SqliteConnection,
    section: &TableSection,
    present: &[&'static Column],
    cache_dir: Option<&Path>,
    dry_run: bool,
) -> Result<SectionResult, CarryError> {
    let name = section.name;
    let ids: Vec<i64> =
        sqlx::query_scalar(&format!("SELECT id FROM {SCHEMA}.\"{name}\" ORDER BY id"))
            .persistent(false)
            .fetch_all(&mut *conn)
            .await?;
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
    // A column the export left out reads as NULL.
    let col = |column: &'static str| {
        if has(column) {
            format!("h.\"{column}\"")
        } else {
            "NULL".to_owned()
        }
    };
    for id in ids {
        let (user_id, created_at, held_path, file): (String, f64, String, Option<Vec<u8>>) =
            sqlx::query_as(&format!(
                "SELECT user_id, created_at, held_path, \"{FILE_COLUMN}\" \
                 FROM {SCHEMA}.\"{name}\" WHERE id = ?1"
            ))
            .bind(id)
            .persistent(false)
            .fetch_one(&mut *conn)
            .await?;
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
        let (dir_for, stored) = (dir.clone(), held_path.clone());
        let (target, bytes) = tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(&dir_for)?;
            destination(&dir_for, id, &stored, &bytes).map(|target| (target, bytes))
        })
        .await
        .map_err(io)?
        .map_err(io)?;
        let target = match target {
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
        let existing: Option<(String, i64)> = sqlx::query_as(
            "SELECT e.user_id, f.created_at FROM main.acquire_held_files f \
             JOIN main.acquire_held_entries e ON e.id = f.entry_id \
             WHERE f.root = 'held' AND f.path = ?1",
        )
        .bind(&file_name)
        .fetch_optional(&mut *conn)
        .await?;
        if let Some((held_user, held_at)) = existing {
            if held_user == user_id && held_at == created_ms {
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
        let path = target.clone();
        tokio::task::spawn_blocking(move || {
            if path.is_file() {
                Ok(())
            } else {
                write_atomically(&path, &bytes)
            }
        })
        .await
        .map_err(io)?
        .map_err(io)?;
        // The entry of the file's song for a track download, else its
        // album, else its download, by the held list's one rule; made when
        // the user has none open. Download tasks are carried before this.
        let key = format!(
            "CASE WHEN COALESCE({rec}, '') != '' AND EXISTS (SELECT 1 \
                 FROM main.download_tasks t WHERE t.id = {task} \
                 AND t.download_type = 'track') THEN 'track:' || lower({rec}) \
             WHEN COALESCE({rg}, '') != '' THEN 'album:' || lower({rg}) \
             ELSE 'task:' || COALESCE(NULLIF({task}, ''), 'v2-held-' || h.id) END",
            rec = col("recording_mbid"),
            rg = col("release_group_mbid"),
            task = col("source_task_id"),
        );
        sqlx::query(&format!(
            "INSERT INTO main.acquire_held_entries (user_id, origin, group_key, \
                 release_group_mbid, recording_mbid, artist_name, album_title, state, \
                 created_at, expires_at) \
             SELECT h.user_id, 'download', {key}, lower(NULLIF({rg}, '')), \
                 CASE WHEN {key} LIKE 'track:%' THEN substr({key}, 7) END, {artist}, {album}, \
                 'held', ?2, MAX(?2, {NOW_MS}) + {KEEP_MS} \
             FROM {SCHEMA}.\"{name}\" h WHERE h.id = ?1 \
               AND NOT EXISTS (SELECT 1 FROM main.acquire_held_entries e \
                 WHERE e.user_id = h.user_id AND e.group_key = {key} \
                   AND e.state IN ('held', 'placing'))",
            rg = col("release_group_mbid"),
            artist = col("artist_name"),
            album = col("album_title"),
        ))
        .bind(id)
        .bind(created_ms)
        .persistent(false)
        .execute(&mut *conn)
        .await?;
        sqlx::query(&format!(
            "INSERT INTO main.acquire_held_files (entry_id, root, path, original_name, \
                 release_mbid, release_track_mbid, recording_mbid, disc_number, track_number, \
                 track_title, file_format, duration_seconds, expected_duration_seconds, \
                 evidence_title, evidence_artist, evidence_score, source, source_task_id, \
                 reason, reason_detail, state, created_at) \
             SELECT e.id, 'held', ?2, COALESCE(NULLIF({original}, ''), ?2), {release}, \
                 {release_track}, lower({recording}), {disc}, {track}, {title}, {format}, \
                 {duration}, {expected}, {evidence_title}, {evidence_artist}, {evidence_score}, \
                 COALESCE({source}, 'soulseek'), {task}, \
                 CASE WHEN h.reason LIKE 'management:%' THEN 'target_occupied' \
                     WHEN h.reason IN {KEPT_REASONS} THEN h.reason \
                     ELSE 'held_by_earlier_version' END, \
                 {detail}, 'held', ?3 \
             FROM {SCHEMA}.\"{name}\" h \
             JOIN main.acquire_held_entries e ON e.user_id = h.user_id \
               AND e.group_key = {key} AND e.state IN ('held', 'placing') \
             WHERE h.id = ?1",
            original = col("original_filename"),
            release = col("release_mbid"),
            release_track = col("release_track_mbid"),
            recording = col("recording_mbid"),
            disc = col("disc_number"),
            track = col("track_number"),
            title = col("track_title"),
            format = col("file_format"),
            duration = col("duration_seconds"),
            expected = col("expected_duration_seconds"),
            evidence_title = col("evidence_title"),
            evidence_artist = col("evidence_artist"),
            evidence_score = col("evidence_score"),
            source = col("source"),
            task = col("source_task_id"),
            detail = col("reason_detail"),
        ))
        .bind(id)
        .bind(&file_name)
        .bind(created_ms)
        .persistent(false)
        .execute(&mut *conn)
        .await?;
        sqlx::query(&format!(
            "UPDATE main.acquire_held_entries \
             SET expires_at = MAX(expires_at, MAX(?2, {NOW_MS}) + {KEEP_MS}) \
             WHERE id = (SELECT entry_id FROM main.acquire_held_files \
                 WHERE root = 'held' AND path = ?1)",
        ))
        .bind(&file_name)
        .bind(created_ms)
        .execute(&mut *conn)
        .await?;
        result.written += 1;
    }
    Ok(result)
}
