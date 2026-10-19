//! Settings rules for naming and tagging scripts, checked over what the
//! one script compiler (`management::script`) makes of the source.

use std::collections::BTreeSet;

use super::presets::{MANAGED_FIELDS, SCRIPT_APPENDABLE_SPELLINGS, SCRIPT_FIELD_SPELLINGS};
use crate::management::script::{self, ScriptFault, Segment, Statement, Target};
use droppedneedle_kernel::runtime_config::settings_error::SettingsError;

/// Naming variable universe (managed fields plus path-only variables).
fn naming_variables() -> BTreeSet<&'static str> {
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

fn script_error(message: String, script_name: &str, line: usize, column: usize) -> SettingsError {
    SettingsError::InvalidInput {
        message: format!("{script_name} line {line}, column {column}: {message}"),
    }
}

fn compile_error(fault: ScriptFault, script_name: &str) -> SettingsError {
    script_error(fault.message, script_name, fault.line, fault.column)
}

/// Check a naming script as every naming script is checked on save:
/// it compiles, stays a relative path and reads only known variables.
pub(crate) fn validate_naming_language(
    source: &str,
    script_name: &str,
) -> Result<(), SettingsError> {
    if source.contains('\n') || source.contains('\r') {
        return Err(script_error(
            "Naming scripts must be a single path template.".to_owned(),
            script_name,
            1,
            1,
        ));
    }
    let segments =
        script::compile_naming(source).map_err(|fault| compile_error(fault, script_name))?;
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
        return Err(script_error(
            "Naming script must stay in a non-empty relative path.".to_owned(),
            script_name,
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
                    return Err(script_error(
                        format!("Unknown naming variable: {name}."),
                        script_name,
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
                    return Err(script_error(
                        format!("Variable {name} does not support a numeric format."),
                        script_name,
                        *line,
                        *column,
                    ));
                }
                if script::format_number(0, spec).is_none() {
                    return Err(script_error(
                        format!("Numeric format {spec} is not valid."),
                        script_name,
                        *line,
                        *column,
                    ));
                }
            }
            Segment::Expression(expr) => {
                for (name, line, column) in expr.variables() {
                    if !variables.contains(name) {
                        return Err(script_error(
                            format!("Unknown naming variable: {name}."),
                            script_name,
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

fn is_custom(name: &str) -> bool {
    name.len() > 7 && name.is_char_boundary(7) && name[..7].eq_ignore_ascii_case("custom.")
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

    fn check_target(
        &self,
        target: &Target,
        append: bool,
        script_name: &str,
    ) -> Result<(), SettingsError> {
        let name = target.name.as_str();
        let custom = is_custom(name).then(|| name[7..].trim());
        let custom_valid = custom
            .is_some_and(|name| !name.is_empty() && !name.contains('\x00') && name.len() <= 255);
        if !self.allowed.contains(name) && !custom_valid {
            return Err(script_error(
                format!("Unknown or invalid tagging target: {name}."),
                script_name,
                target.line,
                target.column,
            ));
        }
        if append && !self.ordered.contains(name) && custom.is_none() {
            return Err(script_error(
                format!("Field {name} does not accept append."),
                script_name,
                target.line,
                target.column,
            ));
        }
        Ok(())
    }

    fn check_reads(&self, expr: &script::Expr, script_name: &str) -> Result<(), SettingsError> {
        for (name, line, column) in expr.variables() {
            if !(self.allowed.contains(name) || self.read_only.contains(name) || is_custom(name)) {
                return Err(script_error(
                    format!("Unknown tagging variable: {name}."),
                    script_name,
                    line,
                    column,
                ));
            }
        }
        Ok(())
    }

    fn check(&self, statements: &[Statement], script_name: &str) -> Result<(), SettingsError> {
        for statement in statements {
            match statement {
                Statement::Set(target, expr) => {
                    self.check_target(target, false, script_name)?;
                    self.check_reads(expr, script_name)?;
                }
                Statement::Append(target, expr) => {
                    self.check_target(target, true, script_name)?;
                    self.check_reads(expr, script_name)?;
                }
                Statement::Delete(target) => self.check_target(target, false, script_name)?,
                Statement::If(condition, body, alternate) => {
                    self.check_reads(condition, script_name)?;
                    self.check(body, script_name)?;
                    self.check(alternate, script_name)?;
                }
            }
        }
        Ok(())
    }
}

pub(super) fn validate_tagging_language(
    source: &str,
    script_name: &str,
) -> Result<(), SettingsError> {
    let statements =
        script::compile_tagging(source).map_err(|fault| compile_error(fault, script_name))?;
    TaggingRules::new().check(&statements, script_name)
}
