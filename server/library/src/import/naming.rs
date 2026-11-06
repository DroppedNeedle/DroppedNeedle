//! Where an imported file goes: the library's import naming script,
//! rendered by the one script language (`management::script`) over the
//! variables Library Management names files with, built from the tags the
//! import writes. The same release and script give the same path here and
//! in a management preview with default path settings.

use droppedneedle_kernel::runtime_config::sections::PathCompatibilitySettings;
use droppedneedle_kernel::runtime_config::settings_error::SettingsError;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::management::profile::{NamingFacts, artist_disambiguations, naming_variables};
use crate::management::script::{ScriptFault, Value, Variables, render_path};
use crate::management::settings::validate_naming_language;
use crate::matching::model::{CreditedArtist, ReleaseMedium};
use crate::matching::{Release, ReleaseTrack};
use crate::tags::picard::release_tags;
use crate::tags::read::HeaderInfo;

/// The variables an imported file's name is made from. `format` is the
/// file's lowercase extension and `header` what its stream headers say.
pub fn variables(
    release: &Release,
    track: &ReleaseTrack,
    format: &str,
    header: &HeaderInfo,
) -> Variables {
    let fields = release_tags(release, track);
    let medium = release
        .media
        .iter()
        .find(|medium| medium.position == track.disc);
    let (album_artist_disambiguation, artist_disambiguation) =
        artist_disambiguations(release, track);
    let facts = NamingFacts {
        extension: format,
        codec: format,
        bit_rate: header.bitrate_kbps.map(i64::from),
        sample_rate: header.sample_rate.map(i64::from),
        bit_depth: header.bit_depth.map(i64::from),
        medium_format: medium
            .and_then(|medium| medium.format.clone())
            .unwrap_or_default(),
        medium_number: i64::from(track.disc),
        album_disambiguation: release.disambiguation.clone().unwrap_or_default(),
        album_artist_disambiguation,
        artist_disambiguation,
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

/// The built-in example albums a script is shown against: one disc, two
/// discs with an edition note, and an album artist whose name another
/// artist shares, told apart by MusicBrainz's disambiguation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExampleAlbum {
    OneDisc,
    TwoDiscs,
    NameShared,
}

impl ExampleAlbum {
    pub const ALL: [Self; 3] = [Self::OneDisc, Self::TwoDiscs, Self::NameShared];
}

/// One example track: its release, the file it comes as, and a tag the
/// file holds besides the release's.
pub struct ExampleTrack {
    pub release: Release,
    pub track: ReleaseTrack,
    pub format: &'static str,
    pub header: HeaderInfo,
    pub comment: &'static str,
}

impl ExampleTrack {
    /// The variables a naming script reads for this track.
    pub fn variables(&self) -> Variables {
        variables(&self.release, &self.track, self.format, &self.header)
    }
}

fn artist(id: &str, name: &str, disambiguation: Option<&str>) -> CreditedArtist {
    CreditedArtist {
        id: id.to_owned(),
        name: name.to_owned(),
        sort_name: Some(name.to_owned()),
        join: String::new(),
        disambiguation: disambiguation.map(str::to_owned),
    }
}

fn medium(position: u32, track_count: u32) -> ReleaseMedium {
    ReleaseMedium {
        position,
        format: Some("CD".to_owned()),
        track_count,
        ..ReleaseMedium::default()
    }
}

fn lossless() -> HeaderInfo {
    HeaderInfo {
        bitrate_kbps: Some(1000),
        sample_rate: Some(44_100),
        bit_depth: Some(16),
        ..HeaderInfo::default()
    }
}

/// The track an example album is shown with. The ids of the second and
/// third are made up.
pub fn example_track(album: ExampleAlbum) -> ExampleTrack {
    let (release, track, format, header, comment) = match album {
        ExampleAlbum::OneDisc => {
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
                artists: vec![artist(
                    "a74b1b7f-71a5-4011-9441-d0b5e4122711",
                    "Radiohead",
                    None,
                )],
                date: Some("1997-05-21".to_owned()),
                original_date: Some("1997-05-21".to_owned()),
                primary_type: Some("Album".to_owned()),
                labels: vec!["Parlophone".to_owned()],
                media: vec![medium(1, 12)],
                tracks: vec![track.clone()],
                ..Release::default()
            };
            (release, track, "flac", lossless(), "Ripped with EAC")
        }
        ExampleAlbum::TwoDiscs => {
            let track = ReleaseTrack {
                title: "Lift".to_owned(),
                disc: 2,
                position: 3,
                absolute_position: 15,
                ..ReleaseTrack::default()
            };
            let release = Release {
                release_group_id: "00000000-0000-4000-8000-000000000002".to_owned(),
                title: "OK Computer OKNOTOK 1997 2017".to_owned(),
                artists: vec![artist(
                    "a74b1b7f-71a5-4011-9441-d0b5e4122711",
                    "Radiohead",
                    None,
                )],
                date: Some("2017-06-23".to_owned()),
                original_date: Some("2017-06-23".to_owned()),
                primary_type: Some("Album".to_owned()),
                labels: vec!["XL Recordings".to_owned()],
                media: vec![medium(1, 12), medium(2, 11)],
                tracks: vec![track.clone()],
                disambiguation: Some("remastered".to_owned()),
                ..Release::default()
            };
            (release, track, "flac", lossless(), "")
        }
        ExampleAlbum::NameShared => {
            let track = ReleaseTrack {
                title: "Under My Wheels".to_owned(),
                disc: 1,
                position: 1,
                absolute_position: 1,
                ..ReleaseTrack::default()
            };
            let release = Release {
                release_group_id: "00000000-0000-4000-8000-000000000003".to_owned(),
                title: "Killer".to_owned(),
                artists: vec![artist(
                    "00000000-0000-4000-8000-000000000004",
                    "Alice Cooper",
                    Some("1964-1975 American rock band"),
                )],
                date: Some("1971-11-27".to_owned()),
                original_date: Some("1971-11-27".to_owned()),
                primary_type: Some("Album".to_owned()),
                labels: vec!["Warner Bros. Records".to_owned()],
                media: vec![medium(1, 8)],
                tracks: vec![track.clone()],
                ..Release::default()
            };
            let header = HeaderInfo {
                bitrate_kbps: Some(320),
                sample_rate: Some(44_100),
                ..HeaderInfo::default()
            };
            (release, track, "mp3", header, "")
        }
    };
    ExampleTrack {
        release,
        track,
        format,
        header,
        comment,
    }
}

/// The path `script` gives the first example track, shown under the
/// setting and checked on save.
pub fn example(script: &str) -> Result<String, ScriptFault> {
    let example = example_track(ExampleAlbum::OneDisc);
    render(script, &example.variables(), "/music")
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
