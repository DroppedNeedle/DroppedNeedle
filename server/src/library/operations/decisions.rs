//! An administrator's decisions on the catalog: settling a
//! re-identification (exact release, custom edition, or leave unmanaged)
//! and undoing an automatic edition. Each runs in the caller's
//! transaction, checks the revisions the administrator was shown, writes
//! the identities as curator decisions, records an audit row in
//! `library_catalog_actions`, and moves the catalog revision.

use rusqlite::{Connection, OptionalExtension as _, Transaction, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::models::{
    CandidateChoice, DecisionMode, OperationError, OperationJob, OperationState,
    ReidentificationCandidate, UndoOutcome,
};
use super::reasons;
use super::store::{PROVIDER, Snapshot, job, job_after, matches_snapshot, snapshot};
use crate::library::identify::models::{
    AlbumIdentity, EvidenceClass, PriorAlbumIdentity, PriorTrackIdentity,
};
use crate::library::identify::sqlite::{album_identity_row, track_identity_rows};

/// Settle a re-identification with the administrator's choice: seal the
/// exact release, seal a custom edition, or leave the album unmanaged.
pub fn select_candidate(
    tx: &Transaction<'_>,
    job_id: &str,
    choice: &CandidateChoice,
    actor: &str,
    now: f64,
) -> Result<OperationJob, OperationError> {
    let current = job(tx, job_id)?
        .filter(OperationJob::is_reidentification)
        .ok_or(OperationError::NotFound(reasons::OPERATION_NOT_FOUND))?;
    let snap =
        snapshot(tx, job_id)?.ok_or(OperationError::NotFound(reasons::OPERATION_NOT_FOUND))?;
    let open = current.state == OperationState::Ready
        || (current.state == OperationState::Succeeded
            && choice.decision_mode == DecisionMode::LeaveUnmanaged);
    if !open || current.row_revision != choice.expected_row_revision {
        return Err(OperationError::Conflict(reasons::CANDIDATES_CHANGED));
    }
    if !matches_snapshot(tx, &snap)? {
        return Err(OperationError::Conflict(
            reasons::ALBUM_CHANGED_SINCE_EVALUATION,
        ));
    }
    let mut evaluation = snap.evaluation.clone().unwrap_or_default();
    let candidate = if choice.candidate_key.is_empty() {
        None
    } else {
        Some(
            evaluation
                .candidates
                .iter()
                .find(|candidate| candidate.candidate_key == choice.candidate_key)
                .cloned()
                .ok_or(OperationError::Conflict(reasons::CANDIDATE_GONE))?,
        )
    };
    let album_id = snap.local_album_id.clone();
    let before = album_identity(tx, &album_id)?;
    let (terminal, reason, selected) = match choice.decision_mode {
        DecisionMode::LeaveUnmanaged => {
            leave_unmanaged(
                tx,
                &album_id,
                candidate.as_ref(),
                before.as_ref(),
                actor,
                now,
            )?;
            evaluation.outcome = "leave_unmanaged".into();
            ("LEFT_UNMANAGED", "MANAGEMENT_EXCLUDED", None)
        }
        DecisionMode::CustomEdition => {
            let candidate = candidate.ok_or(OperationError::Conflict(reasons::CANDIDATE_GONE))?;
            if !choice.confirmation {
                return Err(OperationError::Invalid(
                    reasons::CUSTOM_EDITION_NEEDS_CONFIRMATION,
                ));
            }
            let manifest = seal_custom_edition(tx, &snap, &candidate, before.as_ref(), actor, now)?;
            evaluation.outcome = "custom_edition".into();
            evaluation.custom_manifest_id = Some(manifest);
            (
                "CUSTOM_EDITION_SEALED",
                "CUSTOM_EDITION_SEALED",
                Some(candidate.candidate_key),
            )
        }
        DecisionMode::ExactRelease => {
            let candidate = candidate.ok_or(OperationError::Conflict(reasons::CANDIDATE_GONE))?;
            let needs_confirmation =
                !candidate.automatic_safe || snap.requested_release_mbid.is_some();
            if needs_confirmation && !choice.confirmation {
                return Err(OperationError::Invalid(reasons::CONFIRMATION_REQUIRED));
            }
            seal_exact_release(tx, &album_id, &candidate, actor, now)?;
            evaluation.outcome = "identified".into();
            (
                "IDENTIFIED",
                "EXPLICIT_CANDIDATE_ACCEPTED",
                Some(candidate.candidate_key),
            )
        }
    };
    evaluation.selected_candidate_key = selected.clone();
    settle_reviews(
        tx,
        &album_id,
        if choice.decision_mode == DecisionMode::LeaveUnmanaged {
            "rejected"
        } else {
            "approved"
        },
        selected.as_deref(),
        actor,
    )?;
    let stored = serde_json::to_string(&evaluation)
        .map_err(|error| OperationError::Store(error.to_string()))?;
    tx.execute(
        "UPDATE library_reidentification_snapshots SET selected_candidate_key = ?2, \
         result_json = ?3 WHERE job_id = ?1",
        params![job_id, selected, stored],
    )?;
    let changed = tx.execute(
        "UPDATE library_operation_jobs SET state = 'succeeded', terminal_code = ?2, \
         terminal_at = ?3, updated_at = ?3, row_revision = row_revision + 1, \
         event_revision = event_revision + 1 WHERE id = ?1 AND row_revision = ?4",
        params![job_id, terminal, now, choice.expected_row_revision],
    )?;
    if changed != 1 {
        return Err(OperationError::Conflict(reasons::CANDIDATES_CHANGED));
    }
    record_action(
        tx,
        CatalogAction {
            actor,
            kind: "explicit_reidentification",
            album_id: &album_id,
            job_id: Some(job_id),
            before: serde_json::to_value(&before).unwrap_or_default(),
            after: serde_json::to_value(&evaluation).unwrap_or_default(),
            reason,
        },
        now,
    )?;
    bump_catalog(tx)?;
    job_after(tx, job_id)
}

fn album_identity(conn: &Connection, album_id: &str) -> rusqlite::Result<Option<AlbumIdentity>> {
    conn.query_row(
        "SELECT release_group_mbid, release_mbid, decision_source, row_revision \
         FROM local_album_external_identities WHERE local_album_id = ?1 AND provider = ?2",
        params![album_id, PROVIDER],
        |row| {
            Ok(AlbumIdentity {
                local_album_id: album_id.to_owned(),
                provider: PROVIDER.to_owned(),
                release_group_mbid: Some(row.get(0)?),
                release_mbid: row.get(1)?,
                decision_source: decision_from(&row.get::<_, String>(2)?),
                row_revision: row.get::<_, i64>(3)?.max(1) as u64,
            })
        },
    )
    .optional()
}

fn decision_from(raw: &str) -> crate::library::identify::models::DecisionSource {
    use crate::library::identify::models::DecisionSource;
    match raw {
        "manual" => DecisionSource::Manual,
        "legacy_import" => DecisionSource::LegacyImport,
        _ => DecisionSource::Automatic,
    }
}

fn is_mbid(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok()
}

/// Write the album's identity as a curator decision. Row revisions only
/// move forward.
fn seal_album(
    tx: &Transaction<'_>,
    album_id: &str,
    release_group: &str,
    release: Option<&str>,
    actor: &str,
    now: f64,
) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO local_album_external_identities (local_album_id, provider, \
         release_group_mbid, release_mbid, decision_source, selected_by_user_id, \
         selected_at, row_revision) VALUES (?1, ?2, ?3, ?4, 'manual', ?5, ?6, 1) \
         ON CONFLICT (local_album_id, provider) DO UPDATE SET \
         release_group_mbid = excluded.release_group_mbid, \
         release_mbid = excluded.release_mbid, decision_source = 'manual', \
         selected_by_user_id = excluded.selected_by_user_id, \
         selected_at = excluded.selected_at, row_revision = row_revision + 1",
        params![album_id, PROVIDER, release_group, release, actor, now],
    )?;
    Ok(())
}

