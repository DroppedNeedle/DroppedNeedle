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
    ArtworkChoice, DesiredDocument, DesiredField, EditionPin, Eligibility, FieldMutation, ItemDiff,
    ItemFacts, ManagementError, PinnedProfile, PlannedItem, SidecarMove, TagEditField,
    TagEditIntent, TagEditMode,
};
use super::profile::{self, CompiledScripts, NamingFacts};
use super::reasons::{self, ItemReason};
use super::script;
use super::selection::{AlbumScope, CatalogTrack};
use super::{automatic, takeover};
use crate::matching::model::Release;
use crate::publish::planner::{
    Capability, CollisionGate, DiskPreflight, FileFingerprint, OutputKind, PlanBundle, PlanItem,
    PlanKind, ReleaseIdentity, STAGED_HEADROOM_BYTES,
};
use crate::publish::snapshots::sha256_hex;
use crate::publish::{MAX_PLAN_SUBJECTS, PublishError, Sandbox, collision_key};
use crate::scan::models::EffectivePolicy;
use crate::scan::roots::RootRegistry;
use crate::scan::sqlite_store::holds_new_files;
use crate::tags::{self, TagField};
use crate::wiring::LibrarySetup;
use droppedneedle_kernel::db::Lane;

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
            Some(crate::operations::reasons::match_reason(&code).into())
        }
        Some((state, _)) if state == "unconfirmed" => Some(reasons::unconfirmed_match()),
        Some((state, _)) if state == "unmatched" => Some(reasons::unmatched_album()),
        _ => None,
    })
}

