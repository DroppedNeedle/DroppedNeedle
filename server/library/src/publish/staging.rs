//! Real tag staging: the publish→tags seam.
//!
//! The publisher copies each source into its journal temp, whose name
//! ends in the real audio extension, and the tags save wrapper writes the
//! managed updates in that temp: one temp per file. The hash the wrapper
//! reads back while it verifies the write is the one the journal keeps.
//! A refusal leaves the temp to its row's unwind.
//!
//! Two loudness rules:
//!
//! * Managed-field names outside the save wrapper's fields (Picard's
//!   tag set, see `tags::fields`) block at preview time
//!   ([`from_request`]), never mid-publish.
//! * Items with no managed updates stage byte-identical without
//!   touching the tag stack, so pure moves never trip tag refusals
//!   (read-only formats, mixed ID3) on files whose tags they keep.

use std::collections::BTreeMap;
use std::path::Path;

use super::super::tags::save::{accepts, check_writable, writable};

use super::super::tags::{
    AudioFormat, Refusal, TagEdit, TagField, TagsError, format_for_path, read_document,
};
use super::PublishError;
use super::tags_seam::{
    FULL_FIELDS_VERSION, PictureRef, RICH_FIELDS_VERSION, TAG_DOCUMENT_VERSION, TagDocument,
    TagTarget, TagUpdate, TagUpdates, recorded_since,
};

/// Map managed updates onto save-wrapper edits. An empty value list
/// removes the field (undo uses that for fields an edit added). Values
/// are written as given: new values are checked at preview
/// ([`from_request`]), and values replayed from a file go back exactly
/// as they were. Pictures ride beside the edits (see `stage_tags`).
pub fn tag_edits(updates: &TagUpdates) -> Vec<TagEdit> {
    updates
        .iter()
        .filter_map(|update| match update {
            TagUpdate::Field { field, values } => Some(TagEdit::verbatim(field, values.to_vec())),
            TagUpdate::Spelling {
                field,
                spelling,
                values,
            } => Some(TagEdit::verbatim_spelling(field, spelling, values.to_vec())),
            TagUpdate::Native { key, values } => Some(TagEdit::native(key, values.to_vec())),
            TagUpdate::Pictures(_) => None,
        })
        .collect()
}

/// The updates a caller asks to write, checked: known field names, and
/// values each field can hold (dates parse, counts are numbers, one value
/// where the field holds one). A native item outside the fields may only
/// be removed, and pictures must read. Runs at preview, so Apply never
/// meets a bad value.
pub fn from_request(requested: &BTreeMap<String, Vec<String>>) -> Result<TagUpdates, PublishError> {
    let mut updates = TagUpdates::new();
    for (name, values) in requested {
        if updates.insert_requested_extra(name, values) {
            continue;
        }
        let field = TagField::from_name(name).ok_or_else(|| {
            PublishError::Capability(
                format!("field {name} is outside the staged writer's surface").into(),
            )
        })?;
        if !accepts(&TagEdit::new(field, values.clone())) {
            return Err(PublishError::Capability(
                format!("field {name} cannot hold {values:?}").into(),
            ));
        }
        updates.insert(TagTarget::Field(field), values.clone());
    }
    Ok(updates)
}

/// The extension of a tag write's temp, which names its container to the
/// save wrapper. A container the wrapper only reads is refused here,
/// before anything is journaled.
pub fn writable_extension(format: AudioFormat) -> Result<&'static str, PublishError> {
    check_writable(format)
        .map_err(|refusal| PublishError::Capability(refusal.to_string().into()))?;
    Ok(format.as_str())
}

/// Write one plan item's tags into its journal temp, which already holds
/// the source's bytes and whose extension names the container. Returns
/// the SHA-256 the save wrapper read back while it verified the write.
/// A refusal leaves the temp for its row's unwind to remove.
pub fn stage_tags(
    temp: &Path,
    managed_updates: &TagUpdates,
    pictures: Option<&[super::super::tags::save::PictureSpec]>,
) -> Result<String, PublishError> {
    let edits = tag_edits(managed_updates);
    super::super::tags::save::save_in_place(temp, &edits, pictures)
        .map(|saved| saved.sha256)
        .map_err(|error| map_tags_error(temp, error))
}

