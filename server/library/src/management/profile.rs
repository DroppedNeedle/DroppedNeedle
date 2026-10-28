//! What a profile asks of one file: the tags it should end up with (from
//! the album's chosen edition, the profile's field modes, and its tagging
//! scripts) and the path its naming script gives it.

use std::collections::{BTreeMap, BTreeSet};

use super::models::{FieldMutation, ManagementError, PinnedProfile, Revisions};
use super::reasons::{self, ItemReason};
use super::script::{self, Statement, Value, Variables};
use crate::management::settings;
use crate::matching::model::{Release, ReleaseTrack};
use crate::publish::{TagDocument, TagTarget, TagUpdate, TagUpdates};
use crate::tags::{TagEdit, TagField, picard, save};
use droppedneedle_kernel::runtime_config::sections::{
    FieldMode, LibraryManagement, LibraryManagementProfile, LibraryManagementRootAssignment,
    LibraryManagementRootOverrides, TaggingScript,
};

/// Freeze the profile a manual preview runs with: the stored profile with
/// the request's overrides on top, and the scripts it names.
pub fn pin(
    stored: &LibraryManagement,
    profile_id: &str,
    overrides: Option<LibraryManagementRootOverrides>,
) -> Result<PinnedProfile, ManagementError> {
    if !stored
        .profiles
        .iter()
        .any(|profile| profile.id == profile_id)
    {
        return Err(ManagementError::Invalid(
            "That management profile does not exist; reload the page and pick another.".to_owned(),
        ));
    }
    let assignment = LibraryManagementRootAssignment {
        root_id: "__manual_preview__".to_owned(),
        profile_id: Some(profile_id.to_owned()),
        overrides: overrides.clone(),
        ..LibraryManagementRootAssignment::default()
    };
    let mut pinned = pin_assignment(stored, &assignment)?;
    pinned.overrides = overrides;
    Ok(pinned)
}

/// The assignment a library folder has, or the default profile when it
/// has none.
pub fn root_assignment(
    stored: &LibraryManagement,
    root_id: &str,
) -> LibraryManagementRootAssignment {
    stored
        .root_assignments
        .iter()
        .find(|assignment| assignment.root_id == root_id)
        .cloned()
        .unwrap_or_else(|| LibraryManagementRootAssignment {
            root_id: root_id.to_owned(),
            profile_id: Some(stored.default_profile_id.clone()),
            ..LibraryManagementRootAssignment::default()
        })
}

/// A tag-only profile for the tag editor (as in v2): the folder's own
/// profile with only the edited fields managed: no scripts, genres,
/// lyrics, ReplayGain, scrubbing, renames, moves or artwork.
pub fn tag_edit_profile(
    stored: &LibraryManagement,
    root_id: &str,
    field_names: &[String],
) -> Result<PinnedProfile, ManagementError> {
    let mut pinned = pin_assignment(stored, &root_assignment(stored, root_id))?;
    let profile = &mut pinned.profile;
    profile.metadata.enabled = true;
    profile.metadata.fields = field_names
        .iter()
        .map(
            |name| droppedneedle_kernel::runtime_config::sections::ManagedField {
                field: name.clone(),
                mode: FieldMode::Replace,
                ..Default::default()
            },
        )
        .collect();
    profile.metadata.tagging_script_ids.clear();
    profile.metadata.preserve_fields.clear();
    profile.metadata.scrub_unmanaged_tags = false;
    profile.genres.enabled = false;
    profile.enrichment.lyrics.enabled = false;
    profile.enrichment.replaygain.enabled = false;
    profile.artwork.embedded_enabled = false;
    profile.artwork.external_enabled = false;
    profile.organization.rename_enabled = false;
    profile.organization.move_enabled = false;
    profile.organization.move_sidecars = false;
    profile.revision = settings::profile_revision(profile);
    pinned.tagging_scripts.clear();
    Ok(pinned)
}

/// The folder's own profile, with its folder overrides, that writes tags
/// only: the profile's fields, scripts, overrides, artwork and lyrics, and
/// never a rename or a move. The revision is worked out again, so the
/// files never count as managed by the full profile.
pub fn tags_only(
    stored: &LibraryManagement,
    root_id: &str,
) -> Result<PinnedProfile, ManagementError> {
    let mut pinned = pin_assignment(stored, &root_assignment(stored, root_id))?;
    let organization = &mut pinned.profile.organization;
    organization.rename_enabled = false;
    organization.move_enabled = false;
    organization.move_sidecars = false;
    pinned.profile.revision = settings::profile_revision(&pinned.profile);
    Ok(pinned)
}

/// Freeze the profile a library folder's assignment gives: its profile
/// with the folder's overrides, and the scripts it names.
pub fn pin_assignment(
    stored: &LibraryManagement,
    assignment: &LibraryManagementRootAssignment,
) -> Result<PinnedProfile, ManagementError> {
    let invalid =
        |error: droppedneedle_kernel::runtime_config::settings_error::SettingsError| match error {
            droppedneedle_kernel::runtime_config::settings_error::SettingsError::InvalidInput {
                message,
            } => ManagementError::Invalid(format!(
                "{message} Edit the profile in Settings > Library Management."
            )),
            other => ManagementError::Store(format!("{other:?}")),
        };
    let mut profile = settings::pin_profile(stored, assignment).map_err(invalid)?;
    profile.revision = settings::profile_revision(&profile);
    let (naming_script, multi_disc_naming_script) =
        settings::pin_naming_scripts(stored, &profile).map_err(invalid)?;
    let mut tagging_scripts = Vec::new();
    for script_id in &profile.metadata.tagging_script_ids {
        let script = stored
            .tagging_scripts
            .iter()
            .find(|script| &script.id == script_id)
            .ok_or_else(|| {
                ManagementError::Invalid(format!(
                    "The profile uses a tagging script that no longer exists ({script_id}); \
                     edit the profile in Settings > Library Management."
                ))
            })?;
        tagging_scripts.push(script.clone());
    }
    Ok(PinnedProfile {
        profile,
        naming_script,
        multi_disc_naming_script,
        tagging_scripts,
        overrides: None,
    })
}

/// The revisions a preview pins.
pub fn revisions(stored: &LibraryManagement, pinned: &PinnedProfile, policy: &str) -> Revisions {
    let overrides = serde_json::to_value(&pinned.overrides).unwrap_or(serde_json::Value::Null);
    Revisions {
        profile: pinned.profile.revision.clone(),
        settings: settings::settings_revision(stored),
        naming: settings::naming_policy_revision(
            &pinned.naming_script,
            pinned.multi_disc_naming_script.as_ref(),
        ),
        policy: policy.to_owned(),
        overrides: crate::policy::stable_hash(&overrides),
    }
}

/// A content revision as the `u64` the publish seal compares.
pub fn seal_revision(revision: &str) -> u64 {
    let hex = crate::publish::snapshots::sha256_hex(revision.as_bytes());
    u64::from_str_radix(&hex[..16], 16).unwrap_or(0)
}

/// Tagging scripts compiled once per job.
pub struct CompiledScripts(Vec<(TaggingScript, Result<Vec<Statement>, script::ScriptFault>)>);

impl CompiledScripts {
    pub fn new(pinned: &PinnedProfile) -> Self {
        Self(
            pinned
                .tagging_scripts
                .iter()
                .map(|script| (script.clone(), script::compile_tagging(&script.source)))
                .collect(),
        )
    }
}

/// The tag field a profile field writes; genre is the Genres section's.
fn target(name: &str) -> Option<TagField> {
    use TagField::*;
    Some(match name {
        "title" => Title,
        "title_sort" => TitleSort,
        "artist" => Artist,
        "artists" => Artists,
        "artist_sort" => ArtistSort,
        "album" => Album,
        "album_sort" => AlbumSort,
        "album_artist" => AlbumArtist,
        "album_artists" => AlbumArtists,
        "album_artist_sort" => AlbumArtistSort,
        "track_number" => TrackNumber,
        "total_tracks" => TrackTotal,
        "disc_number" => DiscNumber,
        "total_discs" => DiscTotal,
        "disc_subtitle" => DiscSubtitle,
        "date" => Date,
        "original_date" => OriginalDate,
        "release_status" => ReleaseStatus,
        "release_country" => ReleaseCountry,
        "release_type" => ReleaseType,
        "compilation" => Compilation,
        "asin" => Asin,
        "media" => Media,
        "label" => Label,
        "catalog_number" => CatalogNumber,
        "barcode" => Barcode,
        "isrc" => Isrc,
        "musicbrainz_recording_id" => MusicBrainzRecordingId,
        "musicbrainz_release_track_id" => MusicBrainzReleaseTrackId,
        "musicbrainz_release_id" => MusicBrainzReleaseId,
        "musicbrainz_release_group_id" => MusicBrainzReleaseGroupId,
        "musicbrainz_artist_id" => MusicBrainzArtistId,
        "musicbrainz_album_artist_id" => MusicBrainzAlbumArtistId,
        "work" => Work,
        "musicbrainz_work_id" => MusicBrainzWorkId,
        "composer" => Composer,
        "lyricist" => Lyricist,
        "conductor" => Conductor,
        "performer" => Performer,
        "arranger" => Arranger,
        "remixer" => Remixer,
        "producer" => Producer,
        "engineer" => Engineer,
        "mixer" => Mixer,
        "dj_mixer" => DjMixer,
        "writer" => Writer,
        "director" => Director,
        "movement" => Movement,
        "movement_number" => MovementNumber,
        "movement_count" => MovementCount,
        "comment" => Comment,
        "copyright" => Copyright,
        "encoded_by" => EncodedBy,
        "acoustid_id" => AcoustIdId,
        "acoustid_fingerprint" => AcoustIdFingerprint,
        _ => return None,
    })
}