/// Whether the album's chosen edition is still being placed. Its queue row
/// stays until the files are placed or the album goes back to automatic
/// choice, and its files are about to move, so nothing may be prepared for
/// them yet.
pub(crate) fn remap_gate(
    conn: &Connection,
    album_id: &str,
) -> rusqlite::Result<Option<ItemReason>> {
    let attempts: Option<i64> = conn
        .query_row(
            "SELECT attempts FROM library_edition_remap_queue WHERE local_album_id = ?1",
            params![album_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(attempts.map(reasons::edition_remap_pending))
}

/// Whether the file sits where its folder keeps its own tags (the Local
/// metadata policy, from the folder or a path rule). Management never plans
/// its profile's tags or names for an album with such a file, whatever
/// asked for it; a person's own tag edit is the one exception.
pub(crate) fn keeps_own_tags(registry: &RootRegistry, root_id: &str, relative_path: &str) -> bool {
    registry.resolve(root_id).is_some_and(|root| {
        root.policy_for(&root.path.join(relative_path)) == EffectivePolicy::LocalMetadata
    })
}

fn album_keeps_own_tags(ctx: &PlanContext<'_>, tracks: &[CatalogTrack]) -> bool {
    tracks
        .iter()
        .any(|track| keeps_own_tags(ctx.registry, &track.root_id, &track.relative_path))
}

/// Whether management may touch unconfirmed matches, read per call.
pub(crate) fn manage_unconfirmed(setup: &LibrarySetup) -> bool {
    setup.settings.editions.manage_unconfirmed()
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
    /// A manual tag edit the preview carries.
    pub tag_edit: Option<&'a TagEditIntent>,
    /// The job planning, which owns the generated files it stores.
    pub job_id: &'a str,
    /// When loudness measuring stops for the whole run (see
    /// [`RUN_ANALYSIS_BUDGET`]).
    pub analysis_until: std::time::Instant,
    /// The preview covers more than one album. Such a preview never
    /// rewrites an album the app stepped back from: a folder, an artist
    /// or a "select all" in a list did not name that album.
    pub wide: bool,
}

/// Most time one run spends measuring loudness, over all its albums.
/// Albums past it keep their ReplayGain and say so.
pub const RUN_ANALYSIS_BUDGET: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// A file's overrides with a pending tag edit on top: a reset drops the
/// saved override so the edition's value comes back, and a written value
/// counts as an override for this run. The revision stays the saved one,
/// so apply still refuses when someone changed the overrides meanwhile.
pub fn with_tag_edit(saved: Overrides, edit: &TagEditIntent, track_id: &str) -> Overrides {
    let active: Vec<&TagEditField> = edit
        .fields
        .iter()
        .filter(|field| field.subject_kind == "album" || edit.local_track_id == track_id)
        .collect();
    if active.is_empty() {
        return saved;
    }
    let names: HashSet<&str> = active
        .iter()
        .map(|field| field.field_name.as_str())
        .collect();
    let mut fields: Vec<(TagField, String, Vec<String>)> = saved
        .fields
        .into_iter()
        .filter(|(field, _, _)| !names.contains(field.name()))
        .collect();
    if edit.mode != TagEditMode::ResetCanonical {
        for field in active {
            if let Some(tag) = profile::tag_field(&field.field_name) {
                let mode = if field.values.is_empty() {
                    "clear"
                } else {
                    "replace"
                };
                fields.push((tag, mode.to_owned(), field.values.clone()));
            }
        }
    }
    Overrides {
        fields,
        revision: saved.revision,
    }
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

/// Why an edition's tag write leaves the album alone: its identity is no
/// longer the edition the person asked for.
fn edition_changed() -> ItemReason {
    ItemReason {
        code: "EDITION_CHANGED".to_owned(),
        message: "This album's edition changed after these tags were asked for.".to_owned(),
        action: "Open the album's edition again and preview its tags.".to_owned(),
    }
}

/// Why an edition's tag write leaves the album alone: someone, or the app,
/// stepped back from it after the write was asked for.
fn edition_held() -> ItemReason {
    ItemReason {
        code: "EDITION_TAGS_HELD".to_owned(),
        message: "This album is left unmanaged, so its files keep their tags.".to_owned(),
        action: "Choose \"Manage it again\" on the album page first.".to_owned(),
    }
}

/// Whether the album is left unmanaged, by a person or by the app.
fn left_unmanaged(conn: &Connection, album_id: &str) -> rusqlite::Result<bool> {
    let excluded = conn
        .query_row(
            "SELECT 1 FROM library_management_exclusions WHERE local_album_id = ?1",
            params![album_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    Ok(excluded || takeover::is_marked(conn, album_id)?)
}

fn album_state(
    ctx: &PlanContext<'_>,
    album_id: &str,
    person: bool,
    edition: Option<&EditionPin>,
) -> Result<AlbumState, ManagementError> {
    let store = &ctx.setup.identify_store;
    let db = &ctx.setup.db;
    if person
        && let Some(reason) = db
            .read_blocking("management.remap_gate", |conn| {
                Ok::<_, ManagementError>(remap_gate(conn, album_id)?)
            })
            .map_err(|error| {
                tracing::error!(%error, album = album_id, "edition remap state unreadable; management refused");
                error
            })?
    {
        return Ok(AlbumState::Blocked(reason));
    }
    if ctx.wide
        && db.read_blocking("management.is_marked", |conn| {
            Ok::<_, ManagementError>(takeover::is_marked(conn, album_id)?)
        })?
    {
        return Ok(AlbumState::Blocked(reasons::taken_over()));
    }
    if ctx.wide
        && db.read_blocking("management.holds_new_files", |conn| {
            Ok::<_, ManagementError>(holds_new_files(conn, album_id)?)
        })?
    {
        return Ok(AlbumState::Blocked(automatic::skip_reason(
            automatic::SCAN_HELD_BACK,
        )));
    }
    let Some(identity) = store.album_identity(album_id)? else {
        return Ok(AlbumState::Blocked(reasons::identity_not_accepted()));
    };
    if let Some(pin) = edition
        && (pin.local_album_id != album_id
            || identity.release_mbid.as_deref() != Some(pin.release_mbid.as_str())
            || i64::try_from(identity.row_revision).ok() != Some(pin.identity_revision))
    {
        return Ok(AlbumState::Blocked(edition_changed()));
    }
    if edition.is_some()
        && db.read_blocking("management.edition_held", |conn| {
            Ok::<_, ManagementError>(left_unmanaged(conn, album_id)?)
        })?
    {
        return Ok(AlbumState::Blocked(edition_held()));
    }
    let gate = db
        .read_blocking("management.match_gate", |conn| {
            Ok::<_, ManagementError>(match_gate(conn, album_id, ctx.manage_unconfirmed)?)
        })
        .map_err(|error| {
            tracing::error!(%error, album = album_id, "match state unreadable; management refused");
            error
        })?;
    if let Some(reason) = gate {
        return Ok(AlbumState::Blocked(reason));
    }
    if identity.release_mbid.is_none()
        && let Some(release_group_mbid) = identity.release_group_mbid.clone()
    {
        // A custom edition plans from its sealed manifest.
        let custom = db.read_blocking("management.custom_edition", |conn| {
            Ok::<_, ManagementError>(super::custom::active(conn, album_id)?)
        })?;
        return Ok(match custom {
            Some(release) => AlbumState::Ready {
                release_mbid: release.id.clone(),
                release: Box::new(release),
                release_group_mbid,
                album_identity_revision: identity.row_revision,
            },
            None => AlbumState::Blocked(reasons::release_not_selected()),
        });
    }
    let (Some(release_mbid), Some(release_group_mbid)) = (
        identity.release_mbid.clone(),
        identity.release_group_mbid.clone(),
    ) else {
        return Ok(AlbumState::Blocked(reasons::release_not_selected()));
    };
    let Some(release) = store.release(&release_mbid, None)? else {
        return Ok(AlbumState::Blocked(reasons::release_unavailable()));
    };
    Ok(AlbumState::Ready {
        release: Box::new(release),
        release_mbid,
        release_group_mbid,
        album_identity_revision: identity.row_revision,
    })
}

/// Why a file is left out when its match could not be read.
fn match_unreadable(track_id: &str, error: &crate::identify::stores::StoreError) -> ItemReason {
    tracing::warn!(%error, track_id, "the file's match could not be read; the file is left out");
    reasons::match_unreadable()
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
        PublishError::FolderMissing => reasons::root_unavailable(),
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
        album_disambiguation: release.disambiguation.clone().unwrap_or_default(),
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
    extras: &super::enrich::AlbumExtras,
) -> Result<(), ItemReason> {
    let store = &ctx.setup.identify_store;
    let (recording_mbid, release_track_mbid, mapping_revision, release_track) =
        if super::custom::is_custom(release) {
            // A custom edition's tracks are the album's own files.
            let mapping = store
                .track_identity(&track.id)
                .map_err(|error| match_unreadable(&track.id, &error))?;
            let release_track = release
                .tracks
                .iter()
                .find(|candidate| candidate.id == track.id)
                .ok_or_else(reasons::track_not_mapped)?;
            (
                mapping
                    .as_ref()
                    .and_then(|mapping| mapping.recording_mbid.clone())
                    .unwrap_or_default(),
                String::new(),
                mapping.map_or(0, |mapping| mapping.row_revision),
                release_track,
            )
        } else {
            let mapping = store
                .track_identity(&track.id)
                .map_err(|error| match_unreadable(&track.id, &error))?
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
            (
                recording_mbid,
                release_track_mbid,
                mapping.row_revision,
                release_track,
            )
        };
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
    let hashed =
        crate::publish::paths::hash_regular_file(&path).map_err(|error| path_reason(&error))?;
    let fingerprint = FileFingerprint {
        size: hashed.size,
        sha256: hashed.sha256,
    };
    let doc = crate::publish::staging::document_from_file(&path).map_err(|error| {
        tracing::info!(%error, track = track.id, "tags unreadable at preview");
        reasons::file_unreadable()
    })?;
    let writable = tags::save::writable(format);
    let id3v23 = tags::save::id3_lands_as_v23(&path).map_err(|error| {
        tracing::info!(%error, track = track.id, "tags unreadable at preview");
        reasons::file_unreadable()
    })?;
    let lyrics = super::enrich::lyrics(
        ctx.setup.enrichment.as_deref(),
        &ctx.pinned.profile.enrichment.lyrics,
        release,
        release_track,
        extras.automatic,
    );
    let tag_plan = profile::plan_tags(
        ctx.pinned,
        ctx.scripts,
        &profile::FileInput {
            release,
            track: release_track,
            doc: &doc,
            format: &format_name,
            id3v23,
            writable,
            overrides,
            extras,
            track_id: &track.id,
            lyrics: &lyrics,
        },
    )?;
    let mut tag_plan = tag_plan;
    if writable {
        plan_pictures(ctx, item, extras, &doc, &mut tag_plan)?;
    }
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
    item.diff.scrubbed_raw_tags = tag_plan
        .scrubbed
        .iter()
        .map(|(key, values)| {
            // ID3 comment, link, private and object frames are listed as
            // raw payloads: the preview shows their size, not the bytes.
            let binary = key
                .strip_prefix("id3:")
                .is_some_and(|id| !id.starts_with('T'));
            super::models::ScrubbedTag {
                key: key.clone(),
                value_kind: if binary { "binary" } else { "text" }.to_owned(),
                value_count: values.len(),
                values: if binary {
                    values
                        .iter()
                        .map(|value| {
                            let padding = value.bytes().rev().take_while(|b| *b == b'=').count();
                            format!("{} bytes", (value.len() / 4 * 3).saturating_sub(padding))
                        })
                        .collect()
                } else {
                    values.clone()
                },
                truncated: false,
            }
        })
        .collect();
    item.diff.deferred_sources = tag_plan.deferred.clone();
    item.diff.field_mutations = tag_plan.mutations;
    item.diff.tags_changed = !tag_plan.updates.is_empty();
    if item.diff.tags_changed {
        // A tag write that also changes the file's ID3 version says so.
        let notes = tags::save::id3_upgrade_notes(&path).map_err(|error| {
            tracing::info!(%error, track = track.id, "tags unreadable at preview");
            reasons::file_unreadable()
        })?;
        item.facts.warnings.extend(notes);
        let kept = tags::save::tags_kept(&path).map_err(|error| {
            tracing::info!(%error, track = track.id, "tags unreadable at preview");
            reasons::file_unreadable()
        })?;
        if let Some(message) = kept.refusal {
            return Err(reasons::tags_not_kept(&message));
        }
        // A scrub leaves these alone; the preview says so.
        let metadata = &ctx.pinned.profile.metadata;
        if metadata.enabled && metadata.scrub_unmanaged_tags {
            item.facts
                .warnings
                .extend(left_alone_note(&kept.left_alone));
        }
    }
    item.diff.path_changed = moves;
    let mut outputs = Vec::new();
    let lyrics_settings = &ctx.pinned.profile.enrichment.lyrics;
    let lrc_rel = std::path::Path::new(&dest_rel)
        .with_extension("lrc")
        .to_string_lossy()
        .replace('\\', "/");
    let lrc_there = ctx
        .sandbox
        .resolve_no_symlink(&dest_root, &lrc_rel)
        .is_ok_and(|path| path.symlink_metadata().is_ok());
    if lyrics_settings.enabled
        && lyrics_settings.write_synced
        && let super::enrich::LyricsAnswer::Found(found) = &lyrics
        && let Some(synced) = &found.synced
    {
        if lrc_there {
            let same = ctx
                .sandbox
                .resolve_no_symlink(&dest_root, &lrc_rel)
                .is_ok_and(|path| {
                    crate::publish::paths::holds(&path, &sha256_hex(synced.as_bytes()))
                });
            if !same {
                item.facts
                    .warnings
                    .push(format!("{lrc_rel} already exists, so its lyrics are kept."));
            }
        } else {
            let output = generated_output(
                ctx,
                &dest_root,
                &lrc_rel,
                OutputKind::Lyrics,
                synced.as_bytes(),
            )?;
            outputs.push(output);
        }
    }
    if extras.cover_owner.as_deref() == Some(track.id.as_str())
        && let Some(output) = cover_file(ctx, item, extras, &dest_root, &dest_rel, track)?
    {
        outputs.push(output);
    }
    if lyrics_settings.enabled && lyrics_settings.required {
        let embedded = tag_plan
            .desired
            .get(TagField::Lyrics.name())
            .is_some_and(|values| values.iter().any(|value| !value.trim().is_empty()));
        if !embedded && outputs.is_empty() && !lrc_there {
            return Err(reasons::metadata_unavailable("lyrics"));
        }
    }
    if lyrics_settings.enabled && lyrics == super::enrich::LyricsAnswer::Deferred {
        item.facts
            .warnings
            .push("LRCLIB could not be reached, so no lyrics were looked up.".to_owned());
    }
    item.diff.generated = outputs
        .iter()
        .map(|output| super::models::GeneratedFile {
            kind: output.kind.as_str().to_owned(),
            destination_relative_path: output.dest_rel.clone(),
        })
        .collect();
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
    if item.diff.artwork_changed {
        capabilities.push(Capability::Artwork);
    }
    if moves {
        capabilities.push(if dest_root == track.root_id {
            Capability::SameRootMove
        } else {
            Capability::CrossRootMove
        });
    }
    item.estimated_bytes = fingerprint.size
        + STAGED_HEADROOM_BYTES
        + outputs.iter().map(|output| output.size).sum::<u64>();
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
            mapping_revision,
        },
        override_revision: overrides.revision,
        capabilities,
        format,
        managed_updates: tag_plan.updates,
        sidecars: Vec::new(),
        staged_bytes_estimate: item.estimated_bytes,
        adopt: None,
        replaces: None,
        outputs,
        removals: Vec::new(),
        sealed: None,
    });
    Ok(())
}

/// Keep a generated file's bytes for apply and describe where it goes.
/// The bytes wait in the blob store under the job until it is done.
fn generated_output(
    ctx: &PlanContext<'_>,
    root: &str,
    rel: &str,
    kind: OutputKind,
    bytes: &[u8],
) -> Result<crate::publish::GeneratedOutput, ItemReason> {
    let sha256 = keep_bytes(ctx, "management.generated_output", bytes).map_err(|error| {
        tracing::error!(%error, "generated file not stored");
        reasons::write_failed()
    })?;
    Ok(crate::publish::GeneratedOutput {
        dest_root: root.to_owned(),
        dest_rel: rel.to_owned(),
        sha256,
        size: bytes.len() as u64,
        kind,
    })
}

/// Keep bytes in the blob store under the planning job.
fn keep_blob(ctx: &PlanContext<'_>, bytes: &[u8]) -> Result<String, ItemReason> {
    keep_bytes(ctx, "management.keep_blob", bytes).map_err(|error| {
        tracing::error!(%error, "planned artwork not stored");
        reasons::write_failed()
    })
}

/// Store `bytes` in the blob store with a reference held by the planning
/// job: the file first, then the reference.
fn keep_bytes(
    ctx: &PlanContext<'_>,
    name: &'static str,
    bytes: &[u8],
) -> Result<String, ManagementError> {
    use crate::publish::snapshots::SnapshotStore;
    ctx.setup
        .core
        .blobs
        .keep(
            &ctx.setup.db,
            Lane::Background,
            name,
            &[bytes],
            |tx, shas| {
                let sha = shas[0].clone();
                SnapshotStore::new(tx).add_ref(&sha, "preview", ctx.job_id)?;
                Ok(sha)
            },
        )
        .map_err(|error| ManagementError::Store(error.to_string()))
}

fn image_size(bytes: &[u8]) -> Option<(u32, u32)> {
    let format = image::guess_format(bytes).ok()?;
    image::ImageReader::with_format(std::io::Cursor::new(bytes), format)
        .into_dimensions()
        .ok()
}

fn format_word(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/webp" => "webp",
        _ => "jpeg",
    }
}

/// The preview note for tags a write leaves as they are: how many, and
/// why by kind with their names. `None` when there are none.
fn left_alone_note(left_alone: &[(String, tags::save::LeftAlone)]) -> Option<String> {
    use tags::save::LeftAlone;
    if left_alone.is_empty() {
        return None;
    }
    let groups: Vec<String> = [
        (LeftAlone::NotText, "not text"),
        (LeftAlone::HeldTwice, "held twice"),
        (LeftAlone::OverSizeLimit, "over 1 MB"),
    ]
    .into_iter()
    .filter_map(|(kind, words)| {
        let mut names: Vec<&str> = left_alone
            .iter()
            .filter(|(_, why)| *why == kind)
            .map(|(name, _)| name.as_str())
            .collect();
        names.sort_unstable();
        names.dedup();
        (!names.is_empty()).then(|| format!("{} ({words})", names.join(", ")))
    })
    .collect();
    let (noun, as_it_is, it) = if left_alone.len() == 1 {
        ("tag", "as it is", "it")
    } else {
        ("tags", "as they are", "them")
    };
    Some(format!(
        "{} {noun} kept {as_it_is}, since undo could not restore {it} exactly: {}.",
        left_alone.len(),
        groups.join("; ")
    ))
}

/// Plan the file's embedded pictures (v2's embedded artwork and its
/// picture scrub) as one list:
/// - the front cover: with embedding on, the album's cover, unless the
///   file's front already holds these bytes or, under "never replace with
///   smaller", a larger image (then the file's own front stays);
/// - the other pictures: kept, or removed by a scrub that does not
///   preserve embedded art (which also drops the front when embedding is
///   off).
///
/// A list equal to the file's own plans nothing, so a second run is
/// "unchanged". Undo puts the old pictures back from the before-state.
fn plan_pictures(
    ctx: &PlanContext<'_>,
    item: &mut PlannedItem,
    extras: &super::enrich::AlbumExtras,
    doc: &crate::publish::TagDocument,
    tag_plan: &mut profile::TagPlan,
) -> Result<(), ItemReason> {
    use crate::publish::PictureRef;
    let art = &ctx.pinned.profile.artwork;
    let metadata = &ctx.pinned.profile.metadata;
    let scrub = metadata.enabled
        && metadata.scrub_unmanaged_tags
        && !metadata.preserve_embedded_art_during_scrub;
    if !art.embedded_enabled && !scrub {
        return Ok(());
    }
    let Some(existing) = doc.pictures.clone() else {
        return Ok(());
    };
    let current_front = existing.iter().find(|picture| picture.kind == 3);
    let mut new_cover: Option<(PictureRef, super::enrich::RenderedCover)> = None;
    if art.embedded_enabled {
        match &extras.cover {
            None if extras.cover_deferred => item.facts.warnings.push(
                "The cover could not be fetched right now, so the embedded art is left as it is."
                    .to_owned(),
            ),
            None => {}
            Some(cover) => match super::enrich::render_cover(
                &cover.bytes,
                art.embedded_maximum_size,
                art.embedded_format,
            ) {
                None => item
                    .facts
                    .warnings
                    .push("The cover image could not be read, so no art is embedded.".to_owned()),
                Some(rendered) => {
                    let new_sha = sha256_hex(&rendered.bytes);
                    let holds_it = current_front.is_some_and(|front| front.sha256 == new_sha);
                    let larger = art.never_replace_with_smaller
                        && current_front
                            .and_then(|front| doc.picture_bytes.get(&front.sha256))
                            .and_then(|bytes| image_size(bytes))
                            .is_some_and(|(width, height)| {
                                u64::from(width) * u64::from(height)
                                    > u64::from(rendered.width) * u64::from(rendered.height)
                            });
                    if !holds_it && !larger {
                        new_cover = Some((
                            PictureRef::new(3, rendered.mime.clone(), "", new_sha, None),
                            rendered,
                        ));
                    }
                }
            },
        }
    }
    let mut pictures: Vec<PictureRef> = Vec::new();
    match &new_cover {
        Some((picture, _)) => pictures.push(picture.clone()),
        // Without a new cover the file's front stays, unless a scrub
        // with embedding off removes all art.
        None if art.embedded_enabled || !scrub => pictures.extend(current_front.cloned()),
        None => {}
    }
    if !scrub {
        // Every other picture stays, extra front covers included.
        let front_at = existing.iter().position(|picture| picture.kind == 3);
        pictures.extend(
            existing
                .iter()
                .enumerate()
                .filter(|(index, _)| Some(*index) != front_at)
                .map(|(_, picture)| picture.clone()),
        );
    }
    let unchanged = pictures.len() == existing.len()
        && pictures
            .iter()
            .zip(&existing)
            .all(|(planned, held)| planned.same_picture(held));
    if unchanged {
        return Ok(());
    }
    // Undo puts the file's pictures back from their stored records, so a
    // picture none can hold is never replaced.
    if !existing.iter().chain(&pictures).all(PictureRef::recordable) {
        return Err(reasons::tags_not_kept(
            "An embedded picture has a MIME type undo could not restore.",
        ));
    }
    for picture in &pictures {
        match &new_cover {
            Some((cover, rendered)) if cover.sha256 == picture.sha256 => {
                keep_blob(ctx, &rendered.bytes)?;
            }
            _ => {
                let Some(bytes) = doc.picture_bytes.get(&picture.sha256) else {
                    // Without its bytes a picture cannot be written back.
                    return Ok(());
                };
                keep_blob(ctx, bytes)?;
            }
        }
    }
    let describe = |list: &[PictureRef]| {
        serde_json::Value::from(
            list.iter()
                .map(|picture| format!("{} ({})", picture.mime, &picture.sha256[..12]))
                .collect::<Vec<_>>(),
        )
    };
    tag_plan.mutations.push(FieldMutation {
        name: "pictures".to_owned(),
        operation: if pictures.is_empty() { "clear" } else { "set" }.to_owned(),
        before: describe(&existing),
        after: describe(&pictures),
    });
    tag_plan.updates.set_pictures(pictures);
    item.diff.artwork_changed = true;
    if let Some((picture, rendered)) = new_cover {
        item.desired.artwork.push(ArtworkChoice {
            output_kind: "embedded_art".to_owned(),
            image_type: "front".to_owned(),
            source: "cover_art_archive".to_owned(),
            mime_type: rendered.mime.clone(),
            blob_sha256: picture.sha256,
            width: rendered.width,
            height: rendered.height,
            format: format_word(&rendered.mime).to_owned(),
            destination_relative_path: None,
        });
    }
    Ok(())
}

/// The album's cover file next to its tracks (v2's external artwork):
/// written only where no cover file the profile recognises already sits,
/// in the folder the album ends up in or the one it comes from.
fn cover_file(
    ctx: &PlanContext<'_>,
    item: &mut PlannedItem,
    extras: &super::enrich::AlbumExtras,
    dest_root: &str,
    dest_rel: &str,
    track: &CatalogTrack,
) -> Result<Option<crate::publish::GeneratedOutput>, ItemReason> {
    let art = &ctx.pinned.profile.artwork;
    if !art.external_enabled {
        return Ok(None);
    }
    let Some(cover) = &extras.cover else {
        return Ok(None);
    };
    let Some(rendered) =
        super::enrich::render_cover(&cover.bytes, art.external_maximum_size, art.external_format)
    else {
        return Ok(None);
    };
    let patterns: Vec<Vec<char>> = art
        .local_file_patterns
        .iter()
        .map(|pattern| pattern.to_lowercase().chars().collect())
        .collect();
    let folder_of = |rel: &str| {
        Path::new(rel)
            .parent()
            .map(|parent| parent.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default()
    };
    let has_cover = |root: &str, folder: &str| {
        ctx.sandbox
            .resolve_no_symlink(root, if folder.is_empty() { "." } else { folder })
            .ok()
            .and_then(|dir| std::fs::read_dir(dir).ok())
            .is_some_and(|entries| {
                entries.flatten().any(|entry| {
                    let name: Vec<char> = entry
                        .file_name()
                        .to_string_lossy()
                        .to_lowercase()
                        .chars()
                        .collect();
                    // A `cover.*` file counts even when the profile lists
                    // no patterns: it is where this one would go.
                    name.starts_with(&['c', 'o', 'v', 'e', 'r', '.'])
                        || patterns.iter().any(|pattern| glob(pattern, &name))
                })
            })
    };
    let dest_folder = folder_of(dest_rel);
    if has_cover(dest_root, &dest_folder)
        || has_cover(&track.root_id, &folder_of(&track.relative_path))
    {
        return Ok(None);
    }
    let extension = match rendered.mime.as_str() {
        "image/png" => "png",
        "image/webp" => "webp",
        _ => "jpg",
    };
    let rel = if dest_folder.is_empty() {
        format!("cover.{extension}")
    } else {
        format!("{dest_folder}/cover.{extension}")
    };
    let output = generated_output(ctx, dest_root, &rel, OutputKind::Artwork, &rendered.bytes)?;
    item.desired.artwork.push(ArtworkChoice {
        output_kind: "external_art".to_owned(),
        image_type: "front".to_owned(),
        source: "cover_art_archive".to_owned(),
        mime_type: rendered.mime.clone(),
        blob_sha256: output.sha256.clone(),
        width: rendered.width,
        height: rendered.height,
        format: format_word(&rendered.mime).to_owned(),
        destination_relative_path: Some(rel),
    });
    Ok(Some(output))
}

/// Drop generated files that would land where a sidecar moving with the
/// album lands: the album's own file wins.
fn drop_shadowed_outputs(items: &mut [PlannedItem]) {
    let taken: HashSet<crate::publish::CollisionKey> = items
        .iter()
        .filter_map(|item| item.desired.plan.as_ref())
        .flat_map(|plan| plan.sidecars.iter())
        .map(|sidecar| crate::publish::collision_key(&sidecar.dest_root, &sidecar.dest_rel))
        .collect();
    for item in items.iter_mut() {
        let Some(plan) = item.desired.plan.as_mut() else {
            continue;
        };
        let before = plan.outputs.len();
        plan.outputs.retain(|output| {
            !taken.contains(&crate::publish::collision_key(
                &output.dest_root,
                &output.dest_rel,
            ))
        });
        if plan.outputs.len() != before {
            item.diff.generated.retain(|file| {
                plan.outputs
                    .iter()
                    .any(|output| output.dest_rel == file.destination_relative_path)
            });
            item.facts.warnings.push(
                "The album's own file moves to where a generated file would go, so it is kept."
                    .to_owned(),
            );
        }
    }
}

/// Most files one album's ReplayGain analysis covers (as v2).
const MAX_REPLAYGAIN_TRACKS: usize = 500;

/// Measure the album's ReplayGain when the profile writes it and some file
/// needs it. Files are decoded one at a time within a time budget; an
/// album whose analysis cannot finish is left as it is, with a note.
/// Album values come only from the whole album (`whole_album`): a run on
/// single files measures their track values and leaves album values as
/// they are.
fn measure_replaygain(
    ctx: &PlanContext<'_>,
    tracks: &[CatalogTrack],
    whole_album: bool,
    extras: &mut super::enrich::AlbumExtras,
) {
    use crate::tags::loudness::{self, Unanalysed};
    use droppedneedle_kernel::runtime_config::sections::ReplayGainMode;

    let settings = &ctx.pinned.profile.enrichment.replaygain;
    if !settings.enabled || settings.mode == ReplayGainMode::Preserve || tracks.is_empty() {
        return;
    }
    let paths: Vec<(&str, Option<std::path::PathBuf>)> = tracks
        .iter()
        .map(|track| {
            (
                track.id.as_str(),
                ctx.sandbox
                    .resolve_no_symlink(&track.root_id, &track.relative_path)
                    .ok(),
            )
        })
        .collect();
    if settings.mode == ReplayGainMode::FillMissing {
        let mut needed = vec![TagField::ReplayGainTrackGain, TagField::ReplayGainTrackPeak];
        if settings.album_aware && whole_album {
            needed.extend([TagField::ReplayGainAlbumGain, TagField::ReplayGainAlbumPeak]);
        }
        let complete = paths.iter().all(|(_, path)| {
            path.as_deref()
                .and_then(|path| crate::publish::staging::document_from_file(path).ok())
                .is_some_and(|doc| {
                    needed
                        .iter()
                        .all(|field| doc.managed.field(*field).is_some())
                })
        });
        if complete {
            return;
        }
    }
    if tracks.len() > MAX_REPLAYGAIN_TRACKS {
        extras.replaygain_note = Some(format!(
            "The album has more than {MAX_REPLAYGAIN_TRACKS} files, too many to measure."
        ));
        return;
    }
    let now = std::time::Instant::now();
    if now >= ctx.analysis_until {
        extras.replaygain_note = Some(
            "This run's time for measuring loudness ran out, so the next run measures it."
                .to_owned(),
        );
        return;
    }
    let deadline = (now + loudness::ALBUM_BUDGET).min(ctx.analysis_until);
    let mut measured = Vec::new();
    let mut whole = true;
    for (track_id, path) in &paths {
        let result = match path {
            Some(path) => loudness::analyse_cached(path, deadline),
            None => Err(Unanalysed::Undecodable),
        };
        match result {
            Ok(found) => measured.push((*track_id, found)),
            Err(Unanalysed::OutOfTime) => {
                extras.replaygain_note =
                    Some("Measuring the album took too long, so it was stopped.".to_owned());
                return;
            }
            Err(Unanalysed::Undecodable) => whole = false,
        }
    }
    let album = (settings.album_aware && whole && whole_album)
        .then(|| {
            let all: Vec<&loudness::TrackLoudness> =
                measured.iter().map(|(_, found)| found).collect();
            loudness::album_loudness(&all).map(|value| {
                let peak = all.iter().map(|found| found.peak).fold(0.0, f64::max);
                (loudness::gain_text(value), loudness::peak_text(peak))
            })
        })
        .flatten();
    if !whole {
        extras.replaygain_note = Some(
            "Not every file of the album could be decoded, so no album gain was written."
                .to_owned(),
        );
    } else if measured.iter().all(|(_, found)| found.loudness().is_none()) {
        extras.replaygain_note =
            Some("The album is silent, so there is no loudness to measure.".to_owned());
    } else if settings.album_aware && !whole_album {
        extras.replaygain_note = Some(
            "Album gain is measured over the whole album, so pick the album to write it."
                .to_owned(),
        );
    }
    for (track_id, found) in measured {
        let Some(value) = found.loudness() else {
            continue;
        };
        extras
            .files
            .entry(track_id.to_owned())
            .or_default()
            .replaygain = Some(super::enrich::ReplayGainValues {
            track_gain: loudness::gain_text(value),
            track_peak: loudness::peak_text(found.peak),
            album_gain: album.as_ref().map(|(gain, _)| gain.clone()),
            album_peak: album.as_ref().map(|(_, peak)| peak.clone()),
        });
    }
}

/// One file the AcoustID step looks at.
pub(crate) struct AcoustIdSubject<'a> {
    pub track_id: &'a str,
    pub root_id: &'a str,
    pub relative_path: &'a str,
    pub overrides: &'a Overrides,
}

/// How an album's AcoustID lookup went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AcoustIdLookup {
    /// No file needed one, or no outside sources are attached.
    NotAsked,
    /// No key, or AcoustID refused it.
    NoKey,
    Answered,
    /// AcoustID could not be reached for at least one request.
    Unavailable,
}

