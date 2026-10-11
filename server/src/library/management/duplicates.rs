//! A file a preview could not place because another file already sits
//! at its destination. The administrator picks what to do: keep the
//! existing file (nothing moves), or put the incoming file at another
//! path next to it. Either way it is a small preview of its own, applied
//! (and undone) like any other. Files are never overwritten.

use std::path::Path;

use super::models::{
    DuplicateAction, DuplicateIntent, Eligibility, ItemFilter, JobIntent, JobMode, ManagementError,
    Origin, PreviewHandle, RecycleFirst, Selection, SelectionKind, Summary,
};
use super::planner::{self, PlanContext};
use super::profile::{self, CompiledScripts};
use super::reasons::ItemReason;
use super::selection;
use super::service::{JobSpec, Management, stale};
use super::store::{self, Snapshot};
use crate::library::mutations::MutationError;
use crate::library::publish::planner::PlanKind;
use rusqlite::OptionalExtension as _;

/// A request to settle one blocked file.
pub struct DuplicateRequest {
    pub intent: DuplicateIntent,
    pub expected_source_row_revision: i64,
    pub expected_settings_revision: String,
    pub expected_policy_revision: String,
    pub idempotency_key: String,
}

fn duplicate_changed() -> ItemReason {
    ItemReason {
        code: "DUPLICATE_CHANGED".to_owned(),
        message: "The file or the one in its way changed since the preview.".to_owned(),
        action: "Make a new preview of the album, then settle the clash again.".to_owned(),
    }
}

/// Where a recycled track's catalog path is parked while its job runs.
fn recycled_prefix(job_id: &str) -> String {
    format!(
        "{}recycled-{job_id}/",
        crate::library::publish::HIDDEN_PREFIX
    )
}

fn album_moves_too() -> ItemReason {
    ItemReason {
        code: "ALBUM_MOVES".to_owned(),
        message: "The rest of this album moves to a new folder too, and an album is never split between folders."
            .to_owned(),
        action: "Keep both files, or move the file in the way yourself, then make a new preview of the whole album."
            .to_owned(),
    }
}

fn nothing_to_place() -> ItemReason {
    ItemReason {
        code: "NO_CHANGE".to_owned(),
        message: "This file already sits where the profile wants it, so there is nothing to place."
            .to_owned(),
        action: "Keep both files, or make a new preview of the album.".to_owned(),
    }
}

/// A recycle-bin refusal as a management one, in its own words.
fn recycle_refused(error: MutationError) -> ManagementError {
    match error {
        MutationError::Conflict(reason) | MutationError::Files(reason) => {
            ManagementError::Conflict(format!("{} {}", reason.message, reason.action))
        }
        MutationError::NotFound(_) => ManagementError::Conflict(format!(
            "{} {}",
            duplicate_changed().message,
            duplicate_changed().action
        )),
        MutationError::Store(cause) => ManagementError::Store(cause),
    }
}

/// A plain relative path inside a library folder, or why not.
fn safe_relative(raw: &str) -> Result<String, ManagementError> {
    let trimmed = raw.trim().trim_start_matches('/');
    let bad = trimmed.is_empty()
        || trimmed.contains('\\')
        || trimmed.contains('\0')
        || trimmed
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..");
    if bad {
        return Err(ManagementError::Invalid(
            "The other path must be a plain path inside the library folder.".to_owned(),
        ));
    }
    Ok(trimmed.to_owned())
}