/// A tag field by its name or a script and naming spelling of it.
pub fn tag_field(name: &str) -> Option<TagField> {
    TagField::from_name(name).or(match name {
        "albumartist" => Some(TagField::AlbumArtist),
        "track" => Some(TagField::TrackNumber),
        "disc" => Some(TagField::DiscNumber),
        "year" => Some(TagField::Date),
        "tracktotal" => Some(TagField::TrackTotal),
        "disctotal" => Some(TagField::DiscTotal),
        _ => None,
    })
}

/// A field's current values: the plain name, or its first Vorbis spelling.
fn current(doc: &TagDocument, field: TagField) -> Vec<String> {
    doc.managed
        .field_or_spelling(field)
        .cloned()
        .unwrap_or_default()
}

fn mode_value(
    mode: FieldMode,
    clear: bool,
    existing: &[String],
    canonical: Option<&Vec<String>>,
) -> Option<Vec<String>> {
    match (mode, canonical) {
        (FieldMode::Disabled | FieldMode::Preserve, _) => None,
        (FieldMode::Replace, Some(values)) => Some(values.clone()),
        (FieldMode::Replace, None) if clear => Some(Vec::new()),
        (FieldMode::FillMissing, Some(values)) if existing.is_empty() => Some(values.clone()),
        (FieldMode::Merge, Some(values)) => {
            let mut merged = existing.to_vec();
            for value in values {
                if !merged.contains(value) {
                    merged.push(value.clone());
                }
            }
            Some(merged)
        }
        _ => None,
    }
}

/// Whether an ID3v2.3 write joins this field's values into one: a plain
/// text or TXXX frame (ISRC reads back split, so it stays a list).
fn joined_on_v23(field: TagField) -> bool {
    use crate::tags::fields::Id3Target;
    match field.id3(true) {
        Id3Target::Text(id) => id != "TSRC",
        Id3Target::User(_) => true,
        _ => false,
    }
}

/// [`mode_value`] for a field an ID3v2.3 file holds as one "/"-joined
/// value: the release's values are joined the same way, and merge counts
/// a value as present when it stands between slashes in the file's
/// ("AC/DC" stays whole).
fn joined_mode_value(
    mode: FieldMode,
    clear: bool,
    existing: &[String],
    canonical: &[String],
) -> Option<Vec<String>> {
    if mode != FieldMode::Merge {
        let joined = vec![canonical.join("/")];
        return mode_value(mode, clear, existing, Some(&joined));
    }
    let mut merged = existing.join("/");
    for value in canonical {
        if !format!("/{merged}/").contains(&format!("/{value}/")) {
            if !merged.is_empty() {
                merged.push('/');
            }
            merged.push_str(value);
        }
    }
    Some(vec![merged])
}

/// Every field as a script value: one value as text, several as a list.
pub fn field_variables(fields: &BTreeMap<String, Vec<String>>) -> Variables {
    fields
        .iter()
        .map(|(name, values)| {
            let value = match values.as_slice() {
                [] => Value::Text(String::new()),
                [one] => Value::Text(one.clone()),
                many => Value::List(many.to_vec()),
            };
            (name.clone(), value)
        })
        .collect()
}

/// The tags one file should end up with.
pub struct TagPlan {
    /// What to write: changed fields only.
    pub updates: TagUpdates,
    /// Every tag field after the plan, for display and naming.
    pub desired: BTreeMap<String, Vec<String>>,
    pub mutations: Vec<FieldMutation>,
    pub warnings: Vec<String>,
    /// Native items outside the managed fields a scrub removes, with the
    /// values they held.
    pub scrubbed: Vec<(String, Vec<String>)>,
    /// Genre sources that could not be asked.
    pub deferred: Vec<String>,
}

/// One file as tag planning sees it.
#[derive(Clone, Copy)]
pub struct FileInput<'a> {
    pub release: &'a Release,
    pub track: &'a ReleaseTrack,
    /// The file's tags now.
    pub doc: &'a TagDocument,
    pub format: &'a str,
    /// The write lands as ID3v2.3, which holds several values of one text
    /// frame as one "/"-joined value.
    pub id3v23: bool,
    /// False for containers the writer cannot tag.
    pub writable: bool,
    pub overrides: &'a super::planner::Overrides,
    /// What the outside sources said about the album.
    pub extras: &'a super::enrich::AlbumExtras,
    /// The library track the file is.
    pub track_id: &'a str,
    /// What LRCLIB said about the file's lyrics.
    pub lyrics: &'a super::enrich::LyricsAnswer,
}