/// Whether the profile writes an AcoustID field into a file that holds
/// `held`: managed and not preserved, no override for it, and replace or
/// a file without a value.
fn acoustid_wanted(
    profile: &droppedneedle_kernel::runtime_config::sections::LibraryManagementProfile,
    overrides: &Overrides,
    field: TagField,
    held: bool,
) -> bool {
    use droppedneedle_kernel::runtime_config::sections::FieldMode;
    let metadata = &profile.metadata;
    let Some(managed) = metadata
        .fields
        .iter()
        .find(|managed| managed.field == field.name())
    else {
        return false;
    };
    metadata.enabled
        && !metadata
            .preserve_fields
            .iter()
            .any(|name| name == field.name())
        && !overrides.fields.iter().any(|(each, _, _)| *each == field)
        && match managed.mode {
            FieldMode::Disabled | FieldMode::Preserve => false,
            FieldMode::Replace => true,
            FieldMode::FillMissing | FieldMode::Merge => !held,
        }
}

/// The AcoustID id to write for a file of `recording`: among the results
/// above the batch threshold that list the recording, the best score's
/// (the first on a tie), when it is a UUID.
fn acoustid_id_for(
    results: &[droppedneedle_providers::acoustid::BatchResult],
    recording: &str,
) -> Option<String> {
    use droppedneedle_providers::acoustid::BATCH_MIN_SCORE;
    let mut best: Option<&droppedneedle_providers::acoustid::BatchResult> = None;
    for result in results {
        let names = result
            .recordings()
            .iter()
            .any(|each| each.id.trim().eq_ignore_ascii_case(recording));
        if names
            && result.score() > BATCH_MIN_SCORE
            && best.is_none_or(|best| result.score() > best.score())
        {
            best = Some(result);
        }
    }
    let id = uuid::Uuid::try_parse(best?.id.trim()).ok()?;
    Some(id.hyphenated().to_string())
}

