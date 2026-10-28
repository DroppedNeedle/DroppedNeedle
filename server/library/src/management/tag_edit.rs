//! The tag editor: show one file's tags next to its saved overrides, and
//! turn an edit into a preview. An edit is written like any other
//! management run (staged, journaled, undoable). "Save" also keeps the
//! value as an override so later runs keep it; "write once" only writes
//! it; "reset" drops the override so the edition's value comes back.

use std::collections::BTreeMap;
use std::path::Path;

use rusqlite::{OptionalExtension as _, params};

use super::models::{
    JobIntent, JobMode, ManagementError, Origin, PreviewHandle, Selection, SelectionKind,
    TagEditField, TagEditIntent, TagEditMode,
};
use super::profile;
use super::reasons;
use super::service::{JobSpec, Management};
use crate::tags::fields::FieldKind;
use crate::tags::{TagEdit, TagField, save};

/// Fields the album as a whole owns: an edit changes every file of it.
const ALBUM_FIELDS: &[TagField] = &[
    TagField::Album,
    TagField::AlbumSort,
    TagField::AlbumArtist,
    TagField::AlbumArtists,
    TagField::AlbumArtistSort,
    TagField::Compilation,
    TagField::DiscTotal,
    TagField::Date,
    TagField::OriginalDate,
    TagField::ReleaseStatus,
    TagField::ReleaseCountry,
    TagField::ReleaseType,
    TagField::Label,
    TagField::CatalogNumber,
    TagField::Barcode,
    TagField::Asin,
    TagField::MusicBrainzReleaseId,
    TagField::MusicBrainzReleaseGroupId,
    TagField::MusicBrainzAlbumArtistId,
];

/// Fields that hold several values in order.
const LIST_FIELDS: &[TagField] = &[
    TagField::Artists,
    TagField::AlbumArtists,
    TagField::Genre,
    TagField::Label,
    TagField::CatalogNumber,
    TagField::Isrc,
    TagField::ReleaseType,
    TagField::MusicBrainzArtistId,
    TagField::MusicBrainzAlbumArtistId,
    TagField::MusicBrainzWorkId,
    TagField::Composer,
    TagField::Lyricist,
    TagField::Conductor,
    TagField::Performer,
    TagField::Arranger,
    TagField::Remixer,
    TagField::Producer,
    TagField::Engineer,
    TagField::Mixer,
    TagField::DjMixer,
    TagField::Writer,
    TagField::Director,
];

/// `album` or `track`.
pub fn scope(field: TagField) -> &'static str {
    if ALBUM_FIELDS.contains(&field) {
        "album"
    } else {
        "track"
    }
}

/// `string`, `integer`, `boolean`, or `ordered_strings` (v2's names).
pub fn cardinality(field: TagField) -> &'static str {
    match field.kind() {
        FieldKind::Count => "integer",
        FieldKind::Flag => "boolean",
        _ if LIST_FIELDS.contains(&field) => "ordered_strings",
        _ => "string",
    }
}

/// A file's value as the editor shows it.
fn shown(field: TagField, values: &[String]) -> serde_json::Value {
    match (field.kind(), values) {
        (_, []) => serde_json::Value::Null,
        (FieldKind::Count, [one]) => one
            .split('/')
            .next()
            .and_then(|number| number.trim().parse::<i64>().ok())
            .map_or_else(
                || serde_json::Value::String(one.clone()),
                serde_json::Value::from,
            ),
        (FieldKind::Flag, [one]) => serde_json::Value::Bool(one == "1"),
        _ if LIST_FIELDS.contains(&field) => serde_json::Value::from(values.to_vec()),
        _ => profile::json_value(values),
    }
}

/// An edited value as tag values; empty clears the field.
fn values_of(field: TagField, value: &serde_json::Value) -> Result<Vec<String>, ManagementError> {
    let values: Vec<String> = match value {
        serde_json::Value::Null => Vec::new(),
        serde_json::Value::Bool(flag) => vec![if *flag { "1" } else { "0" }.to_owned()],
        serde_json::Value::Number(number) => vec![number.to_string()],
        serde_json::Value::String(text) => vec![text.trim().to_owned()],
        serde_json::Value::Array(items) => items
            .iter()
            .map(|item| match item {
                serde_json::Value::String(text) => text.trim().to_owned(),
                other => other.to_string(),
            })
            .collect(),
        serde_json::Value::Object(_) => {
            return Err(ManagementError::Invalid(format!(
                "The value for {} must be text, a number, or a list.",
                field.name()
            )));
        }
    };
    let values: Vec<String> = values
        .into_iter()
        .filter(|value| !value.is_empty())
        .collect();
    if !values.is_empty() && !save::accepts(&TagEdit::new(field, values.clone())) {
        return Err(ManagementError::Invalid(format!(
            "\"{}\" is not a valid value for {}.",
            values.join("; "),
            field.name()
        )));
    }
    Ok(values)
}