/// Plan the tags for one file from its edition, the profile and its
/// scripts. `writable` is false for containers the writer cannot tag:
/// their tags are left as they are and the plan says so.
pub fn plan_tags(
    pinned: &PinnedProfile,
    scripts: &CompiledScripts,
    file: &FileInput<'_>,
) -> Result<TagPlan, ItemReason> {
    let FileInput {
        release,
        track,
        doc,
        format,
        id3v23,
        writable,
        overrides,
        extras,
        track_id,
        lyrics,
    } = *file;
    let profile: &LibraryManagementProfile = &pinned.profile;
    let mut desired: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for field in TagField::ALL {
        let values = current(doc, field);
        if !values.is_empty() {
            desired.insert(field.name().to_owned(), values);
        }
    }
    let before = desired.clone();
    let mut warnings = Vec::new();
    let mut deferred: Vec<String> = Vec::new();
    let genre_kept = profile
        .metadata
        .preserve_fields
        .iter()
        .any(|name| name == "genre");
    if profile.genres.enabled && !genre_kept {
        let existing = desired.get("genre").cloned().unwrap_or_default();
        let projection = super::genre::project(&profile.genres, release, &extras.genres, &existing);
        for source in &projection.deferred {
            let name = match *source {
                "listenbrainz" => "ListenBrainz",
                _ => "Last.fm",
            };
            warnings.push(if projection.preserved_existing {
                format!("{name} genres could not be read, so the file keeps its genres.")
            } else {
                format!("{name} genres could not be read; the genres come from the other sources.")
            });
        }
        deferred = projection
            .deferred
            .iter()
            .map(|source| (*source).to_owned())
            .collect();
        let mut genres = projection.genres;
        let constrained = matches!(format, "m4a" | "aac" | "wav");
        if constrained
            && (profile.genres.write_primary_only_for_constrained_formats
                || profile
                    .metadata
                    .format_compatibility
                    .constrained_genres_primary_only)
        {
            genres.truncate(1);
        }
        if !genres.is_empty() {
            desired.insert("genre".to_owned(), genres);
        }
    }
    let lyrics_settings = &profile.enrichment.lyrics;
    if lyrics_settings.enabled && lyrics_settings.write_plain {
        let has = desired
            .get(TagField::Lyrics.name())
            .is_some_and(|values| values.iter().any(|value| !value.trim().is_empty()));
        if let super::enrich::LyricsAnswer::Found(found) = lyrics
            && let Some(plain) = &found.plain
            && !(lyrics_settings.preserve_existing && has)
        {
            desired.insert(TagField::Lyrics.name().to_owned(), vec![plain.clone()]);
        }
    }
    let file_extras = extras.files.get(track_id);
    replaygain(
        profile,
        file_extras.and_then(|extra| extra.replaygain.as_ref()),
        extras.replaygain_note.as_deref(),
        &mut desired,
        &mut warnings,
    )?;
    if profile.metadata.enabled {
        let custom = super::custom::is_custom(release);
        // A custom edition keeps the names as credited: there is no
        // MusicBrainz release to ask for the artists' own.
        let naming = if custom {
            picard::ArtistNaming::credited()
        } else {
            picard::ArtistNaming::new(
                &profile.metadata.artist_credits,
                release.artist_names.as_ref(),
            )
        };
        // Artist fields whose names were not fetched are left out, and
        // keep the file's values below, never cleared.
        let (mut canonical, unresolved) = picard::named_release_tags(release, track, &naming);
        // Credits come from the recording's MusicBrainz relationships;
        // without them (not fetched, or fetched for another recording) a
        // credit field is left alone, never cleared.
        let relations = if custom {
            None
        } else {
            release.track_relations(track)
        };
        if let Some(relations) = relations {
            canonical.extend(picard::credit_tags(
                relations,
                &naming,
                super::enrich::other_roles(profile),
            ));
        }
        // Credits stored before the "Other roles" were fetched (used when
        // their refetch failed) do not say the track has none.
        let other_roles_fetched = release
            .recording_relations
            .as_ref()
            .is_some_and(|stored| stored.version >= super::enrich::OTHER_ROLES_RELATIONS_VERSION);
        // AcoustID values come from the file's audio. These fields never
        // clear, so a value not worked out leaves the file's alone.
        if let Some(found) = file_extras {
            for (field, value) in [
                (TagField::AcoustIdFingerprint, &found.acoustid_print),
                (TagField::AcoustIdId, &found.acoustid_id),
            ] {
                if let Some(value) = value {
                    canonical.insert(field.name().to_owned(), vec![value.clone()]);
                }
            }
        }
        warnings.extend(
            extras
                .acoustid_notes
                .iter()
                .filter(|(track, _)| track == track_id)
                .map(|(_, note)| note.clone()),
        );
        let mut credits_missing = false;
        let mut names_missing = false;
        let mut preserved: BTreeSet<&str> = profile
            .metadata
            .preserve_fields
            .iter()
            .filter_map(|name| target(name).map(TagField::name))
            .collect();
        if custom {
            // A custom edition has no release behind it: the file keeps
            // whatever release ids it holds (v2's custom release claims).
            preserved.insert(TagField::MusicBrainzReleaseId.name());
            preserved.insert(TagField::MusicBrainzReleaseTrackId.name());
        }
        for managed in &profile.metadata.fields {
            let Some(field) = target(&managed.field) else {
                continue;
            };
            if preserved.contains(field.name()) {
                continue;
            }
            if unresolved.contains(&field) {
                names_missing |= !matches!(managed.mode, FieldMode::Disabled | FieldMode::Preserve);
                continue;
            }
            if let Some(asked) = super::enrich::credit_asked(profile, &managed.field) {
                // Custom editions and roles the Relationships settings
                // leave out keep the file's credits.
                if custom || !asked {
                    continue;
                }
                if relations.is_none() {
                    credits_missing |= extras.credits != super::enrich::Credits::NotAsked
                        && !matches!(managed.mode, FieldMode::Disabled | FieldMode::Preserve);
                    continue;
                }
                if !other_roles_fetched && super::enrich::other_role_field(&managed.field) {
                    continue;
                }
                // A medley's work has no one title: the file keeps its own.
                if field == TagField::Work && relations.is_some_and(|found| found.works.len() > 1) {
                    continue;
                }
            }
            let existing = desired.get(field.name()).cloned().unwrap_or_default();
            if id3v23
                && joined_on_v23(field)
                && let Some(values) = canonical.get(field.name())
            {
                if let Some(values) = joined_mode_value(
                    managed.mode,
                    managed.clear_when_canonical_missing,
                    &existing,
                    values,
                ) {
                    desired.insert(field.name().to_owned(), values);
                }
                continue;
            }
            if let Some(values) = mode_value(
                managed.mode,
                managed.clear_when_canonical_missing,
                &existing,
                canonical.get(field.name()),
            ) {
                desired.insert(field.name().to_owned(), values);
            }
        }
        if credits_missing && extras.credits == super::enrich::Credits::Ready {
            // Fetched within the week, yet not for this track's recording
            // (or a work came without its credits): asking again gives
            // the same, so the file keeps its credits and is not held.
            tracing::info!(
                release = release.id,
                track = track.id,
                "MusicBrainz credits do not cover this track's recording; its credits are kept"
            );
            warnings.push(
                "MusicBrainz now lists this track differently; its credits and work are left as \
                 they are until it is identified again."
                    .to_owned(),
            );
        } else if credits_missing {
            if extras.automatic {
                return Err(credits_unavailable());
            }
            warnings.push(
                "MusicBrainz credits could not be read, so the file keeps its credits and work."
                    .to_owned(),
            );
        }
        if names_missing && extras.names == super::enrich::Credits::Ready {
            // Fetched just now or within the week, yet without an artist
            // the stored credits name: MusicBrainz changed the credit
            // since identification, and asking again gives the same.
            tracing::info!(
                release = release.id,
                "MusicBrainz no longer credits an artist the stored edition names; \
                 the artist tags are kept"
            );
            warnings.push(
                "MusicBrainz now credits this release differently; its artist tags are left as \
                 they are until it is identified again."
                    .to_owned(),
            );
        } else if names_missing && extras.names != super::enrich::Credits::NotAsked {
            if extras.automatic {
                return Err(names_unavailable());
            }
            warnings.push(
                "MusicBrainz artist names could not be read, so the file keeps its artist tags."
                    .to_owned(),
            );
        }
        let mut protected: BTreeSet<String> =
            preserved.iter().map(|name| (*name).to_owned()).collect();
        // Saved overrides win over the edition and over scripts.
        for (field, mode, values) in &overrides.fields {
            let name = field.name().to_owned();
            let value = match mode.as_str() {
                "replace" => values.clone(),
                "clear" => Vec::new(),
                _ => before.get(&name).cloned().unwrap_or_default(),
            };
            desired.insert(name.clone(), value);
            protected.insert(name);
        }
        for (script, program) in &scripts.0 {
            let program = program
                .as_ref()
                .map_err(|fault| reasons::script_failed(&script.name, &fault.to_string()))?;
            let refresh = |fields: &BTreeMap<String, Vec<String>>| field_variables(fields);
            script::run_tagging(
                program,
                &mut desired,
                &protected,
                &refresh,
                &mut script::Budget::default(),
            )
            .map_err(|fault| reasons::script_failed(&script.name, &fault.to_string()))?;
            // Scripts may use naming spellings (`albumartist`); fold them
            // onto the tag field, and say so for names nothing can write.
            let names: Vec<String> = desired.keys().cloned().collect();
            for name in names {
                if TagField::from_name(&name).is_some() {
                    continue;
                }
                let values = desired.remove(&name).unwrap_or_default();
                match tag_field(&name) {
                    Some(field) => {
                        desired.insert(field.name().to_owned(), values);
                    }
                    None => warnings.push(format!(
                        "The script \"{}\" set \"{name}\", which this server cannot write; it was skipped.",
                        script.name
                    )),
                }
            }
        }
    }
    desired.retain(|_, values| !values.is_empty());
    if id3v23 {
        // Compare in the form the file can hold, or every plan would see
        // a change.
        for (name, values) in desired.iter_mut() {
            if values.len() > 1 && TagField::from_name(name).is_some_and(joined_on_v23) {
                *values = vec![values.join("/")];
            }
        }
    }
    let mut updates = TagUpdates::new();
    let mut mutations = Vec::new();
    let names: BTreeSet<&String> = before.keys().chain(desired.keys()).collect();
    for name in names {
        let old = before.get(name).cloned().unwrap_or_default();
        let new = desired.get(name).cloned().unwrap_or_default();
        if old == new {
            continue;
        }
        let Some(field) = TagField::from_name(name) else {
            continue;
        };
        if doc.opaque.contains(name) {
            warnings.push(format!(
                "\"{name}\" holds a value in a form undo could not put back, so it is left as it is."
            ));
            continue;
        }
        // MP4 has no home for some fields (performer, arranger).
        let homeless = format == "m4a" && field.mp4().is_none();
        if !new.is_empty() && (homeless || !save::accepts(&TagEdit::new(field, new.clone()))) {
            warnings.push(format!(
                "\"{name}\" cannot hold {} in a {} file, so it is left as it is.",
                new.join("; "),
                format.to_uppercase()
            ));
            continue;
        }
        mutations.push(FieldMutation {
            name: name.clone(),
            operation: if new.is_empty() { "clear" } else { "set" }.to_owned(),
            before: json_value(&old),
            after: json_value(&new),
        });
        updates.insert(TagTarget::Field(field), new);
    }
    // Scrubbing (v2's rule): every native item outside the managed
    // fields goes, except the ones the profile preserves and v2's other
    // managed spellings, which always stay. The values are recorded in
    // the before-state, so undo and a baseline restore put them back.
    let mut scrubbed = Vec::new();
    if profile.metadata.enabled && profile.metadata.scrub_unmanaged_tags {
        let preserved: Vec<String> = profile
            .metadata
            .preserve_fields
            .iter()
            .flat_map(|name| preserved_native(name))
            .collect();
        for (key, values) in &doc.custom {
            let folded = key.to_ascii_lowercase();
            if preserved.contains(&folded) || scrub_keeps(&folded) {
                continue;
            }
            let target = TagTarget::Native(key.clone());
            mutations.push(FieldMutation {
                name: target.name(),
                operation: "clear".to_owned(),
                before: json_value(values),
                after: serde_json::Value::Null,
            });
            updates.insert(target, Vec::new());
            scrubbed.push((key.clone(), values.clone()));
        }
    }
    if !writable && !updates.is_empty() {
        warnings.push(format!(
            "Tags cannot be written to {} files, so only the file's path is managed.",
            format.to_uppercase()
        ));
        updates = TagUpdates::new();
        mutations.clear();
        scrubbed.clear();
    }
    // What the file holds after the write: skipped fields keep their value.
    let mut desired = before;
    for update in updates.iter() {
        if let TagUpdate::Field { field, values } = update {
            if values.is_empty() {
                desired.remove(field.name());
            } else {
                desired.insert(field.name().to_owned(), values.to_vec());
            }
        }
    }
    Ok(TagPlan {
        updates,
        desired,
        mutations,
        warnings,
        scrubbed,
        deferred,
    })
}

