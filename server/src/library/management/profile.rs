//! What a profile asks of one file: the tags it should end up with (from
//! the album's chosen edition, the profile's field modes, and its tagging
//! scripts) and the path its naming script gives it.

use std::collections::{BTreeMap, BTreeSet};

use super::models::{FieldMutation, ManagementError, PinnedProfile, Revisions};
use super::reasons::{self, ItemReason};
use super::script::{self, Statement, Value, Variables};
use crate::library::matching::model::{Release, ReleaseTrack};
use crate::library::publish::TagDocument;
use crate::library::tags::{TagEdit, TagField, picard, save};
use crate::runtime_config::sections::{
    FieldMode, LibraryManagement, LibraryManagementProfile, LibraryManagementRootAssignment,
    LibraryManagementRootOverrides, TaggingScript,
};
use crate::settings::management as settings;

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
        .map(|name| crate::runtime_config::sections::ManagedField {
            field: name.clone(),
            mode: FieldMode::Replace,
            ..Default::default()
        })
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

/// Freeze the profile a library folder's assignment gives: its profile
/// with the folder's overrides, and the scripts it names.
pub fn pin_assignment(
    stored: &LibraryManagement,
    assignment: &LibraryManagementRootAssignment,
) -> Result<PinnedProfile, ManagementError> {
    let invalid = |error: crate::settings::error::SettingsError| match error {
        crate::settings::error::SettingsError::InvalidInput { message } => {
            ManagementError::Invalid(format!(
                "{message} Edit the profile in Settings > Library Management."
            ))
        }
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
        overrides: crate::settings::library_policy::stable_hash(&overrides),
    }
}