/// One field in the editor.
#[derive(Debug, Clone)]
pub struct EditorField {
    pub field_name: String,
    pub scope: &'static str,
    pub cardinality: &'static str,
    pub current_value: serde_json::Value,
    pub override_id: Option<String>,
    pub override_mode: Option<String>,
    pub override_row_revision: Option<i64>,
}

/// What the editor needs to show one file.
#[derive(Debug, Clone)]
pub struct EditorContext {
    pub local_track_id: String,
    pub local_album_id: String,
    pub root_id: String,
    pub profile_id: String,
    pub profile_name: String,
    pub settings_revision: String,
    pub policy_revision: String,
    pub track_revision: i64,
    pub album_revision: i64,
    pub accepted_identity: bool,
    /// Why the file has no accepted identity, as a reason code.
    pub identity_reason: Option<String>,
    pub fields: Vec<EditorField>,
}

/// An edit to preview.
pub struct TagEditRequest {
    pub local_track_id: String,
    pub mode: TagEditMode,
    pub expected_settings_revision: String,
    pub expected_policy_revision: String,
    /// (field name, value). A reset ignores the value.
    pub fields: Vec<(String, serde_json::Value)>,
    pub idempotency_key: Option<String>,
}

struct Subject {
    track_id: String,
    album_id: String,
    root_id: String,
    relative_path: String,
    track_revision: i64,
    album_revision: i64,
}

/// A saved override: (subject kind, field) -> (id, mode, row revision).
type SavedOverrides = BTreeMap<(String, String), (String, String, i64)>;

impl Management {
    fn subject(&self, track_id: &str) -> Result<Subject, ManagementError> {
        self.setup.db.read_blocking("management.subject", |conn| {
            conn.query_row(
                "SELECT t.id, t.local_album_id, t.root_id, t.relative_path, t.row_revision, \
                 a.row_revision FROM local_tracks t JOIN local_albums a ON a.id = t.local_album_id \
                 WHERE t.id = ?1 AND t.availability = 'indexed'",
                params![track_id],
                |row| {
                    Ok(Subject {
                        track_id: row.get(0)?,
                        album_id: row.get(1)?,
                        root_id: row.get(2)?,
                        relative_path: row.get(3)?,
                        track_revision: row.get(4)?,
                        album_revision: row.get(5)?,
                    })
                },
            )
            .optional()?
            .ok_or(ManagementError::NotFound(
                "This track is not in the library.",
            ))
        })
    }

    fn saved_overrides(&self, subject: &Subject) -> Result<SavedOverrides, ManagementError> {
        self.setup
            .db
            .read_blocking("management.saved_overrides", |conn| {
                let mut statement = conn.prepare(
                    "SELECT subject_kind, field_name, id, mode, row_revision \
                 FROM library_management_overrides \
                 WHERE (subject_kind = 'album' AND local_album_id = ?1) \
                 OR (subject_kind = 'track' AND local_track_id = ?2)",
                )?;
                let rows =
                    statement.query_map(params![subject.album_id, subject.track_id], |row| {
                        Ok((
                            (row.get::<_, String>(0)?, row.get::<_, String>(1)?),
                            (
                                row.get::<_, String>(2)?,
                                row.get::<_, String>(3)?,
                                row.get::<_, i64>(4)?,
                            ),
                        ))
                    })?;
                Ok(rows.collect::<rusqlite::Result<_>>()?)
            })
    }