/// An automatic run's file whose MusicBrainz credits could not be read:
/// it is written once they can be, never with its credits left stale.
fn credits_unavailable() -> ItemReason {
    ItemReason {
        code: "METADATA_UNAVAILABLE".to_owned(),
        message: "MusicBrainz credits for this file could not be read, so it was not written."
            .to_owned(),
        action: "Automatic management looks again by itself; a preview by hand writes the other \
                 fields and keeps the file's credits."
            .to_owned(),
    }
}

/// An automatic run's file whose artist names could not be read from
/// MusicBrainz: it is written once they can be.
fn names_unavailable() -> ItemReason {
    ItemReason {
        code: "METADATA_UNAVAILABLE".to_owned(),
        message: "MusicBrainz artist names for this file could not be read, so it was not written."
            .to_owned(),
        action: "Automatic management looks again by itself; a preview by hand writes the other \
                 fields and keeps the file's artist tags."
            .to_owned(),
    }
}

/// Native spellings (folded keys) a scrub never removes: every spelling
/// in v2's per-format maps (`_ID3_TXXX`, the ID3 frame list, the Vorbis
/// and MP4 key maps), plus spellings of v2's other managed names
/// (comment, encoder, copyright, publisher, grouping). Spellings a
/// writable field owns never reach the scrub; they are listed so the
/// list can be checked against v2's whole.
const SCRUB_KEEPS: &[&str] = &[
    "id3:comm",
    "id3:grp1",
    "id3:ipls",
    "id3:mvin",
    "id3:mvnm",
    "id3:sylt",
    "id3:talb",
    "id3:tcmp",
    "id3:tcom",
    "id3:tcon",
    "id3:tcop",
    "id3:tdor",
    "id3:tdrc",
    "id3:tenc",
    "id3:text",
    "id3:tipl",
    "id3:tit1",
    "id3:tit2",
    "id3:tmcl",
    "id3:tmed",
    "id3:tpe1",
    "id3:tpe2",
    "id3:tpe3",
    "id3:tpe4",
    "id3:tpos",
    "id3:tpub",
    "id3:trck",
    "id3:tso2",
    "id3:tsoa",
    "id3:tsop",
    "id3:tsot",
    "id3:tsrc",
    "id3:tsst",
    "id3:txxx:acoustid fingerprint",
    "id3:txxx:acoustid id",
    "id3:txxx:arranger",
    "id3:txxx:artists",
    "id3:txxx:asin",
    "id3:txxx:barcode",
    "id3:txxx:catalognumber",
    "id3:txxx:label",
    "id3:txxx:musicbrainz album artist id",
    "id3:txxx:musicbrainz album id",
    "id3:txxx:musicbrainz album release country",
    "id3:txxx:musicbrainz album status",
    "id3:txxx:musicbrainz album type",
    "id3:txxx:musicbrainz artist id",
    "id3:txxx:musicbrainz release group id",
    "id3:txxx:musicbrainz release track id",
    "id3:txxx:musicbrainz work id",
    "id3:txxx:producer",
    "id3:txxx:replaygain_album_gain",
    "id3:txxx:replaygain_album_peak",
    "id3:txxx:replaygain_track_gain",
    "id3:txxx:replaygain_track_peak",
    "id3:txxx:work",
    "id3:uslt",
    "mp4:----:acoustid fingerprint",
    "mp4:----:acoustid id",
    "mp4:----:arranger",
    "mp4:----:artists",
    "mp4:----:asin",
    "mp4:----:barcode",
    "mp4:----:catalognumber",
    "mp4:----:conductor",
    "mp4:----:discsubtitle",
    "mp4:----:isrc",
    "mp4:----:label",
    "mp4:----:lyricist",
    "mp4:----:media",
    "mp4:----:musicbrainz album artist id",
    "mp4:----:musicbrainz album id",
    "mp4:----:musicbrainz album release country",
    "mp4:----:musicbrainz album status",
    "mp4:----:musicbrainz album type",
    "mp4:----:musicbrainz artist id",
    "mp4:----:musicbrainz release group id",
    "mp4:----:musicbrainz release track id",
    "mp4:----:musicbrainz track id",
    "mp4:----:musicbrainz work id",
    "mp4:----:originaldate",
    "mp4:----:performer",
    "mp4:----:producer",
    "mp4:----:remixer",
    "mp4:----:replaygain_album_gain",
    "mp4:----:replaygain_album_peak",
    "mp4:----:replaygain_track_gain",
    "mp4:----:replaygain_track_peak",
    "mp4:aart",
    "mp4:cpil",
    "mp4:cprt",
    "mp4:disk",
    "mp4:soaa",
    "mp4:soal",
    "mp4:soar",
    "mp4:sonm",
    "mp4:trkn",
    "mp4:\u{a9}alb",
    "mp4:\u{a9}art",
    "mp4:\u{a9}cmt",
    "mp4:\u{a9}day",
    "mp4:\u{a9}gen",
    "mp4:\u{a9}lyr",
    "mp4:\u{a9}mvc",
    "mp4:\u{a9}mvi",
    "mp4:\u{a9}mvn",
    "mp4:\u{a9}nam",
    "mp4:\u{a9}too",
    "mp4:\u{a9}wrk",
    "mp4:\u{a9}wrt",
    "vorbis:acoustid_fingerprint",
    "vorbis:acoustid_id",
    "vorbis:album",
    "vorbis:albumartist",
    "vorbis:albumartists",
    "vorbis:albumartistsort",
    "vorbis:albumsort",
    "vorbis:arranger",
    "vorbis:artist",
    "vorbis:artists",
    "vorbis:artistsort",
    "vorbis:asin",
    "vorbis:barcode",
    "vorbis:catalognumber",
    "vorbis:comment",
    "vorbis:compilation",
    "vorbis:composer",
    "vorbis:conductor",
    "vorbis:copyright",
    "vorbis:date",
    "vorbis:description",
    "vorbis:discnumber",
    "vorbis:discsubtitle",
    "vorbis:disctotal",
    "vorbis:encoded-by",
    "vorbis:encodedby",
    "vorbis:genre",
    "vorbis:isrc",
    "vorbis:label",
    "vorbis:lyricist",
    "vorbis:lyrics",
    "vorbis:media",
    "vorbis:movement",
    "vorbis:movementname",
    "vorbis:movementnumber",
    "vorbis:movementtotal",
    "vorbis:musicbrainz_albumartistid",
    "vorbis:musicbrainz_albumid",
    "vorbis:musicbrainz_albumstatus",
    "vorbis:musicbrainz_albumtype",
    "vorbis:musicbrainz_artistid",
    "vorbis:musicbrainz_releasegroupid",
    "vorbis:musicbrainz_releasetrackid",
    "vorbis:musicbrainz_trackid",
    "vorbis:musicbrainz_workid",
    "vorbis:organization",
    "vorbis:originaldate",
    "vorbis:performer",
    "vorbis:producer",
    "vorbis:publisher",
    "vorbis:releasecountry",
    "vorbis:releasestatus",
    "vorbis:releasetype",
    "vorbis:remixer",
    "vorbis:replaygain_album_gain",
    "vorbis:replaygain_album_peak",
    "vorbis:replaygain_track_gain",
    "vorbis:replaygain_track_peak",
    "vorbis:syncedlyrics",
    "vorbis:title",
    "vorbis:titlesort",
    "vorbis:totaldiscs",
    "vorbis:totaltracks",
    "vorbis:tracknumber",
    "vorbis:tracktotal",
    "vorbis:unsyncedlyrics",
    "vorbis:work",
];

/// Whether a scrub keeps this native item (a folded key): v2's managed
/// names in every spelling, every MusicBrainz and AcoustID id, and
/// ReplayGain items beyond the written ones.
fn scrub_keeps(folded: &str) -> bool {
    if SCRUB_KEEPS.contains(&folded) {
        return true;
    }
    let name = folded
        .split_once(':')
        .map_or(folded, |(_, rest)| rest)
        .trim_start_matches("txxx:")
        .trim_start_matches("----:");
    name.starts_with("musicbrainz")
        || name.starts_with("acoustid")
        || name.starts_with("replaygain")
}

/// The native items (folded keys) a preserved name keeps from a scrub,
/// for common names outside the managed fields (`grouping`, `mood`) and
/// for any key named outright (`vorbis:mood`). A managed field never
/// reaches the scrub.
fn preserved_native(name: &str) -> Vec<String> {
    let name = name.trim().to_ascii_lowercase();
    if name.contains(':') {
        return vec![name];
    }
    let keys: &[&str] = match name.as_str() {
        "grouping" => &["id3:tit1", "vorbis:grouping", "mp4:\u{a9}grp"],
        "bpm" => &["id3:tbpm", "vorbis:bpm"],
        "encoder" => &["id3:tsse", "vorbis:encoder"],
        "mood" => &["id3:tmoo", "vorbis:mood", "mp4:----:mood"],
        "language" => &["id3:tlan", "vorbis:language", "mp4:----:language"],
        "script" => &["id3:txxx:script", "vorbis:script", "mp4:----:script"],
        _ => &[],
    };
    keys.iter().map(|key| (*key).to_owned()).collect()
}

