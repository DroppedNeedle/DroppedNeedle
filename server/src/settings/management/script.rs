//! The naming and tagging script language port and the shipped
//! structural compiler.

use std::collections::BTreeSet;

use super::presets::{MANAGED_FIELD_NAMES, MERGEABLE_MANAGED_FIELD_NAMES};
use crate::settings::error::SettingsError;

/// Naming variable universe (managed fields plus path-only variables).
pub fn naming_variables() -> BTreeSet<&'static str> {
    let mut variables: BTreeSet<&'static str> = MANAGED_FIELD_NAMES.iter().copied().collect();
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

/// One compiled naming segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamingSegment {
    /// Literal text, when a literal segment.
    pub literal: Option<String>,
    /// Legacy variable name, when a `{variable}` segment.
    pub legacy_variable: Option<String>,
    /// Numeric format spec, when present.
    pub format_spec: Option<String>,
    /// Variables referenced by an expression segment.
    pub expression_variables: Vec<ExprVariable>,
    /// Source line (1-based).
    pub line: i64,
    /// Source column (1-based).
    pub column: i64,
}

/// One variable reference inside an expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExprVariable {
    /// Variable name.
    pub name: String,
    /// Source line (1-based).
    pub line: i64,
    /// Source column (1-based).
    pub column: i64,
}

/// One compiled tagging statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaggingStatement {
    /// Operation (`set`, `append`, `delete`, `if`, ...).
    pub operation: String,
    /// Target field, when the statement has one.
    pub target: Option<String>,
    /// Variables referenced by the statement expression.
    pub expression_variables: Vec<ExprVariable>,
    /// Source line (1-based).
    pub line: i64,
    /// Source column (1-based).
    pub column: i64,
}

/// Script compile failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptError {
    /// Human explanation.
    pub message: String,
    /// Source line (1-based).
    pub line: i64,
    /// Source column (1-based).
    pub column: i64,
}

/// Script-language compiler port. The shipped structural compiler
/// segments naming templates and tagging programs exactly and scans
/// expressions for identifiers; the full expression compiler (function
/// names, arity, evaluation) is library-engine follow-up work behind
/// this same port.
pub trait ScriptCompiler: Send + Sync {
    /// Compile a naming template into segments.
    fn compile_naming(
        &self,
        source: &str,
        script_name: &str,
    ) -> Result<Vec<NamingSegment>, ScriptError>;
    /// Compile a tagging program into statements.
    fn compile_tagging(
        &self,
        source: &str,
        script_name: &str,
    ) -> Result<Vec<TaggingStatement>, ScriptError>;
}

fn script_error(message: String, script_name: &str, line: i64, column: i64) -> SettingsError {
    SettingsError::InvalidInput {
        message: format!("{script_name} line {line}, column {column}: {message}"),
    }
}

