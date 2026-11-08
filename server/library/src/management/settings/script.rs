//! Settings rules for naming and tagging scripts, checked over what the
//! one script compiler (`management::script`) makes of the source.

use std::collections::BTreeSet;

use super::presets::{MANAGED_FIELDS, SCRIPT_APPENDABLE_SPELLINGS, SCRIPT_FIELD_SPELLINGS};
use crate::management::script::{self, ScriptFault, Segment, Statement, Target};
use droppedneedle_kernel::runtime_config::settings_error::SettingsError;

/// Naming variable universe (managed fields plus path-only variables).
pub(crate) fn naming_variables() -> BTreeSet<&'static str> {
    let mut variables: BTreeSet<&'static str> = MANAGED_FIELDS
        .iter()
        .map(|field| field.name)
        .chain(SCRIPT_FIELD_SPELLINGS.iter().copied())
        .collect();
    variables.extend(
        [
            "genre",
            "genres",
            "primary_genre",
            "artist_display",
            "artists",
            "artist_sorts",
            "album_artist_display",
            "album_artists",
            "album_artist_sorts",
            "albumartist",
            "initial",
            "year",
            "track",
            "disc",
            "ext",
            "extension",
            "medium",
            "album_disambiguation",
            "albumartist_disambiguation",
            "artist_disambiguation",
            "medium_format",
            "medium_number",
            "musicbrainz_id",
            "artist_mbid",
            "codec",
            "quality",
            "bitrate",
            "sample_rate",
            "bit_depth",
            "artwork_type",
            "artwork_comment",
            "artwork_extension",
            "artwork_format",
        ]
        .iter()
        .copied(),
    );
    variables
}

/// Why a script breaks a settings rule, and the name it did not know
/// when that is the reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LanguageFault {
    pub fault: ScriptFault,
    pub unknown_name: Option<String>,
}

impl From<ScriptFault> for LanguageFault {
    fn from(fault: ScriptFault) -> Self {
        Self {
            fault,
            unknown_name: None,
        }
    }
}

fn rule_fault(message: String, line: usize, column: usize) -> LanguageFault {
    LanguageFault::from(ScriptFault {
        message,
        line,
        column,
        path_too_long: false,
    })
}

fn unknown(message: String, name: &str, line: usize, column: usize) -> LanguageFault {
    LanguageFault {
        unknown_name: Some(name.to_owned()),
        ..rule_fault(message, line, column)
    }
}

fn settings_error(fault: LanguageFault, script_name: &str) -> SettingsError {
    let fault = fault.fault;
    SettingsError::InvalidInput {
        message: format!(
            "{script_name} line {}, column {}: {}",
            fault.line, fault.column, fault.message
        ),
    }
}

/// Check a naming script as every naming script is checked on save:
/// it compiles, stays a relative path and reads only known variables.
pub(crate) fn validate_naming_language(
    source: &str,
    script_name: &str,
) -> Result<(), SettingsError> {
    check_naming_language(source).map_err(|fault| settings_error(fault, script_name))
}