impl Management {
    /// Start a preview that settles one blocked file. Blocking.
    pub fn create_duplicate_preview(
        &self,
        request: DuplicateRequest,
        actor: &str,
    ) -> Result<PreviewHandle, ManagementError> {
        if request.idempotency_key.trim().is_empty() {
            return Err(ManagementError::Invalid(
                "A request key is required.".to_owned(),
            ));
        }
        let mut intent = request.intent;
        if intent.action == DuplicateAction::KeepIncomingAlternate {
            let raw = intent
                .alternate_relative_path
                .as_deref()
                .unwrap_or_default();
            intent.alternate_relative_path = Some(safe_relative(raw)?);
        }
        let source = self.snapshot(&intent.source_job_id)?;
        if !matches!(
            source.job.state.as_str(),
            "ready" | "succeeded" | "stopped" | "failed"
        ) {
            return Err(stale(
                "The preview this file came from is still running. Wait for it to finish.",
            ));
        }
        if source.job.row_revision != request.expected_source_row_revision {
            return Err(stale(
                "The preview this file came from changed since the page loaded. Reload the page and try again.",
            ));
        }
        let item = self.source_item(&source, intent.source_plan_item_ordinal)?;
        let track_id = item.track_id.clone().ok_or(ManagementError::NotFound(
            "That file is not in the preview.",
        ))?;
        let stored = self.settings()?;
        self.check_revisions(
            &stored,
            &request.expected_settings_revision,
            &request.expected_policy_revision,
        )?;
        if source.settings_revision != crate::settings::management::settings_revision(&stored) {
            return Err(stale(
                "Library Management settings changed since that preview. Make a new preview first.",
            ));
        }
        let pinned = Self::pinned(&source)?;
        let selection = Selection {
            kind: SelectionKind::Tracks,
            ids: vec![track_id],
            catalog_filter: None,
        };
        let registry = self.setup.live_registry();
        self.start_job(JobSpec {
            mode: JobMode::DuplicateResolution,
            origin: Origin::Manual,
            actor: Some(actor),
            idempotency_key: Some(&request.idempotency_key),
            revisions: profile::revisions(&stored, &pinned, registry.policy_revision()),
            selection_json: serde_json::to_string(&selection)?,
            pinned: &pinned,
            target_root_id: source.target_root_id.as_deref(),
            linked_job_id: Some(&source.job.id),
            intent: JobIntent {
                duplicate: Some(intent),
                ..JobIntent::default()
            },
            proposed_settings_revision: None,
            retention_hours: stored.preview_retention_hours,
        })
    }