pub(super) fn validate_naming_language(
    source: &str,
    script_name: &str,
    compiler: &dyn ScriptCompiler,
) -> Result<(), SettingsError> {
    if source.contains('\n') || source.contains('\r') {
        return Err(script_error(
            "Naming scripts must be a single path template.".to_owned(),
            script_name,
            1,
            1,
        ));
    }
    let segments = compiler
        .compile_naming(source, script_name)
        .map_err(|cause| script_error(cause.message, script_name, cause.line, cause.column))?;
    let path_shape: String = segments
        .iter()
        .map(|segment| {
            segment
                .literal
                .clone()
                .unwrap_or_else(|| "value".to_owned())
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
        if let Some(legacy) = &segment.legacy_variable {
            if !variables.contains(legacy.as_str()) {
                return Err(script_error(
                    format!("Unknown naming variable: {legacy}."),
                    script_name,
                    segment.line,
                    segment.column,
                ));
            }
            if segment.format_spec.is_some()
                && !matches!(
                    legacy.as_str(),
                    "track"
                        | "disc"
                        | "track_number"
                        | "disc_number"
                        | "total_tracks"
                        | "total_discs"
                        | "medium_number"
                )
            {
                return Err(script_error(
                    format!("Variable {legacy} does not support a numeric format."),
                    script_name,
                    segment.line,
                    segment.column,
                ));
            }
        }
        for variable in &segment.expression_variables {
            if !variables.contains(variable.name.as_str()) {
                return Err(script_error(
                    format!("Unknown naming variable: {}.", variable.name),
                    script_name,
                    variable.line,
                    variable.column,
                ));
            }
        }
    }
    Ok(())
}

pub(super) fn validate_tagging_language(
    source: &str,
    script_name: &str,
    compiler: &dyn ScriptCompiler,
) -> Result<(), SettingsError> {
    let allowed: BTreeSet<&str> = MANAGED_FIELD_NAMES
        .iter()
        .copied()
        .chain(["genre"])
        .collect();
    let ordered: BTreeSet<&str> = MERGEABLE_MANAGED_FIELD_NAMES
        .iter()
        .copied()
        .chain(["genre"])
        .collect();
    let read_only: BTreeSet<&str> = [
        "artist_display",
        "album_artist_display",
        "artists",
        "album_artists",
        "genres",
        "year",
        "primary_genre",
    ]
    .iter()
    .copied()
    .collect();
    let statements = compiler
        .compile_tagging(source, script_name)
        .map_err(|cause| script_error(cause.message, script_name, cause.line, cause.column))?;
    for statement in &statements {
        if let Some(target) = &statement.target {
            let custom = if target.len() > 7
                && target.is_char_boundary(7)
                && target[..7].eq_ignore_ascii_case("custom.")
            {
                Some(target[7..].trim())
            } else {
                None
            };
            let custom_valid = custom.is_some_and(|name| {
                !name.is_empty() && !name.contains('\x00') && name.len() <= 255
            });
            if !allowed.contains(target.as_str()) && !custom_valid {
                return Err(script_error(
                    format!("Unknown or invalid tagging target: {target}."),
                    script_name,
                    statement.line,
                    statement.column,
                ));
            }
            if statement.operation == "append"
                && !ordered.contains(target.as_str())
                && custom.is_none()
            {
                return Err(script_error(
                    format!("Field {target} does not accept append."),
                    script_name,
                    statement.line,
                    statement.column,
                ));
            }
        }
        for variable in &statement.expression_variables {
            if !(allowed.contains(variable.name.as_str())
                || read_only.contains(variable.name.as_str())
                || variable.name.len() > 7
                    && variable.name.is_char_boundary(7)
                    && variable.name[..7].eq_ignore_ascii_case("custom."))
            {
                return Err(script_error(
                    format!("Unknown tagging variable: {}.", variable.name),
                    script_name,
                    variable.line,
                    variable.column,
                ));
            }
        }
    }
    Ok(())
}

/// Structural script compiler: exact segmentation, identifier scanning
/// for expressions. Catches unknown variables/targets, bad numeric
/// formats, multi-line naming, bad path shapes, and append-to-scalar;
/// malformed expressions (bad function names, arity, syntax) need the
/// full compiler behind the same port.
#[derive(Debug, Default)]
pub struct StructuralCompiler;

impl StructuralCompiler {
    /// Build the structural compiler.
    pub fn new() -> Self {
        Self
    }
}

/// Scan identifiers out of an expression body: string literals skipped,
/// dotted names kept whole, function-call callees skipped.
fn scan_expression_variables(body: &str, line: i64) -> Vec<ExprVariable> {
    let mut out = Vec::new();
    let chars: Vec<char> = body.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        if ch == '"' || ch == '\'' {
            let quote = ch;
            index += 1;
            while index < chars.len() {
                if chars[index] == '\\' {
                    index += 2;
                    continue;
                }
                if chars[index] == quote {
                    index += 1;
                    break;
                }
                index += 1;
            }
            continue;
        }
        if ch.is_ascii_alphabetic() || ch == '_' {
            let start = index;
            while index < chars.len()
                && (chars[index].is_ascii_alphanumeric()
                    || chars[index] == '_'
                    || chars[index] == '.')
            {
                index += 1;
            }
            let mut name: String = chars[start..index].iter().collect();
            while name.ends_with('.') {
                name.pop();
            }
            // A name followed by `(` is a function call, not a variable.
            let mut lookahead = index;
            while lookahead < chars.len() && chars[lookahead].is_whitespace() {
                lookahead += 1;
            }
            if lookahead < chars.len() && chars[lookahead] == '(' {
                continue;
            }
            if !name.is_empty() {
                out.push(ExprVariable {
                    name,
                    line,
                    column: start as i64 + 1,
                });
            }
            continue;
        }
        index += 1;
    }
    out
}

