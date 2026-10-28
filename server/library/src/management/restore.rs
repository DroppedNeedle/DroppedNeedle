//! Back to the originals: a baseline restore preview puts chosen files
//! back the way they were before DroppedNeedle first managed them (tags
//! and path), as a job you inspect and then apply like any other. The
//! purge drops every saved original for good, after showing what it
//! would remove and asking for a typed confirmation.

use droppedneedle_kernel::db::Lane;
use rusqlite::{OptionalExtension as _, params};

use super::models::{
    Eligibility, FieldMutation, JobIntent, JobMode, ManagementError, Origin, PlannedItem,
    PreviewHandle, Selection, Summary,
};
use super::profile;
use super::reasons::{self, ItemReason};
use super::selection;
use super::service::{JobSpec, Management, UNDO_COPY_MISSING, stale};
use super::store::Snapshot;
use crate::clock::LibraryClock as _;
use crate::manage::BaselineSkip;
use crate::publish::record::changed_since_written;
use crate::publish::snapshots::sha256_hex;
use crate::publish::undo::BaselineBlock;

/// The phrase a purge must be confirmed with (as in v2).
pub const PURGE_CONFIRMATION: &str = "CONFIRM";

/// A request for a baseline restore preview.
pub struct RestoreRequest {
    pub selection: Selection,
    pub expected_settings_revision: String,
    pub expected_policy_revision: String,
    pub idempotency_key: String,
}

/// What purging the saved originals would remove.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PurgeImpact {
    pub baseline_count: i64,
    pub referenced_blob_count: i64,
    pub referenced_blob_bytes: i64,
    /// Writes still in flight that may need an original.
    pub blocked_journal_count: i64,
    /// Restore jobs waiting or running.
    pub active_restore_count: i64,
    /// Proves the purge confirms exactly this impact.
    pub impact_token: String,
}

/// What a purge removed.
#[derive(Debug, Clone)]
pub struct PurgeOutcome {
    pub purged_baseline_count: i64,
    pub detached_reference_count: i64,
    pub cleaned_blob_count: i64,
    /// True when the idempotency key named an earlier purge.
    pub existing: bool,
}

/// A purge request.
pub struct PurgeRequest {
    pub impact_token: String,
    pub typed_confirmation: String,
    pub idempotency_key: String,
}

fn baseline_reason(skip: &BaselineSkip) -> ItemReason {
    let reason = |code: &str, message: &str, action: &str| ItemReason {
        code: code.to_owned(),
        message: message.to_owned(),
        action: action.to_owned(),
    };
    match skip {
        BaselineSkip::Gone => reasons::file_changed(),
        BaselineSkip::CopyMissing => reason(
            UNDO_COPY_MISSING,
            "The saved copy of this file's earlier tags is missing.",
            "This file stays as it is.",
        ),
        BaselineSkip::Plan(BaselineBlock::MissingBaseline) => reason(
            "BASELINE_SNAPSHOT_MISSING",
            "DroppedNeedle never changed this file, so there is no original to go back to.",
            "Nothing to do: the file is already as you had it.",
        ),
        BaselineSkip::Plan(BaselineBlock::MissingRoot) => reasons::root_unavailable(),
        BaselineSkip::Plan(BaselineBlock::FormatMismatch | BaselineBlock::CurrentChanged) => {
            reasons::file_changed()
        }
        BaselineSkip::Plan(BaselineBlock::IdentityChanged) => reason(
            "FILE_CHANGED",
            "The album's match changed while the restore was being planned.",
            "Make a new restore preview.",
        ),
        BaselineSkip::Plan(BaselineBlock::OriginalOccupied) => reason(
            "PATH_COLLISION_DIFFERENT",
            "Another file now sits where this file was originally, and files are never overwritten.",
            "Move the other file away, then make a new restore preview.",
        ),
    }
}

impl Management {
    /// Start a baseline restore preview over whole albums. Blocking.
    pub fn create_restore_preview(
        &self,
        request: RestoreRequest,
        actor: &str,
    ) -> Result<PreviewHandle, ManagementError> {
        if request.idempotency_key.trim().is_empty() {
            return Err(ManagementError::Invalid(
                "A restore request key is required.".to_owned(),
            ));
        }
        let stored = self.settings()?;
        let registry = self.check_revisions(
            &stored,
            &request.expected_settings_revision,
            &request.expected_policy_revision,
        )?;
        let selection = selection::normalize(request.selection)?;
        // The profile only labels the job: a restore writes the saved
        // originals, not anything a profile plans.
        let pinned = profile::pin(&stored, &stored.default_profile_id, None)?;
        self.start_job(JobSpec {
            mode: JobMode::BaselineRestore,
            origin: Origin::Manual,
            actor: Some(actor),
            idempotency_key: Some(&request.idempotency_key),
            revisions: profile::revisions(&stored, &pinned, registry.policy_revision()),
            selection_json: serde_json::to_string(&selection)?,
            pinned: &pinned,
            target_root_id: None,
            linked_job_id: None,
            intent: JobIntent::default(),
            proposed_settings_revision: None,
            retention_hours: stored.preview_retention_hours,
        })
    }