/// [`validate_naming_language`]'s check, answering where and why.
pub(crate) fn check_naming_language(source: &str) -> Result<(), LanguageFault> {
    if source.contains('\n') || source.contains('\r') {
        return Err(rule_fault(
            "Naming scripts must be a single path template.".to_owned(),
            1,
            1,
        ));
    }
    let segments = script::compile_naming(source)?;
    let path_shape: String = segments
        .iter()
        .map(|segment| match segment {
            Segment::Literal(text) => text.as_str(),
            Segment::Variable { .. } | Segment::Expression(_) => "value",
        })
        .collect();
    if path_shape.starts_with('/')
        || path_shape
            .split('/')
            .any(|part| matches!(part, "" | "." | ".."))
    {
        return Err(rule_fault(
            "Naming script must stay in a non-empty relative path.".to_owned(),
            1,
            1,
        ));
    }
    let variables = naming_variables();
    for segment in &segments {
        match segment {
            Segment::Literal(_) => {}
            Segment::Variable {
                name,
                spec,
                line,
                column,
            } => {
                if !variables.contains(name.as_str()) {
                    return Err(unknown(
                        format!("Unknown naming variable: {name}."),
                        name,
                        *line,
                        *column,
                    ));
                }
                let Some(spec) = spec else { continue };
                if !matches!(
                    name.as_str(),
                    "track"
                        | "disc"
                        | "track_number"
                        | "disc_number"
                        | "total_tracks"
                        | "total_discs"
                        | "medium_number"
                ) {
                    return Err(rule_fault(
                        format!("Variable {name} does not support a numeric format."),
                        *line,
                        *column,
                    ));
                }
                if script::format_number(0, spec).is_none() {
                    return Err(rule_fault(
                        format!("Numeric format {spec} is not valid."),
                        *line,
                        *column,
                    ));
                }
            }
            Segment::Expression(expr) => {
                for (name, line, column) in expr.variables() {
                    if !variables.contains(name) {
                        return Err(unknown(
                            format!("Unknown naming variable: {name}."),
                            name,
                            line,
                            column,
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

/// What a tagging script may write, append to, and read.
struct TaggingRules {
    allowed: BTreeSet<&'static str>,
    ordered: BTreeSet<&'static str>,
    read_only: BTreeSet<&'static str>,
}

impl TaggingRules {
    fn new() -> Self {
        Self {
            allowed: MANAGED_FIELDS
                .iter()
                .map(|field| field.name)
                .chain(SCRIPT_FIELD_SPELLINGS.iter().copied())
                .collect(),
            ordered: MANAGED_FIELDS
                .iter()
                .filter(|field| field.mergeable)
                .map(|field| field.name)
                .chain(SCRIPT_APPENDABLE_SPELLINGS.iter().copied())
                .collect(),
            read_only: [
                "artist_display",
                "album_artist_display",
                "artists",
                "album_artists",
                "genres",
                "year",
                "primary_genre",
            ]
            .into_iter()
            .collect(),
        }
    }

    fn check_target(&self, target: &Target, append: bool) -> Result<(), LanguageFault> {
        let name = target.name.as_str();
        if script::is_custom_name(name) {
            return Err(rule_fault(
                format!("Custom tags can be read but not written: {name}."),
                target.line,
                target.column,
            ));
        }
        if !self.allowed.contains(name) {
            return Err(unknown(
                format!("Unknown or invalid tagging target: {name}."),
                name,
                target.line,
                target.column,
            ));
        }
        if append && !self.ordered.contains(name) {
            return Err(rule_fault(
                format!("Field {name} does not accept append."),
                target.line,
                target.column,
            ));
        }
        Ok(())
    }

    fn check_reads(&self, expr: &script::Expr) -> Result<(), LanguageFault> {
        for (name, line, column) in expr.variables() {
            if !(self.allowed.contains(name)
                || self.read_only.contains(name)
                || script::is_custom_name(name))
            {
                return Err(unknown(
                    format!("Unknown tagging variable: {name}."),
                    name,
                    line,
                    column,
                ));
            }
        }
        Ok(())
    }

    fn check(&self, statements: &[Statement]) -> Result<(), LanguageFault> {
        for statement in statements {
            match statement {
                Statement::Set(target, expr) => {
                    self.check_target(target, false)?;
                    self.check_reads(expr)?;
                }
                Statement::Append(target, expr) => {
                    self.check_target(target, true)?;
                    self.check_reads(expr)?;
                }
                Statement::Delete(target) => self.check_target(target, false)?,
                Statement::If(condition, body, alternate) => {
                    self.check_reads(condition)?;
                    self.check(body)?;
                    self.check(alternate)?;
                }
            }
        }
        Ok(())
    }
}

/// Every name a tagging script may read.
pub(crate) fn tagging_variables() -> BTreeSet<&'static str> {
    let rules = TaggingRules::new();
    rules.allowed.union(&rules.read_only).copied().collect()
}

pub(super) fn validate_tagging_language(
    source: &str,
    script_name: &str,
) -> Result<(), SettingsError> {
    check_tagging_language(source).map_err(|fault| settings_error(fault, script_name))
}

/// [`validate_tagging_language`]'s check, answering where and why.
pub(crate) fn check_tagging_language(source: &str) -> Result<(), LanguageFault> {
    let statements = script::compile_tagging(source)?;
    TaggingRules::new().check(&statements)
}