/// Accept the candidate's exact release: every indexed file must map to
/// its own release track.
fn seal_exact_release(
    tx: &Transaction<'_>,
    album_id: &str,
    candidate: &ReidentificationCandidate,
    actor: &str,
    now: f64,
) -> Result<(), OperationError> {
    let indexed = indexed_track_ids(tx, album_id)?;
    let release = candidate.evidence.release_mbid.clone();
    let mapping = exact_mapping(&indexed, candidate).filter(|_| release.is_some());
    let Some(mapping) = mapping else {
        return Err(OperationError::Invalid(
            reasons::EXACT_RELEASE_MAPPING_INCOMPLETE,
        ));
    };
    seal_album(
        tx,
        album_id,
        &candidate.evidence.release_group_mbid,
        release.as_deref(),
        actor,
        now,
    )?;
    for track in mapping {
        upsert_track_identity(
            tx,
            &TrackRow {
                local_track_id: &track.local_track_id,
                recording_mbid: &track.recording_mbid,
                release_mbid: release.as_deref(),
                release_track_mbid: Some(&track.release_track_mbid),
                medium_position: Some(track.disc_number),
                release_track_position: Some(track.position),
                decision_source: "manual",
            },
            now,
        )?;
    }
    tx.execute(
        "DELETE FROM library_custom_edition_active WHERE local_album_id = ?1",
        params![album_id],
    )?;
    tx.execute(
        "DELETE FROM library_management_exclusions WHERE local_album_id = ?1",
        params![album_id],
    )?;
    Ok(())
}