impl ScriptCompiler for StructuralCompiler {
    fn compile_naming(
        &self,
        source: &str,
        script_name: &str,
    ) -> Result<Vec<NamingSegment>, ScriptError> {
        let mut segments = Vec::new();
        let mut literal = String::new();
        let mut index = 0;
        let chars: Vec<char> = source.chars().collect();
        let fail = |message: &str, column: usize| ScriptError {
            message: format!("{script_name}: {message}"),
            line: 1,
            column: column as i64,
        };
        // v2's legacy placeholder: {name} or {name:spec}.
        let legacy = |body: &str| -> Option<(String, Option<String>)> {
            let (name, spec) = match body.split_once(':') {
                Some((name, spec)) => (name, Some(spec.to_owned())),
                None => (body, None),
            };
            if name.is_empty()
                || !name
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '.')
            {
                return None;
            }
            if let Some(spec) = &spec
                && (spec.is_empty()
                    || !spec
                        .chars()
                        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '.'))
            {
                return None;
            }
            Some((name.to_owned(), spec))
        };
        while index < chars.len() {
            if chars[index] == '{' {
                // Find the matching close brace (nesting-aware).
                let mut depth = 0;
                let mut end = None;
                for (offset, ch) in chars[index..].iter().enumerate() {
                    if *ch == '{' {
                        depth += 1;
                    } else if *ch == '}' {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(index + offset);
                            break;
                        }
                    }
                }
                let Some(end) = end else {
                    return Err(fail("Unmatched '{' in naming template.", index + 1));
                };
                if !literal.is_empty() {
                    segments.push(NamingSegment {
                        literal: Some(std::mem::take(&mut literal)),
                        legacy_variable: None,
                        format_spec: None,
                        expression_variables: Vec::new(),
                        line: 1,
                        column: 1,
                    });
                }
                let body: String = chars[index + 1..end].iter().collect();
                match legacy(&body) {
                    Some((name, spec)) => segments.push(NamingSegment {
                        literal: None,
                        legacy_variable: Some(name),
                        format_spec: spec,
                        expression_variables: Vec::new(),
                        line: 1,
                        column: index as i64 + 1,
                    }),
                    None => segments.push(NamingSegment {
                        literal: None,
                        legacy_variable: None,
                        format_spec: None,
                        expression_variables: scan_expression_variables(&body, 1),
                        line: 1,
                        column: index as i64 + 1,
                    }),
                }
                index = end + 1;
            } else if chars[index] == '}' {
                return Err(fail("Unmatched '}' in naming template.", index + 1));
            } else {
                literal.push(chars[index]);
                index += 1;
            }
        }
        if !literal.is_empty() {
            segments.push(NamingSegment {
                literal: Some(literal),
                legacy_variable: None,
                format_spec: None,
                expression_variables: Vec::new(),
                line: 1,
                column: 1,
            });
        }
        Ok(segments)
    }

    fn compile_tagging(
        &self,
        source: &str,
        script_name: &str,
    ) -> Result<Vec<TaggingStatement>, ScriptError> {
        let mut statements = Vec::new();
        let mut depth = 0;
        for (number, raw_line) in source.lines().enumerate() {
            let line_number = number as i64 + 1;
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let fail = |message: &str| ScriptError {
                message: format!("{script_name}: {message}"),
                line: line_number,
                column: 1,
            };
            let mut words = line.splitn(2, char::is_whitespace);
            let keyword = words.next().unwrap_or("").to_lowercase();
            let rest = words.next().unwrap_or("").trim();
            match keyword.as_str() {
                "if" => {
                    depth += 1;
                    statements.push(TaggingStatement {
                        operation: "if".to_owned(),
                        target: None,
                        expression_variables: scan_expression_variables(rest, line_number),
                        line: line_number,
                        column: 1,
                    });
                }
                "else" => {
                    if depth == 0 {
                        return Err(fail("else without if."));
                    }
                    statements.push(TaggingStatement {
                        operation: "else".to_owned(),
                        target: None,
                        expression_variables: Vec::new(),
                        line: line_number,
                        column: 1,
                    });
                }
                "end" => {
                    if depth == 0 {
                        return Err(fail("end without if."));
                    }
                    depth -= 1;
                    statements.push(TaggingStatement {
                        operation: "end".to_owned(),
                        target: None,
                        expression_variables: Vec::new(),
                        line: line_number,
                        column: 1,
                    });
                }
                "set" | "append" | "delete" => {
                    let (target, expression) = match rest.split_once('=') {
                        Some((target, expression)) => (target.trim().to_owned(), expression.trim()),
                        None => {
                            if keyword == "delete" && !rest.is_empty() {
                                (rest.to_owned(), "")
                            } else {
                                return Err(fail(&format!(
                                    "{keyword} needs a target and a value."
                                )));
                            }
                        }
                    };
                    if target.is_empty() {
                        return Err(fail(&format!("{keyword} needs a target.")));
                    }
                    statements.push(TaggingStatement {
                        operation: keyword,
                        target: Some(target),
                        expression_variables: scan_expression_variables(expression, line_number),
                        line: line_number,
                        column: 1,
                    });
                }
                _ => {
                    return Err(fail(&format!("Unknown tagging statement: {keyword}.")));
                }
            }
        }
        if depth != 0 {
            return Err(ScriptError {
                message: format!("{script_name}: Unclosed if block."),
                line: 1,
                column: 1,
            });
        }
        Ok(statements)
    }
}
