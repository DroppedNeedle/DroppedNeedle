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
//!   ([`check_managed_updates`]), never mid-publish.
//! * Items with no managed updates stage byte-identical without
//!   touching the tag stack, so pure moves never trip tag refusals
//!   (read-only formats, mixed ID3) on files whose tags they keep.

use std::collections::BTreeMap;
use std::path::Path;

use super::super::tags::save::{accepts, check_writable, writable};

use super::super::tags::{Refusal, TagEdit, TagField, TagsError, format_for_path, read_document};
use super::PublishError;
use super::tags_seam::{
    FULL_FIELDS_VERSION, PICTURES_KEY, PictureRef, RICH_FIELDS_VERSION, TAG_DOCUMENT_VERSION,
    TagDocument, recorded_since,
};

/// Map managed updates onto save-wrapper edits. An empty value list
/// removes the field (undo uses that for fields an edit added). Values
/// are written as given: new values are checked at preview
/// ([`check_new_values`]), and values replayed from a file go back
/// exactly as they were. Unknown names block loudly.
pub fn check_managed_updates(
    managed_updates: &BTreeMap<String, Vec<String>>,
) -> Result<Vec<TagEdit>, PublishError> {
    let mut edits = Vec::with_capacity(managed_updates.len());
    for (name, values) in managed_updates {
        if let Some(key) = name.strip_prefix(NATIVE_PREFIX) {
            edits.push(TagEdit::native(key, values.clone()));
            continue;
        }
        if name == PICTURES_KEY {
            // Pictures ride beside the tag edits (see `pictures_update`).
            continue;
        }
        match TagField::from_managed_name(name) {
            Some((field, None)) => edits.push(TagEdit::verbatim(field, values.clone())),
            Some((field, Some(spelling))) => {
                edits.push(TagEdit::verbatim_spelling(field, spelling, values.clone()));
            }
            None => {
                return Err(PublishError::Capability(format!(
                    "field {name} is outside the staged writer's surface"
                )));
            }
        }
    }
    Ok(edits)
}

/// Check values a caller asks to write: known names, and values each
/// field can hold (dates parse, counts are numbers, one value where the
/// field holds one). Runs at preview, so Apply never meets them.
pub fn check_new_values(
    managed_updates: &BTreeMap<String, Vec<String>>,
) -> Result<(), PublishError> {
    for (name, values) in managed_updates {
        // Native items outside the fields are only ever removed (a scrub).
        if name.starts_with(NATIVE_PREFIX) && values.is_empty() {
            continue;
        }
        if name == PICTURES_KEY
            && values
                .iter()
                .all(|value| PictureRef::decode(value).is_some())
        {
            continue;
        }
        let field = TagField::from_name(name).ok_or_else(|| {
            PublishError::Capability(format!(
                "field {name} is outside the staged writer's surface"
            ))
        })?;
        if !accepts(&TagEdit::new(field, values.clone())) {
            return Err(PublishError::Capability(format!(
                "field {name} cannot hold {values:?}"
            )));
        }
    }
    Ok(())
}

/// Managed-update names for native items outside the writable fields
/// (`native:vorbis:COMPOSER`): a scrub removes them, and undo or a
/// baseline restore writes them back.
pub const NATIVE_PREFIX: &str = "native:";

/// Audio extensions the staged writer routes. Anything else (WMA
/// included) has no staged writer and blocks before any mutation.
fn staged_extension(format: &str) -> Result<&'static str, PublishError> {
    match format.to_lowercase().as_str() {
        "flac" => Ok("flac"),
        "mp3" => Ok("mp3"),
        "ogg" => Ok("ogg"),
        "opus" => Ok("opus"),
        "m4a" => Ok("m4a"),
        "aac" => Ok("aac"),
        "wav" => Ok("wav"),
        other => Err(PublishError::Capability(format!(
            "format {other} has no staged writer"
        ))),
    }
}

/// The extension of a tag write's temp, which names its container to the
/// save wrapper. A container the wrapper only reads is refused here,
/// before anything is journaled.
pub fn writable_extension(format: &str) -> Result<&'static str, PublishError> {
    let extension = staged_extension(format)?;
    let format = format_for_path(&Path::new("staged").with_extension(extension)).map_err(|_| {
        PublishError::Capability(format!("format {extension} has no staged writer"))
    })?;
    check_writable(format).map_err(|refusal| PublishError::Capability(refusal.to_string()))?;
    Ok(extension)
}