    /// Plan putting every selected album back to its originals. An album
    /// restores as a whole or not at all.
    pub(super) fn plan_baseline_restore(
        &self,
        snapshot: &Snapshot,
        worker: &str,
    ) -> Result<Option<Summary>, ManagementError> {
        let selection: Selection = serde_json::from_str(&snapshot.selection_json)?;
        let mut albums = self
            .setup
            .db
            .read_blocking("management.restore_albums", |conn| {
                selection::albums(conn, &selection)
            })?;
        for scope in &mut albums {
            scope.track_ids = None;
        }
        let wide = albums
            .iter()
            .map(|scope| scope.album_id.as_str())
            .collect::<std::collections::HashSet<_>>()
            .len()
            > 1;
        let mut summary = Summary::default();
        let mut ordinal = 0;
        for (bundle_ordinal, scope) in albums.iter().enumerate() {
            let tracks = self
                .setup
                .db
                .read_blocking("management.restore_tracks", |conn| {
                    selection::tracks(conn, scope)
                })?;
            if tracks.is_empty() {
                continue;
            }
            let track_ids: Vec<String> = tracks.iter().map(|track| track.id.clone()).collect();
            let album_id = scope.album_id.clone();
            let override_of = |track_id: &str| self.override_revision(&album_id, track_id);
            let files = self
                .setup
                .publishing
                .baseline_files(&track_ids, &override_of)
                .map_err(|error| ManagementError::Store(format!("{error:?}")))?;
            let mut items = Vec::new();
            let base = |track: &selection::CatalogTrack, ordinal: i64| PlannedItem {
                ordinal,
                bundle_ordinal: bundle_ordinal as i64,
                album_id: scope.album_id.clone(),
                album_revision: scope.album_revision,
                track_id: Some(track.id.clone()),
                source_root: track.root_id.clone(),
                source_rel: track.relative_path.clone(),
                dest_root: None,
                dest_rel: None,
                eligibility: Eligibility::Eligible,
                estimated_bytes: 0,
                fingerprint: String::new(),
                desired: Default::default(),
                diff: Default::default(),
                facts: super::models::ItemFacts {
                    audio_format: track.file_format.to_lowercase(),
                    catalog_track_title: Some(track.title.clone()),
                    catalog_artist_name: track.artist_name.clone(),
                    catalog_album_title: Some(track.album_title.clone()),
                    catalog_track_number: Some(track.track_number),
                    catalog_disc_number: Some(track.disc_number),
                    ..Default::default()
                },
            };
            for track in &tracks {
                let mut item = base(track, ordinal + items.len() as i64);
                if let Some(file) = files
                    .eligible
                    .iter()
                    .find(|file| file.plan.track_id == track.id)
                {
                    let plan = file.plan.clone();
                    item.dest_root = Some(plan.dest_root.clone());
                    item.dest_rel = Some(plan.dest_rel.clone());
                    item.fingerprint = plan.fingerprint.sha256.clone();
                    item.estimated_bytes = plan.staged_bytes_estimate;
                    item.diff.path_changed =
                        plan.source_root != plan.dest_root || plan.source_rel != plan.dest_rel;
                    for (name, after) in &plan.managed_updates.named() {
                        let before = file.current.get(name).cloned().unwrap_or_default();
                        if &before == after {
                            continue;
                        }
                        item.diff.field_mutations.push(FieldMutation {
                            name: name.clone(),
                            operation: if after.is_empty() { "clear" } else { "set" }.to_owned(),
                            before: profile::json_value(&before),
                            after: profile::json_value(after),
                        });
                    }
                    item.diff.tags_changed = !item.diff.field_mutations.is_empty();
                    if item.diff.changes_anything() {
                        item.desired.plan = Some(plan);
                    }
                } else if let Some((_, skip)) = files
                    .blocked
                    .iter()
                    .find(|(track_id, _)| track_id == &track.id)
                {
                    match skip {
                        // A file never changed is fine as it is: it does
                        // not hold its album back.
                        BaselineSkip::Plan(BaselineBlock::MissingBaseline) => {
                            item.dest_root = Some(item.source_root.clone());
                            item.dest_rel = Some(item.source_rel.clone());
                            item.facts.warnings.push(baseline_reason(skip).message);
                        }
                        _ => item.block(baseline_reason(skip)),
                    }
                }
                items.push(item);
            }
            if wide {
                // A restore of more than one album never writes over a
                // file changed outside since the app wrote it: restore the
                // album by itself for that.
                let changed =
                    self.setup
                        .db
                        .read_blocking("management.restore_changed", |conn| {
                            let mut changed = Vec::new();
                            for item in items.iter().filter(|item| item.desired.plan.is_some()) {
                                if let Some(track_id) = &item.track_id
                                    && changed_since_written(
                                        conn,
                                        track_id,
                                        &item.source_root,
                                        &item.source_rel,
                                        &item.fingerprint,
                                    )?
                                {
                                    changed.push(track_id.clone());
                                }
                            }
                            Ok::<_, ManagementError>(changed)
                        })?;
                for item in items.iter_mut().filter(|item| {
                    item.track_id
                        .as_ref()
                        .is_some_and(|id| changed.contains(id))
                }) {
                    item.block(super::takeover::changed_outside_reason());
                }
                // As a many-album preview does: where the folder is managed
                // automatically, the album is left unmanaged too.
                if !changed.is_empty() && self.managed_automatically(&scope.album_id)? {
                    let now = self.setup.clock.now_secs();
                    self.setup.db.write_blocking(
                        Lane::Background,
                        "management.restore_mark_changed",
                        |tx| {
                            Ok::<_, ManagementError>(super::takeover::mark(
                                tx,
                                &scope.album_id,
                                super::takeover::Cause::FilesChanged,
                                &changed,
                                Some(&snapshot.job.id),
                                now,
                            )?)
                        },
                    )?;
                }
            }
            // A file whose saved copy is gone cannot be restored at all,
            // so it does not hold the rest of the album back.
            if items.iter().any(|item| {
                item.eligibility == Eligibility::Blocked
                    && item
                        .reason()
                        .is_none_or(|reason| reason.code != UNDO_COPY_MISSING)
            }) {
                for item in items
                    .iter_mut()
                    .filter(|item| item.eligibility.writes() && item.diff.changes_anything())
                {
                    item.block(reasons::bundle_blocked());
                }
            }
            ordinal += items.len() as i64;
            summary.bundle_count += 1;
            for item in &items {
                summary.count(item);
            }
            if !self.keep_items(snapshot, worker, &items)? {
                return Ok(None);
            }
        }
        summary.selected_item_count = Some(summary.item_count);
        summary.expanded_track_count = summary.item_count;
        Ok(Some(summary))
    }