/// One file's place on the accepted release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MappedTrack {
    pub local_track_id: String,
    pub recording_mbid: String,
    pub release_track_mbid: String,
    pub disc_number: u32,
    pub position: u32,
}

/// Every indexed file mapped to its own release track, or `None` (v2
/// `_complete_track_identity_mapping`).
pub fn exact_mapping(
    indexed: &[String],
    candidate: &ReidentificationCandidate,
) -> Option<Vec<MappedTrack>> {
    let mut mapped: Vec<MappedTrack> = Vec::new();
    for evidence in &candidate.evidence.track_evidence {
        if evidence.classification == EvidenceClass::Contradictory {
            return None;
        }
        let place = candidate
            .tracks
            .iter()
            .find(|track| track.local_track_id == evidence.local_track_id)?;
        let track = MappedTrack {
            local_track_id: evidence.local_track_id.clone(),
            recording_mbid: evidence.recording_mbid.clone()?,
            release_track_mbid: evidence.release_track_mbid.clone()?,
            disc_number: place.disc_number.filter(|disc| *disc > 0)?,
            position: place.position.filter(|position| *position > 0)?,
        };
        if mapped.iter().any(|seen| {
            seen.local_track_id == track.local_track_id
                || seen.release_track_mbid == track.release_track_mbid
        }) {
            return None;
        }
        mapped.push(track);
    }
    let mut mapped_ids: Vec<&str> = mapped.iter().map(|t| t.local_track_id.as_str()).collect();
    let mut wanted: Vec<&str> = indexed.iter().map(String::as_str).collect();
    mapped_ids.sort_unstable();
    wanted.sort_unstable();
    if mapped_ids != wanted {
        return None;
    }
    mapped.sort_by_key(|track| indexed.iter().position(|id| *id == track.local_track_id));
    Some(mapped)
}

fn indexed_track_ids(conn: &Connection, album_id: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT id FROM local_tracks WHERE local_album_id = ?1 AND availability = 'indexed' \
         ORDER BY id",
    )?;
    stmt.query_map(params![album_id], |row| row.get(0))?
        .collect()
}

/// One track identity row to write.
struct TrackRow<'a> {
    local_track_id: &'a str,
    recording_mbid: &'a str,
    release_mbid: Option<&'a str>,
    release_track_mbid: Option<&'a str>,
    medium_position: Option<u32>,
    release_track_position: Option<u32>,
    decision_source: &'a str,
}

/// Write one track identity; an existing row takes the next revision.
fn upsert_track_identity(
    tx: &Transaction<'_>,
    row: &TrackRow<'_>,
    now: f64,
) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO local_track_external_identities (local_track_id, provider, \
         recording_mbid, release_mbid, release_track_mbid, medium_position, \
         release_track_position, decision_source, selected_at, row_revision) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1) \
         ON CONFLICT (local_track_id, provider) DO UPDATE SET \
         recording_mbid = excluded.recording_mbid, release_mbid = excluded.release_mbid, \
         release_track_mbid = excluded.release_track_mbid, \
         medium_position = excluded.medium_position, \
         release_track_position = excluded.release_track_position, \
         decision_source = excluded.decision_source, selected_at = excluded.selected_at, \
         row_revision = row_revision + 1",
        params![
            row.local_track_id,
            PROVIDER,
            row.recording_mbid,
            row.release_mbid,
            row.release_track_mbid,
            row.medium_position,
            row.release_track_position,
            row.decision_source,
            now,
        ],
    )?;
    Ok(())
}

