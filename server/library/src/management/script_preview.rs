//! A script shown against the built-in example albums while it is being
//! written: the path a naming script gives each example track, or the
//! tags a tagging script changes on it, and every value the script can
//! read there. Nothing is saved and no file is read; a script that does
//! not compile or fails on an example answers why, as data.

use std::collections::{BTreeMap, BTreeSet};

use droppedneedle_kernel::runtime_config::sections::PathCompatibilitySettings;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::profile::{self, fold_script_names};
use super::script::{self, MAX_SOURCE_CHARACTERS, ScriptFault, Variables};
use super::settings::{
    LanguageFault, check_naming_language, check_tagging_language, naming_variables,
    tagging_variables,
};
use crate::import::naming::{self, ExampleAlbum, ExampleTrack, example_track};
use crate::tags::picard::release_tags;

/// Which script is being written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScriptPreviewKind {
    /// The library's import naming script: the file name must end in the
    /// file's extension.
    ImportNaming,
    /// A Library Management naming script.
    Naming,
    /// A Library Management tagging script.
    Tagging,
}

/// A script to show against the example albums.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ScriptPreviewRequest {
    pub kind: ScriptPreviewKind,
    pub source: String,
}

/// Why a script fails, and where.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ScriptPreviewFault {
    pub message: String,
    pub line: u32,
    pub column: u32,
    /// The known name closest to the one the script misspelt.
    pub suggestion: Option<ScriptNameSuggestion>,
}

/// A misspelt name and the known name it is closest to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ScriptNameSuggestion {
    /// The name as the script wrote it.
    pub found: String,
    pub name: String,
}

/// One tag a tagging script changes; `after` is empty when it removes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ScriptTagChange {
    pub name: String,
    pub before: Vec<String>,
    pub after: Vec<String>,
}

/// The script on one example album.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ScriptPreviewSample {
    pub sample: ExampleAlbum,
    /// The path a naming script gives the example track.
    pub path: Option<String>,
    /// The path's names as written, when one of them came out empty
    /// (each empty one as `""`).
    pub empty_name_parts: Option<Vec<String>>,
    /// Why the script fails on this example.
    pub fault: Option<ScriptPreviewFault>,
    /// The tags a tagging script changes, by name.
    pub tags: Vec<ScriptTagChange>,
    /// Every value the script can read here, as text.
    pub values: BTreeMap<String, String>,
}

/// The script on every example album.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ScriptPreviewResponse {
    /// Why the script does not compile or breaks a rule; the examples
    /// then carry their values only.
    pub fault: Option<ScriptPreviewFault>,
    pub samples: Vec<ScriptPreviewSample>,
}

/// Why a preview was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScriptPreviewError {
    #[error("A script can be at most {MAX_SOURCE_CHARACTERS} characters long.")]
    TooLong,
}

/// Show `request`'s script against every example album.
pub fn preview(
    request: &ScriptPreviewRequest,
) -> Result<ScriptPreviewResponse, ScriptPreviewError> {
    if request.source.chars().count() > MAX_SOURCE_CHARACTERS {
        return Err(ScriptPreviewError::TooLong);
    }
    let tagging = request.kind == ScriptPreviewKind::Tagging;
    let checked = if tagging {
        check_tagging_language(&request.source)
    } else {
        check_naming_language(&request.source)
    };
    let known = if tagging {
        tagging_variables()
    } else {
        naming_variables()
    };
    let fault = checked.err().map(|fault| language_fault(fault, &known));
    let samples = ExampleAlbum::ALL
        .into_iter()
        .map(|album| {
            let example = example_track(album);
            let mut sample = if tagging {
                tagging_sample(&example, &request.source, fault.is_none())
            } else {
                naming_sample(&example, request, fault.is_none())
            };
            sample.sample = album;
            sample
        })
        .collect();
    Ok(ScriptPreviewResponse { fault, samples })
}

fn texts(variables: &Variables) -> BTreeMap<String, String> {
    variables
        .iter()
        .map(|(name, value)| (name.clone(), value.text()))
        .collect()
}

fn empty_sample(values: BTreeMap<String, String>) -> ScriptPreviewSample {
    ScriptPreviewSample {
        sample: ExampleAlbum::OneDisc,
        path: None,
        empty_name_parts: None,
        fault: None,
        tags: Vec::new(),
        values,
    }
}