    /// What purging every saved original would remove. Blocking.
    pub fn purge_impact(&self) -> Result<PurgeImpact, ManagementError> {
        let (mut impact, kept) = self
            .setup
            .db
            .read_blocking("management.purge_impact", |conn| {
                Ok::<_, ManagementError>((purge_impact(conn)?, baseline_blob_sizes(conn)?))
            })?;
        let dir = self.setup.core.blobs.dir();
        impact.referenced_blob_bytes = kept
            .into_iter()
            .map(|(sha256, in_database)| {
                in_database.unwrap_or_else(|| dir.size_of(&sha256).unwrap_or(0) as i64)
            })
            .sum();
        Ok(impact)
    }

    /// Drop every saved original for good, once the typed confirmation
    /// and the impact token match and no restore or write is in flight.
    /// A retried request with the same key returns the first result.
    /// Blocking.
    pub fn purge(
        &self,
        request: PurgeRequest,
        actor: &str,
    ) -> Result<PurgeOutcome, ManagementError> {
        if request.typed_confirmation != PURGE_CONFIRMATION {
            return Err(ManagementError::Invalid(format!(
                "Type {PURGE_CONFIRMATION} exactly to delete the saved originals for good."
            )));
        }
        if request.idempotency_key.trim().is_empty() {
            return Err(ManagementError::Invalid(
                "A purge request key is required.".to_owned(),
            ));
        }
        // Under the publish lock: no write is between saving an original
        // and referring to it while the purge runs.
        let publishing = self
            .setup
            .publish
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let outcome = self.setup.db.write_blocking(Lane::Foreground, "management.purge", |tx| {
            let earlier = tx
                .query_row(
                    "SELECT impact_token, purged_baseline_count, detached_reference_count \
                     FROM library_management_baseline_purges WHERE idempotency_key = ?1",
                    params![request.idempotency_key],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, i64>(2)?,
                        ))
                    },
                )
                .optional()?;
            if let Some((token, purged, detached)) = earlier {
                if token != request.impact_token {
                    return Err(stale(
                        "This request key was already used for another purge.",
                    ));
                }
                return Ok(PurgeOutcome {
                    purged_baseline_count: purged,
                    detached_reference_count: detached,
                    cleaned_blob_count: 0,
                    existing: true,
                });
            }
            let impact = purge_impact(tx)?;
            if impact.impact_token != request.impact_token {
                return Err(stale(
                    "The saved originals changed since you looked. Check what the purge removes again.",
                ));
            }
            if impact.blocked_journal_count > 0 || impact.active_restore_count > 0 {
                return Err(ManagementError::Conflict(
                    "A restore or a file write is still running. Wait for it to finish, then purge."
                        .to_owned(),
                ));
            }
            let detached = tx.execute(
                "DELETE FROM library_publish_blob_refs WHERE owner_kind IN ('baseline', 'v2_baseline')",
                [],
            )? as i64;
            tx.execute("DELETE FROM library_publish_baselines", [])?;
            // Originals v2 recorded: without this, files v2 managed
            // would wait for an original that will never come.
            tx.execute(
                "UPDATE library_track_management_state SET baseline_id = NULL, \
                 row_revision = row_revision + 1 WHERE baseline_id IS NOT NULL",
                [],
            )?;
            tx.execute("DELETE FROM library_management_baselines", [])?;
            tx.execute(
                "INSERT INTO library_management_baseline_purges (idempotency_key, impact_token, \
                 actor_user_id, purged_baseline_count, detached_reference_count, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    request.idempotency_key,
                    request.impact_token,
                    actor,
                    impact.baseline_count,
                    detached,
                    self.setup.clock.now_secs()
                ],
            )?;
            Ok(PurgeOutcome {
                purged_baseline_count: impact.baseline_count,
                detached_reference_count: detached,
                cleaned_blob_count: 0,
                existing: false,
            })
        })?;
        // The sweep needs no publish lock: the undo folder's own lock keeps
        // it off a copy that is being kept.
        drop(publishing);
        let blobs = &self.setup.core.blobs;
        let cleaned = crate::publish::snapshots::sweep_unreferenced_blobs(&self.setup.db)
            .and_then(|rows| Ok(rows + blobs.sweep(&self.setup.db)?))
            .map_err(|error| ManagementError::Store(error.to_string()))?;
        tracing::info!(
            purged = outcome.purged_baseline_count,
            cleaned,
            "saved originals purged"
        );
        Ok(PurgeOutcome {
            cleaned_blob_count: cleaned as i64,
            ..outcome
        })
    }
}