/// A content revision as the `u64` the publish seal compares.
pub fn seal_revision(revision: &str) -> u64 {
    let hex = crate::library::publish::snapshots::sha256_hex(revision.as_bytes());
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

/// Profile field names to the tag fields they cover. Names v3 has no
/// source for (composer, lyricist, AcoustID and similar) cover nothing:
/// the file keeps what it has.
fn targets(name: &str) -> &'static [TagField] {
    use TagField::*;
    match name {
        "title" => &[Title],
        "artist" => &[Artist, ArtistSort],
        "artists" => &[Artists],
        // Release facts travel with the album title.
        "album" => &[
            Album,
            ReleaseStatus,
            ReleaseCountry,
            ReleaseType,
            Asin,
            Compilation,
        ],
        "albumartist" | "album_artist" => &[AlbumArtist, AlbumArtistSort],
        "track_number" | "track" => &[TrackNumber],
        "disc_number" | "disc" => &[DiscNumber, DiscSubtitle],
        "total_tracks" => &[TrackTotal],
        "total_discs" => &[DiscTotal],
        "year" | "date" => &[Date, OriginalDate],
        "barcode" => &[Barcode],
        "catalog_number" => &[CatalogNumber],
        "label" => &[Label],
        "media" => &[Media],
        "musicbrainz_recording_id" => &[MusicBrainzRecordingId, MusicBrainzReleaseTrackId],
        "musicbrainz_release_id" => &[MusicBrainzReleaseId],
        "musicbrainz_release_group_id" => &[MusicBrainzReleaseGroupId],
        "musicbrainz_artist_id" => &[MusicBrainzArtistId],
        "musicbrainz_album_artist_id" => &[MusicBrainzAlbumArtistId],
        _ => &[],
    }
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
    if let Some(values) = doc.managed.get(field.name()) {
        return values.clone();
    }
    let prefix = format!("{}:", field.name());
    doc.managed
        .iter()
        .find(|(name, _)| name.starts_with(&prefix))
        .map(|(_, values)| values.clone())
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
    pub updates: BTreeMap<String, Vec<String>>,
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
        let canonical = picard::release_tags(release, track);
        let mut preserved: BTreeSet<&str> = profile
            .metadata
            .preserve_fields
            .iter()
            .flat_map(|name| targets(name).iter().map(|field| field.name()))
            .collect();
        if super::custom::is_custom(release) {
            // A custom edition has no release behind it: the file keeps
            // whatever release ids it holds (v2's custom release claims).
            preserved.insert(TagField::MusicBrainzReleaseId.name());
            preserved.insert(TagField::MusicBrainzReleaseTrackId.name());
        }
        for managed in &profile.metadata.fields {
            for field in targets(&managed.field) {
                if preserved.contains(field.name()) {
                    continue;
                }
                let existing = desired.get(field.name()).cloned().unwrap_or_default();
                if let Some(values) = mode_value(
                    managed.mode,
                    managed.clear_when_canonical_missing,
                    &existing,
                    canonical.get(field.name()),
                ) {
                    desired.insert(field.name().to_owned(), values);
                }
            }
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
    let mut updates = BTreeMap::new();
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
        if !new.is_empty() && !save::accepts(&TagEdit::new(field, new.clone())) {
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
        updates.insert(name.clone(), new);
    }
    // Scrubbing (v2's rule): every native item outside the managed
    // fields goes, except the ones the profile preserves. The values are
    // recorded in the before-state, so undo and a baseline restore put
    // them back.
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
            if preserved.contains(&folded) {
                continue;
            }
            let name = format!("{}{key}", crate::library::publish::staging::NATIVE_PREFIX);
            mutations.push(FieldMutation {
                name: name.clone(),
                operation: "clear".to_owned(),
                before: json_value(values),
                after: serde_json::Value::Null,
            });
            updates.insert(name, Vec::new());
            scrubbed.push((key.clone(), values.clone()));
        }
    }
    if !writable && !updates.is_empty() {
        warnings.push(format!(
            "Tags cannot be written to {} files, so only the file's path is managed.",
            format.to_uppercase()
        ));
        updates.clear();
        mutations.clear();
        scrubbed.clear();
    }
    // What the file holds after the write: skipped fields keep their value.
    let mut desired = before;
    for (name, values) in &updates {
        if values.is_empty() {
            desired.remove(name);
        } else {
            desired.insert(name.clone(), values.clone());
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

/// The native items (folded keys) a preserved profile field name keeps
/// from a scrub, for names outside the managed fields (`composer`,
/// `comment`, an ISRC) and for any key named outright (`vorbis:mood`).
fn preserved_native(name: &str) -> Vec<String> {
    let name = name.trim().to_ascii_lowercase();
    if name.contains(':') {
        return vec![name];
    }
    let keys: &[&str] = match name.as_str() {
        "composer" => &["id3:tcom", "vorbis:composer", "mp4:\u{a9}wrt"],
        "lyricist" => &["id3:text", "vorbis:lyricist", "mp4:----:lyricist"],
        "conductor" => &["id3:tpe3", "vorbis:conductor", "mp4:----:conductor"],
        "remixer" => &["id3:tpe4", "vorbis:remixer", "mp4:----:remixer"],
        "comment" => &["vorbis:comment", "vorbis:description", "mp4:\u{a9}cmt"],
        "grouping" => &["id3:tit1", "vorbis:grouping", "mp4:\u{a9}grp"],
        "bpm" => &["id3:tbpm", "vorbis:bpm"],
        "isrc" => &["id3:tsrc", "vorbis:isrc", "mp4:----:isrc"],
        "copyright" => &["id3:tcop", "vorbis:copyright", "mp4:cprt"],
        "encoded_by" | "encodedby" => &["id3:tenc", "vorbis:encodedby", "mp4:\u{a9}too"],
        "encoder" => &["id3:tsse", "vorbis:encoder", "mp4:\u{a9}too"],
        "mood" => &["id3:tmoo", "vorbis:mood", "mp4:----:mood"],
        "language" => &["id3:tlan", "vorbis:language", "mp4:----:language"],
        "script" => &["id3:txxx:script", "vorbis:script", "mp4:----:script"],
        "acoustid_id" => &[
            "id3:txxx:acoustid id",
            "vorbis:acoustid_id",
            "mp4:----:acoustid id",
        ],
        "acoustid_fingerprint" => &[
            "id3:txxx:acoustid fingerprint",
            "vorbis:acoustid_fingerprint",
            "mp4:----:acoustid fingerprint",
        ],
        "musicbrainz_work_id" => &[
            "id3:txxx:musicbrainz work id",
            "vorbis:musicbrainz_workid",
            "mp4:----:musicbrainz work id",
        ],
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
    use crate::runtime_config::sections::ReplayGainMode;
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
