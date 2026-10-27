//! A file a preview could not place because another file already sits
//! at its destination. The administrator picks what to do: keep the
//! existing file (nothing moves), or put the incoming file at another
//! path next to it, or let one replace the other (the replaced file goes
//! to the recycle bin). Each is a small preview of its own, applied
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
use super::service::{JobSpec, Management, Refused, stale};
use super::store::{self, Snapshot};
use crate::mutations::{MutationError, NO_RECYCLE_BIN, REMOVAL_CHANGED};
use crate::publish::planner::PlanKind;
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

fn case_only_clash() -> ItemReason {
    ItemReason {
        code: "CASE_ONLY_CLASH".to_owned(),
        message: "The file in the way has the same name in different letter case, so it cannot be replaced in place."
            .to_owned(),
        action: "Keep both files, or rename one of them yourself, then make a new preview of the album."
            .to_owned(),
    }
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
        MutationError::Conflict(reason) if reason.code == REMOVAL_CHANGED.code => {
            ManagementError::Conflict(format!(
                "{} {}",
                duplicate_changed().message,
                duplicate_changed().action
            ))
        }
        MutationError::Conflict(reason) | MutationError::Files(reason) => {
            ManagementError::Conflict(format!("{} {}", reason.message, reason.action))
        }
        MutationError::NotFound(_) => ManagementError::Conflict(format!(
            "{} {}",
            duplicate_changed().message,
            duplicate_changed().action
        )),
        MutationError::Store(cause) => ManagementError::Store(cause),
        MutationError::Busy(cause) => ManagementError::Busy(cause),
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
        let registry = self.check_revisions(
            &stored,
            &request.expected_settings_revision,
            &request.expected_policy_revision,
        )?;
        if source.settings_revision != crate::management::settings::settings_revision(&stored) {
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
    /// keeps the file in a dated folder, so it can be put back from the
    /// Recycle Bin page.
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
        let mutations = crate::mutations::Mutations::new(&self.setup);
        let changed = || {
            ManagementError::Conflict(format!(
                "{} {}",
                duplicate_changed().message,
                duplicate_changed().action
            ))
        };
        match intent.action {
            DuplicateAction::RecycleIncomingKeepExisting => {
                let item = self.source_item(snapshot, 0)?;
                let track_id = item.track_id.clone().ok_or_else(changed)?;
                // The removal itself checks the track is still at that path
                // and its file still holds the bytes the preview read.
                mutations
                    .recycle_duplicate(
                        &track_id,
                        (&item.source_root, &item.source_rel),
                        &item.fingerprint,
                        &actor,
                    )
                    .map_err(recycle_refused)?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    /// Give the publisher the file a duplicate resolution replaces: it
    /// is set aside under the publish journal, recycled to a fresh bin
    /// entry after commit, and put back if the publish unwinds.
    /// While the download settings cannot be read no bin is known, and
    /// the job waits as it does for a busy database.
    pub(super) fn plan_replacements(
        &self,
        items: &[super::models::PlannedItem],
        plans: &mut [crate::publish::PlanItem],
    ) -> Result<(), Refused> {
        let mutations = crate::mutations::Mutations::new(&self.setup);
        for item in items {
            let Some(first) = &item.desired.recycle_first else {
                continue;
            };
            let Some(track_id) = item.track_id.as_deref() else {
                continue;
            };
            let name = Path::new(&first.relative_path)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            // While no bin is known the job waits; a folder with no usable
            // bin refuses only its own album, and the job moves on.
            let recycle_to = match mutations.recycle_entry(&first.root_id, &name) {
                Ok(entry) => entry,
                Err(MutationError::Conflict(reason)) if reason.code == NO_RECYCLE_BIN.code => {
                    return Err(Refused::Reason(ItemReason {
                        code: "RECYCLE_FAILED".to_owned(),
                        message: format!(
                            "{}'s library folder has no usable recycle bin, so it cannot be replaced.",
                            first.relative_path
                        ),
                        action: "Make the folder's .recycle a plain folder, or keep both files."
                            .to_owned(),
                    }));
                }
                Err(MutationError::Conflict(reason)) => {
                    return Err(Refused::Busy(ManagementError::Busy(
                        reason.message.to_owned(),
                    )));
                }
                Err(error) => return Err(Refused::Busy(ManagementError::Busy(error.to_string()))),
            }
            .ok_or_else(|| ItemReason {
                    code: "RECYCLE_FAILED".to_owned(),
                    message: format!(
                        "No recycle bin is available for {}, so it cannot be replaced.",
                        first.relative_path
                    ),
                    action: "Set a recycle bin in the download settings, or keep both files."
                        .to_owned(),
                })?;
            for plan in plans.iter_mut().filter(|plan| plan.track_id == track_id) {
                plan.replaces = Some(crate::publish::Replacement {
                    sha256: first.sha256.clone(),
                    track_id: first.local_track_id.clone(),
                    recycle_to: recycle_to.display().to_string(),
                });
            }
        }
        Ok(())
    }

    fn source_item(
        &self,
        source: &Snapshot,
        ordinal: i64,
    ) -> Result<super::models::PlannedItem, ManagementError> {
        let page = self
            .setup
            .db
            .read_blocking("management.source_item", |conn| {
                Ok::<_, ManagementError>(store::items_page(
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
        let sandbox = self.setup.publishing.publish_sandbox().map_err(|error| {
            ManagementError::Conflict(format!("No library folder can be written: {error}"))
        })?;
        let registry = self.setup.core.live_registry();
        let ctx = PlanContext {
            setup: &self.setup,
            pinned: &pinned,
            scripts: &scripts,
            sandbox: &sandbox,
            registry: &registry,
            target_root: snapshot.target_root_id.as_deref(),
            manage_unconfirmed: planner::manage_unconfirmed(&self.setup),
            tag_edit: None,
            job_id: &snapshot.job.id,
            analysis_until: std::time::Instant::now() + planner::RUN_ANALYSIS_BUDGET,
            wide: false,
        };
        let albums = self
            .setup
            .db
            .read_blocking("management.duplicate_albums", |conn| {
                selection::albums(conn, &selection)
            })?;
        let Some(scope) = albums.first() else {
            return Ok(Some(Summary::default()));
        };
        let tracks = self
            .setup
            .db
            .read_blocking("management.duplicate_tracks", |conn| {
                selection::tracks(conn, scope)
            })?;
        let Some(track) = tracks.first() else {
            return Ok(Some(Summary::default()));
        };
        let mut item = planner::plan_one(&ctx, scope, track, 0)?;
        let existing_there = sandbox
            .resolve_no_symlink(&intent.existing_root_id, &intent.existing_relative_path)
            .is_ok_and(|path| path.symlink_metadata().is_ok());
        let still_clashes = item.dest_root.as_deref() == Some(intent.existing_root_id.as_str())
            && item.dest_rel.as_deref().is_some_and(|rel| {
                crate::publish::collision_key(&intent.existing_root_id, rel)
                    == crate::publish::collision_key(
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
            let siblings =
                self.setup
                    .db
                    .read_blocking("management.duplicate_siblings", |conn| {
                        Ok::<_, ManagementError>(store::bundle_items(
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
                        .and_then(|path| crate::publish::paths::hash_regular_file(&path).ok())
                        .map(|hashed| hashed.sha256);
                    let tracked: Option<String> =
                        self.setup
                            .db
                            .read_blocking("management.duplicate_tracked", |conn| {
                                Ok::<_, ManagementError>(
                                    conn.query_row(
                                        "SELECT id FROM local_tracks WHERE root_id = ?1 \
                                 AND relative_path = ?2 AND availability = 'indexed'",
                                        rusqlite::params![
                                            intent.existing_root_id,
                                            intent.existing_relative_path
                                        ],
                                        |row| row.get(0),
                                    )
                                    .optional()?,
                                )
                            })?;
                    let named_other = intent
                        .existing_local_track_id
                        .as_ref()
                        .is_some_and(|expected| tracked.as_ref() != Some(expected));
                    // The new file takes the exact path of the one it
                    // replaces; a name that differs only in letter case
                    // is two files, settled by hand.
                    let same_name =
                        item.dest_rel.as_deref() == Some(intent.existing_relative_path.as_str());
                    let bin = crate::mutations::Mutations::new(&self.setup)
                        .bin_for(&intent.existing_root_id)
                        .map(|bin| bin.display().to_string())
                        .unwrap_or_else(|| "the recycle bin".to_owned());
                    match (item.desired.plan.is_some(), existing) {
                        (true, Some(_)) if !same_name => item.block(case_only_clash()),
                        (true, Some(sha256)) if !named_other => {
                            item.desired.recycle_first = Some(RecycleFirst {
                                root_id: intent.existing_root_id.clone(),
                                relative_path: intent.existing_relative_path.clone(),
                                sha256,
                                local_track_id: tracked,
                            });
                            item.facts.warnings.push(format!(
                                "This file takes the place of {}, which moves to the recycle bin at {bin} once this file is in place.",
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
