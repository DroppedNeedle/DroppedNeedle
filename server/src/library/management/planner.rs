//! Plan one album: decide, file by file, what its tags and path should
//! be, and give every file that is left alone one reason.
//!
//! Planning only reads. It pins what apply needs (source bytes, identity,
//! the exact tags to write, the destination) so apply can refuse anything
//! that moved underneath. An album is all or nothing: when one file is
//! blocked, the others wait with a reason saying so.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use rusqlite::{Connection, OptionalExtension as _, params};

use super::models::{
    DesiredDocument, DesiredField, Eligibility, ItemDiff, ItemFacts, ManagementError,
    PinnedProfile, PlannedItem,
};
use super::profile::{self, CompiledScripts, NamingFacts};
use super::reasons::{self, ItemReason};
use super::script;
use super::selection::{AlbumScope, CatalogTrack};
use crate::library::identify::stores::{IdentityStore as _, ReleaseStore as _};
use crate::library::matching::model::Release;
use crate::library::publish::planner::{
    Capability, CollisionGate, DiskPreflight, FileFingerprint, PlanBundle, PlanItem, PlanKind,
    ReleaseIdentity,
};
use crate::library::publish::snapshots::sha256_hex;
use crate::library::publish::{MAX_PLAN_SUBJECTS, PublishError, Sandbox, collision_key};
use crate::library::scan::models::EffectivePolicy;
use crate::library::scan::roots::RootRegistry;
use crate::library::tags::{self, TagField};
use crate::library::wiring::LibrarySetup;

/// Staged-bytes headroom over the source size for disk preflight.
const STAGED_HEADROOM_BYTES: u64 = 65_536;

/// Why an album's match keeps its files from being managed, if it does.
/// Only "no row" means confirmed; a read failure refuses, so an unsure
/// album's files are never changed by accident.
pub(crate) fn match_gate(
    conn: &Connection,
    album_id: &str,
    manage_unconfirmed: bool,
) -> rusqlite::Result<Option<ItemReason>> {
    if manage_unconfirmed {
        return Ok(None);
    }
    let state = conn
        .query_row(
            "SELECT state, reason_code FROM library_album_match_state WHERE local_album_id = ?1",
            params![album_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    Ok(match state {
        Some((_, code)) if code.starts_with("CHOSEN_") => {
            Some(crate::library::operations::reasons::match_reason(&code).into())
        }
        Some((state, _)) if state == "unconfirmed" => Some(reasons::unconfirmed_match()),
        Some((state, _)) if state == "unmatched" => Some(reasons::unmatched_album()),
        _ => None,
    })
}

/// Whether management may touch unconfirmed matches, read per call.
pub(crate) fn manage_unconfirmed(setup: &LibrarySetup) -> bool {
    setup
        .config
        .get::<crate::runtime_config::sections::EditionPreferences>()
        .map(|prefs| prefs.manage_unconfirmed)
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "edition preferences unreadable; unconfirmed albums wait");
            false
        })
}

/// Per-file tag overrides an administrator saved (album-wide and for the
/// file), and their revision. A changed override makes apply refuse.
#[derive(Debug, Clone, Default)]
pub struct Overrides {
    pub fields: Vec<(TagField, String, Vec<String>)>,
    pub revision: u64,
}

fn override_values(json: &str) -> Vec<String> {
    match serde_json::from_str::<serde_json::Value>(json) {
        Ok(serde_json::Value::Array(values)) => values
            .into_iter()
            .map(|value| match value {
                serde_json::Value::String(text) => text,
                other => other.to_string(),
            })
            .collect(),
        Ok(serde_json::Value::Null) | Err(_) => Vec::new(),
        Ok(serde_json::Value::String(text)) => vec![text],
        Ok(other) => vec![other.to_string()],
    }
}