/// Keep the album out of Library Management. An existing identity keeps
/// its release group but loses the exact edition; without one, the
/// candidate's release group is kept when its names do not conflict.
fn leave_unmanaged(
    tx: &Transaction<'_>,
    album_id: &str,
    candidate: Option<&ReidentificationCandidate>,
    before: Option<&AlbumIdentity>,
    actor: &str,
    now: f64,
) -> rusqlite::Result<()> {
    if before.is_some() {
        tx.execute(
            "UPDATE local_album_external_identities SET release_mbid = NULL, \
             decision_source = 'manual', selected_by_user_id = ?2, selected_at = ?3, \
             row_revision = row_revision + 1 WHERE local_album_id = ?1 AND provider = ?4",
            params![album_id, actor, now, PROVIDER],
        )?;
    } else if let Some(candidate) = candidate.filter(|candidate| names_hold(candidate)) {
        seal_album(
            tx,
            album_id,
            &candidate.evidence.release_group_mbid,
            None,
            actor,
            now,
        )?;
    }
    tx.execute(
        "UPDATE local_track_external_identities SET release_mbid = NULL, \
         release_track_mbid = NULL, medium_position = NULL, release_track_position = NULL, \
         decision_source = 'manual', selected_at = ?2, row_revision = row_revision + 1 \
         WHERE provider = ?3 AND local_track_id IN (SELECT id FROM local_tracks \
         WHERE local_album_id = ?1 AND availability = 'indexed')",
        params![album_id, now, PROVIDER],
    )?;
    tx.execute(
        "INSERT INTO library_management_exclusions (local_album_id, reason, \
         excluded_by_user_id, excluded_at, row_revision) \
         VALUES (?1, 'administrator_choice', ?2, ?3, 1) \
         ON CONFLICT (local_album_id) DO UPDATE SET reason = excluded.reason, \
         excluded_by_user_id = excluded.excluded_by_user_id, \
         excluded_at = excluded.excluded_at, row_revision = row_revision + 1",
        params![album_id, actor, now],
    )?;
    tx.execute(
        "DELETE FROM library_custom_edition_active WHERE local_album_id = ?1",
        params![album_id],
    )?;
    Ok(())
}

/// The candidate's release group is a real MBID and its album title and
/// artist do not contradict the files.
fn names_hold(candidate: &ReidentificationCandidate) -> bool {
    is_mbid(&candidate.evidence.release_group_mbid)
        && candidate.album_title_classification != EvidenceClass::Contradictory
        && candidate.album_artist_classification != EvidenceClass::Contradictory
}

/// The album and file metadata a custom edition manifest freezes.
#[derive(Serialize)]
struct ManifestAlbum {
    album_artist_sort_name: Option<String>,
    year: Option<i64>,
    original_release_date: Option<String>,
    primary_genre: Option<String>,
    is_compilation: bool,
}

#[derive(Serialize, Deserialize)]
struct ManifestTrackMeta {
    album_artist_name: Option<String>,
    year: Option<i64>,
    genre: Option<String>,
    title_sort: Option<String>,
    artist_sort: Option<String>,
    album_sort: Option<String>,
    album_artist_sort: Option<String>,
    disc_subtitle: Option<String>,
    is_compilation: bool,
}

/// One indexed file as the manifest records it.
struct ManifestTrack {
    id: String,
    row_revision: i64,
    stat_revision: String,
    tag_revision: String,
    title: String,
    artist_name: String,
    album_title: String,
    disc_number: i64,
    track_number: i64,
    artist_mbid: Option<String>,
    file_format: String,
    duration_seconds: Option<f64>,
    meta: ManifestTrackMeta,
}