/// Each blob a saved original holds, with its size when its bytes are
/// still in the database. The rest are files, sized outside the read.
fn baseline_blob_sizes(
    conn: &rusqlite::Connection,
) -> rusqlite::Result<Vec<(String, Option<i64>)>> {
    let mut stmt = conn.prepare(
        "SELECT r.sha256, (SELECT LENGTH(b.bytes) FROM library_publish_blobs b \
         WHERE b.sha256 = r.sha256) FROM (SELECT DISTINCT sha256 FROM library_publish_blob_refs \
         WHERE owner_kind IN ('baseline', 'v2_baseline')) r",
    )?;
    let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    rows.collect()
}

/// Count what a purge would remove, and the token that proves it. The
/// size of the copies is filled in by the caller, outside any write.
fn purge_impact(conn: &rusqlite::Connection) -> rusqlite::Result<PurgeImpact> {
    let count = |sql: &str| conn.query_row(sql, [], |row| row.get::<_, i64>(0));
    let v3 = count("SELECT COUNT(*) FROM library_publish_baselines")?;
    let v2 = count(
        "SELECT COUNT(*) FROM library_management_baselines b WHERE NOT EXISTS \
         (SELECT 1 FROM library_publish_baselines p WHERE p.track_id = b.local_track_id)",
    )?;
    let newest = count("SELECT COALESCE(MAX(created_day), 0) FROM library_publish_baselines")?;
    let blobs = count(
        "SELECT COUNT(DISTINCT sha256) FROM library_publish_blob_refs \
         WHERE owner_kind IN ('baseline', 'v2_baseline')",
    )?;
    let journals = count(
        "SELECT COUNT(*) FROM library_publish_journal \
         WHERE state NOT IN ('cleaned', 'compensated', 'needs_attention')",
    )?;
    let restores = count(
        "SELECT COUNT(*) FROM library_management_job_snapshots s \
         JOIN library_operation_jobs j ON j.id = s.job_id \
         WHERE s.mode = 'baseline_restore' AND j.state IN ('queued', 'running', 'paused', 'ready') \
         AND (j.state <> 'ready' OR COALESCE(s.preview_expires_at, 0) > CAST(strftime('%s', 'now') AS REAL))",
    )?;
    let baseline_count = v3 + v2;
    let material = format!("{baseline_count}:{newest}:{blobs}:{journals}:{restores}");
    Ok(PurgeImpact {
        baseline_count,
        referenced_blob_count: blobs,
        referenced_blob_bytes: 0,
        blocked_journal_count: journals,
        active_restore_count: restores,
        impact_token: sha256_hex(material.as_bytes()),
    })
}