/// Put measured ReplayGain into the file's fields as the profile's mode
/// says: preserve never writes, fill-missing writes absent fields only,
/// replace writes them all. Album values follow `album_aware`.
fn replaygain(
    profile: &LibraryManagementProfile,
    measured: Option<&super::enrich::ReplayGainValues>,
    note: Option<&str>,
    desired: &mut BTreeMap<String, Vec<String>>,
    warnings: &mut Vec<String>,
) -> Result<(), ItemReason> {
    use droppedneedle_kernel::runtime_config::sections::ReplayGainMode;
    let settings = &profile.enrichment.replaygain;
    if !settings.enabled || settings.mode == ReplayGainMode::Preserve {
        return Ok(());
    }
    let mut values: Vec<(TagField, Option<&String>)> = vec![
        (
            TagField::ReplayGainTrackGain,
            measured.map(|found| &found.track_gain),
        ),
        (
            TagField::ReplayGainTrackPeak,
            measured.map(|found| &found.track_peak),
        ),
    ];
    if settings.album_aware {
        values.push((
            TagField::ReplayGainAlbumGain,
            measured.and_then(|found| found.album_gain.as_ref()),
        ));
        values.push((
            TagField::ReplayGainAlbumPeak,
            measured.and_then(|found| found.album_peak.as_ref()),
        ));
    }
    let mut missing = false;
    for (field, value) in values {
        let present = desired.contains_key(field.name());
        match value {
            Some(value) if settings.mode == ReplayGainMode::Replace || !present => {
                desired.insert(field.name().to_owned(), vec![value.clone()]);
            }
            Some(_) => {}
            None => missing |= !present,
        }
    }
    if missing {
        if settings.required {
            return Err(reasons::metadata_unavailable("ReplayGain"));
        }
        warnings.push(match note {
            Some(note) => format!("ReplayGain was not written: {note}"),
            None => {
                "ReplayGain could not be measured for this file, so it was not written.".to_owned()
            }
        });
    }
    Ok(())
}

/// Field values as the pages show them: one value as text, several as a
/// list, none as null.
pub fn json_value(values: &[String]) -> serde_json::Value {
    match values {
        [] => serde_json::Value::Null,
        [one] => serde_json::Value::String(one.clone()),
        many => serde_json::Value::from(many.to_vec()),
    }
}

/// Facts about a file that naming reads besides its tags.
pub struct NamingFacts<'a> {
    pub extension: &'a str,
    pub codec: &'a str,
    pub bit_rate: Option<i64>,
    pub sample_rate: Option<i64>,
    pub bit_depth: Option<i64>,
    pub medium_format: String,
    pub medium_number: i64,
    /// The edition's MusicBrainz disambiguation, or empty.
    pub album_disambiguation: String,
}

fn first(fields: &BTreeMap<String, Vec<String>>, name: &str) -> String {
    fields
        .get(name)
        .and_then(|values| values.first())
        .cloned()
        .unwrap_or_default()
}

fn list(fields: &BTreeMap<String, Vec<String>>, name: &str) -> Value {
    Value::List(fields.get(name).cloned().unwrap_or_default())
}

fn number(text: &str) -> Value {
    text.split('/')
        .next()
        .and_then(|part| part.trim().parse::<i64>().ok())
        .map_or_else(|| Value::Text(text.to_owned()), Value::Int)
}

/// The one-letter bucket for an album artist ("The Cure" files under C).
fn initial(artist: &str) -> String {
    let name = artist.trim();
    let lowered = name.to_lowercase();
    let name = if lowered.starts_with("the ") {
        name.get(4..).unwrap_or(name).trim_start()
    } else {
        name
    };
    match name.chars().next() {
        Some(c) if c.is_alphabetic() => c.to_uppercase().collect(),
        _ => "#".to_owned(),
    }
}