/// Saved overrides for one file.
pub fn overrides(conn: &Connection, album_id: &str, track_id: &str) -> rusqlite::Result<Overrides> {
    let mut statement = conn.prepare_cached(
        "SELECT id, row_revision, field_name, mode, value_json FROM library_management_overrides \
         WHERE local_album_id = ?1 OR local_track_id = ?2 \
         ORDER BY subject_kind, field_name, id",
    )?;
    let rows = statement.query_map(params![album_id, track_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;
    let mut fields = Vec::new();
    let mut fingerprint = String::new();
    for row in rows {
        let (id, revision, name, mode, value) = row?;
        fingerprint.push_str(&format!("{id}:{revision};"));
        if let Some(field) = profile::tag_field(&name) {
            fields.push((field, mode, override_values(&value)));
        }
    }
    Ok(Overrides {
        fields,
        revision: if fingerprint.is_empty() {
            0
        } else {
            profile::seal_revision(&fingerprint)
        },
    })
}

/// Everything planning needs that is the same for every album.
pub struct PlanContext<'a> {
    pub setup: &'a LibrarySetup,
    pub pinned: &'a PinnedProfile,
    pub scripts: &'a CompiledScripts,
    pub sandbox: &'a Sandbox,
    pub registry: &'a RootRegistry,
    pub target_root: Option<&'a str>,
    pub manage_unconfirmed: bool,
}

/// What an album's own state says before any file is read.
enum AlbumState {
    Blocked(ItemReason),
    Ready {
        release: Box<Release>,
        release_mbid: String,
        release_group_mbid: String,
        album_identity_revision: u64,
    },
}

fn album_state(ctx: &PlanContext<'_>, album_id: &str) -> Result<AlbumState, ManagementError> {
    let store = &ctx.setup.identify_store;
    let Some(identity) = store.album_identity(album_id) else {
        return Ok(AlbumState::Blocked(reasons::identity_not_accepted()));
    };
    let gate = store
        .with_connection(|conn| match_gate(conn, album_id, ctx.manage_unconfirmed))
        .map_err(|error| {
            tracing::error!(%error, album = album_id, "match state unreadable; management refused");
            ManagementError::from(error)
        })?;
    if let Some(reason) = gate {
        return Ok(AlbumState::Blocked(reason));
    }
    let (Some(release_mbid), Some(release_group_mbid)) = (
        identity.release_mbid.clone(),
        identity.release_group_mbid.clone(),
    ) else {
        return Ok(AlbumState::Blocked(reasons::release_not_selected()));
    };
    let Some(release) = store.release(&release_mbid, None) else {
        return Ok(AlbumState::Blocked(reasons::release_unavailable()));
    };
    Ok(AlbumState::Ready {
        release: Box::new(release),
        release_mbid,
        release_group_mbid,
        album_identity_revision: identity.row_revision,
    })
}

fn base_item(
    track: &CatalogTrack,
    scope: &AlbumScope,
    ordinal: i64,
    bundle_ordinal: i64,
) -> PlannedItem {
    PlannedItem {
        ordinal,
        bundle_ordinal,
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
        desired: DesiredDocument::default(),
        diff: ItemDiff::default(),
        facts: ItemFacts {
            audio_format: track.file_format.to_lowercase(),
            catalog_track_title: Some(track.title.clone()),
            catalog_artist_name: track.artist_name.clone(),
            catalog_album_title: Some(track.album_title.clone()),
            catalog_track_number: Some(track.track_number),
            catalog_disc_number: Some(track.disc_number),
            ..ItemFacts::default()
        },
    }
}

/// The reason a path problem gives.
fn path_reason(error: &PublishError) -> ItemReason {
    match error {
        PublishError::UnsafePath(_) => reasons::symlink_unsupported(),
        _ => reasons::file_unreadable(),
    }
}

/// Where a file goes, from the naming script and the rename and move
/// switches. `None` keeps its path.
fn destination(
    ctx: &PlanContext<'_>,
    release: &Release,
    track: &CatalogTrack,
    disc: u32,
    fields: &BTreeMap<String, Vec<String>>,
) -> Result<Option<(String, String)>, ItemReason> {
    let organization = &ctx.pinned.profile.organization;
    let dest_root = ctx.target_root.unwrap_or(&track.root_id).to_owned();
    if !organization.rename_enabled && !organization.move_enabled && ctx.target_root.is_none() {
        return Ok(None);
    }
    let root = ctx
        .registry
        .resolve(&dest_root)
        .filter(|root| root.policy != EffectivePolicy::Excluded)
        .ok_or_else(reasons::root_read_only)?;
    let naming = match &ctx.pinned.multi_disc_naming_script {
        Some(multi) if release.media.len() > 1 => multi,
        _ => &ctx.pinned.naming_script,
    };
    let source = Path::new(&track.relative_path);
    let extension = source
        .extension()
        .map(|ext| ext.to_string_lossy().into_owned())
        .unwrap_or_default();
    let medium = release.media.iter().find(|medium| medium.position == disc);
    let facts = NamingFacts {
        extension: &script::cased_extension(&extension, organization.compatibility.extension_case),
        codec: &track.file_format,
        bit_rate: track.bit_rate,
        sample_rate: track.sample_rate,
        bit_depth: track.bit_depth,
        medium_format: medium.and_then(|m| m.format.clone()).unwrap_or_default(),
        medium_number: i64::from(disc),
    };
    let variables = profile::naming_variables(fields, &facts);
    let rendered = script::render_path(
        &naming.source,
        &variables,
        &organization.compatibility,
        &root.path.to_string_lossy(),
    )
    .map_err(|fault| {
        if fault.path_too_long {
            reasons::path_too_long()
        } else {
            reasons::script_failed(&naming.name, &fault.to_string())
        }
    })?;
    let rendered = Path::new(&rendered);
    let parent = if organization.move_enabled || ctx.target_root.is_some() {
        rendered.parent()
    } else {
        source.parent()
    };
    let name = if organization.rename_enabled {
        rendered.file_name()
    } else {
        source.file_name()
    };
    let Some(name) = name else {
        return Err(reasons::script_failed(
            &naming.name,
            "it made an empty file name",
        ));
    };
    let dest = match parent {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(name),
        _ => Path::new(name).to_path_buf(),
    };
    Ok(Some((dest_root, dest.to_string_lossy().into_owned())))
}

/// Plan one file. Errors are the file's reason to be left alone.
#[allow(clippy::too_many_arguments)]
fn plan_file(
    ctx: &PlanContext<'_>,
    item: &mut PlannedItem,
    track: &CatalogTrack,
    release: &Release,
    release_mbid: &str,
    release_group_mbid: &str,
    album_identity_revision: u64,
    overrides: &Overrides,
) -> Result<(), ItemReason> {
    let store = &ctx.setup.identify_store;
    let mapping = store
        .track_identity(&track.id)
        .ok_or_else(reasons::track_not_mapped)?;
    let (Some(recording_mbid), Some(release_track_mbid)) = (
        mapping.recording_mbid.clone(),
        mapping.release_track_mbid.clone(),
    ) else {
        return Err(reasons::track_not_mapped());
    };
    let release_track = release
        .tracks
        .iter()
        .find(|candidate| candidate.id.eq_ignore_ascii_case(&release_track_mbid))
        .ok_or_else(reasons::track_not_mapped)?;
    let format = tags::format_for_path(Path::new(&track.relative_path))
        .map_err(|_| reasons::format_unsupported(&track.file_format))?;
    let format_name = format.as_str().to_owned();
    item.facts.audio_format = format_name.clone();
    if ctx
        .registry
        .resolve(&track.root_id)
        .is_none_or(|root| root.policy == EffectivePolicy::Excluded)
    {
        return Err(reasons::root_unavailable());
    }
    let path = ctx
        .sandbox
        .resolve_no_symlink(&track.root_id, &track.relative_path)
        .map_err(|error| path_reason(&error))?;
    let bytes = crate::library::publish::paths::read_regular_file(&path)
        .map_err(|error| path_reason(&error))?;
    let fingerprint = FileFingerprint {
        size: bytes.len() as u64,
        sha256: sha256_hex(&bytes),
    };
    drop(bytes);
    let doc = crate::library::publish::staging::document_from_file(&path).map_err(|error| {
        tracing::info!(%error, track = track.id, "tags unreadable at preview");
        reasons::file_unreadable()
    })?;
    let writable = tags::save::writable(format);
    let tag_plan = profile::plan_tags(
        ctx.pinned,
        ctx.scripts,
        &profile::FileInput {
            release,
            track: release_track,
            doc: &doc,
            format: &format_name,
            writable,
            overrides,
        },
    )?;
    item.facts
        .warnings
        .extend(tag_plan.warnings.iter().cloned());
    item.desired.fields = tag_plan
        .desired
        .iter()
        .map(|(name, value)| DesiredField {
            name: name.clone(),
            value: profile::json_value(value),
        })
        .collect();
    let destination = destination(ctx, release, track, release_track.disc, &tag_plan.desired)?;
    let (dest_root, dest_rel) =
        destination.unwrap_or_else(|| (track.root_id.clone(), track.relative_path.clone()));
    let moves = dest_root != track.root_id || dest_rel != track.relative_path;
    item.dest_root = Some(dest_root.clone());
    item.dest_rel = Some(dest_rel.clone());
    item.fingerprint = fingerprint.sha256.clone();
    item.diff.field_mutations = tag_plan.mutations;
    item.diff.tags_changed = !tag_plan.updates.is_empty();
    item.diff.path_changed = moves;
    if !item.facts.warnings.is_empty() {
        item.eligibility = Eligibility::Warning;
    }
    if !item.diff.changes_anything() {
        return Ok(());
    }
    let mut capabilities = Vec::new();
    if item.diff.tags_changed {
        capabilities.push(Capability::Metadata);
    }
    if moves {
        capabilities.push(if dest_root == track.root_id {
            Capability::SameRootMove
        } else {
            Capability::CrossRootMove
        });
    }
    item.estimated_bytes = fingerprint.size + STAGED_HEADROOM_BYTES;
    item.desired.plan = Some(PlanItem {
        track_id: track.id.clone(),
        source_root: track.root_id.clone(),
        source_rel: track.relative_path.clone(),
        dest_root,
        dest_rel,
        kind: if moves {
            PlanKind::Move
        } else {
            PlanKind::SamePath
        },
        fingerprint,
        identity: ReleaseIdentity {
            release_mbid: release_mbid.to_owned(),
            release_group_mbid: release_group_mbid.to_owned(),
            recording_mbid,
            release_track_mbid,
            album_identity_revision,
            mapping_revision: mapping.row_revision,
        },
        override_revision: overrides.revision,
        capabilities,
        format: format_name,
        managed_updates: tag_plan.updates,
        sidecars: Vec::new(),
        staged_bytes_estimate: item.estimated_bytes,
        adopt: None,
    });
    Ok(())
}

/// Block files whose destination is taken: by another file of this run
/// or by a file on disk that is not the file itself.
fn check_collisions(ctx: &PlanContext<'_>, items: &mut [PlannedItem]) {
    let mut seen = HashSet::new();
    for item in items.iter_mut() {
        let Some(plan) = item.desired.plan.clone() else {
            if let (Some(root), Some(rel)) = (&item.dest_root, &item.dest_rel) {
                seen.insert(collision_key(root, rel));
            }
            continue;
        };
        if !seen.insert(collision_key(&plan.dest_root, &plan.dest_rel)) {
            item.block(reasons::position_collision());
            continue;
        }
        if plan.kind == PlanKind::SamePath {
            continue;
        }
        let single = PlanBundle {
            id: String::new(),
            items: vec![plan.clone()],
            profile_revision: 0,
            naming_revision: 0,
            policy_revision: 0,
            catalog_revision: 0,
        };
        match CollisionGate::check_bundle(ctx.sandbox, &single) {
            Ok(()) => {}
            Err(PublishError::Collision(_)) => {
                let identical = ctx
                    .sandbox
                    .resolve_no_symlink(&plan.dest_root, &plan.dest_rel)
                    .ok()
                    .and_then(|path| crate::library::publish::paths::read_regular_file(&path).ok())
                    .is_some_and(|bytes| sha256_hex(&bytes) == plan.fingerprint.sha256);
                item.block(reasons::path_collision(identical));
            }
            Err(error) => item.block(path_reason(&error)),
        }
    }
}

/// Plan every selected file of one album, numbering items from
/// `first_ordinal`.
pub fn plan_album(
    ctx: &PlanContext<'_>,
    scope: &AlbumScope,
    tracks: &[CatalogTrack],
    bundle_ordinal: i64,
    first_ordinal: i64,
) -> Result<Vec<PlannedItem>, ManagementError> {
    let mut items: Vec<PlannedItem> = tracks
        .iter()
        .enumerate()
        .map(|(index, track)| base_item(track, scope, first_ordinal + index as i64, bundle_ordinal))
        .collect();
    if tracks.len() > MAX_PLAN_SUBJECTS {
        for item in &mut items {
            item.block(reasons::bundle_too_large(MAX_PLAN_SUBJECTS));
        }
        return Ok(items);
    }
    let (release, release_mbid, release_group_mbid, album_identity_revision) =
        match album_state(ctx, &scope.album_id)? {
            AlbumState::Blocked(reason) => {
                for item in &mut items {
                    item.block(reason.clone());
                }
                return Ok(items);
            }
            AlbumState::Ready {
                release,
                release_mbid,
                release_group_mbid,
                album_identity_revision,
            } => (
                release,
                release_mbid,
                release_group_mbid,
                album_identity_revision,
            ),
        };
    for (item, track) in items.iter_mut().zip(tracks) {
        let overrides = ctx
            .setup
            .identify_store
            .with_connection(|conn| overrides(conn, &scope.album_id, &track.id))?;
        if let Err(reason) = plan_file(
            ctx,
            item,
            track,
            &release,
            &release_mbid,
            &release_group_mbid,
            album_identity_revision,
            &overrides,
        ) {
            item.block(reason);
        }
    }
    check_collisions(ctx, &mut items);
    let writes: Vec<PlanItem> = items
        .iter()
        .filter_map(|item| item.desired.plan.clone())
        .collect();
    if !writes.is_empty() {
        let bundle = PlanBundle {
            id: String::new(),
            items: writes,
            profile_revision: 0,
            naming_revision: 0,
            policy_revision: 0,
            catalog_revision: 0,
        };
        let probe = crate::library::adapters::FsSpaceProbe::new(ctx.setup.root_dirs.clone());
        if DiskPreflight::check(&bundle, &probe).is_err() {
            for item in items.iter_mut().filter(|item| item.desired.plan.is_some()) {
                item.block(reasons::insufficient_space());
            }
        }
    }
    let blocked = items
        .iter()
        .any(|item| item.eligibility == Eligibility::Blocked);
    if blocked {
        for item in items
            .iter_mut()
            .filter(|item| item.eligibility.writes() && item.diff.changes_anything())
        {
            item.block(reasons::bundle_blocked());
        }
    }
    Ok(items)
}

/// Group a bundle's writable plans back into what apply publishes.
pub fn bundle_plans(items: &[PlannedItem]) -> Vec<PlanItem> {
    items
        .iter()
        .filter(|item| item.eligibility.writes())
        .filter_map(|item| item.desired.plan.clone())
        .collect()
}