/// Read the current semantic tag document for one audio file: every
/// field the staged writer can write back. Undo and baseline restore
/// replay `managed` through staging, so anything stored here must
/// round-trip; unknown frames and custom tags stay preserved by the
/// save wrapper's byte-level handling, not by this document.
pub fn document_from_file(path: &Path) -> Result<TagDocument, PublishError> {
    let name = message_name_path(path);
    let fields = read_document(path).map_err(|error| match error {
        TagsError::UnrecognizedExtension { extension } => {
            PublishError::Capability(format!("format {extension} has no staged writer").into())
        }
        other => PublishError::Validation(sanitize_tags_message(&name, other).into()),
    })?;
    Ok(document_from_fields(fields))
}

/// The semantic document of a file's fields, however they were read
/// (from the file itself, or from tags captured before an earlier
/// version of DroppedNeedle changed it).
pub fn document_from_fields(fields: super::super::tags::FieldDocument) -> TagDocument {
    let mut managed = TagUpdates::new();
    for (field, values) in fields.values {
        managed.insert(TagTarget::Field(field), values);
    }
    // Each spelling's own values sort after the field's own entry, so on
    // replay they override what the field entry wrote to every spelling.
    for ((field, spelling), values) in fields.spellings {
        managed.insert(TagTarget::Spelling(field, spelling), values);
    }
    let mut picture_bytes = BTreeMap::new();
    let pictures = fields
        .pictures
        .into_iter()
        .map(|picture| {
            let sha256 = super::snapshots::sha256_hex(&picture.data);
            picture_bytes.insert(sha256.clone(), picture.data);
            PictureRef::new(
                picture.kind,
                picture.mime,
                &picture.description,
                sha256,
                picture.info,
            )
        })
        .collect();
    TagDocument {
        pictures: Some(pictures),
        picture_bytes,
        managed,
        custom: fields.native,
        unknown_frames: BTreeMap::new(),
        opaque: fields
            .opaque
            .into_iter()
            .map(|field| field.name().to_owned())
            .collect(),
        version: TAG_DOCUMENT_VERSION,
    }
}

/// The before-state document for a write of `managed_updates`: the
/// file's fields, plus an empty entry for each field the write adds, so
/// undo removes what the write put there. A write that would overwrite
/// a field the file holds in a shape undo cannot put back is refused
/// here, before anything is staged.
pub fn document_for_write(path: &Path, updates: &TagUpdates) -> Result<TagDocument, PublishError> {
    let mut document = document_from_file(path)?;
    let opaque = updates.targets().find_map(|target| match target {
        TagTarget::Field(field) if document.opaque.iter().any(|name| name == field.name()) => {
            Some(field.name())
        }
        _ => None,
    });
    if let Some(name) = opaque {
        return Err(PublishError::Capability(
            format!(
                "field {name} of '{}' holds a value undo could not restore",
                message_name_path(path)
            )
            .into(),
        ));
    }
    for target in updates.targets() {
        // A scrubbed native item's before-state is its values now.
        let before = match target {
            TagTarget::Native(key) => document.custom.get(key).cloned().unwrap_or_default(),
            _ => Vec::new(),
        };
        document.managed.insert_missing(target.clone(), before);
    }
    // Replaced pictures are the ones the file holds now, so undo writes
    // them back. One the before-state cannot record blocks the write.
    if updates.pictures().is_some() {
        let held = document.pictures.clone().unwrap_or_default();
        if !held.iter().all(PictureRef::recordable) {
            return Err(PublishError::Capability(
                format!(
                    "a picture in '{}' has a MIME type undo could not restore",
                    message_name_path(path)
                )
                .into(),
            ));
        }
        if document.managed.pictures().is_none() {
            document.managed.set_pictures(held);
        }
    }
    Ok(document)
}