/// The variables a naming script reads, from the file's planned tags.
pub fn naming_variables(
    fields: &BTreeMap<String, Vec<String>>,
    facts: &NamingFacts<'_>,
) -> Variables {
    let mut variables = field_variables(fields);
    let artist = first(fields, "artist");
    let album_artist = first(fields, "album_artist");
    let albumartist = if album_artist.is_empty() {
        artist.clone()
    } else {
        album_artist.clone()
    };
    let date = first(fields, "date");
    let genre = first(fields, "genre");
    let kbps = facts.bit_rate.map(|bits| (bits + 500) / 1000).unwrap_or(0);
    let pairs = [
        ("artist", Value::Text(artist.clone())),
        ("artist_display", Value::Text(artist)),
        ("artists", list(fields, "artists")),
        ("artist_sorts", list(fields, "artist_sort")),
        ("album_artist", Value::Text(album_artist.clone())),
        ("album_artist_display", Value::Text(album_artist)),
        (
            "album_artists",
            Value::List(fields.get("album_artist").cloned().unwrap_or_default()),
        ),
        ("album_artist_sorts", list(fields, "album_artist_sort")),
        ("initial", Value::Text(initial(&albumartist))),
        ("albumartist", Value::Text(albumartist)),
        ("genre", Value::Text(genre.clone())),
        ("primary_genre", Value::Text(genre)),
        ("genres", list(fields, "genre")),
        ("year", Value::Text(date.chars().take(4).collect())),
        ("track", number(&first(fields, "track_number"))),
        ("disc", number(&first(fields, "disc_number"))),
        ("ext", Value::Text(facts.extension.to_owned())),
        ("extension", Value::Text(facts.extension.to_owned())),
        ("medium", Value::Text(first(fields, "media"))),
        (
            "musicbrainz_id",
            Value::Text(first(fields, "musicbrainz_release_group_id")),
        ),
        (
            "artist_mbid",
            Value::Text(first(fields, "musicbrainz_artist_id")),
        ),
        ("codec", Value::Text(facts.codec.to_owned())),
        (
            "quality",
            Value::Text(if facts.bit_depth.is_some() {
                "lossless".to_owned()
            } else {
                format!("{kbps}kbps")
            }),
        ),
        ("bitrate", Value::Int(kbps)),
        (
            "sample_rate",
            facts.sample_rate.map_or(Value::Null, Value::Int),
        ),
        ("bit_depth", facts.bit_depth.map_or(Value::Null, Value::Int)),
        (
            "album_disambiguation",
            Value::Text(facts.album_disambiguation.clone()),
        ),
        ("medium_format", Value::Text(facts.medium_format.clone())),
        ("medium_number", Value::Int(facts.medium_number)),
    ];
    for (name, value) in pairs {
        variables.insert(name.to_owned(), value);
    }
    variables
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::management::enrich::{AlbumExtras, Credits, LyricsAnswer};
    use crate::management::planner::Overrides;
    use crate::matching::model::{RecordingRelations, ReleaseRelations, RoleCredit, WorkCredit};
    use droppedneedle_kernel::runtime_config::sections::RelationshipType;

    /// The classical probe: Gould and Karajan's Beethoven concerto, then
    /// Sibelius and a spoken track that performs no work.
    fn classical() -> (Release, ReleaseRelations) {
        recorded("release_relations_classical.json", &[])
    }

    /// Relationships to retype, each `(artist, recorded type, new type)`.
    type Retype = [(&'static str, &'static str, &'static str)];

    /// A recorded relations answer as planning sees it, with the
    /// relationships in `retype` retyped in the JSON first: the types the
    /// recordings lack, made from recorded shapes.
    fn recorded(fixture: &str, retype: &Retype) -> (Release, ReleaseRelations) {
        fn retype_in(value: &mut serde_json::Value, retype: &Retype) {
            match value {
                serde_json::Value::Object(map) => {
                    let artist = map
                        .get("artist")
                        .and_then(|artist| artist.get("name"))
                        .and_then(serde_json::Value::as_str);
                    let kind = map.get("type").and_then(serde_json::Value::as_str);
                    if let Some((_, _, to)) = retype
                        .iter()
                        .find(|(name, from, _)| Some(*name) == artist && Some(*from) == kind)
                    {
                        map.insert("type".to_owned(), serde_json::Value::from(*to));
                    }
                    map.values_mut().for_each(|child| retype_in(child, retype));
                }
                serde_json::Value::Array(items) => {
                    items.iter_mut().for_each(|item| retype_in(item, retype));
                }
                _ => {}
            }
        }
        let path = format!(
            "{}/../tests/fixtures/musicbrainz/{fixture}",
            env!("CARGO_MANIFEST_DIR")
        );
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).expect("probe reads")).expect("parses");
        retype_in(&mut value, retype);
        let wire: droppedneedle_providers::musicbrainz::MbRelease =
            serde_json::from_value(value).expect("probe decodes");
        let relations = crate::identify::sources::relations_from_wire(&wire, 1);
        let tracks = wire.media[0]
            .tracks
            .iter()
            .map(|track| ReleaseTrack {
                id: track.id.clone(),
                recording_id: track.recording.as_ref().expect("recording").id.clone(),
                title: track.title.clone().unwrap_or_default(),
                disc: 1,
                position: track.position.unwrap_or(0),
                ..ReleaseTrack::default()
            })
            .collect();
        let release = Release {
            id: wire.id.clone(),
            release_group_id: "22222222-2222-4222-8222-222222222222".to_owned(),
            title: wire.title.clone().unwrap_or_default(),
            tracks,
            ..Release::default()
        };
        (release, relations)
    }

    /// The Picard profile with every credit field on replace, clear as
    /// given, and `relationships` adjusted by `adjust`.
    fn credit_profile(
        clear: bool,
        adjust: impl Fn(&mut droppedneedle_kernel::runtime_config::sections::RelationshipCreditSettings),
    ) -> PinnedProfile {
        let mut settings = settings::initial_settings("");
        for profile in &mut settings.profiles {
            for field in &mut profile.metadata.fields {
                if super::super::enrich::CREDIT_FIELDS
                    .iter()
                    .any(|(name, _)| *name == field.field)
                {
                    field.mode = FieldMode::Replace;
                    field.clear_when_canonical_missing = clear;
                }
            }
            adjust(&mut profile.metadata.relationships);
            profile.genres.enabled = false;
        }
        pin(&settings, settings::PICARD_ORGANIZER_PROFILE_ID, None).expect("pinned")
    }

    /// A file holding a credit of every kind already.
    fn tagged() -> TagDocument {
        let managed: BTreeMap<String, Vec<String>> = [
            "composer",
            "lyricist",
            "conductor",
            "performer",
            "arranger",
            "remixer",
            "producer",
            "engineer",
            "mixer",
            "dj_mixer",
            "writer",
            "director",
            "work",
            "musicbrainz_work_id",
        ]
        .iter()
        .map(|name| ((*name).to_owned(), vec![format!("Old {name}")]))
        .collect();
        serde_json::from_value(serde_json::json!({"managed": managed, "custom": {}}))
            .expect("document")
    }

    fn plan_credits(
        pinned: &PinnedProfile,
        release: &Release,
        index: usize,
        extras: &AlbumExtras,
    ) -> Result<TagPlan, ItemReason> {
        let doc = tagged();
        plan_tags(
            pinned,
            &CompiledScripts::new(pinned),
            &FileInput {
                release,
                track: &release.tracks[index],
                doc: &doc,
                format: "flac",
                id3v23: false,
                writable: true,
                overrides: &Overrides::default(),
                extras,
                track_id: "track",
                lyrics: &LyricsAnswer::Off,
            },
        )
    }

    fn credit(role: &str, relation_type: &str, name: &str, attributes: &[&str]) -> RoleCredit {
        RoleCredit {
            role: role.to_owned(),
            relation_type: relation_type.to_owned(),
            name: name.to_owned(),
            artist_id: format!("id-{name}"),
            attributes: attributes.iter().map(|value| (*value).to_owned()).collect(),
            ..RoleCredit::default()
        }
    }

    /// Credits from the recording's and its work's MusicBrainz
    /// relationships, each role as Picard writes it. Only what was
    /// fetched for this very recording is used; anything else leaves the
    /// file's credits alone, even with clear ticked, and an automatic run
    /// will not write the file at all.
    #[test]
    fn credits_come_from_the_fetched_relationships() {
        let (mut release, relations) = classical();
        release.graft_relations(&relations);
        let ready = AlbumExtras {
            credits: Credits::Ready,
            ..AlbumExtras::default()
        };
        let pinned = credit_profile(true, |_| {});
        let value =
            |plan: &TagPlan, name: &str| plan.desired.get(name).cloned().unwrap_or_default();
        let owned = |values: &[&str]| values.iter().map(|v| (*v).to_owned()).collect::<Vec<_>>();
        let old = |name: &str| vec![format!("Old {name}")];

        // Each role's values; roles nobody holds are cleared.
        let plan = plan_credits(&pinned, &release, 0, &ready).expect("planned");
        assert_eq!(value(&plan, "composer"), owned(&["Ludwig van Beethoven"]));
        assert_eq!(value(&plan, "conductor"), owned(&["Herbert von Karajan"]));
        assert_eq!(
            value(&plan, "performer"),
            owned(&["Glenn Gould (piano)", "Berliner Philharmoniker (orchestra)"])
        );
        assert_eq!(
            value(&plan, "work"),
            owned(&[
                "Concerto for Piano and Orchestra no. 3 in C minor, op. 37: I. Allegro con brio"
            ])
        );
        assert_eq!(
            value(&plan, "musicbrainz_work_id"),
            owned(&["d6843235-961a-37ab-85a4-a0e7e027f752"])
        );
        for name in ["lyricist", "arranger", "remixer", "producer"] {
            assert_eq!(value(&plan, name), Vec::<String>::new(), "{name}");
        }
        assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);

        // Fetched with nothing there: clear removes them.
        let plan = plan_credits(&pinned, &release, 6, &ready).expect("planned");
        assert_eq!(value(&plan, "composer"), Vec::<String>::new());
        assert_eq!(value(&plan, "work"), Vec::<String>::new());

        // A role switched off, or the switch itself, is left alone; the
        // work fields ignore those settings.
        let no_conductor = credit_profile(true, |relationships| {
            relationships
                .types
                .retain(|kind| *kind != RelationshipType::Conductor);
        });
        let plan = plan_credits(&no_conductor, &release, 0, &ready).expect("planned");
        assert_eq!(value(&plan, "conductor"), old("conductor"));
        assert_eq!(value(&plan, "composer"), owned(&["Ludwig van Beethoven"]));
        let switched_off = credit_profile(true, |relationships| relationships.enabled = false);
        let plan = plan_credits(&switched_off, &release, 0, &ready).expect("planned");
        assert_eq!(value(&plan, "composer"), old("composer"));
        assert_eq!(
            value(&plan, "musicbrainz_work_id"),
            owned(&["d6843235-961a-37ab-85a4-a0e7e027f752"])
        );

        // Not fetched: left alone with one warning, or an automatic run
        // refuses the file.
        let (bare, _) = classical();
        let unavailable = AlbumExtras {
            credits: Credits::Unavailable,
            ..AlbumExtras::default()
        };
        let plan = plan_credits(&pinned, &bare, 0, &unavailable).expect("planned");
        assert_eq!(value(&plan, "composer"), old("composer"));
        assert_eq!(plan.warnings.len(), 1, "{:?}", plan.warnings);
        let automatic = AlbumExtras {
            credits: Credits::Unavailable,
            automatic: true,
            ..AlbumExtras::default()
        };
        let reason = plan_credits(&pinned, &bare, 0, &automatic)
            .err()
            .expect("an automatic run refuses the file");
        assert_eq!(reason.code, "METADATA_UNAVAILABLE");

        // A track the answer left out, or one fetched for another
        // recording, keeps its credits.
        let mut partial = relations.clone();
        partial.tracks.remove(&release.tracks[1].id);
        if let Some(entry) = partial.tracks.get_mut(&release.tracks[2].id) {
            entry.recording_id = "another-recording".to_owned();
        }
        let (mut grafted, _) = classical();
        grafted.graft_relations(&partial);
        // Asking again would give the same answer, so an automatic run
        // writes the rest with a warning instead of waiting for ever.
        let fetched_automatic = AlbumExtras {
            credits: Credits::Ready,
            automatic: true,
            ..AlbumExtras::default()
        };
        for index in [1, 2] {
            let plan = plan_credits(&pinned, &grafted, index, &ready).expect("planned");
            assert_eq!(value(&plan, "composer"), old("composer"), "track {index}");
            let plan = plan_credits(&pinned, &grafted, index, &fetched_automatic)
                .expect("an automatic run is not blocked");
            assert_eq!(value(&plan, "composer"), old("composer"), "track {index}");
            assert_eq!(
                plan.warnings,
                vec![
                    "MusicBrainz now lists this track differently; its credits and work are \
                     left as they are until it is identified again."
                        .to_owned()
                ],
                "track {index}"
            );
        }
        release.recording_relations = grafted.recording_relations.clone();
        if let Some(stored) = release.recording_relations.as_mut() {
            stored.tracks.insert(
                release.tracks[2].id.clone(),
                RecordingRelations {
                    recording_id: "another-recording".to_owned(),
                    ..RecordingRelations::default()
                },
            );
        }
        let plan = plan_credits(&pinned, &release, 2, &ready).expect("planned");
        assert_eq!(value(&plan, "composer"), old("composer"));
    }

    /// Picard 2.13.3's other relationship types (`_ARTIST_REL_TYPES`), on
    /// the recorded answers and on recorded relationships retyped to the
    /// types they lack. With "Other roles" off, every track plans what the
    /// main roles alone gave. With it on, engineer, mixer, DJ-mixer,
    /// writer and director are written, and a folded type only under a
    /// main role that is ticked too. Credits stored before these roles
    /// were fetched leave the five as the file has them.
    #[test]
    fn other_roles_follow_picard() {
        use droppedneedle_kernel::runtime_config::sections::RelationshipCreditSettings;
        /// The types parsed before "Other roles".
        const MAIN_TYPES: [&str; 9] = [
            "arranger",
            "composer",
            "conductor",
            "instrument",
            "performer",
            "vocal",
            "lyricist",
            "producer",
            "remixer",
        ];
        fn without(kind: RelationshipType) -> impl Fn(&mut RelationshipCreditSettings) {
            move |relationships| relationships.types.retain(|held| *held != kind)
        }
        let main_only = |relations: &ReleaseRelations| {
            let mut kept = relations.clone();
            for track in kept.tracks.values_mut() {
                track
                    .credits
                    .retain(|credit| MAIN_TYPES.contains(&credit.relation_type.as_str()));
            }
            kept
        };
        let ready = AlbumExtras {
            credits: Credits::Ready,
            ..AlbumExtras::default()
        };
        let desired = |pinned: &PinnedProfile, release: &Release, index: usize| {
            plan_credits(pinned, release, index, &ready)
                .expect("planned")
                .desired
        };
        let get = |desired: &BTreeMap<String, Vec<String>>, name: &str| {
            desired.get(name).cloned().unwrap_or_default()
        };
        let owned = |values: &[&str]| values.iter().map(|v| (*v).to_owned()).collect::<Vec<_>>();
        let none = Vec::<String>::new();
        let old = |name: &str| vec![format!("Old {name}")];
        let grafted = |(mut release, relations): (Release, ReleaseRelations)| {
            release.graft_relations(&relations);
            (release, relations)
        };

        let classical = "release_relations_classical.json";
        let pop = "release_relations_pop.json";
        let (karajan, gould, berlin, beethoven) = (
            "Herbert von Karajan",
            "Glenn Gould",
            "Berliner Philharmoniker",
            "Ludwig van Beethoven",
        );
        let engineer_mix_librettist = [
            (karajan, "conductor", "engineer"),
            (gould, "instrument", "mix-DJ"),
            (berlin, "performing orchestra", "mix"),
            (beethoven, "composer", "librettist"),
        ];
        let dj_director_folded = [
            (karajan, "conductor", "mix-DJ"),
            (berlin, "performing orchestra", "audio director"),
            (gould, "instrument", "orchestrator"),
            (beethoven, "composer", "chorus master"),
        ];
        let unmapped = [
            (karajan, "conductor", "mastering"),
            (berlin, "performing orchestra", "recording"),
        ];
        let variants: [(&str, &Retype); 5] = [
            (classical, &[]),
            (pop, &[]),
            (classical, &engineer_mix_librettist),
            (classical, &dj_director_folded),
            (classical, &unmapped),
        ];

        // Other off: exactly what the main roles alone plan, every track.
        let off = credit_profile(true, without(RelationshipType::Other));
        for (fixture, retype) in variants {
            let (release, relations) = grafted(recorded(fixture, retype));
            let mut today = release.clone();
            today.graft_relations(&main_only(&relations));
            for index in 0..release.tracks.len() {
                assert_eq!(
                    desired(&off, &release, index),
                    desired(&off, &today, index),
                    "{fixture} {retype:?} track {index}"
                );
            }
        }

        let on = credit_profile(true, |_| {});
        let (release, _) = grafted(recorded(classical, &[]));
        assert_eq!(
            get(&desired(&on, &release, 0), "performer"),
            owned(&["Glenn Gould (piano)", "Berliner Philharmoniker (orchestra)"])
        );
        let no_performer = credit_profile(true, without(RelationshipType::Performer));
        assert_eq!(
            get(&desired(&no_performer, &release, 0), "performer"),
            old("performer")
        );
        let (release, _) = grafted(recorded(pop, &[]));
        let bad_romance = release
            .tracks
            .iter()
            .position(|track| track.id == "9ab08ef5-1ed9-3175-bcf4-aa232008a5f2")
            .expect("Bad Romance");
        assert_eq!(
            get(&desired(&on, &release, bad_romance), "writer"),
            owned(&["Stefani Germanotta", "Nadir Khayat"])
        );

        let (release, _) = grafted(recorded(classical, &engineer_mix_librettist));
        let planned = desired(&on, &release, 0);
        assert_eq!(get(&planned, "engineer"), owned(&[karajan]));
        assert_eq!(get(&planned, "mixer"), owned(&[berlin]));
        assert_eq!(get(&planned, "dj_mixer"), none, "a DJ-mix with attributes");
        assert_eq!(get(&planned, "lyricist"), owned(&[beethoven]));
        let no_lyricist = credit_profile(true, without(RelationshipType::Lyricist));
        assert_eq!(
            get(&desired(&no_lyricist, &release, 0), "lyricist"),
            old("lyricist")
        );

        let (release, _) = grafted(recorded(classical, &dj_director_folded));
        let planned = desired(&on, &release, 0);
        assert_eq!(get(&planned, "dj_mixer"), owned(&[karajan]));
        assert_eq!(get(&planned, "director"), owned(&[berlin]));
        assert_eq!(get(&planned, "arranger"), owned(&[gould]));
        assert_eq!(
            get(&planned, "performer"),
            owned(&["Ludwig van Beethoven (chorus master)"])
        );
        let no_arranger = credit_profile(true, without(RelationshipType::Arranger));
        assert_eq!(
            get(&desired(&no_arranger, &release, 0), "arranger"),
            old("arranger")
        );

        let (release, _) = grafted(recorded(classical, &unmapped));
        let planned = desired(&on, &release, 0);
        for name in [
            "engineer",
            "mixer",
            "dj_mixer",
            "writer",
            "director",
            "conductor",
        ] {
            assert_eq!(get(&planned, name), none, "{name}");
        }
        assert_eq!(get(&planned, "performer"), owned(&["Glenn Gould (piano)"]));

        // Credits stored before these roles were fetched (a failed
        // refetch): the five keep the file's values, clear or not.
        let (mut release, relations) = recorded(classical, &engineer_mix_librettist);
        let mut stored = main_only(&relations);
        stored.version = 1;
        release.graft_relations(&stored);
        let planned = desired(&on, &release, 0);
        for name in ["engineer", "mixer", "dj_mixer", "writer", "director"] {
            assert_eq!(get(&planned, name), old(name), "{name}");
        }
        assert_eq!(get(&planned, "performer"), none);
    }

    /// ID3v2.3 holds several values as one "/"-joined value. Planned in
    /// that form, a second plan finds nothing to change, and merge never
    /// adds a name the joined value already holds ("AC/DC" stays whole).
    #[test]
    fn id3v23_joined_credits_are_stable_under_merge() {
        let (mut release, _) = classical();
        let track = release.tracks[0].clone();
        release.recording_relations = Some(ReleaseRelations {
            fetched_at_ms: 1,
            version: 1,
            tracks: [(
                track.id.clone(),
                RecordingRelations {
                    recording_id: track.recording_id.clone(),
                    credits: vec![
                        credit("composer", "composer", "Lady Gaga", &[]),
                        credit("composer", "composer", "RedOne", &[]),
                        credit("producer", "producer", "AC/DC", &[]),
                    ],
                    ..RecordingRelations::default()
                },
            )]
            .into_iter()
            .collect(),
        });
        let mut settings = settings::initial_settings("");
        for profile in &mut settings.profiles {
            profile.genres.enabled = false;
            for field in &mut profile.metadata.fields {
                if matches!(field.field.as_str(), "composer" | "producer") {
                    field.mode = FieldMode::Merge;
                }
            }
        }
        let pinned = pin(&settings, settings::PICARD_ORGANIZER_PROFILE_ID, None).expect("pinned");
        let ready = AlbumExtras {
            credits: Credits::Ready,
            ..AlbumExtras::default()
        };
        let mut doc: TagDocument = serde_json::from_value(serde_json::json!({
            "managed": {"producer": ["AC/DC"]},
            "custom": {}
        }))
        .expect("document");
        for pass in 0..2 {
            let plan = plan_tags(
                &pinned,
                &CompiledScripts::new(&pinned),
                &FileInput {
                    release: &release,
                    track: &release.tracks[0],
                    doc: &doc,
                    format: "mp3",
                    id3v23: true,
                    writable: true,
                    overrides: &Overrides::default(),
                    extras: &ready,
                    track_id: "track",
                    lyrics: &LyricsAnswer::Off,
                },
            )
            .expect("planned");
            assert_eq!(
                plan.desired["composer"],
                vec!["Lady Gaga/RedOne"],
                "pass {pass}"
            );
            assert_eq!(plan.desired["producer"], vec!["AC/DC"], "pass {pass}");
            let credits_changed = plan.updates.targets().any(|target| {
                matches!(
                    target,
                    TagTarget::Field(TagField::Composer | TagField::Producer)
                )
            });
            assert_eq!(
                credits_changed,
                pass == 0,
                "pass {pass}: {:?}",
                plan.updates
            );
            doc.managed = TagUpdates::from_fields(plan.desired.clone()).expect("fields");
        }
    }

    /// Picard's performer wording and the work rules, case by case.
    #[test]
    fn performers_and_works_read_as_picard_writes_them() {
        let (mut release, _) = classical();
        let track = release.tracks[0].clone();
        let entry = RecordingRelations {
            recording_id: track.recording_id.clone(),
            credits: vec![
                credit("performer", "instrument", "A", &["drums (drum set)"]),
                credit("performer", "instrument", "B", &["additional", "guitar"]),
                credit(
                    "performer",
                    "instrument",
                    "C",
                    &["guest", "solo", "violin", "viola"],
                ),
                credit("performer", "vocal", "D", &[]),
                credit("performer", "vocal", "E", &["guest"]),
                credit("performer", "performer", "F", &[]),
                credit("performer", "instrument", "A", &["piano"]),
                credit("performer", "instrument", "A", &["drums (drum set)"]),
                credit("composer", "composer", "G", &[]),
                credit("composer", "composer", "G", &["additional"]),
            ],
            works: vec![
                WorkCredit {
                    id: "work-1".to_owned(),
                    title: Some("Part One".to_owned()),
                },
                WorkCredit {
                    id: "work-2".to_owned(),
                    title: Some("Part Two".to_owned()),
                },
            ],
        };
        release.recording_relations = Some(ReleaseRelations {
            fetched_at_ms: 1,
            version: 1,
            tracks: [(track.id.clone(), entry)].into_iter().collect(),
        });
        let ready = AlbumExtras {
            credits: Credits::Ready,
            ..AlbumExtras::default()
        };
        let plan =
            plan_credits(&credit_profile(true, |_| {}), &release, 0, &ready).expect("planned");
        assert_eq!(
            plan.desired["performer"],
            vec![
                "A (drums (drum set))",
                "B (additional guitar)",
                "C (guest solo violin and viola)",
                "D (vocals)",
                "E (guest vocals)",
                "F",
                "A (piano)",
            ]
        );
        assert_eq!(plan.desired["composer"], vec!["G"]);
        // A medley's title would be wrong: the file keeps its own.
        assert_eq!(plan.desired["work"], vec!["Old work"]);
        assert_eq!(
            plan.desired["musicbrainz_work_id"],
            vec!["work-1", "work-2"]
        );
    }

    /// An artist credited under another name is written under their
    /// MusicBrainz name; the credited name stands in only when the
    /// artist has none.
    #[test]
    fn a_credit_uses_the_artists_own_name() {
        let mut probe: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/musicbrainz/release_relations_classical.json"
        ))
        .expect("probe parses");
        let relations = probe["media"][0]["tracks"][0]["recording"]["relations"]
            .as_array_mut()
            .expect("relations");
        let piano = relations
            .iter_mut()
            .find(|relation| relation["type"] == "instrument")
            .expect("the piano credit");
        piano["target-credit"] = "G. Gould".into();
        let conductor = relations
            .iter_mut()
            .find(|relation| relation["type"] == "conductor")
            .expect("the conductor credit");
        conductor["target-credit"] = "H. v. Karajan".into();
        conductor["artist"]["name"] = "".into();
        let wire: droppedneedle_providers::musicbrainz::MbRelease =
            serde_json::from_value(probe).expect("probe decodes");
        let relations = crate::identify::sources::relations_from_wire(&wire, 1);
        let first = &relations.tracks[&wire.media[0].tracks[0].id];
        let name = |kind: &str| {
            first
                .credits
                .iter()
                .find(|credit| credit.relation_type == kind)
                .map(|credit| credit.name.clone())
        };
        assert_eq!(name("instrument").as_deref(), Some("Glenn Gould"));
        assert_eq!(name("conductor").as_deref(), Some("H. v. Karajan"));
    }

    /// MusicBrainz or translated artist names are written only from a
    /// fetched names part. Not fetched leaves the six artist fields as
    /// the file has them, even on replace with clear, and an automatic
    /// run waits; an artist MusicBrainz no longer lists on the release
    /// keeps them too, with a warning, and never blocks. A profile at the
    /// defaults writes the credited names and needs no names part.
    #[test]
    fn artist_names_need_their_lookup() {
        use droppedneedle_kernel::runtime_config::sections::ArtistStandardization;
        let mut probe: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/musicbrainz/release_names_sakamoto.json"
        ))
        .expect("probe parses");
        probe["release-group"] = serde_json::json!({"id": "22222222-2222-4222-8222-222222222222"});
        let wire: droppedneedle_providers::musicbrainz::MbRelease =
            serde_json::from_value(probe).expect("probe decodes");
        let names = crate::identify::sources::names_from_wire(&wire, 1);
        let bare = crate::identify::sources::release_from_wire(wire, Vec::new()).expect("release");
        let mut named = bare.clone();
        named.graft_names(&names);

        let profile = |standardization: ArtistStandardization| {
            let mut settings = settings::initial_settings("");
            for profile in &mut settings.profiles {
                for field in &mut profile.metadata.fields {
                    if super::super::enrich::ARTIST_FIELDS.contains(&field.field.as_str()) {
                        field.mode = FieldMode::Replace;
                        field.clear_when_canonical_missing = true;
                    }
                }
                profile.metadata.artist_credits.standardization = standardization;
                profile.genres.enabled = false;
            }
            pin(&settings, settings::PICARD_ORGANIZER_PROFILE_ID, None).expect("pinned")
        };
        let canonical = profile(ArtistStandardization::Canonical);
        let doc: TagDocument = serde_json::from_value(serde_json::json!({
            "managed": {
                "artist": ["Old"], "artists": ["Old"], "artist_sort": ["Old"],
                "album_artist": ["Old"], "album_artists": ["Old"],
                "album_artist_sort": ["Old"], "title": ["Old"],
            },
            "custom": {},
        }))
        .expect("document");
        let plan = |pinned: &PinnedProfile, release: &Release, names: Credits, automatic: bool| {
            let extras = AlbumExtras {
                names,
                automatic,
                ..AlbumExtras::default()
            };
            plan_tags(
                pinned,
                &CompiledScripts::new(pinned),
                &FileInput {
                    release,
                    track: &release.tracks[0],
                    doc: &doc,
                    format: "flac",
                    id3v23: false,
                    writable: true,
                    overrides: &Overrides::default(),
                    extras: &extras,
                    track_id: "track",
                    lyrics: &LyricsAnswer::Off,
                },
            )
        };
        let value =
            |plan: &TagPlan, name: &str| plan.desired.get(name).cloned().unwrap_or_default();
        let owned = |values: &[&str]| values.iter().map(|v| (*v).to_owned()).collect::<Vec<_>>();
        let artist_fields = |plan: &TagPlan| {
            super::super::enrich::ARTIST_FIELDS
                .iter()
                .map(|name| value(plan, name))
                .collect::<Vec<_>>()
        };
        let kept = vec![owned(&["Old"]); 6];

        // Not fetched: the artist fields stay; the title is still written.
        let missing = plan(&canonical, &bare, Credits::Unavailable, false).expect("planned");
        assert_eq!(artist_fields(&missing), kept);
        assert_eq!(value(&missing, "title"), owned(&["andata"]));
        assert_eq!(
            missing.warnings,
            vec![
                "MusicBrainz artist names could not be read, so the file keeps its artist tags."
                    .to_owned()
            ]
        );
        let reason = plan(&canonical, &bare, Credits::Unavailable, true)
            .err()
            .expect("an automatic run waits");
        assert_eq!(reason.code, "METADATA_UNAVAILABLE");

        // Fetched: the MusicBrainz names are written.
        let written = plan(&canonical, &named, Credits::Ready, true).expect("planned");
        assert_eq!(value(&written, "artist"), owned(&["坂本龍一"]));
        assert_eq!(
            value(&written, "artist_sort"),
            owned(&["Sakamoto, Ryūichi"])
        );
        assert_eq!(
            value(&written, "album_artist"),
            owned(&["坂本龍一 + 高谷史郎"])
        );
        assert_eq!(
            value(&written, "album_artists"),
            owned(&["坂本龍一", "高谷史郎"])
        );
        assert!(written.warnings.is_empty(), "{:?}", written.warnings);

        // MusicBrainz no longer lists Takatani: the album artist fields
        // stay, the track's are written, and an automatic run goes on.
        let mut moved = named.clone();
        if let Some(names) = moved.artist_names.as_mut() {
            names.artists.remove("7493b80f-de18-4f9c-a7bd-08d1ea5adcff");
        }
        let changed = plan(&canonical, &moved, Credits::Ready, true).expect("not blocked");
        assert_eq!(value(&changed, "artist"), owned(&["坂本龍一"]));
        assert_eq!(value(&changed, "album_artist"), owned(&["Old"]));
        assert_eq!(
            changed.warnings,
            vec![
                "MusicBrainz now credits this release differently; its artist tags are left as \
                 they are until it is identified again."
                    .to_owned()
            ]
        );

        // The defaults: credited names, no names part needed.
        let credited = profile(ArtistStandardization::Credited);
        let default = plan(&credited, &bare, Credits::NotAsked, true).expect("planned");
        assert_eq!(value(&default, "artist"), owned(&["Ryuichi Sakamoto"]));
        assert_eq!(
            value(&default, "album_artist_sort"),
            owned(&["Sakamoto, Ryūichi + Takatani, Shiro"])
        );
        assert_eq!(
            default.desired,
            plan(&credited, &named, Credits::NotAsked, true)
                .expect("planned")
                .desired
        );
    }

    #[test]
    fn field_modes_follow_v2() {
        let canonical = vec!["New".to_owned()];
        let old = vec!["Old".to_owned()];
        assert_eq!(
            mode_value(FieldMode::Replace, false, &old, Some(&canonical)),
            Some(canonical.clone())
        );
        assert_eq!(
            mode_value(FieldMode::FillMissing, false, &old, Some(&canonical)),
            None
        );
        assert_eq!(
            mode_value(FieldMode::Merge, false, &old, Some(&canonical)),
            Some(vec!["Old".to_owned(), "New".to_owned()])
        );
        assert_eq!(
            mode_value(FieldMode::Replace, true, &old, None),
            Some(Vec::new())
        );
        assert_eq!(
            mode_value(FieldMode::Preserve, true, &old, Some(&canonical)),
            None
        );
        assert_eq!(initial("The Cure"), "C");
        assert_eq!(initial("2Pac"), "#");
    }
}
