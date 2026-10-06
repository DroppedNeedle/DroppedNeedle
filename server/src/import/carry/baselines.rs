//! The baseline step: every carried v2 original-file baseline becomes the
//! v3 baseline "restore original" reads.
//!
//! It runs after the catalog, v2's baseline rows and the blob bytes have
//! landed. Each baseline's tag snapshot is translated
//! ([`crate::library::publish::v2_baseline`]) and stored the way v3's
//! publisher stores its own: the before-state blob, the baseline row, and
//! a blob reference. v2's own snapshot and the sidecar and artwork blobs
//! it lists stay referenced too, so nothing a baseline held is ever swept.
//!
//! A track that already has a v3 baseline keeps it (baselines never
//! change). A baseline that cannot be translated is listed with its reason
//! and gets no v3 baseline; v3 then refuses to manage that track, because
//! its first managed write would otherwise record the file as v2 left it
//! as the "original".

use sqlx::SqliteConnection;

use super::bundle::SCHEMA;
use super::{CarryError, SectionResult};
use crate::export::sections::library::TRACKS;
use crate::export::sections::management::{BASELINES, BLOB_BYTES};
use crate::library::publish::snapshots::sha256_hex;
use crate::library::publish::v2_baseline::translate;

/// Report entity and section marker name.
pub(crate) const ENTITY: &str = "original_baseline";

/// One carried v2 baseline row.
#[derive(sqlx::FromRow)]
struct Candidate {
    local_track_id: String,
    original_root_id: String,
    original_relative_path: String,
    format: String,
    semantic_snapshot_blob_sha256: String,
    created_at: f64,
    ancillary_snapshot_json: Option<String>,
}

async fn bundle_has(conn: &mut SqliteConnection, table: &str) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(&format!(
        "SELECT EXISTS(SELECT 1 FROM {SCHEMA}.sqlite_master WHERE type = 'table' AND name = ?1)"
    ))
    .bind(table)
    .persistent(false)
    .fetch_one(&mut *conn)
    .await
}

/// Blob hashes an ancillary snapshot lists.
fn ancillary_blobs(json: Option<&str>) -> Vec<String> {
    let Some(entries) = json
        .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
        .and_then(|value| value.as_array().cloned())
    else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|entry| entry.get("blob_sha256").and_then(|sha| sha.as_str()))
        .map(str::to_owned)
        .collect()
}

/// Translate and store every carried baseline. On a dry run, count only.
pub(crate) async fn apply(
    conn: &mut SqliteConnection,
    dry_run: bool,
) -> Result<SectionResult, CarryError> {
    let (baselines, bytes) = (BASELINES.name, BLOB_BYTES.name);
    let mut result = SectionResult::default();
    if !bundle_has(conn, baselines).await? || !bundle_has(conn, bytes).await? {
        return Ok(result);
    }
    let candidates: Vec<Candidate> = sqlx::query_as(&format!(
        "SELECT local_track_id, original_root_id, original_relative_path, format, \
         semantic_snapshot_blob_sha256, CAST(created_at AS REAL) AS created_at, \
         ancillary_snapshot_json FROM {SCHEMA}.\"{baselines}\" ORDER BY id"
    ))
    .persistent(false)
    .fetch_all(&mut *conn)
    .await?;
    result.rows = candidates.len() as u64;
    let track_landed = if dry_run {
        format!(
            "EXISTS (SELECT 1 FROM main.local_tracks WHERE id = ?1) \
             OR EXISTS (SELECT 1 FROM {SCHEMA}.\"{}\" WHERE id = ?1)",
            TRACKS.name
        )
    } else {
        "EXISTS (SELECT 1 FROM main.library_management_baselines WHERE local_track_id = ?1)"
            .to_owned()
    };
    for candidate in candidates {
        let track = candidate.local_track_id.clone();
        let landed: bool = sqlx::query_scalar(&format!("SELECT {track_landed}"))
            .bind(&track)
            .persistent(false)
            .fetch_one(&mut *conn)
            .await?;
        if !landed {
            result.counts.dropped_invalid += 1;
            result.note(track, "dropped_invalid", "its track did not land in v3");
            continue;
        }
        let snapshot: Option<Vec<u8>> = sqlx::query_scalar(&format!(
            "SELECT bytes FROM {SCHEMA}.\"{bytes}\" WHERE sha256 = ?1"
        ))
        .bind(&candidate.semantic_snapshot_blob_sha256)
        .persistent(false)
        .fetch_optional(&mut *conn)
        .await?;
        let Some(snapshot) = snapshot else {
            result.counts.dropped_invalid += 1;
            result.note(
                track,
                "dropped_invalid",
                "its tag snapshot file was missing or damaged in v2's blob folder; \
                 v3 will not manage this track",
            );
            continue;
        };
        let before = match translate(
            &snapshot,
            &candidate.format,
            &candidate.original_root_id,
            &candidate.original_relative_path,
        ) {
            Ok(before) => before,
            Err(error) => {
                result.counts.dropped_invalid += 1;
                result.note(
                    track,
                    "dropped_invalid",
                    format!("{error}; v3 will not manage this track"),
                );
                continue;
            }
        };
        let blob =
            serde_json::to_vec(&before).map_err(|error| CarryError::Io(error.to_string()))?;
        let blob_sha = sha256_hex(&blob);
        let existing: Option<String> = sqlx::query_scalar(
            "SELECT blob_sha256 FROM main.library_publish_baselines WHERE track_id = ?1",
        )
        .bind(&track)
        .fetch_optional(&mut *conn)
        .await?;
        match existing {
            Some(kept) if kept == blob_sha => {
                result.counts.skipped_identical += 1;
                continue;
            }
            Some(_) => {
                result.counts.conflict_kept_existing += 1;
                result.note(
                    track,
                    "conflict_kept_existing",
                    "v3 already recorded an original for this track and keeps it; it may be \
                     the state v2 had already changed",
                );
                continue;
            }
            None => {}
        }
        result.counts.imported += 1;
        if dry_run {
            continue;
        }
        sqlx::query(
            "INSERT OR IGNORE INTO main.library_publish_blobs (sha256, bytes) VALUES (?1, ?2)",
        )
        .bind(&blob_sha)
        .bind(&blob)
        .execute(&mut *conn)
        .await?;
        sqlx::query(
            "INSERT INTO main.library_publish_baselines \
             (track_id, blob_sha256, original_root, original_rel, created_day) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .bind(&track)
        .bind(&blob_sha)
        .bind(&candidate.original_root_id)
        .bind(&candidate.original_relative_path)
        .bind((candidate.created_at / 86_400.0).floor() as i64)
        .execute(&mut *conn)
        .await?;
        let mut owned = vec![(blob_sha, "baseline")];
        owned.push((
            candidate.semantic_snapshot_blob_sha256.clone(),
            "v2_baseline",
        ));
        for sha in ancillary_blobs(candidate.ancillary_snapshot_json.as_deref()) {
            owned.push((sha, "v2_baseline"));
        }
        for (sha, kind) in owned {
            sqlx::query(
                "INSERT OR IGNORE INTO main.library_publish_blob_refs (sha256, owner_kind, owner_id) \
                 SELECT ?1, ?2, ?3 WHERE EXISTS \
                 (SELECT 1 FROM main.library_publish_blobs WHERE sha256 = ?1)",
            )
            .bind(&sha)
            .bind(kind)
            .bind(&track)
            .execute(&mut *conn)
            .await?;
        }
        result.written += 1;
    }
    Ok(result)
}