/// Work out the AcoustID print and id of every file the profile writes
/// them into, into `extras`. A print comes from the store when one was
/// kept for the file as it is on disk now, else from its audio, within
/// the run's analysis budget (`until`). Ids come from one AcoustID lookup
/// per album, in requests of at most twenty prints; a print of zero
/// seconds is never looked up (AcoustID answers it with nothing, which
/// would read as "no match"). With `ids_only` only the files that would
/// get an id are looked at, and no print is put in `extras`.
pub(crate) fn acoustid_values(
    setup: &LibrarySetup,
    sandbox: &Sandbox,
    profile: &droppedneedle_kernel::runtime_config::sections::LibraryManagementProfile,
    subjects: &[AcoustIdSubject<'_>],
    until: std::time::Instant,
    ids_only: bool,
    extras: &mut super::enrich::AlbumExtras,
) -> AcoustIdLookup {
    use super::enrich::{AcoustIdAnswer, AcoustIdPrint};
    use droppedneedle_providers::acoustid::MAX_BATCH;

    let port = setup.enrichment.as_deref();
    // Without a key no id is looked up, so a file that wants only an id
    // is not decoded.
    let mut keyed: Option<bool> = None;
    let mut outcome = AcoustIdLookup::NotAsked;
    let mut out_of_time: Option<&str> = None;
    // The file whose plan says why ids were left out.
    let mut first_wanting_id: Option<&str> = None;
    // (track, stat revision, print, recording)
    let mut lookups: Vec<(&str, String, AcoustIdPrint, String)> = Vec::new();
    for subject in subjects {
        let Ok(path) = sandbox.resolve_no_symlink(subject.root_id, subject.relative_path) else {
            continue;
        };
        if !tags::format_for_path(&path).is_ok_and(tags::save::writable) {
            continue;
        }
        let Ok(doc) = crate::publish::staging::document_from_file(&path) else {
            continue;
        };
        let held = |field: TagField| {
            doc.managed
                .field(field)
                .is_some_and(|values| !values.is_empty())
        };
        let want_print = !ids_only
            && acoustid_wanted(
                profile,
                subject.overrides,
                TagField::AcoustIdFingerprint,
                held(TagField::AcoustIdFingerprint),
            );
        let recording = match setup.identify_store.track_identity(subject.track_id) {
            Ok(mapping) => mapping
                .and_then(|mapping| mapping.recording_mbid)
                .filter(|recording| !recording.trim().is_empty()),
            Err(error) => {
                tracing::warn!(
                    %error,
                    track = subject.track_id,
                    "the file's match could not be read; no AcoustID values for it this run"
                );
                continue;
            }
        };
        let mut want_id = port.is_some()
            && recording.is_some()
            && acoustid_wanted(
                profile,
                subject.overrides,
                TagField::AcoustIdId,
                held(TagField::AcoustIdId),
            );
        if want_id {
            first_wanting_id.get_or_insert(subject.track_id);
            let has_key = *keyed.get_or_insert_with(|| {
                port.is_some_and(|port| {
                    super::enrich::wait(Box::pin(async {
                        Ok::<_, String>(port.acoustid_lookup(&[]).await)
                    })) != Some(AcoustIdAnswer::NoKey)
                })
            });
            if !has_key {
                outcome = AcoustIdLookup::NoKey;
                want_id = false;
            }
        }
        if !want_print && !want_id {
            continue;
        }
        let Ok(meta) = path.symlink_metadata() else {
            continue;
        };
        let revision = crate::scan::exact_stat_revision(
            meta.len(),
            crate::scan::mtime_ns_from_metadata(&meta),
        );
        // A stored print that cannot be read is logged and taken from the
        // audio again: that gives the same print.
        let stored = setup
            .identify_store
            .fingerprint(subject.track_id, &revision)
            .unwrap_or_else(|error| {
                tracing::warn!(%error, track = subject.track_id, "stored fingerprint unreadable");
                None
            });
        let print = match stored {
            Some((print, duration_seconds)) => AcoustIdPrint {
                print,
                duration_seconds,
            },
            None if std::time::Instant::now() >= until => {
                out_of_time.get_or_insert(subject.track_id);
                continue;
            }
            None => match tags::generate_fingerprint(&path) {
                Ok(found) if !found.fingerprint.is_empty() => AcoustIdPrint {
                    print: found.fingerprint,
                    duration_seconds: found.duration_seconds,
                },
                Ok(_) => continue,
                Err(error) => {
                    tracing::info!(%error, track = subject.track_id, "AcoustID print failed");
                    continue;
                }
            },
        };
        if want_print {
            extras
                .files
                .entry(subject.track_id.to_owned())
                .or_default()
                .acoustid_print = Some(print.print.clone());
        }
        if want_id
            && print.duration_seconds > 0
            && let Some(recording) = recording
        {
            lookups.push((subject.track_id, revision, print, recording));
        }
    }
    if let Some(track) = out_of_time {
        extras.acoustid_notes.push((
            track.to_owned(),
            "AcoustID values were not worked out: this run's time for analysing audio ran out, \
             so the next run does them."
                .to_owned(),
        ));
    }
    if let Some(port) = port {
        for chunk in lookups.chunks(MAX_BATCH) {
            let prints: Vec<AcoustIdPrint> =
                chunk.iter().map(|(_, _, print, _)| print.clone()).collect();
            let answer = super::enrich::wait(Box::pin(async {
                Ok::<_, String>(port.acoustid_lookup(&prints).await)
            }))
            .unwrap_or(AcoustIdAnswer::Unavailable);
            let results = match answer {
                AcoustIdAnswer::Answered(results) => results,
                AcoustIdAnswer::NoKey => {
                    outcome = AcoustIdLookup::NoKey;
                    break;
                }
                AcoustIdAnswer::Unavailable => {
                    outcome = AcoustIdLookup::Unavailable;
                    break;
                }
            };
            outcome = AcoustIdLookup::Answered;
            for ((track, revision, print, recording), results) in chunk.iter().zip(&results) {
                let matched = results.iter().any(|result| {
                    result.score() > droppedneedle_providers::acoustid::BATCH_MIN_SCORE
                        && !result.recordings().is_empty()
                });
                if let Err(error) = setup.identify_store.save_fingerprint(
                    track,
                    revision,
                    &print.print,
                    print.duration_seconds,
                    matched,
                ) {
                    tracing::warn!(%error, track, "fingerprint not kept");
                }
                if let Some(id) = acoustid_id_for(results, recording) {
                    extras
                        .files
                        .entry((*track).to_owned())
                        .or_default()
                        .acoustid_id = Some(id);
                }
            }
        }
    }
    let note = match outcome {
        AcoustIdLookup::NoKey => {
            Some("AcoustID IDs need an AcoustID key in Settings > Library; the files keep theirs.")
        }
        AcoustIdLookup::Unavailable => {
            Some("AcoustID could not be reached; the files keep their AcoustID IDs.")
        }
        _ => None,
    };
    if let (Some(note), Some(track)) = (note, first_wanting_id) {
        extras
            .acoustid_notes
            .push((track.to_owned(), note.to_owned()));
    }
    outcome
}

/// Most sidecar files one album may carry, and most directory entries
/// looked at to find them (as in v2).
const MAX_SIDECARS: usize = 1_000;
const MAX_SIDECAR_ENTRIES: usize = 10_000;

/// Shell-style match with `*` and `?`, on already case-folded text.
fn glob(pattern: &[char], text: &[char]) -> bool {
    match (pattern.first(), text.first()) {
        (None, None) => true,
        (Some('*'), _) => {
            glob(&pattern[1..], text) || (!text.is_empty() && glob(pattern, &text[1..]))
        }
        (Some('?'), Some(_)) => glob(&pattern[1..], &text[1..]),
        (Some(p), Some(t)) if p == t => glob(&pattern[1..], &text[1..]),
        _ => false,
    }
}

/// True when a sidecar pattern matches `relative` (folded). Patterns with
/// a slash match the path below the album folder; others match files in
/// the album folder itself.
fn sidecar_match(patterns: &[Vec<char>], relative: &str) -> bool {
    let text: Vec<char> = relative.chars().collect();
    patterns
        .iter()
        .any(|pattern| (pattern.contains(&'/') || !relative.contains('/')) && glob(pattern, &text))
}

/// Files below `dir` relative to it, sorted, skipping symlinked folders.
/// `None` when the folder is too large to walk or a matching file is a
/// symbolic link.
fn sidecar_candidates(dir: &Path, deep: bool, patterns: &[Vec<char>]) -> Option<Vec<String>> {
    let mut found = Vec::new();
    let mut pending = vec![String::new()];
    let mut examined = 0;
    while let Some(prefix) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(dir.join(&prefix)) else {
            continue;
        };
        let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            examined += 1;
            if examined > MAX_SIDECAR_ENTRIES {
                return None;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let relative = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let folded = caseless::default_case_fold_str(&relative);
            if kind.is_dir() {
                if deep {
                    pending.push(relative);
                }
                continue;
            }
            if !sidecar_match(patterns, &folded) {
                continue;
            }
            if kind.is_symlink() {
                return None;
            }
            if kind.is_file() && tags::format_for_path(Path::new(&relative)).is_err() {
                found.push(relative);
            }
        }
    }
    if found.len() > MAX_SIDECARS {
        return None;
    }
    found.sort();
    Some(found)
}