/// What a baseline restore writes: the baseline's fields, plus a removal
/// for every other writable field, so fields added by any later write go
/// away. No removal touches a field the baseline held in an unwritable
/// shape, or one the file holds in such a shape now (that would block
/// the restore). Read-only containers get no tag writes at all, and a
/// baseline from before documents recorded every field gets no removals:
/// it cannot say what was absent. Fields newer than the baseline's shape
/// (ISRC in a version 2 baseline) get no removal either.
pub fn restore_updates(
    baseline: &TagDocument,
    current: &TagDocument,
    rel_path: &str,
) -> TagUpdates {
    let writable_format = format_for_path(Path::new(rel_path)).is_ok_and(writable);
    if !writable_format || baseline.version < FULL_FIELDS_VERSION {
        return baseline.managed.clone();
    }
    let mut updates = baseline.managed.clone();
    // The baseline's pictures go back when the file's differ (a later
    // run embedded art), when the baseline's shape recorded them.
    // A baseline stored before records refused tabs may hold one in a
    // description; it reads back as the file now reads it, with a space.
    if let (Some(original), Some(now)) = (&baseline.pictures, &current.pictures)
        && baseline.version >= RICH_FIELDS_VERSION
    {
        let original: Vec<PictureRef> = original.iter().map(PictureRef::normalised).collect();
        if original.len() != now.len()
            || original
                .iter()
                .zip(now)
                .any(|(original, now)| !original.same_picture(now))
        {
            updates.set_pictures(original);
        }
    }
    // Native items the baseline held and the file no longer holds as it
    // did (a later scrub) go back. Items added since are left alone.
    for (key, values) in &baseline.custom {
        if current.custom.get(key) != Some(values) {
            updates.insert(TagTarget::Native(key.clone()), values.clone());
        }
    }
    for field in TagField::ALL {
        if baseline.version < recorded_since(field) {
            continue;
        }
        let name = field.name();
        let opaque = |document: &TagDocument| document.opaque.iter().any(|kept| kept == name);
        if !opaque(baseline) && !opaque(current) {
            updates.insert_missing(TagTarget::Field(field), Vec::new());
        }
    }
    updates
}

/// Map a tag failure onto the publisher gates. Deterministic
/// refusals (read-only format, unencodable item, mixed tag) are
/// capability blocks: retrying cannot help. Verify and save faults
/// are validation: the bytes moved under the preview.
fn map_tags_error(work: &Path, error: TagsError) -> PublishError {
    match error {
        TagsError::UnrecognizedExtension { extension } => {
            PublishError::Capability(format!("format {extension} has no staged writer").into())
        }
        TagsError::Io { path, source } => {
            PublishError::Io(format!("io error on '{}': {source}", message_name(&path)).into())
        }
        TagsError::TagRead { path, reason }
        | TagsError::Probe { path, reason }
        | TagsError::Decode { path, reason }
        | TagsError::Fingerprint { path, reason } => PublishError::Validation(
            format!("unreadable audio '{}': {reason}", message_name(&path)).into(),
        ),
        TagsError::SaveRefused { refusal, .. } => match refusal {
            Refusal::VerifyMismatch { .. } | Refusal::SaveFailed { .. } => {
                PublishError::Validation(
                    format!(
                        "staged write failed for '{}': {refusal}",
                        message_name_path(work)
                    )
                    .into(),
                )
            }
            other => PublishError::Capability(other.to_string().into()),
        },
    }
}

/// File name for a 4xx message. Staging works on hidden temp paths
/// under the sandbox; the absolute server path never leaves the
/// server, so failures name the file only.
fn message_name(path: &str) -> String {
    message_name_path(Path::new(path))
}

fn message_name_path(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file")
        .to_owned()
}

/// Re-render a tag failure naming the file only. `TagsError`
/// displays carry the absolute work path, which must not reach a
/// 4xx body.
fn sanitize_tags_message(name: &str, error: TagsError) -> String {
    match error {
        TagsError::UnrecognizedExtension { extension } => {
            format!("unrecognized audio extension '.{extension}'")
        }
        TagsError::Io { source, .. } => {
            format!("io error on '{name}': {source}")
        }
        TagsError::TagRead { reason, .. } => {
            format!("tag read failed for '{name}': {reason}")
        }
        TagsError::Probe { reason, .. } => {
            format!("probe failed for '{name}': {reason}")
        }
        TagsError::Decode { reason, .. } => {
            format!("decode failed for '{name}': {reason}")
        }
        TagsError::Fingerprint { reason, .. } => {
            format!("fingerprint failed for '{name}': {reason}")
        }
        TagsError::SaveRefused { refusal, .. } => {
            format!("save refused for '{name}': {refusal}")
        }
    }
}
