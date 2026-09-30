//! Real tag staging: the publish→tags seam (integrator-owned).
//!
//! The publisher stages destination bytes through the tags slice save
//! wrapper instead of appending a semantic sidecar. Staging runs on a
//! hidden work copy carrying the real audio extension (the journal
//! temps end in `.tmp`, which the tag router would refuse); on
//! success the staged bytes flow into the normal temp/journal/rename
//! protocol, on refusal the work copy is removed and nothing is
//! staged.
//!
//! Two loudness rules:
//!
//! * Managed-field names outside the save wrapper's five text keys
//!   block at preview time ([`check_managed_updates`]), never
//!   mid-publish.
//! * Items with no managed updates stage byte-identical without
//!   touching the tag stack, so pure moves never trip tag refusals
//!   (read-only formats, mixed ID3) on files whose tags they keep.
//!
//! `super::super::tags` resolves to the wired tags slice in the
//! library build and to the `tags` shim in the standalone publish
//! briefs; both spellings name the same module.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use lofty::tag::ItemKey;

use super::super::tags::{Refusal, TagEdit, TagsError, save_tags};
use super::PublishError;
use super::tags_seam::TagDocument;

/// Managed-field name to save-wrapper key. The wrapper supports five
/// text keys; everything else is outside the writable surface.
fn key_for_name(name: &str) -> Option<ItemKey> {
    match name {
        "title" => Some(ItemKey::TrackTitle),
        "artist" => Some(ItemKey::TrackArtist),
        "album" => Some(ItemKey::AlbumTitle),
        "album_artist" => Some(ItemKey::AlbumArtist),
        "genre" => Some(ItemKey::Genre),
        _ => None,
    }
}

/// Map managed updates onto save-wrapper edits. Unknown names block
/// loudly: the caller runs this at preview time so Apply never meets
/// a name the writer cannot express.
pub fn check_managed_updates(
    managed_updates: &BTreeMap<String, Vec<String>>,
) -> Result<Vec<TagEdit>, PublishError> {
    let mut edits = Vec::with_capacity(managed_updates.len());
    for (name, values) in managed_updates {
        match key_for_name(name) {
            Some(key) => edits.push(TagEdit::new(key, values.clone())),
            None => {
                return Err(PublishError::Capability(format!(
                    "field {name} is outside the staged writer's surface"
                )));
            }
        }
    }
    Ok(edits)
}

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

/// Hidden work copy for tag staging: the journal temp with its `.tmp`
/// suffix swapped for `.staging.<ext>` so the tag router sees the
/// real container. The hidden prefix and journal id survive, so the
/// work copy stays pruned and unique.
fn work_path_for(temp: &Path, extension: &str) -> Result<PathBuf, PublishError> {
    let file_name = temp
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| PublishError::UnsafePath("staging temp name is not UTF-8".into()))?;
    let Some(parent) = temp.parent() else {
        return Err(PublishError::UnsafePath(
            "staging temp has no parent directory".into(),
        ));
    };
    let stem = file_name.strip_suffix(".tmp").unwrap_or(file_name);
    Ok(parent.join(format!("{stem}.staging.{extension}")))
}

/// Render staged bytes for one plan item. `temp` is the item's
/// journal temp (its parent directory already exists); the work copy
/// is removed on every path out.
pub fn render_staged_bytes(
    source: &[u8],
    format: &str,
    managed_updates: &BTreeMap<String, Vec<String>>,
    temp: &Path,
) -> Result<Vec<u8>, PublishError> {
    if managed_updates.is_empty() {
        return Ok(source.to_vec());
    }
    let edits = check_managed_updates(managed_updates)?;
    let extension = staged_extension(format)?;
    let work = work_path_for(temp, extension)?;
    if let Err(error) = std::fs::write(&work, source) {
        return Err(PublishError::Io(error.to_string()));
    }
    let staged = match save_tags(&work, &edits) {
        Ok(_) => std::fs::read(&work).map_err(PublishError::from),
        Err(error) => Err(map_tags_error(&work, error)),
    };
    let _ = std::fs::remove_file(&work);
    staged
}

/// Read the current semantic tag document for one audio file, keeping
/// only fields the staged writer can write back. Undo and baseline
/// restore replay `managed` through staging, so anything stored here
/// must round-trip; unknown frames and custom tags stay preserved by
/// the save wrapper's byte-level handling, not by this document.
pub fn document_from_file(path: &Path) -> Result<TagDocument, PublishError> {
    let name = message_name_path(path);
    let format = super::super::tags::format_for_path(path).map_err(|error| match error {
        TagsError::UnrecognizedExtension { extension } => {
            PublishError::Capability(format!("format {extension} has no staged writer"))
        }
        other => PublishError::Validation(sanitize_tags_message(&name, other)),
    })?;
    let tag = super::super::tags::read::read_tag_only(path, format)
        .map_err(|error| PublishError::Validation(sanitize_tags_message(&name, error)))?;
    let mut managed = BTreeMap::new();
    if !tag.title.is_empty() {
        managed.insert("title".to_owned(), vec![tag.title]);
    }
    if !tag.artist.is_empty() {
        managed.insert("artist".to_owned(), vec![tag.artist]);
    }
    if !tag.album.is_empty() {
        managed.insert("album".to_owned(), vec![tag.album]);
    }
    if let Some(album_artist) = tag.album_artist
        && !album_artist.is_empty()
    {
        managed.insert("album_artist".to_owned(), vec![album_artist]);
    }
    if !tag.genres.is_empty() {
        managed.insert("genre".to_owned(), tag.genres);
    } else if let Some(genre) = tag.genre
        && !genre.is_empty()
    {
        managed.insert("genre".to_owned(), vec![genre]);
    }
    Ok(TagDocument {
        managed,
        custom: BTreeMap::new(),
        unknown_frames: BTreeMap::new(),
    })
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