/// Sidecars (cover art, cue sheets, logs) travel with the album when its
/// folder moves, as v2 did: the files in the first moving file's folder
/// that match the profile's patterns go to its new folder. A sidecar
/// that would land on an existing file blocks the album instead.
fn plan_sidecars(ctx: &PlanContext<'_>, items: &mut [PlannedItem]) {
    let organization = &ctx.pinned.profile.organization;
    if !organization.move_sidecars || organization.sidecar_patterns.is_empty() {
        return;
    }
    // Every folder the album's files move out of (one per disc for an
    // album kept in disc subfolders) takes its own sidecars along.
    let mut folders = HashSet::new();
    let firsts: Vec<usize> = items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| {
            let plan = item.desired.plan.as_ref()?;
            let folder = Path::new(&plan.source_rel).parent()?.to_path_buf();
            (plan.kind == PlanKind::Move && folders.insert((plan.source_root.clone(), folder)))
                .then_some(index)
        })
        .collect();
    let mut seen = HashSet::new();
    for index in firsts {
        plan_folder_sidecars(ctx, items, index, &mut seen);
    }
}

/// Sidecars of one moving folder, carried by the item at `index`.
fn plan_folder_sidecars(
    ctx: &PlanContext<'_>,
    items: &mut [PlannedItem],
    index: usize,
    seen: &mut HashSet<crate::publish::CollisionKey>,
) {
    let organization = &ctx.pinned.profile.organization;
    let Some(plan) = items[index].desired.plan.clone() else {
        return;
    };
    let parent = |rel: &str| {
        Path::new(rel)
            .parent()
            .map(|parent| parent.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let (source_dir, dest_dir) = (parent(&plan.source_rel), parent(&plan.dest_rel));
    if source_dir == dest_dir && plan.source_root == plan.dest_root {
        return;
    }
    if source_dir.is_empty() {
        items[index].facts.warnings.push(
            "Files that travel with albums (cover.jpg, .cue sheets) are not moved for an album              at the top of a library folder."
                .to_owned(),
        );
        if items[index].eligibility == Eligibility::Eligible {
            items[index].eligibility = Eligibility::Warning;
        }
        return;
    }
    let patterns: Vec<Vec<char>> = organization
        .sidecar_patterns
        .iter()
        .map(|pattern| caseless::default_case_fold_str(pattern).chars().collect())
        .collect();
    let deep = patterns.iter().any(|pattern| pattern.contains(&'/'));
    let Ok(source_path) = ctx
        .sandbox
        .resolve_no_symlink(&plan.source_root, &source_dir)
    else {
        items[index].block(reasons::sidecar_unreadable());
        return;
    };
    let Some(found) = sidecar_candidates(&source_path, deep, &patterns) else {
        items[index].block(reasons::sidecar_unreadable());
        return;
    };
    let audio: HashSet<_> = items
        .iter()
        .filter_map(|item| item.desired.plan.as_ref())
        .map(|plan| collision_key(&plan.dest_root, &plan.dest_rel))
        .collect();
    let mut sidecars = Vec::new();
    for relative in found {
        let source_rel = format!("{source_dir}/{relative}");
        let dest_rel = if dest_dir.is_empty() {
            relative.clone()
        } else {
            format!("{dest_dir}/{relative}")
        };
        let key = collision_key(&plan.dest_root, &dest_rel);
        let occupied = audio.contains(&key)
            || !seen.insert(key)
            || ctx
                .sandbox
                .resolve_no_symlink(&plan.dest_root, &dest_rel)
                .map_or(true, |path| {
                    path.symlink_metadata().is_ok() || folded_sibling_exists(&path)
                });
        if occupied {
            items[index].block(reasons::sidecar_collision());
            return;
        }
        let hashed = ctx
            .sandbox
            .resolve_no_symlink(&plan.source_root, &source_rel)
            .and_then(|path| crate::publish::paths::hash_regular_file(&path));
        let Ok(hashed) = hashed else {
            items[index].block(reasons::sidecar_unreadable());
            return;
        };
        sidecars.push(crate::publish::planner::SidecarPlan {
            source_root: plan.source_root.clone(),
            source_rel: source_rel.clone(),
            dest_root: plan.dest_root.clone(),
            dest_rel: dest_rel.clone(),
            fingerprint: FileFingerprint {
                size: hashed.size,
                sha256: hashed.sha256,
            },
        });
    }
    if sidecars.is_empty() {
        return;
    }
    let item = &mut items[index];
    item.diff.sidecars = sidecars
        .iter()
        .map(|sidecar| SidecarMove {
            source_relative_path: sidecar.source_rel.clone(),
            destination_relative_path: sidecar.dest_rel.clone(),
        })
        .collect();
    if let Some(plan) = item.desired.plan.as_mut() {
        let bytes: u64 = sidecars
            .iter()
            .map(|sidecar| sidecar.fingerprint.size)
            .sum();
        plan.staged_bytes_estimate += bytes;
        item.estimated_bytes += bytes;
        plan.capabilities.push(Capability::Sidecars);
        plan.sidecars = sidecars;
    }
}

/// True when another name in the folder folds to the same name.
fn folded_sibling_exists(path: &Path) -> bool {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name().and_then(|n| n.to_str()))
    else {
        return false;
    };
    let wanted = caseless::default_case_fold_str(name);
    std::fs::read_dir(parent).is_ok_and(|entries| {
        entries.filter_map(Result::ok).any(|entry| {
            entry.file_name().to_str().is_some_and(|other| {
                other != name && caseless::default_case_fold_str(other) == wanted
            })
        })
    })
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
            put_back: None,
            added: None,
            removal: None,
        };
        match CollisionGate::check_bundle(ctx.sandbox, &single) {
            Ok(()) => {}
            Err(PublishError::Collision(_)) => {
                let identical = ctx
                    .sandbox
                    .resolve_no_symlink(&plan.dest_root, &plan.dest_rel)
                    .is_ok_and(|path| {
                        crate::publish::paths::holds(&path, &plan.fingerprint.sha256)
                    });
                item.block(reasons::path_collision(identical));
            }
            Err(error) => item.block(path_reason(&error)),
        }
    }
}