/// Seal the files as they are under the candidate's release group: a
/// manual release-group identity plus a versioned manifest of every file.
/// Returns the manifest id.
fn seal_custom_edition(
    tx: &Transaction<'_>,
    snap: &Snapshot,
    candidate: &ReidentificationCandidate,
    before: Option<&AlbumIdentity>,
    actor: &str,
    now: f64,
) -> Result<String, OperationError> {
    let album_id = snap.local_album_id.as_str();
    if !names_hold(candidate) {
        return Err(OperationError::Invalid(
            reasons::CUSTOM_EDITION_NAMES_CONFLICT,
        ));
    }
    if before.is_some_and(|identity| {
        identity.release_group_mbid.as_deref().is_some_and(|group| {
            !group.eq_ignore_ascii_case(&candidate.evidence.release_group_mbid)
        })
    }) {
        return Err(OperationError::Invalid(
            reasons::CUSTOM_EDITION_GROUP_CONFLICT,
        ));
    }
    let (title, album_artist, artist_mbid, album_meta, album_revision): (
        String,
        String,
        Option<String>,
        ManifestAlbum,
        i64,
    ) = tx.query_row(
        "SELECT a.title, COALESCE(a.album_artist_name, ''), i.provider_artist_id, \
         a.album_artist_sort_name, a.year, a.original_release_date, a.primary_genre, \
         a.is_compilation, a.row_revision FROM local_albums a \
         LEFT JOIN local_artist_external_identities i \
         ON i.local_artist_id = a.album_artist_id AND i.provider = ?2 WHERE a.id = ?1",
        params![album_id, PROVIDER],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                ManifestAlbum {
                    album_artist_sort_name: row.get(3)?,
                    year: row.get(4)?,
                    original_release_date: row.get(5)?,
                    primary_genre: row.get(6)?,
                    is_compilation: row.get::<_, i64>(7)? != 0,
                },
                row.get(8)?,
            ))
        },
    )?;
    if title.trim().is_empty() || album_artist.trim().is_empty() {
        return Err(OperationError::Invalid(
            reasons::CUSTOM_EDITION_NAMES_MISSING,
        ));
    }
    if let (Some(known), Some(offered)) = (artist_mbid.as_deref(), candidate.artist_mbid.as_deref())
        && !known.eq_ignore_ascii_case(offered)
    {
        return Err(OperationError::Invalid(
            reasons::CUSTOM_EDITION_ARTIST_CONFLICT,
        ));
    }
    let tracks = manifest_tracks(tx, album_id)?;
    let mut positions: Vec<(i64, i64)> = tracks
        .iter()
        .map(|track| (track.disc_number, track.track_number))
        .collect();
    positions.sort_unstable();
    let unique = positions.windows(2).all(|pair| pair[0] != pair[1]);
    if tracks.is_empty()
        || !unique
        || positions
            .iter()
            .any(|(disc, track)| *disc < 1 || *track < 1)
    {
        return Err(OperationError::Invalid(reasons::CUSTOM_EDITION_POSITIONS));
    }

    seal_album(
        tx,
        album_id,
        &candidate.evidence.release_group_mbid,
        None,
        actor,
        now,
    )?;
    // Supported recordings from the candidate; otherwise a file keeps the
    // recording it already had, without an edition.
    for track in &tracks {
        let offered = candidate
            .evidence
            .track_evidence
            .iter()
            .find(|evidence| {
                evidence.local_track_id == track.id
                    && evidence.classification == EvidenceClass::Supported
            })
            .and_then(|evidence| evidence.recording_mbid.clone());
        let kept: Option<String> = tx
            .query_row(
                "SELECT recording_mbid FROM local_track_external_identities \
                 WHERE local_track_id = ?1 AND provider = ?2",
                params![track.id, PROVIDER],
                |row| row.get(0),
            )
            .optional()?;
        match offered.or(kept) {
            Some(recording) => {
                upsert_track_identity(
                    tx,
                    &TrackRow {
                        local_track_id: &track.id,
                        recording_mbid: &recording,
                        release_mbid: None,
                        release_track_mbid: None,
                        medium_position: None,
                        release_track_position: None,
                        decision_source: "manual",
                    },
                    now,
                )?;
            }
            None => {
                tx.execute(
                    "DELETE FROM local_track_external_identities \
                     WHERE local_track_id = ?1 AND provider = ?2",
                    params![track.id, PROVIDER],
                )?;
            }
        }
    }

    let identity_revision: i64 = tx.query_row(
        "SELECT row_revision FROM local_album_external_identities \
         WHERE local_album_id = ?1 AND provider = ?2",
        params![album_id, PROVIDER],
        |row| row.get(0),
    )?;
    let version: i64 = tx.query_row(
        "SELECT COALESCE(MAX(version), 0) + 1 FROM library_custom_edition_manifests \
         WHERE local_album_id = ?1",
        params![album_id],
        |row| row.get(0),
    )?;
    let manifest_id = uuid::Uuid::new_v4().to_string();
    let album_json = serde_json::to_string(&album_meta)
        .map_err(|error| OperationError::Store(error.to_string()))?;
    let mut hasher = Sha256::new();
    hasher.update(
        format!(
            "{album_id}\0{version}\0{}\0{title}\0{album_artist}\0{album_json}\0{}\n",
            candidate.evidence.release_group_mbid, snap.expected_input_revision
        )
        .as_bytes(),
    );
    for track in &tracks {
        hasher.update(
            format!(
                "{}\0{}\0{}\0{}\0{}\n",
                track.id,
                track.stat_revision,
                track.tag_revision,
                track.disc_number,
                track.track_number
            )
            .as_bytes(),
        );
    }
    let content_hash: String = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    tx.execute(
        "INSERT INTO library_custom_edition_manifests (id, local_album_id, version, \
         release_group_mbid, album_title, album_artist_name, artist_mbid, album_metadata_json, \
         source_album_revision, source_identity_revision, input_revision, content_hash, \
         selected_candidate_key, sealed_by_user_id, sealed_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
            manifest_id,
            album_id,
            version,
            candidate.evidence.release_group_mbid,
            title,
            album_artist,
            artist_mbid,
            album_json,
            album_revision,
            identity_revision,
            snap.expected_input_revision,
            content_hash,
            candidate.candidate_key,
            actor,
            now,
        ],
    )?;
    let mut ordered: Vec<&ManifestTrack> = tracks.iter().collect();
    ordered.sort_by(|a, b| {
        (a.disc_number, a.track_number, &a.id).cmp(&(b.disc_number, b.track_number, &b.id))
    });
    for (ordinal, track) in ordered.into_iter().enumerate() {
        let identity: Option<(String, i64)> = tx
            .query_row(
                "SELECT recording_mbid, row_revision FROM local_track_external_identities \
                 WHERE local_track_id = ?1 AND provider = ?2",
                params![track.id, PROVIDER],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let meta = serde_json::to_string(&track.meta)
            .map_err(|error| OperationError::Store(error.to_string()))?;
        tx.execute(
            "INSERT INTO library_custom_edition_tracks (manifest_id, ordinal, local_track_id, \
             source_track_revision, source_identity_revision, stat_revision, tag_revision, \
             title, artist_name, album_title, album_artist_name, disc_number, track_number, \
             recording_mbid, artist_mbid, album_artist_mbid, metadata_json, file_format, \
             duration_seconds) VALUES \
             (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
            params![
                manifest_id,
                ordinal as i64,
                track.id,
                track.row_revision,
                identity.as_ref().map(|(_, revision)| *revision),
                track.stat_revision,
                track.tag_revision,
                track.title,
                track.artist_name,
                track.album_title,
                album_artist,
                track.disc_number,
                track.track_number,
                identity.map(|(recording, _)| recording),
                track.artist_mbid,
                artist_mbid,
                meta,
                track.file_format,
                track.duration_seconds,
            ],
        )?;
    }
    tx.execute(
        "INSERT INTO library_custom_edition_active (local_album_id, manifest_id, \
         activated_at, row_revision) VALUES (?1, ?2, ?3, 1) \
         ON CONFLICT (local_album_id) DO UPDATE SET manifest_id = excluded.manifest_id, \
         activated_at = excluded.activated_at, row_revision = row_revision + 1",
        params![album_id, manifest_id, now],
    )?;
    tx.execute(
        "DELETE FROM library_management_exclusions WHERE local_album_id = ?1",
        params![album_id],
    )?;
    Ok(manifest_id)
}