    /// One file's tags, saved overrides and match, for the tag editor.
    /// Blocking.
    pub fn tag_editor(&self, track_id: &str) -> Result<EditorContext, ManagementError> {
        let subject = self.subject(track_id)?;
        let sandbox = self.setup.publishing.publish_sandbox().map_err(|_| {
            ManagementError::Conflict(
                "The track's library folder is not available right now.".to_owned(),
            )
        })?;
        let path = sandbox
            .resolve_no_symlink(&subject.root_id, &subject.relative_path)
            .map_err(|_| {
                ManagementError::Conflict(
                    "This file is a symbolic link or sits outside its library folder, so its tags cannot be edited."
                        .to_owned(),
                )
            })?;
        if !Path::new(&path).is_file() {
            return Err(ManagementError::Conflict(
                "The audio file is no longer on disk. Rescan the library.".to_owned(),
            ));
        }
        let doc = crate::publish::staging::document_from_file(&path).map_err(|error| {
            tracing::info!(%error, track_id, "tag editor could not read the file");
            ManagementError::Conflict("The audio file's tags could not be read.".to_owned())
        })?;
        let stored = self.settings()?;
        let pinned = profile::pin_assignment(
            &stored,
            &profile::root_assignment(&stored, &subject.root_id),
        )?;
        let saved = self.saved_overrides(&subject)?;
        let store = &self.setup.identify_store;
        let identity_reason = match store.album_identity(&subject.album_id)? {
            None => Some(reasons::identity_not_accepted().code),
            Some(album) if album.release_mbid.is_none() || album.release_group_mbid.is_none() => {
                Some(reasons::release_not_selected().code)
            }
            Some(_) => match store.track_identity(&subject.track_id)? {
                Some(track)
                    if track.recording_mbid.is_some() && track.release_track_mbid.is_some() =>
                {
                    None
                }
                _ => Some(reasons::track_not_mapped().code),
            },
        };
        let fields = TagField::ALL
            .into_iter()
            .map(|field| {
                let scope = scope(field);
                let saved = saved.get(&(scope.to_owned(), field.name().to_owned()));
                EditorField {
                    field_name: field.name().to_owned(),
                    scope,
                    cardinality: cardinality(field),
                    current_value: shown(
                        field,
                        doc.managed.field(field).map_or(&[], Vec::as_slice),
                    ),
                    override_id: saved.map(|(id, _, _)| id.clone()),
                    override_mode: saved.map(|(_, mode, _)| mode.clone()),
                    override_row_revision: saved.map(|(_, _, revision)| *revision),
                }
            })
            .collect();
        Ok(EditorContext {
            local_track_id: subject.track_id,
            local_album_id: subject.album_id,
            root_id: subject.root_id,
            profile_id: pinned.profile.id.clone(),
            profile_name: pinned.profile.name.clone(),
            settings_revision: crate::management::settings::settings_revision(&stored),
            policy_revision: self
                .setup
                .core
                .policy_revision()
                .map_err(|error| ManagementError::Store(error.to_string()))?,
            track_revision: subject.track_revision,
            album_revision: subject.album_revision,
            accepted_identity: identity_reason.is_none(),
            identity_reason,
            fields,
        })
    }

    /// Turn a tag edit into a preview of the file (or, for album fields,
    /// the whole album). Blocking.
    pub fn create_tag_edit(
        &self,
        request: TagEditRequest,
        actor: &str,
    ) -> Result<PreviewHandle, ManagementError> {
        let subject = self.subject(&request.local_track_id)?;
        if request.fields.is_empty() || request.fields.len() > TagField::ALL.len() {
            return Err(ManagementError::Invalid(
                "Pick at least one tag to edit.".to_owned(),
            ));
        }
        let saved = self.saved_overrides(&subject)?;
        let mut names = Vec::new();
        let mut fields = Vec::new();
        for (name, value) in &request.fields {
            let field = TagField::from_name(name).ok_or_else(|| {
                ManagementError::Invalid(format!("\"{name}\" is not a tag the editor can change."))
            })?;
            if names.contains(name) {
                return Err(ManagementError::Invalid(format!(
                    "{name} is in the edit twice; edit each tag once."
                )));
            }
            names.push(name.clone());
            let scope = scope(field);
            let existing = saved.get(&(scope.to_owned(), name.clone()));
            let values = if request.mode == TagEditMode::ResetCanonical {
                if existing.is_none() {
                    return Err(ManagementError::Invalid(format!(
                        "{name} has no saved value to reset."
                    )));
                }
                Vec::new()
            } else {
                values_of(field, value)?
            };
            fields.push(TagEditField {
                field_name: name.clone(),
                subject_kind: scope.to_owned(),
                values,
                override_id: existing.map(|(id, _, _)| id.clone()),
                expected_override_row_revision: existing.map(|(_, _, revision)| *revision),
                previous: match existing {
                    Some((id, mode, _)) => {
                        self.setup
                            .db
                            .read_blocking("management.create_tag_edit", |conn| {
                                Ok::<_, ManagementError>(conn
                            .query_row(
                                "SELECT value_json FROM library_management_overrides WHERE id = ?1",
                                params![id],
                                |row| row.get::<_, String>(0),
                            )
                            .optional()?
                            .map(|value| (value, mode.clone())))
                            })?
                    }
                    None => None,
                },
            });
        }
        let stored = self.settings()?;
        let registry = self.check_revisions(
            &stored,
            &request.expected_settings_revision,
            &request.expected_policy_revision,
        )?;
        let pinned = profile::tag_edit_profile(&stored, &subject.root_id, &names)?;
        let selection = Selection {
            kind: SelectionKind::Tracks,
            ids: vec![subject.track_id.clone()],
            catalog_filter: None,
        };
        self.start_job(JobSpec {
            mode: JobMode::Preview,
            origin: Origin::Manual,
            actor: Some(actor),
            idempotency_key: request.idempotency_key.as_deref(),
            revisions: profile::revisions(&stored, &pinned, registry.policy_revision()),
            selection_json: serde_json::to_string(&selection)?,
            pinned: &pinned,
            target_root_id: None,
            linked_job_id: None,
            intent: JobIntent {
                tag_edit: Some(TagEditIntent {
                    local_track_id: subject.track_id,
                    local_album_id: subject.album_id,
                    mode: request.mode,
                    fields,
                }),
                ..JobIntent::default()
            },
            proposed_settings_revision: None,
            retention_hours: stored.preview_retention_hours,
        })
    }
}
