//! Where an imported file goes: the library's import naming script,
//! rendered by the one script language (`management::script`) over the
//! variables Library Management names files with, built from the tags the
//! import writes. The same release and script give the same path here and
//! in a management preview with default path settings.

use droppedneedle_kernel::runtime_config::sections::PathCompatibilitySettings;
use droppedneedle_kernel::runtime_config::settings_error::SettingsError;

use crate::management::profile::{NamingFacts, naming_variables};
use crate::management::script::{ScriptFault, Value, Variables, render_path};
use crate::management::settings::validate_naming_language;
use crate::matching::model::{CreditedArtist, ReleaseMedium};
use crate::matching::{Release, ReleaseTrack};
use crate::tags::picard::release_tags;

/// The variables an imported file's name is made from. `format` is the
/// file's lowercase extension.
pub fn variables(release: &Release, track: &ReleaseTrack, format: &str) -> Variables {
    let fields = release_tags(release, track);
    let medium = release
        .media
        .iter()
        .find(|medium| medium.position == track.disc);
    let facts = NamingFacts {
        extension: format,
        codec: format,
        bit_rate: None,
        sample_rate: None,
        bit_depth: None,
        medium_format: medium
            .and_then(|medium| medium.format.clone())
            .unwrap_or_default(),
        medium_number: i64::from(track.disc),
        album_disambiguation: release.disambiguation.clone().unwrap_or_default(),
    };
    naming_variables(&fields, &facts)
}

/// The relative path `script` gives a file under the root at `root`,
/// cleaned with Library Management's default path settings. The file name
/// must be a name followed by `.{ext}`: a file without its audio
/// extension could be neither placed nor scanned.
pub fn render(script: &str, variables: &Variables, root: &str) -> Result<String, ScriptFault> {
    let path = render_path(
        script,
        variables,
        &PathCompatibilitySettings::default(),
        root,
    )?;
    let ext = variables.get("ext").map(Value::text).unwrap_or_default();
    let file_name = path.rsplit('/').next().unwrap_or_default();
    let named = file_name
        .len()
        .checked_sub(ext.len() + 1)
        .filter(|stem| *stem > 0)
        .and_then(|stem| file_name.get(stem..))
        .is_some_and(|suffix| suffix.eq_ignore_ascii_case(&format!(".{ext}")));
    if !named {
        return Err(ScriptFault {
            message: format!("The file name must end in .{ext}."),
            line: 1,
            column: 1,
            path_too_long: false,
        });
    }
    Ok(path)
}

/// The path `script` gives a fixed example track, shown under the setting.
pub fn example(script: &str) -> Result<String, ScriptFault> {
    let artist = CreditedArtist {
        id: "a74b1b7f-71a5-4011-9441-d0b5e4122711".to_owned(),
        name: "Radiohead".to_owned(),
        sort_name: Some("Radiohead".to_owned()),
        join: String::new(),
    };
    let track = ReleaseTrack {
        title: "Paranoid Android".to_owned(),
        disc: 1,
        position: 2,
        absolute_position: 2,
        ..ReleaseTrack::default()
    };
    let release = Release {
        release_group_id: "b1392450-e666-3926-a536-22c65f834433".to_owned(),
        title: "OK Computer".to_owned(),
        artists: vec![artist],
        date: Some("1997-05-21".to_owned()),
        original_date: Some("1997-05-21".to_owned()),
        media: vec![ReleaseMedium {
            position: 1,
            format: Some("CD".to_owned()),
            track_count: 12,
            ..ReleaseMedium::default()
        }],
        tracks: vec![track.clone()],
        ..Release::default()
    };
    render(script, &variables(&release, &track, "flac"), "/music")
}

/// Check a naming script a person saves: it passes the checks every
/// naming script passes, and places the example track.
pub fn check(script: &str) -> Result<(), SettingsError> {
    validate_naming_language(script, "Import naming script")?;
    example(script)
        .map(drop)
        .map_err(|fault| SettingsError::InvalidInput {
            message: format!("Import naming script: {fault}"),
        })
}