fn manifest_tracks(conn: &Connection, album_id: &str) -> rusqlite::Result<Vec<ManifestTrack>> {
    let mut stmt = conn.prepare(
        "SELECT id, row_revision, stat_revision, COALESCE(tag_revision, ''), title, \
         COALESCE(artist_name, ''), album_title, disc_number, track_number, \
         embedded_artist_mbid, file_format, duration_seconds, album_artist_name, year, genre, \
         title_sort, artist_sort, album_sort, album_artist_sort, disc_subtitle, is_compilation \
         FROM local_tracks WHERE local_album_id = ?1 AND availability = 'indexed'",
    )?;
    stmt.query_map(params![album_id], |row| {
        Ok(ManifestTrack {
            id: row.get(0)?,
            row_revision: row.get(1)?,
            stat_revision: row.get(2)?,
            tag_revision: row.get(3)?,
            title: row.get(4)?,
            artist_name: row.get(5)?,
            album_title: row.get(6)?,
            disc_number: row.get(7)?,
            track_number: row.get(8)?,
            artist_mbid: row.get(9)?,
            file_format: row.get(10)?,
            duration_seconds: row.get(11)?,
            meta: ManifestTrackMeta {
                album_artist_name: row.get(12)?,
                year: row.get(13)?,
                genre: row.get(14)?,
                title_sort: row.get(15)?,
                artist_sort: row.get(16)?,
                album_sort: row.get(17)?,
                album_artist_sort: row.get(18)?,
                disc_subtitle: row.get(19)?,
                is_compilation: row.get::<_, i64>(20)? != 0,
            },
        })
    })?
    .collect()
}

/// Close the album's pending reviews: the administrator decided.
fn settle_reviews(
    tx: &Transaction<'_>,
    album_id: &str,
    state: &str,
    selected: Option<&str>,
    actor: &str,
) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE library_identify_reviews SET state = ?2, resolved_by_user_id = ?3, \
         selected_candidate_key = ?4, updated_ms = ?5 \
         WHERE local_album_id = ?1 AND state = 'pending'",
        params![album_id, state, actor, selected, now_ms()],
    )?;
    Ok(())
}