/// Write one plan item's tags into its journal temp, which already holds
/// the source's bytes and whose extension names the container. Returns
/// the SHA-256 the save wrapper read back while it verified the write.
/// A refusal leaves the temp for its row's unwind to remove.
pub fn stage_tags(
    temp: &Path,
    managed_updates: &BTreeMap<String, Vec<String>>,
    pictures: Option<&[super::super::tags::save::PictureSpec]>,
) -> Result<String, PublishError> {
    let edits = check_managed_updates(managed_updates)?;
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
            PublishError::Capability(format!("format {extension} has no staged writer"))
        }
        other => PublishError::Validation(sanitize_tags_message(&name, other)),
    })?;
    Ok(document_from_fields(fields))
}

/// The semantic document of a file's fields, however they were read
/// (from the file itself, or from tags captured before an earlier
/// version of DroppedNeedle changed it).
pub fn document_from_fields(fields: super::super::tags::FieldDocument) -> TagDocument {
    let mut managed: BTreeMap<String, Vec<String>> = fields
        .values
        .into_iter()
        .map(|(field, values)| (field.name().to_owned(), values))
        .collect();
    // Each spelling's own values sort after the field's own entry, so on
    // replay they override what the field entry wrote to every spelling.
    for ((field, spelling), values) in fields.spellings {
        managed.insert(field.spelling_name(spelling), values);
    }
    let mut picture_bytes = BTreeMap::new();
    let pictures = fields
        .pictures
        .into_iter()
        .map(|picture| {
            let sha256 = super::snapshots::sha256_hex(&picture.data);
            picture_bytes.insert(sha256.clone(), picture.data);
            PictureRef {
                kind: picture.kind,
                mime: picture.mime,
                description: picture.description,
                sha256,
                info: picture.info,
            }
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
pub fn document_for_write(
    path: &Path,
    managed_updates: &BTreeMap<String, Vec<String>>,
) -> Result<TagDocument, PublishError> {
    let mut document = document_from_file(path)?;
    if let Some(name) = managed_updates
        .keys()
        .find(|name| document.opaque.contains(name))
    {
        return Err(PublishError::Capability(format!(
            "field {name} of '{}' holds a value undo could not restore",
            message_name_path(path)
        )));
    }
    for name in managed_updates.keys() {
        // A scrubbed native item's before-state is its values now, and
        // replaced pictures are the ones the file holds now, so undo
        // writes them back.
        let before = if name == PICTURES_KEY {
            document
                .pictures
                .iter()
                .flatten()
                .map(PictureRef::encode)
                .collect()
        } else {
            name.strip_prefix(NATIVE_PREFIX)
                .and_then(|key| document.custom.get(key))
                .cloned()
                .unwrap_or_default()
        };
        document.managed.entry(name.clone()).or_insert(before);
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
) -> BTreeMap<String, Vec<String>> {
    let writable_format = format_for_path(Path::new(rel_path)).is_ok_and(writable);
    if !writable_format || baseline.version < FULL_FIELDS_VERSION {
        return baseline.managed.clone();
    }
    let mut updates = baseline.managed.clone();
    // The baseline's pictures go back when the file's differ (a later
    // run embedded art), when the baseline's shape recorded them.
    if let (Some(original), Some(now)) = (&baseline.pictures, &current.pictures)
        && baseline.version >= RICH_FIELDS_VERSION
        && (original.len() != now.len()
            || original
                .iter()
                .zip(now)
                .any(|(original, now)| !original.same_picture(now)))
    {
        updates.insert(
            PICTURES_KEY.to_owned(),
            original.iter().map(PictureRef::encode).collect(),
        );
    }
    // Native items the baseline held and the file no longer holds as it
    // did (a later scrub) go back. Items added since are left alone.
    for (key, values) in &baseline.custom {
        if current.custom.get(key) != Some(values) {
            updates.insert(format!("{NATIVE_PREFIX}{key}"), values.clone());
        }
    }
    for field in TagField::ALL {
        if baseline.version < recorded_since(field) {
            continue;
        }
        let name = field.name();
        let opaque = |document: &TagDocument| document.opaque.iter().any(|kept| kept == name);
        if !opaque(baseline) && !opaque(current) {
            updates.entry(name.to_owned()).or_default();
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
            PublishError::Capability(format!("format {extension} has no staged writer"))
        }
        TagsError::Io { path, source } => {
            PublishError::Io(format!("io error on '{}': {source}", message_name(&path)))
        }
        TagsError::TagRead { path, reason }
        | TagsError::Probe { path, reason }
        | TagsError::Decode { path, reason }
        | TagsError::Fingerprint { path, reason } => PublishError::Validation(format!(
            "unreadable audio '{}': {reason}",
            message_name(&path)
        )),
        TagsError::SaveRefused { refusal, .. } => match refusal {
            Refusal::VerifyMismatch { .. } | Refusal::SaveFailed { .. } => {
                PublishError::Validation(format!(
                    "staged write failed for '{}': {refusal}",
                    message_name_path(work)
                ))
            }
            other => PublishError::Capability(other.to_string()),
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