/// Plan every selected file of one album, numbering items from
/// `first_ordinal`. `person` is set when a person started the job, which
/// holds back an album whose chosen edition is still being placed.
/// `edition` blocks the album unless its identity is still that edition.
pub fn plan_album(
    ctx: &PlanContext<'_>,
    scope: &AlbumScope,
    tracks: &[CatalogTrack],
    bundle_ordinal: i64,
    first_ordinal: i64,
    person: bool,
    edition: Option<&EditionPin>,
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
    // A person's tag edit writes only the fields they named, never a name
    // or a path: it edits the folder's own tags, so it goes ahead.
    if ctx.tag_edit.is_none() && album_keeps_own_tags(ctx, tracks) {
        for item in &mut items {
            item.block(reasons::keeps_own_tags());
        }
        return Ok(items);
    }
    let (mut release, release_mbid, release_group_mbid, album_identity_revision) =
        match album_state(ctx, &scope.album_id, person, edition)? {
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
    // Only a person's own preview asks MusicBrainz at their priority.
    let mut extras = super::enrich::gather(
        ctx.setup.enrichment.as_deref(),
        &ctx.setup.identify_store,
        &ctx.pinned.profile,
        &mut release,
        !person,
    );
    // Link the album's artists from its match, with any names the lookup
    // just stored, so a run that writes their ids and new names keeps
    // them. No merge here: it would move the album's revision under the
    // plan.
    if let Err(error) = ctx.setup.identify_store.link_album_artists(&scope.album_id) {
        tracing::warn!(
            %error,
            album_id = scope.album_id,
            "the album's artist links could not be worked out; the plan uses the links on file"
        );
    }
    extras.cover_owner = tracks.first().map(|track| track.id.clone());
    // A selection of single files is not the whole album.
    measure_replaygain(ctx, tracks, scope.track_ids.is_none(), &mut extras);
    let mut saved = Vec::with_capacity(tracks.len());
    for track in tracks {
        let mut overrides = ctx.setup.db.read_blocking("management.overrides", |conn| {
            Ok::<_, ManagementError>(overrides(conn, &scope.album_id, &track.id)?)
        })?;
        if let Some(edit) = ctx.tag_edit {
            overrides = with_tag_edit(overrides, edit, &track.id);
        }
        saved.push(overrides);
    }
    let subjects: Vec<AcoustIdSubject<'_>> = tracks
        .iter()
        .zip(&saved)
        .map(|(track, overrides)| AcoustIdSubject {
            track_id: &track.id,
            root_id: &track.root_id,
            relative_path: &track.relative_path,
            overrides,
        })
        .collect();
    acoustid_values(
        ctx.setup,
        ctx.sandbox,
        &ctx.pinned.profile,
        &subjects,
        ctx.analysis_until,
        false,
        &mut extras,
    );
    for ((item, track), overrides) in items.iter_mut().zip(tracks).zip(&saved) {
        if let Err(reason) = plan_file(
            ctx,
            item,
            track,
            &release,
            &release_mbid,
            &release_group_mbid,
            album_identity_revision,
            overrides,
            &extras,
        ) {
            item.block(reason);
        }
    }
    plan_sidecars(ctx, &mut items);
    drop_shadowed_outputs(&mut items);
    check_collisions(ctx, &mut items);
    // A person's preview of the album itself still plans, and says what
    // applying it does.
    if ctx.tag_edit.is_none()
        && ctx.setup.db.read_blocking("management.is_marked", |conn| {
            Ok::<_, ManagementError>(takeover::is_marked(conn, &scope.album_id)?)
        })?
    {
        for item in &mut items {
            item.facts
                .warnings
                .push(reasons::TAKEN_OVER_WARNING.to_owned());
            if item.eligibility == Eligibility::Eligible {
                item.eligibility = Eligibility::Warning;
            }
        }
    }
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
            put_back: None,
            added: None,
            removal: None,
        };
        let probe = crate::adapters::FsSpaceProbe::new(ctx.setup.root_dirs.clone());
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

/// Plan one file of an album on its own, without the album-wide checks
/// (sidecars, collisions, disk space): what it would get if nothing else
/// stood in its way. Duplicate resolution starts from this.
pub fn plan_one(
    ctx: &PlanContext<'_>,
    scope: &AlbumScope,
    track: &CatalogTrack,
    ordinal: i64,
) -> Result<PlannedItem, ManagementError> {
    let mut item = base_item(track, scope, ordinal, 0);
    if album_keeps_own_tags(ctx, std::slice::from_ref(track)) {
        item.block(reasons::keeps_own_tags());
        return Ok(item);
    }
    // Duplicate resolution is not held back by an edition being placed.
    match album_state(ctx, &scope.album_id, false, None)? {
        AlbumState::Blocked(reason) => item.block(reason),
        AlbumState::Ready {
            mut release,
            release_mbid,
            release_group_mbid,
            album_identity_revision,
        } => {
            let overrides = ctx.setup.db.read_blocking("management.overrides", |conn| {
                Ok::<_, ManagementError>(overrides(conn, &scope.album_id, &track.id)?)
            })?;
            let mut extras = super::enrich::gather(
                ctx.setup.enrichment.as_deref(),
                &ctx.setup.identify_store,
                &ctx.pinned.profile,
                &mut release,
                false,
            );
            measure_replaygain(ctx, std::slice::from_ref(track), false, &mut extras);
            acoustid_values(
                ctx.setup,
                ctx.sandbox,
                &ctx.pinned.profile,
                &[AcoustIdSubject {
                    track_id: &track.id,
                    root_id: &track.root_id,
                    relative_path: &track.relative_path,
                    overrides: &overrides,
                }],
                ctx.analysis_until,
                false,
                &mut extras,
            );
            if let Err(reason) = plan_file(
                ctx,
                &mut item,
                track,
                &release,
                &release_mbid,
                &release_group_mbid,
                album_identity_revision,
                &overrides,
                &extras,
            ) {
                item.block(reason);
            }
        }
    }
    Ok(item)
}

/// Block a planned file whose destination is taken, as album planning
/// does. Used when a single file gets a new destination.
pub fn check_destination(ctx: &PlanContext<'_>, item: &mut PlannedItem) {
    check_collisions(ctx, std::slice::from_mut(item));
}

/// Group a bundle's writable plans back into what apply publishes.
pub fn bundle_plans(items: &[PlannedItem]) -> Vec<PlanItem> {
    items
        .iter()
        .filter(|item| item.eligibility.writes())
        .filter_map(|item| item.desired.plan.clone())
        .collect()
}