fn now_ms() -> i64 {
    crate::library::clock::now_ms() as i64
}

/// One audit row for a catalog decision.
struct CatalogAction<'a> {
    actor: &'a str,
    kind: &'a str,
    album_id: &'a str,
    job_id: Option<&'a str>,
    before: serde_json::Value,
    after: serde_json::Value,
    reason: &'a str,
}

fn record_action(
    tx: &Transaction<'_>,
    action: CatalogAction<'_>,
    now: f64,
) -> rusqlite::Result<String> {
    let id = uuid::Uuid::new_v4().to_string();
    tx.execute(
        "INSERT INTO library_catalog_actions (id, actor_user_id, action_kind, local_album_id, \
         operation_job_id, before_json, after_json, reason_code, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            id,
            action.actor,
            action.kind,
            action.album_id,
            action.job_id,
            action.before.to_string(),
            action.after.to_string(),
            action.reason,
            now,
        ],
    )?;
    Ok(id)
}

/// Move the catalog revision so cached reads of the album refresh.
fn bump_catalog(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO library_catalog_revision (singleton, value) VALUES (1, 0) \
         ON CONFLICT (singleton) DO NOTHING",
        [],
    )?;
    tx.execute(
        "UPDATE library_catalog_revision SET value = value + 1 WHERE singleton = 1",
        [],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Undoing an automatic edition.
// ---------------------------------------------------------------------------

/// The revisions an administrator echoes to undo the album's automatic
/// edition, while the undo still applies.
pub fn live_automatic_edition_undo(
    conn: &Connection,
    album_id: &str,
) -> rusqlite::Result<Option<(i64, i64)>> {
    conn.query_row(
        "SELECT u.expected_post_album_revision, u.expected_post_identity_revision \
         FROM library_automatic_edition_undo u \
         JOIN local_albums a ON a.id = u.local_album_id \
         JOIN local_album_external_identities i \
         ON i.local_album_id = u.local_album_id AND i.provider = ?2 \
         WHERE u.local_album_id = ?1 AND u.consumed_at IS NULL \
         AND i.decision_source = 'automatic' \
         AND i.row_revision = u.expected_post_identity_revision \
         AND a.row_revision = u.expected_post_album_revision",
        params![album_id, PROVIDER],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()
}

/// Put back what the album's last automatic edition replaced, column for
/// column (v2's restore). With no earlier album identity, the album goes
/// to review instead. Track rows a curator holds now are never rewritten,
/// and pins are never touched.
pub fn undo_automatic_edition(
    tx: &Transaction<'_>,
    album_id: &str,
    expected_album_revision: i64,
    expected_identity_revision: i64,
    actor: &str,
    now: f64,
) -> Result<UndoOutcome, OperationError> {
    let undo: Option<(String, Option<String>, String, i64, i64)> = tx
        .query_row(
            "SELECT id, prior_identity_json, prior_track_identities_json, \
             expected_post_album_revision, expected_post_identity_revision \
             FROM library_automatic_edition_undo \
             WHERE local_album_id = ?1 AND consumed_at IS NULL",
            params![album_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?;
    let Some((undo_id, prior_json, prior_tracks_json, post_album_revision, post_identity_revision)) =
        undo
    else {
        return Err(OperationError::NotFound(reasons::NO_AUTOMATIC_EDITION));
    };
    let album_revision: Option<i64> = tx
        .query_row(
            "SELECT row_revision FROM local_albums WHERE id = ?1",
            params![album_id],
            |row| row.get(0),
        )
        .optional()?;
    let current = album_identity_row(tx, album_id)?;
    let fresh = album_revision == Some(expected_album_revision)
        && expected_album_revision == post_album_revision
        && current.as_ref().is_some_and(|identity| {
            identity.decision_source.as_deref() == Some("automatic")
                && identity.row_revision == Some(expected_identity_revision)
                && expected_identity_revision == post_identity_revision
        });
    if !fresh {
        return Err(OperationError::Conflict(reasons::UNDO_STALE));
    }
    let unreadable = |error: serde_json::Error| OperationError::Store(error.to_string());
    let prior: Option<PriorAlbumIdentity> = prior_json
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(unreadable)?;
    let prior_tracks: Vec<PriorTrackIdentity> =
        serde_json::from_str(&prior_tracks_json).map_err(unreadable)?;
    let current_tracks = track_identity_rows(tx, album_id)?;

    let mut review_id = None;
    let outcome = match prior
        .filter(|prior| prior.release_group_mbid.is_some() && prior.decision_source.is_some())
    {
        Some(prior) => {
            tx.execute(
                "UPDATE local_album_external_identities SET release_group_mbid = ?2, \
                 release_mbid = ?3, decision_source = ?4, matcher_version = ?5, \
                 attempt_id = ?6, \
                 selected_by_user_id = (SELECT id FROM auth_users WHERE id = ?7), \
                 selected_at = ?8, row_revision = row_revision + 1 \
                 WHERE local_album_id = ?1 AND provider = ?9",
                params![
                    album_id,
                    prior.release_group_mbid,
                    prior.release_mbid,
                    prior.decision_source,
                    prior.matcher_version,
                    prior.attempt_id,
                    prior.selected_by_user_id,
                    prior.selected_at.unwrap_or(now),
                    PROVIDER,
                ],
            )?;
            "restored"
        }
        None => {
            tx.execute(
                "DELETE FROM local_album_external_identities \
                 WHERE local_album_id = ?1 AND provider = ?2",
                params![album_id, PROVIDER],
            )?;
            let id = uuid::Uuid::new_v4().to_string();
            tx.execute(
                "INSERT INTO library_identify_reviews (id, local_album_id, reason_code, \
                 candidates_json, state, created_ms, updated_ms) \
                 VALUES (?1, ?2, 'AUTOMATIC_EDITION_CLEARED_TO_REVIEW', '[]', 'pending', ?3, ?3)",
                params![id, album_id, now_ms()],
            )?;
            review_id = Some(id);
            "cleared_to_review"
        }
    };
    for track_id in indexed_track_ids(tx, album_id)? {
        let now_row = current_tracks
            .iter()
            .find(|row| row.local_track_id == track_id);
        if now_row.is_some_and(|row| row.decision_source.as_deref() != Some("automatic")) {
            // A curator's (or the tags') row stays as it is.
            continue;
        }
        let before = prior_tracks
            .iter()
            .find(|row| row.local_track_id == track_id && row.recording_mbid.is_some());
        match before {
            Some(row) => restore_track_row(tx, row, now)?,
            None if now_row.is_some() => {
                tx.execute(
                    "DELETE FROM local_track_external_identities \
                     WHERE local_track_id = ?1 AND provider = ?2",
                    params![track_id, PROVIDER],
                )?;
            }
            None => {}
        }
    }
    let action = record_action(
        tx,
        CatalogAction {
            actor,
            kind: "undo_automatic_edition",
            album_id,
            job_id: None,
            before: serde_json::json!({ "identity": current, "tracks": current_tracks }),
            after: serde_json::json!({
                "outcome": outcome,
                "review_id": review_id,
                "undo_snapshot_id": undo_id,
            }),
            reason: "AUTOMATIC_EDITION_UNDONE",
        },
        now,
    )?;
    tx.execute(
        "UPDATE library_automatic_edition_undo SET consumed_at = ?2, consumed_action_id = ?3 \
         WHERE id = ?1",
        params![undo_id, now, action],
    )?;
    bump_catalog(tx)?;
    Ok(UndoOutcome {
        local_album_id: album_id.to_owned(),
        outcome: outcome.to_owned(),
        review_id,
    })
}

/// Write one snapshot track row back exactly; an existing row takes the
/// next revision.
fn restore_track_row(
    tx: &Transaction<'_>,
    row: &PriorTrackIdentity,
    now: f64,
) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO local_track_external_identities (local_track_id, provider, \
         recording_mbid, release_mbid, release_track_mbid, medium_position, \
         release_track_position, decision_source, attempt_id, selected_at, row_revision) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 1) \
         ON CONFLICT (local_track_id, provider) DO UPDATE SET \
         recording_mbid = excluded.recording_mbid, release_mbid = excluded.release_mbid, \
         release_track_mbid = excluded.release_track_mbid, \
         medium_position = excluded.medium_position, \
         release_track_position = excluded.release_track_position, \
         decision_source = excluded.decision_source, attempt_id = excluded.attempt_id, \
         selected_at = excluded.selected_at, row_revision = row_revision + 1",
        params![
            row.local_track_id,
            PROVIDER,
            row.recording_mbid,
            row.release_mbid,
            row.release_track_mbid,
            row.medium_position,
            row.release_track_position,
            row.decision_source.as_deref().unwrap_or("automatic"),
            row.attempt_id,
            row.selected_at.unwrap_or(now),
        ],
    )?;
    Ok(())
}