    /// When a duplicate resolution is applied: move the file its action
    /// sends to the recycle bin, after checking it is still the file the
    /// preview showed. True when that was the whole job. The recycle bin
    /// keeps the file in a dated folder, so it can be moved back by hand.
    pub(super) fn recycle_for_duplicate(
        &self,
        snapshot: &Snapshot,
    ) -> Result<bool, ManagementError> {
        let Some(intent) = snapshot.intent().duplicate else {
            return Ok(false);
        };
        let actor = snapshot
            .job
            .requested_by_user_id
            .clone()
            .unwrap_or_default();
        let mutations = crate::library::mutations::Mutations::new(&self.setup);
        let changed = || {
            ManagementError::Conflict(format!(
                "{} {}",
                duplicate_changed().message,
                duplicate_changed().action
            ))
        };
        let sandbox = self.setup.publish_sandbox().map_err(|_| {
            ManagementError::Conflict(
                "The library folders are not available right now. Try again later.".to_owned(),
            )
        })?;
        match intent.action {
            DuplicateAction::RecycleIncomingKeepExisting => {
                let item = self.source_item(snapshot, 0)?;
                let track_id = item.track_id.clone().ok_or_else(changed)?;
                let unchanged = sandbox
                    .resolve_no_symlink(&item.source_root, &item.source_rel)
                    .ok()
                    .and_then(|path| crate::library::publish::paths::read_regular_file(&path).ok())
                    .is_some_and(|bytes| {
                        crate::library::publish::snapshots::sha256_hex(&bytes) == item.fingerprint
                    });
                if !unchanged {
                    return Err(changed());
                }
                mutations
                    .remove_track(&track_id, true, &actor)
                    .map_err(recycle_refused)?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    /// In the worker, just before an album is published: move the file a
    /// duplicate resolution sends to the recycle bin, after checking it
    /// still holds the bytes the preview saw. The move is recorded on the
    /// album's work row first thing after it happens, so a retry after a
    /// crash goes straight on to publishing. Returns the moves, to put
    /// back if publishing fails.
    pub(super) fn recycle_before_publish(
        &self,
        snapshot: &Snapshot,
        ordinal: i64,
        items: &[super::models::PlannedItem],
    ) -> Result<Vec<(String, String)>, ItemReason> {
        let Some(first) = items
            .iter()
            .find_map(|item| item.desired.recycle_first.clone())
        else {
            return Ok(Vec::new());
        };
        let job_id = snapshot.job.id.as_str();
        let failed = |message: String| ItemReason {
            code: "RECYCLE_FAILED".to_owned(),
            message,
            action: "Move the file in the way yourself, then make a new preview of the album."
                .to_owned(),
        };
        let earlier = self
            .read(|conn| Ok(store::work_recycled(conn, job_id, ordinal)?))
            .map_err(|_| failed("The album's progress could not be read.".to_owned()))?;
        let sandbox = self
            .setup
            .publish_sandbox()
            .map_err(|_| failed("The library folders are not available right now.".to_owned()))?;
        let current = sandbox
            .resolve_no_symlink(&first.root_id, &first.relative_path)
            .ok()
            .and_then(|path| crate::library::publish::paths::read_regular_file(&path).ok())
            .map(|bytes| crate::library::publish::snapshots::sha256_hex(&bytes));
        if !earlier.is_empty() && current.is_none() {
            // Recycled by an earlier attempt that did not finish.
            return Ok(earlier);
        }
        if current.as_deref() != Some(first.sha256.as_str()) {
            return Err(duplicate_changed());
        }
        let mutations = crate::library::mutations::Mutations::new(&self.setup);
        let bin = mutations
            .bin_for(&first.root_id)
            .map(|bin| bin.display().to_string())
            .unwrap_or_else(|| "the recycle bin".to_owned());
        let actor = snapshot
            .job
            .requested_by_user_id
            .clone()
            .unwrap_or_default();
        let moved: Vec<(std::path::PathBuf, std::path::PathBuf)> = match &first.local_track_id {
            Some(track_id) => mutations
                .remove_track(track_id, true, &actor)
                .map(|removed| removed.recycled),
            None => mutations
                .recycle_untracked(&first.root_id, &first.relative_path)
                .and_then(|destination| {
                    sandbox
                        .resolve_no_symlink(&first.root_id, &first.relative_path)
                        .map(|original| vec![(original, destination)])
                        .map_err(|_| {
                            crate::library::mutations::MutationError::Store(
                                "recycled file's old path did not resolve".to_owned(),
                            )
                        })
                }),
        }
        .map_err(|error| {
            let refused = recycle_refused(error);
            failed(format!(
                "The file in the way, {}, could not be moved to the recycle bin at {bin}. {refused}",
                first.relative_path
            ))
        })?;
        let moves: Vec<(String, String)> = moved
            .iter()
            .map(|(from, to)| (from.display().to_string(), to.display().to_string()))
            .collect();
        // The recycled track keeps its catalog row (marked missing), so
        // its path is parked under a hidden name: the new file can then
        // take the path, and a put-back gives it back.
        if first.local_track_id.is_some() {
            let parked = self.read(|conn| {
                Ok(conn.execute(
                    "UPDATE local_tracks SET relative_path = ?3 || relative_path \
                     WHERE root_id = ?1 AND relative_path = ?2 AND availability <> 'indexed'",
                    rusqlite::params![first.root_id, first.relative_path, recycled_prefix(job_id)],
                )?)
            });
            if let Err(error) = parked {
                tracing::error!(%error, job_id, "recycled track's path not parked");
            }
        }
        if let Err(error) =
            self.read(|conn| Ok(store::set_work_recycled(conn, job_id, ordinal, &moves)?))
        {
            tracing::error!(%error, job_id, "recycled file not recorded; putting it back");
            self.put_back_recycled(snapshot, ordinal, &moves);
            return Err(failed(
                "The album's progress could not be saved.".to_owned(),
            ));
        }
        Ok(moves)
    }

    /// Put files a duplicate resolution recycled back where they were,
    /// and forget them. True when every file went back.
    pub(super) fn put_back_recycled(
        &self,
        snapshot: &Snapshot,
        ordinal: i64,
        moves: &[(String, String)],
    ) -> bool {
        if moves.is_empty() {
            return true;
        }
        let actor = snapshot
            .job
            .requested_by_user_id
            .clone()
            .unwrap_or_default();
        // Never over another file: a file now at the old path keeps it,
        // and the recycled one stays in the bin.
        let (paths, taken): (Vec<_>, Vec<_>) = moves
            .iter()
            .map(|(from, to)| (std::path::PathBuf::from(from), std::path::PathBuf::from(to)))
            .partition(|(from, _)| from.symlink_metadata().is_err());
        for (from, to) in &taken {
            tracing::error!(
                original = %from.display(),
                recycled = %to.display(),
                "recycled file not put back: its old path is taken"
            );
        }
        // Give a parked catalog path back before the rescan finds the file.
        let prefix = recycled_prefix(&snapshot.job.id);
        if let Err(error) = self.read(|conn| {
            Ok(conn.execute(
                "UPDATE local_tracks SET relative_path = substr(relative_path, length(?1) + 1) \
                 WHERE substr(relative_path, 1, length(?1)) = ?1",
                rusqlite::params![prefix],
            )?)
        }) {
            tracing::warn!(%error, "parked catalog path not given back");
        }
        let returned = paths.is_empty()
            || crate::library::mutations::Mutations::new(&self.setup).put_back(&paths, &actor);
        let back = returned && taken.is_empty();
        if back
            && let Err(error) = self.read(|conn| {
                Ok(store::set_work_recycled(
                    conn,
                    &snapshot.job.id,
                    ordinal,
                    &[],
                )?)
            })
        {
            tracing::warn!(%error, "recycle record not cleared after putting the file back");
        }
        back
    }

    /// A duplicate resolution paused or stopped between its recycle and
    /// its publish: put the recycled files back.
    pub(super) fn put_back_unfinished(&self, snapshot: &Snapshot) {
        let ordinals = self
            .read(|conn| Ok(store::unfinished_recycles(conn, &snapshot.job.id)?))
            .unwrap_or_default();
        for ordinal in ordinals {
            let moves = self
                .read(|conn| Ok(store::work_recycled(conn, &snapshot.job.id, ordinal)?))
                .unwrap_or_default();
            if !self.put_back_recycled(snapshot, ordinal, &moves) {
                tracing::error!(
                    job_id = snapshot.job.id,
                    "a recycled file could not be put back; it stays in the recycle bin"
                );
            }
        }
    }

    fn source_item(
        &self,
        source: &Snapshot,
        ordinal: i64,
    ) -> Result<super::models::PlannedItem, ManagementError> {
        let page = self.read(|conn| {
            Ok(store::items_page(
                conn,
                &source.job.id,
                ordinal - 1,
                1,
                &ItemFilter::default(),
            )?)
        })?;
        page.items
            .into_iter()
            .find(|item| item.ordinal == ordinal)
            .ok_or(ManagementError::NotFound(
                "That file is not in the preview.",
            ))
    }

    /// Plan the settled file: fresh from the profile, then given the
    /// destination the chosen action asks for.
    pub(super) fn plan_duplicate(
        &self,
        snapshot: &Snapshot,
        worker: &str,
    ) -> Result<Option<Summary>, ManagementError> {
        let intent = snapshot
            .intent()
            .duplicate
            .ok_or_else(|| ManagementError::Store("duplicate job has no request".to_owned()))?;
        let selection: Selection = serde_json::from_str(&snapshot.selection_json)?;
        let pinned = Self::pinned(snapshot)?;
        let scripts = CompiledScripts::new(&pinned);
        let sandbox = self.setup.publish_sandbox().map_err(|error| {
            ManagementError::Conflict(format!("No library folder can be written: {error}"))
        })?;
        let registry = self.setup.live_registry();
        let ctx = PlanContext {
            setup: &self.setup,
            pinned: &pinned,
            scripts: &scripts,
            sandbox: &sandbox,
            registry: &registry,
            target_root: snapshot.target_root_id.as_deref(),
            manage_unconfirmed: planner::manage_unconfirmed(&self.setup),
            tag_edit: None,
        };
        let albums = self.read(|conn| selection::albums(conn, &selection))?;
        let Some(scope) = albums.first() else {
            return Ok(Some(Summary::default()));
        };
        let tracks = self.read(|conn| selection::tracks(conn, scope))?;
        let Some(track) = tracks.first() else {
            return Ok(Some(Summary::default()));
        };
        let mut item = planner::plan_one(&ctx, scope, track, 0)?;
        let existing_there = sandbox
            .resolve_no_symlink(&intent.existing_root_id, &intent.existing_relative_path)
            .is_ok_and(|path| path.symlink_metadata().is_ok());
        let still_clashes = item.dest_root.as_deref() == Some(intent.existing_root_id.as_str())
            && item.dest_rel.as_deref().is_some_and(|rel| {
                crate::library::publish::collision_key(&intent.existing_root_id, rel)
                    == crate::library::publish::collision_key(
                        &intent.existing_root_id,
                        &intent.existing_relative_path,
                    )
            });
        if item.eligibility.writes() && (!existing_there || !still_clashes) {
            item.block(duplicate_changed());
        }
        // Placing this one file must not split its album: when the rest
        // of the album moves too, only keeping both (or recycling the new
        // file) is allowed here.
        let places_incoming = matches!(
            intent.action,
            DuplicateAction::KeepIncomingAlternate | DuplicateAction::RecycleExistingKeepIncoming
        );
        if item.eligibility.writes() && places_incoming {
            let source = self.snapshot(&intent.source_job_id)?;
            let blocked_one = self.source_item(&source, intent.source_plan_item_ordinal)?;
            let siblings = self.read(|conn| {
                Ok(store::bundle_items(
                    conn,
                    &intent.source_job_id,
                    blocked_one.bundle_ordinal,
                )?)
            })?;
            let album_moves = siblings.iter().any(|sibling| {
                sibling.ordinal != blocked_one.ordinal
                    && (sibling.diff.path_changed
                        || sibling
                            .dest_rel
                            .as_ref()
                            .is_some_and(|dest| dest != &sibling.source_rel))
            });
            if album_moves {
                item.block(album_moves_too());
            }
        }
        if item.eligibility.writes() {
            match intent.action {
                DuplicateAction::KeepExisting => {
                    item.desired.plan = None;
                    item.dest_root = Some(item.source_root.clone());
                    item.dest_rel = Some(item.source_rel.clone());
                    item.diff = Default::default();
                    item.facts
                        .warnings
                        .push("Both files stay where they are; nothing is written.".to_owned());
                    item.eligibility = Eligibility::Warning;
                }
                DuplicateAction::KeepIncomingAlternate => {
                    let alternate = intent.alternate_relative_path.clone().unwrap_or_default();
                    let same_extension = Path::new(&alternate)
                        .extension()
                        .map(|ext| ext.to_ascii_lowercase())
                        == Path::new(&item.source_rel)
                            .extension()
                            .map(|ext| ext.to_ascii_lowercase());
                    if !same_extension {
                        item.block(ItemReason {
                            code: "PATH_INVALID".to_owned(),
                            message: "The other path must keep the file's extension.".to_owned(),
                            action: "Pick a name that ends like the file does.".to_owned(),
                        });
                    } else if let Some(plan) = item.desired.plan.as_mut() {
                        plan.dest_rel = alternate.clone();
                        plan.kind = if plan.source_root == plan.dest_root
                            && plan.source_rel == plan.dest_rel
                        {
                            PlanKind::SamePath
                        } else {
                            PlanKind::Move
                        };
                        item.dest_rel = Some(alternate);
                        item.diff.path_changed = plan.kind == PlanKind::Move;
                        planner::check_destination(&ctx, &mut item);
                    } else {
                        item.block(nothing_to_place());
                    }
                }
                DuplicateAction::RecycleIncomingKeepExisting => {
                    // Nothing is published: applying moves this file to
                    // the recycle bin.
                    item.desired.plan = None;
                    item.dest_root = None;
                    item.dest_rel = None;
                    item.diff = Default::default();
                    item.diff.path_changed = true;
                    item.facts.warnings.push(
                        "This file moves to the recycle bin; the file in its way stays.".to_owned(),
                    );
                    item.eligibility = Eligibility::Warning;
                }
                DuplicateAction::RecycleExistingKeepIncoming => {
                    // The file in the way goes to the recycle bin first;
                    // this file then takes its place.
                    let existing = sandbox
                        .resolve_no_symlink(
                            &intent.existing_root_id,
                            &intent.existing_relative_path,
                        )
                        .ok()
                        .and_then(|path| {
                            crate::library::publish::paths::read_regular_file(&path).ok()
                        })
                        .map(|bytes| crate::library::publish::snapshots::sha256_hex(&bytes));
                    let tracked: Option<String> = self.read(|conn| {
                        Ok(conn
                            .query_row(
                                "SELECT id FROM local_tracks WHERE root_id = ?1 \
                                 AND relative_path = ?2 AND availability = 'indexed'",
                                rusqlite::params![
                                    intent.existing_root_id,
                                    intent.existing_relative_path
                                ],
                                |row| row.get(0),
                            )
                            .optional()?)
                    })?;
                    let named_other = intent
                        .existing_local_track_id
                        .as_ref()
                        .is_some_and(|expected| tracked.as_ref() != Some(expected));
                    match (item.desired.plan.is_some(), existing) {
                        (true, Some(sha256)) if !named_other => {
                            item.desired.recycle_first = Some(RecycleFirst {
                                root_id: intent.existing_root_id.clone(),
                                relative_path: intent.existing_relative_path.clone(),
                                sha256,
                                local_track_id: tracked,
                            });
                            item.facts.warnings.push(format!(
                                "{} moves to the recycle bin first, then this file takes its place.",
                                intent.existing_relative_path
                            ));
                            item.eligibility = Eligibility::Warning;
                        }
                        (false, _) => item.block(nothing_to_place()),
                        _ => item.block(duplicate_changed()),
                    }
                }
            }
        }
        let mut summary = Summary {
            bundle_count: 1,
            selected_item_count: Some(1),
            expanded_track_count: 1,
            ..Summary::default()
        };
        summary.count(&item);
        if !self.keep_items(snapshot, worker, std::slice::from_ref(&item))? {
            return Ok(None);
        }
        Ok(Some(summary))
    }
}