fn naming_sample(
    example: &ExampleTrack,
    request: &ScriptPreviewRequest,
    compiles: bool,
) -> ScriptPreviewSample {
    let variables = example.variables();
    let mut sample = empty_sample(texts(&variables));
    if !compiles {
        return sample;
    }
    let compat = PathCompatibilitySettings::default();
    let text = match script::render_text(&request.source, &variables, &compat) {
        Ok(text) => text,
        Err(fault) => {
            sample.fault = Some(plain_fault(fault));
            return sample;
        }
    };
    let parts: Vec<String> = text.split('/').map(str::to_owned).collect();
    if parts.iter().any(String::is_empty) {
        sample.fault = Some(plain_fault(ScriptFault {
            message: "A folder or file name comes out empty.".to_owned(),
            line: 1,
            column: 1,
            path_too_long: false,
        }));
        sample.empty_name_parts = Some(parts);
        return sample;
    }
    let rendered = match request.kind {
        ScriptPreviewKind::ImportNaming => naming::render(&request.source, &variables, "/music"),
        _ => script::render_path(&request.source, &variables, &compat, "/music"),
    };
    match rendered {
        Ok(path) => sample.path = Some(path),
        Err(fault) => sample.fault = Some(plain_fault(fault)),
    }
    sample
}

fn tagging_sample(example: &ExampleTrack, source: &str, compiles: bool) -> ScriptPreviewSample {
    let mut before = release_tags(&example.release, &example.track);
    if !example.comment.is_empty() {
        before.insert("comment".to_owned(), vec![example.comment.to_owned()]);
    }
    let mut sample = empty_sample(texts(&profile::tagging_variables(
        &before,
        &Variables::new(),
    )));
    if !compiles {
        return sample;
    }
    let program = match script::compile_tagging(source) {
        Ok(program) => program,
        Err(fault) => {
            sample.fault = Some(plain_fault(fault));
            return sample;
        }
    };
    let mut after = before.clone();
    let refresh = |fields: &BTreeMap<String, Vec<String>>| {
        profile::tagging_variables(fields, &Variables::new())
    };
    if let Err(fault) = script::run_tagging(
        &program,
        &mut after,
        &BTreeSet::new(),
        &refresh,
        &mut script::Budget::default(),
    ) {
        sample.fault = Some(plain_fault(fault));
        return sample;
    }
    fold_script_names(&mut after, "", &mut Vec::new());
    after.retain(|_, values| !values.is_empty());
    let names: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    sample.tags = names
        .into_iter()
        .filter_map(|name| {
            let was = before.get(name).cloned().unwrap_or_default();
            let now = after.get(name).cloned().unwrap_or_default();
            (was != now).then(|| ScriptTagChange {
                name: name.clone(),
                before: was,
                after: now,
            })
        })
        .collect();
    sample
}

fn position(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn plain_fault(fault: ScriptFault) -> ScriptPreviewFault {
    ScriptPreviewFault {
        message: fault.message,
        line: position(fault.line),
        column: position(fault.column),
        suggestion: None,
    }
}

fn language_fault(fault: LanguageFault, known: &BTreeSet<&'static str>) -> ScriptPreviewFault {
    let suggestion = fault.unknown_name.and_then(|found| {
        closest(&found, known).map(|name| ScriptNameSuggestion {
            found,
            name: name.to_owned(),
        })
    });
    ScriptPreviewFault {
        suggestion,
        ..plain_fault(fault.fault)
    }
}

/// The known name a misspelt one is closest to: at most two edits away,
/// and fewer than half its letters.
fn closest<'a>(found: &str, known: &BTreeSet<&'a str>) -> Option<&'a str> {
    let found = found.to_lowercase();
    let limit = (found.chars().count().saturating_sub(1) / 2).min(2);
    known
        .iter()
        .map(|name| (edit_distance(&found, name), *name))
        .filter(|(distance, _)| *distance <= limit)
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, name)| name)
}

/// Levenshtein distance, counted in characters.
fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, left) in a.chars().enumerate() {
        let mut current = Vec::with_capacity(b.len() + 1);
        current.push(i + 1);
        for (j, right) in b.iter().enumerate() {
            let replace = previous[j] + usize::from(left != *right);
            current.push(replace.min(previous[j + 1] + 1).min(current[j] + 1));
        }
        previous = current;
    }
    previous[b.len()]
}
