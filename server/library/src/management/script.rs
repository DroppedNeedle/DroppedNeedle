//! The small expression language behind naming and tagging scripts.
//!
//! Naming scripts are templates: literal text with `{variable}`,
//! `{variable:02d}` or `{expression}` holes, rendered into a relative
//! path. Tagging scripts are line programs of `set field = expr`,
//! `append field = expr`, `delete field` and `if expr:` ... `else:` ...
//! `end`. Expressions are literals, variables, `[lists]` and calls to a
//! fixed set of safe functions. Evaluation is bounded (steps, value
//! sizes, total output) so a script can never run away.
//!
//! This is the only implementation of the language: settings validation
//! compiles scripts here too, so a script that saves also compiles.

use std::collections::BTreeMap;

use unicode_normalization::UnicodeNormalization as _;

use droppedneedle_kernel::runtime_config::sections::{
    ExtensionCase, PathCompatibilitySettings, UnicodeNormalization,
};

const MAX_EXPRESSION_DEPTH: usize = 20;
const MAX_EXPRESSION_TOKENS: usize = 2_048;
const MAX_PROGRAM_STATEMENTS: usize = 500;
const MAX_RUNTIME_STEPS: usize = 10_000;
const MAX_LIST_VALUES: usize = 100;
const MAX_VALUE_CHARACTERS: usize = 8_192;
const MAX_TOTAL_OUTPUT_CHARACTERS: usize = 65_536;
const MAX_SOURCE_CHARACTERS: usize = 32_768;

/// Windows device names are disarmed even on Linux, so a library copied
/// to a Windows share stays valid.
const RESERVED_NAMES: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// A script value.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Value {
    #[default]
    Null,
    Text(String),
    Int(i64),
    Bool(bool),
    List(Vec<String>),
}

impl Value {
    fn truthy(&self) -> bool {
        match self {
            Self::Null => false,
            Self::Text(text) => !text.is_empty(),
            Self::Int(value) => *value != 0,
            Self::Bool(value) => *value,
            Self::List(values) => !values.is_empty(),
        }
    }

    fn is_empty(&self) -> bool {
        match self {
            Self::Null => true,
            Self::Text(text) => text.is_empty(),
            Self::List(values) => values.is_empty(),
            Self::Int(_) | Self::Bool(_) => false,
        }
    }

    /// The value as text: lists join with `; `.
    pub fn text(&self) -> String {
        match self {
            Self::Null => String::new(),
            Self::Text(text) => text.clone(),
            Self::Int(value) => value.to_string(),
            Self::Bool(value) => if *value { "true" } else { "false" }.to_owned(),
            Self::List(values) => values.join("; "),
        }
    }

    /// The value as an ordered list of strings (empty for null).
    pub fn values(&self) -> Vec<String> {
        match self {
            Self::Null => Vec::new(),
            Self::List(values) => values.clone(),
            other => vec![other.text()],
        }
    }
}

/// Why a script failed: a plain sentence and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptFault {
    pub message: String,
    pub line: usize,
    pub column: usize,
    /// The rendered path broke a length limit (rather than the script
    /// being wrong).
    pub path_too_long: bool,
}

impl std::fmt::Display for ScriptFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} (line {}, column {})",
            self.message, self.line, self.column
        )
    }
}

fn fault<T>(message: impl Into<String>, line: usize, column: usize) -> Result<T, ScriptFault> {
    Err(ScriptFault {
        message: message.into(),
        line,
        column,
        path_too_long: false,
    })
}

// ---------------------------------------------------------------------------
// Expressions.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Open,
    Close,
    Comma,
    OpenList,
    CloseList,
    Text(String),
    Int(i64),
    Ident(String),
    End,
}

#[derive(Debug, Clone)]
struct Positioned {
    token: Token,
    line: usize,
    column: usize,
}

/// One parsed expression.
#[derive(Debug, Clone)]
pub enum Expr {
    Literal(Value),
    Variable {
        name: String,
        line: usize,
        column: usize,
    },
    List(Vec<Expr>, usize, usize),
    Call {
        name: String,
        args: Vec<Expr>,
        line: usize,
        column: usize,
    },
}

fn arity(name: &str) -> Option<(usize, usize)> {
    Some(match name {
        "default" | "fallback" | "concat" | "all" | "and" | "any" | "or" => (1, MAX_LIST_VALUES),
        "if" | "conditional" | "replace" => (3, 3),
        "equals" | "eq" | "contains" => (2, 2),
        "slice" | "pad" => (2, 3),
        "lower" | "upper" | "title" | "first" | "sortname" | "sort_name" | "asciifold"
        | "ascii_fold" | "not" | "is_empty" | "empty" => (1, 1),
        "join" | "pathsafe" | "path_safe" => (1, 2),
        _ => return None,
    })
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '.'
}

fn tokenize(source: &str, line: usize, column: usize) -> Result<Vec<Positioned>, ScriptFault> {
    let chars: Vec<char> = source.chars().collect();
    let mut tokens = Vec::new();
    let (mut i, mut line, mut column) = (0, line, column);
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            if c == '\n' {
                line += 1;
                column = 1;
            } else {
                column += 1;
            }
            i += 1;
            continue;
        }
        let (at_line, at_column) = (line, column);
        let token = match c {
            '(' => Token::Open,
            ')' => Token::Close,
            ',' => Token::Comma,
            '[' => Token::OpenList,
            ']' => Token::CloseList,
            '"' | '\'' => {
                let mut text = String::new();
                let mut j = i + 1;
                column += 1;
                loop {
                    let Some(&next) = chars.get(j) else {
                        return fault("String literal is not closed.", at_line, at_column);
                    };
                    if next == c {
                        j += 1;
                        column += 1;
                        break;
                    }
                    if next == '\n' || next == '\r' {
                        return fault("String literals cannot span lines.", line, column);
                    }
                    if next != '\\' {
                        text.push(next);
                        j += 1;
                        column += 1;
                        continue;
                    }
                    let Some(&escaped) = chars.get(j + 1) else {
                        return fault("String literal ends with an escape.", line, column);
                    };
                    if escaped == 'u' {
                        let digits: String = chars.iter().skip(j + 2).take(4).collect();
                        let code = (digits.len() == 4)
                            .then(|| u32::from_str_radix(&digits, 16).ok())
                            .flatten()
                            .and_then(char::from_u32);
                        let Some(code) = code else {
                            return fault("String has an invalid Unicode escape.", line, column);
                        };
                        text.push(code);
                        j += 6;
                        column += 6;
                        continue;
                    }
                    let replacement = match escaped {
                        'n' => '\n',
                        'r' => '\r',
                        't' => '\t',
                        '\\' => '\\',
                        other if other == c => c,
                        _ => return fault("String has an unsupported escape.", line, column),
                    };
                    text.push(replacement);
                    j += 2;
                    column += 2;
                }
                i = j;
                tokens.push(Positioned {
                    token: Token::Text(text),
                    line: at_line,
                    column: at_column,
                });
                continue;
            }
            digit if digit.is_ascii_digit() => {
                let start = i;
                while chars.get(i).is_some_and(char::is_ascii_digit) {
                    i += 1;
                }
                let digits: String = chars[start..i].iter().collect();
                column += i - start;
                let Ok(value) = digits.parse::<i64>() else {
                    return fault("Number is too large.", at_line, at_column);
                };
                tokens.push(Positioned {
                    token: Token::Int(value),
                    line: at_line,
                    column: at_column,
                });
                continue;
            }
            start if is_ident_start(start) => {
                let begin = i;
                while chars.get(i).is_some_and(|c| is_ident_char(*c)) {
                    i += 1;
                }
                let ident: String = chars[begin..i].iter().collect();
                column += i - begin;
                tokens.push(Positioned {
                    token: Token::Ident(ident),
                    line: at_line,
                    column: at_column,
                });
                continue;
            }
            other => return fault(format!("Unexpected character '{other}'."), line, column),
        };
        tokens.push(Positioned {
            token,
            line: at_line,
            column: at_column,
        });
        i += 1;
        column += 1;
        if tokens.len() > MAX_EXPRESSION_TOKENS {
            return fault("Expression has too many tokens.", at_line, at_column);
        }
    }
    tokens.push(Positioned {
        token: Token::End,
        line,
        column,
    });
    Ok(tokens)
}

struct Parser {
    tokens: Vec<Positioned>,
    at: usize,
}

impl Parser {
    fn current(&self) -> &Positioned {
        &self.tokens[self.at.min(self.tokens.len() - 1)]
    }

    fn primary(&mut self, depth: usize) -> Result<Expr, ScriptFault> {
        let token = self.current().clone();
        if depth > MAX_EXPRESSION_DEPTH {
            return fault("Expression is nested too deeply.", token.line, token.column);
        }
        self.at += 1;
        match token.token {
            Token::Text(text) => Ok(Expr::Literal(Value::Text(text))),
            Token::Int(value) => Ok(Expr::Literal(Value::Int(value))),
            Token::OpenList => Ok(Expr::List(
                self.arguments(&Token::CloseList, depth)?,
                token.line,
                token.column,
            )),
            Token::Ident(name) => {
                match name.to_lowercase().as_str() {
                    "true" => return Ok(Expr::Literal(Value::Bool(true))),
                    "false" => return Ok(Expr::Literal(Value::Bool(false))),
                    "null" => return Ok(Expr::Literal(Value::Null)),
                    _ => {}
                }
                if self.current().token != Token::Open {
                    return Ok(Expr::Variable {
                        name,
                        line: token.line,
                        column: token.column,
                    });
                }
                self.at += 1;
                let args = self.arguments(&Token::Close, depth)?;
                let folded = name.to_lowercase();
                let Some((minimum, maximum)) = arity(&folded) else {
                    return fault(
                        format!("Unknown function: {name}."),
                        token.line,
                        token.column,
                    );
                };
                if args.len() < minimum || args.len() > maximum {
                    let expected = if minimum == maximum {
                        minimum.to_string()
                    } else {
                        format!("{minimum}-{maximum}")
                    };
                    return fault(
                        format!("{name}() expects {expected} arguments."),
                        token.line,
                        token.column,
                    );
                }
                Ok(Expr::Call {
                    name: folded,
                    args,
                    line: token.line,
                    column: token.column,
                })
            }
            _ => fault(
                "Expected a value, variable, list, or function call.",
                token.line,
                token.column,
            ),
        }
    }

    fn arguments(&mut self, closing: &Token, depth: usize) -> Result<Vec<Expr>, ScriptFault> {
        let mut values = Vec::new();
        if &self.current().token == closing {
            self.at += 1;
            return Ok(values);
        }
        loop {
            values.push(self.primary(depth + 1)?);
            if values.len() > MAX_LIST_VALUES {
                let at = self.current();
                return fault("Expression contains too many values.", at.line, at.column);
            }
            let at = self.current().clone();
            if &at.token == closing {
                self.at += 1;
                return Ok(values);
            }
            if at.token != Token::Comma {
                return fault("Expected ',' or a closing bracket.", at.line, at.column);
            }
            self.at += 1;
        }
    }
}

impl Expr {
    /// Every variable the expression reads, with where it is written.
    pub fn variables(&self) -> Vec<(&str, usize, usize)> {
        let mut found = Vec::new();
        let mut pending = vec![self];
        while let Some(expr) = pending.pop() {
            match expr {
                Self::Literal(_) => {}
                Self::Variable { name, line, column } => {
                    found.push((name.as_str(), *line, *column));
                }
                Self::List(items, ..) | Self::Call { args: items, .. } => {
                    pending.extend(items.iter().rev());
                }
            }
        }
        found
    }
}

/// Parse one expression.
pub fn compile_expression(source: &str, line: usize, column: usize) -> Result<Expr, ScriptFault> {
    if source.chars().count() > MAX_SOURCE_CHARACTERS {
        return fault("Expression source is too long.", line, column);
    }
    if source.trim().is_empty() {
        return fault("Expression is empty.", line, column);
    }
    let mut parser = Parser {
        tokens: tokenize(source, line, column)?,
        at: 0,
    };
    let expression = parser.primary(1)?;
    let rest = parser.current();
    if rest.token != Token::End {
        return fault(
            "Unexpected text after the expression.",
            rest.line,
            rest.column,
        );
    }
    Ok(expression)
}

/// Evaluation limits for one script run.
#[derive(Debug, Default)]
pub struct Budget {
    steps: usize,
    output: usize,
}

impl Budget {
    fn step(&mut self, line: usize, column: usize) -> Result<(), ScriptFault> {
        self.steps += 1;
        if self.steps > MAX_RUNTIME_STEPS {
            return fault("Script exceeded the runtime step limit.", line, column);
        }
        Ok(())
    }

    fn account(&mut self, value: Value, line: usize, column: usize) -> Result<Value, ScriptFault> {
        let texts: Vec<&str> = match &value {
            Value::Text(text) => vec![text.as_str()],
            Value::List(values) => {
                if values.len() > MAX_LIST_VALUES {
                    return fault("Script produced too many list values.", line, column);
                }
                values.iter().map(String::as_str).collect()
            }
            _ => Vec::new(),
        };
        if texts.iter().any(|text| text.contains('\0')) {
            return fault("Script produced unsafe text.", line, column);
        }
        let size: usize = texts.iter().map(|text| text.chars().count()).sum();
        if size > MAX_VALUE_CHARACTERS {
            return fault("Script produced a value that is too long.", line, column);
        }
        self.output += size;
        if self.output > MAX_TOTAL_OUTPUT_CHARACTERS {
            return fault("Script exceeded the total output limit.", line, column);
        }
        Ok(value)
    }
}

fn integer(value: &Value, line: usize, column: usize) -> Result<i64, ScriptFault> {
    match value {
        Value::Int(value) => Ok(*value),
        Value::Text(text) => text
            .trim()
            .parse()
            .or_else(|_| fault("Function requires an integer.", line, column)),
        _ => fault("Function requires an integer.", line, column),
    }
}

/// Python-style slice bounds over `len` items.
fn slice_bounds(len: usize, start: i64, end: Option<i64>) -> (usize, usize) {
    let clamp = |index: i64| -> usize {
        let len = len as i64;
        let index = if index < 0 { len + index } else { index };
        index.clamp(0, len) as usize
    };
    let from = clamp(start);
    let to = end.map_or(len, clamp);
    (from, to.max(from))
}

fn title_case(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut previous_cased = false;
    for c in text.chars() {
        if c.is_alphabetic() {
            if previous_cased {
                out.extend(c.to_lowercase());
            } else {
                out.extend(c.to_uppercase());
            }
            previous_cased = true;
        } else {
            out.push(c);
            previous_cased = false;
        }
    }
    out
}

fn ascii_fold(text: &str) -> String {
    text.nfkd().filter(char::is_ascii).collect()
}

fn invalid_path_char(c: char) -> bool {
    matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || c.is_control()
}

fn call(name: &str, values: Vec<Value>, line: usize, column: usize) -> Result<Value, ScriptFault> {
    let text = |index: usize| values.get(index).map(Value::text).unwrap_or_default();
    Ok(match name {
        "default" | "fallback" => values
            .into_iter()
            .find(|value| !value.is_empty())
            .unwrap_or(Value::Null),
        "if" | "conditional" => {
            let mut values = values.into_iter();
            let condition = values.next().unwrap_or_default();
            let (then, otherwise) = (values.next(), values.next());
            if condition.truthy() { then } else { otherwise }.unwrap_or_default()
        }
        "equals" | "eq" => Value::Bool(values.first() == values.get(1)),
        "contains" => match values.first() {
            Some(Value::List(items)) => Value::Bool(items.contains(&text(1))),
            _ => Value::Bool(text(0).contains(&text(1))),
        },
        "replace" => Value::Text(text(0).replace(&text(1), &text(2))),
        "slice" => {
            let start = integer(&values[1], line, column)?;
            let end = values
                .get(2)
                .map(|value| integer(value, line, column))
                .transpose()?;
            match &values[0] {
                Value::List(items) => {
                    let (from, to) = slice_bounds(items.len(), start, end);
                    Value::List(items[from..to].to_vec())
                }
                other => {
                    let chars: Vec<char> = other.text().chars().collect();
                    let (from, to) = slice_bounds(chars.len(), start, end);
                    Value::Text(chars[from..to].iter().collect())
                }
            }
        }
        "pad" => {
            let width = integer(&values[1], line, column)?;
            let fill = if values.len() == 3 {
                text(2)
            } else {
                "0".to_owned()
            };
            let mut fill_chars = fill.chars();
            let (Some(fill), None) = (fill_chars.next(), fill_chars.next()) else {
                return fault("pad() width or fill is invalid.", line, column);
            };
            if !(0..=1_024).contains(&width) {
                return fault("pad() width or fill is invalid.", line, column);
            }
            let value = text(0);
            let missing = (width as usize).saturating_sub(value.chars().count());
            Value::Text(
                std::iter::repeat_n(fill, missing)
                    .chain(value.chars())
                    .collect(),
            )
        }
        "lower" => Value::Text(text(0).to_lowercase()),
        "upper" => Value::Text(text(0).to_uppercase()),
        "title" => Value::Text(title_case(&text(0))),
        "first" => match values.into_iter().next().unwrap_or_default() {
            Value::List(items) if !items.is_empty() => Value::Text(items[0].clone()),
            other => other,
        },
        "join" => {
            let separator = if values.len() == 2 {
                text(1)
            } else {
                "; ".to_owned()
            };
            match &values[0] {
                Value::List(items) => Value::Text(items.join(&separator)),
                other => Value::Text(other.text()),
            }
        }
        "sortname" | "sort_name" => {
            let value = text(0).trim().to_owned();
            for article in ["The ", "An ", "A "] {
                if value.len() >= article.len()
                    && value.is_char_boundary(article.len())
                    && value[..article.len()].eq_ignore_ascii_case(article)
                {
                    let rest = &value[article.len()..];
                    return Ok(Value::Text(format!(
                        "{rest}, {}",
                        &value[..article.len() - 1]
                    )));
                }
            }
            Value::Text(value)
        }
        "asciifold" | "ascii_fold" => Value::Text(ascii_fold(&text(0))),
        "pathsafe" | "path_safe" => {
            let replacement = if values.len() == 2 {
                text(1)
            } else {
                "_".to_owned()
            };
            let mut chars = replacement.chars();
            let (Some(replacement), None) = (chars.next(), chars.next()) else {
                return fault(
                    "path_safe() replacement must be one safe character.",
                    line,
                    column,
                );
            };
            if matches!(replacement, '/' | '\\' | '\0') {
                return fault(
                    "path_safe() replacement must be one safe character.",
                    line,
                    column,
                );
            }
            Value::Text(
                text(0)
                    .chars()
                    .map(|c| if invalid_path_char(c) { replacement } else { c })
                    .collect(),
            )
        }
        "concat" => Value::Text(values.iter().map(Value::text).collect()),
        "not" => Value::Bool(!values.first().is_some_and(Value::truthy)),
        "all" | "and" => Value::Bool(values.iter().all(Value::truthy)),
        "any" | "or" => Value::Bool(values.iter().any(Value::truthy)),
        "is_empty" | "empty" => Value::Bool(values.first().is_none_or(Value::is_empty)),
        _ => return fault(format!("Unknown function: {name}."), line, column),
    })
}

/// Variables a script reads.
pub type Variables = BTreeMap<String, Value>;

/// Evaluate one expression.
pub fn evaluate(
    expr: &Expr,
    variables: &Variables,
    budget: &mut Budget,
) -> Result<Value, ScriptFault> {
    match expr {
        Expr::Literal(value) => {
            budget.step(1, 1)?;
            budget.account(value.clone(), 1, 1)
        }
        Expr::Variable { name, .. } => {
            budget.step(1, 1)?;
            budget.account(variables.get(name).cloned().unwrap_or_default(), 1, 1)
        }
        Expr::List(items, line, column) => {
            budget.step(*line, *column)?;
            let mut values = Vec::with_capacity(items.len());
            for item in items {
                values.push(evaluate(item, variables, budget)?.text());
            }
            budget.account(Value::List(values), *line, *column)
        }
        Expr::Call {
            name,
            args,
            line,
            column,
        } => {
            budget.step(*line, *column)?;
            let mut values = Vec::with_capacity(args.len());
            for arg in args {
                values.push(evaluate(arg, variables, budget)?);
            }
            let value = call(name, values, *line, *column)?;
            budget.account(value, *line, *column)
        }
    }
}

// ---------------------------------------------------------------------------
// Naming templates.
// ---------------------------------------------------------------------------

/// One part of a compiled naming script.
#[derive(Debug, Clone)]
pub enum Segment {
    Literal(String),
    Variable {
        name: String,
        spec: Option<String>,
        line: usize,
        column: usize,
    },
    Expression(Expr),
}

/// Parse a naming script into its parts.
pub fn compile_naming(source: &str) -> Result<Vec<Segment>, ScriptFault> {
    if source.chars().count() > MAX_SOURCE_CHARACTERS {
        return fault("Naming script source is too long.", 1, 1);
    }
    if source.is_empty() {
        return fault("Naming script is empty.", 1, 1);
    }
    let mut segments = Vec::new();
    let mut literal = String::new();
    let (mut line, mut column) = (1, 1);
    let mut rest = source;
    while let Some(c) = rest.chars().next() {
        if c == '}' {
            return fault("Unexpected closing brace.", line, column);
        }
        if c != '{' {
            literal.push(c);
            if c == '\n' {
                line += 1;
                column = 1;
            } else {
                column += 1;
            }
            rest = &rest[c.len_utf8()..];
            continue;
        }
        if !literal.is_empty() {
            segments.push(Segment::Literal(std::mem::take(&mut literal)));
        }
        let Some(end) = rest.find('}') else {
            return fault("Expression brace is not closed.", line, column);
        };
        let inner = &rest[1..end];
        let body = inner.trim();
        if body.is_empty() {
            return fault("Naming expression is empty.", line, column);
        }
        let body_column = column + 1 + (inner.len() - inner.trim_start().len());
        let (name, spec) = match body.split_once(':') {
            Some((name, spec)) => (name, Some(spec)),
            None => (body, None),
        };
        let legacy = name.chars().next().is_some_and(is_ident_start)
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && spec.is_none_or(|spec| !spec.is_empty());
        if legacy {
            segments.push(Segment::Variable {
                name: name.to_owned(),
                spec: spec.map(str::to_owned),
                line,
                column,
            });
        } else {
            segments.push(Segment::Expression(compile_expression(
                body,
                line,
                body_column,
            )?));
        }
        let consumed = &rest[..=end];
        if let Some(last_break) = consumed.rfind('\n') {
            line += consumed.matches('\n').count();
            column = consumed[last_break + 1..].chars().count() + 1;
        } else {
            column += consumed.chars().count();
        }
        rest = &rest[end + 1..];
    }
    if !literal.is_empty() {
        segments.push(Segment::Literal(literal));
    }
    Ok(segments)
}

/// Widest numeric format a naming script may ask for. `format!` panics on
/// a width past `u16::MAX`, and no file name needs more than this.
const MAX_NUMBER_WIDTH: usize = 64;

/// Format an integer with a Python-style spec: `d`, `3d`, `02d`. `None`
/// when the spec is malformed or wider than [`MAX_NUMBER_WIDTH`].
pub(crate) fn format_number(value: i64, spec: &str) -> Option<String> {
    let digits = spec.strip_suffix('d').unwrap_or(spec);
    let (zero, width) = match digits.strip_prefix('0') {
        Some(rest) if !rest.is_empty() => (true, rest),
        _ => (false, digits),
    };
    let width: usize = if width.is_empty() {
        0
    } else {
        width.parse().ok()?
    };
    if width > MAX_NUMBER_WIDTH {
        return None;
    }
    Some(if zero {
        if value < 0 {
            format!(
                "-{:0>width$}",
                value.unsigned_abs(),
                width = width.saturating_sub(1)
            )
        } else {
            format!("{value:0>width$}")
        }
    } else {
        format!("{value:>width$}")
    })
}

fn normalize(text: &str, form: UnicodeNormalization) -> String {
    match form {
        UnicodeNormalization::NFC => text.nfc().collect(),
        UnicodeNormalization::NFKC => text.nfkc().collect(),
    }
}

fn reserved(component: &str) -> bool {
    let stem = component
        .split('.')
        .next()
        .unwrap_or_default()
        .to_lowercase();
    RESERVED_NAMES.contains(&stem.as_str())
}

/// Truncate to at most `max` UTF-8 bytes on a character boundary.
fn truncate_bytes(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn sanitize_value(value: &str, compat: &PathCompatibilitySettings) -> String {
    value
        .chars()
        .map(|c| {
            let bad = if compat.windows_compatible {
                invalid_path_char(c)
            } else {
                matches!(c, '/' | '\\') || c.is_control()
            };
            if bad {
                compat.separator_replacement.clone()
            } else {
                c.to_string()
            }
        })
        .collect()
}

fn clean_component(component: &str, compat: &PathCompatibilitySettings, last: bool) -> String {
    let replacement = compat.separator_replacement.as_str();
    let mut cleaned = sanitize_value(&normalize(component, compat.unicode_normalization), compat);
    if compat.replace_non_ascii {
        let folded = ascii_fold(&cleaned);
        cleaned = if folded.is_empty() {
            replacement.to_owned()
        } else {
            folded
        };
    }
    if compat.replace_spaces_with_underscores {
        cleaned = cleaned.replace(' ', "_");
    }
    let strip = |text: &str| text.trim_matches(|c| c == ' ' || c == '.').to_owned();
    if compat.windows_compatible {
        cleaned = strip(&cleaned);
        if !cleaned.is_empty() && reserved(&cleaned) {
            cleaned = format!("{replacement}{cleaned}");
        }
    }
    if cleaned.is_empty() {
        cleaned = replacement.to_owned();
    }
    let maximum = usize::try_from(compat.maximum_component_length.max(1)).unwrap_or(255);
    if cleaned.len() > maximum {
        let suffix = std::path::Path::new(&cleaned)
            .extension()
            .map(|ext| format!(".{}", ext.to_string_lossy()))
            .filter(|suffix| last && suffix.len() < maximum);
        cleaned = match suffix {
            Some(suffix) => {
                let stem = &cleaned[..cleaned.len() - suffix.len()];
                let mut stem = truncate_bytes(stem, maximum - suffix.len()).to_owned();
                if compat.windows_compatible {
                    stem = stem.trim_end_matches([' ', '.']).to_owned();
                }
                format!("{stem}{suffix}")
            }
            None => {
                let cut = truncate_bytes(&cleaned, maximum).to_owned();
                if compat.windows_compatible {
                    strip(&cut)
                } else {
                    cut
                }
            }
        };
        if cleaned.is_empty() {
            cleaned = replacement.to_owned();
        }
        if compat.windows_compatible && reserved(&cleaned) {
            cleaned = truncate_bytes(&format!("{replacement}{cleaned}"), maximum).to_owned();
        }
    }
    cleaned
}

/// The file extension as the profile wants it cased.
pub fn cased_extension(extension: &str, case: ExtensionCase) -> String {
    match case {
        ExtensionCase::Preserve => extension.to_owned(),
        ExtensionCase::Lower => extension.to_lowercase(),
        ExtensionCase::Upper => extension.to_uppercase(),
    }
}

/// Render a naming template into a relative path, cleaned for the
/// profile's path compatibility settings. `root` (the destination
/// root's absolute path) counts toward the path length limits.
pub fn render_path(
    source: &str,
    variables: &Variables,
    compat: &PathCompatibilitySettings,
    root: &str,
) -> Result<String, ScriptFault> {
    let mut budget = Budget::default();
    let mut rendered = String::new();
    for segment in compile_naming(source)? {
        let text = match segment {
            Segment::Literal(text) => {
                rendered.push_str(&text);
                continue;
            }
            Segment::Variable {
                name,
                spec,
                line,
                column,
            } => {
                let text = variables.get(&name).map(Value::text).unwrap_or_default();
                match spec {
                    None => text,
                    Some(spec) => {
                        let number = text.trim().parse::<i64>().ok();
                        match number.and_then(|number| format_number(number, &spec)) {
                            Some(formatted) => formatted,
                            None => {
                                return fault(
                                    format!(
                                        "{{{name}:{spec}}} needs a whole number, but {name} is \"{text}\"."
                                    ),
                                    line,
                                    column,
                                );
                            }
                        }
                    }
                }
            }
            Segment::Expression(expr) => evaluate(&expr, variables, &mut budget)?.text(),
        };
        rendered.push_str(&sanitize_value(&text, compat));
    }
    // Windows-compatible names lose leading and trailing dots, so a value
    // of `.` or `..` becomes the replacement there, as `...` already does.
    // Without that cleaning it would name a folder outside the path.
    let parts: Vec<&str> = rendered.split('/').collect();
    if rendered.is_empty()
        || rendered.starts_with('/')
        || parts.iter().any(|part| {
            part.is_empty() || (!compat.windows_compatible && matches!(*part, "." | ".."))
        })
    {
        return fault(
            "The naming script made an empty folder or file name, or a path that leaves the library folder.",
            1,
            1,
        );
    }
    let cleaned: Vec<String> = parts
        .iter()
        .enumerate()
        .map(|(index, part)| clean_component(part, compat, index + 1 == parts.len()))
        .collect();
    let relative = cleaned.join("/");
    let limit = usize::try_from(compat.maximum_path_length.max(1)).unwrap_or(4096);
    let absolute = format!("{}/{relative}", root.trim_end_matches('/'));
    let too_long = relative.len() > limit
        || absolute.len() > limit
        || (compat.windows_legacy_path_limit && absolute.chars().count() > 259);
    if too_long {
        return Err(ScriptFault {
            message: "The path this file would get is longer than the profile allows.".to_owned(),
            line: 1,
            column: 1,
            path_too_long: true,
        });
    }
    Ok(relative)
}

// ---------------------------------------------------------------------------
// Tagging programs.
// ---------------------------------------------------------------------------

/// One tagging statement.
#[derive(Debug, Clone)]
pub enum Statement {
    Set(Target, Expr),
    Append(Target, Expr),
    Delete(Target),
    If(Expr, Vec<Statement>, Vec<Statement>),
}

/// The field a statement writes, and where the statement starts.
#[derive(Debug, Clone)]
pub struct Target {
    pub name: String,
    pub line: usize,
    pub column: usize,
}

fn valid_target(target: &str) -> bool {
    let mut chars = target.chars();
    chars.next().is_some_and(is_ident_start)
        && chars
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ' ' | ':' | '/' | '-'))
}

fn parse_block(
    lines: &[&str],
    start: usize,
    depth: usize,
    count: &mut usize,
) -> Result<(Vec<Statement>, usize, Option<&'static str>), ScriptFault> {
    if depth > MAX_EXPRESSION_DEPTH {
        return fault("Conditional nesting is too deep.", start + 1, 1);
    }
    let mut statements = Vec::new();
    let mut at = start;
    while at < lines.len() {
        let raw = lines[at];
        let stripped = raw.trim();
        let line = at + 1;
        let column = raw.len() - raw.trim_start().len() + 1;
        if stripped.is_empty() || stripped.starts_with('#') {
            at += 1;
            continue;
        }
        if stripped == "else:" {
            return Ok((statements, at + 1, Some("else:")));
        }
        if stripped == "end" {
            return Ok((statements, at + 1, Some("end")));
        }
        *count += 1;
        if *count > MAX_PROGRAM_STATEMENTS {
            return fault("Tagging script has too many statements.", line, column);
        }
        if let Some(condition) = stripped.strip_prefix("if ") {
            let Some(condition) = condition.strip_suffix(':') else {
                return fault("Conditional must end with ':'.", line, column);
            };
            let expr = compile_expression(condition.trim(), line, column + 3)?;
            let (body, next, terminator) = parse_block(lines, at + 1, depth + 1, count)?;
            let (alternate, next, terminator) = if terminator == Some("else:") {
                parse_block(lines, next, depth + 1, count)?
            } else {
                (Vec::new(), next, terminator)
            };
            if terminator != Some("end") {
                return fault("Conditional is missing 'end'.", line, column);
            }
            statements.push(Statement::If(expr, body, alternate));
            at = next;
            continue;
        }
        if let Some(target) = stripped.strip_prefix("delete ") {
            let target = target.trim();
            if !valid_target(target) {
                return fault("Delete target is invalid.", line, column);
            }
            statements.push(Statement::Delete(Target {
                name: target.to_owned(),
                line,
                column,
            }));
            at += 1;
            continue;
        }
        let (append, rest) = if let Some(rest) = stripped.strip_prefix("set ") {
            (false, rest)
        } else if let Some(rest) = stripped.strip_prefix("append ") {
            (true, rest)
        } else {
            return fault(
                "Expected set, append, delete, or if statement.",
                line,
                column,
            );
        };
        let Some((target, source)) = rest.split_once('=') else {
            return fault(
                "Expected set, append, delete, or if statement.",
                line,
                column,
            );
        };
        let target = target.trim();
        if !valid_target(target) {
            return fault("Statement target is invalid.", line, column);
        }
        let expr_column = raw.find('=').map_or(column, |index| index + 2);
        let expr = compile_expression(source.trim(), line, expr_column)?;
        let target = Target {
            name: target.to_owned(),
            line,
            column,
        };
        statements.push(if append {
            Statement::Append(target, expr)
        } else {
            Statement::Set(target, expr)
        });
        at += 1;
    }
    Ok((statements, at, None))
}

/// Parse a tagging program.
pub fn compile_tagging(source: &str) -> Result<Vec<Statement>, ScriptFault> {
    if source.chars().count() > MAX_SOURCE_CHARACTERS {
        return fault("Tagging script source is too long.", 1, 1);
    }
    let lines: Vec<&str> = source.lines().collect();
    let mut count = 0;
    let (statements, _, terminator) = parse_block(&lines, 0, 1, &mut count)?;
    if let Some(terminator) = terminator {
        return fault(format!("Unexpected '{terminator}'."), 1, 1);
    }
    if statements.is_empty() {
        return fault("Tagging script has no executable statements.", 1, 1);
    }
    Ok(statements)
}

/// Script variables from the fields a tagging program is editing.
pub type Refresh = dyn Fn(&BTreeMap<String, Vec<String>>) -> Variables;

/// Run a tagging program over `fields` (managed field name to values).
/// `variables` supplies read-only values; it is refreshed from `fields`
/// before each statement by `refresh`. Fields in `protected` keep their
/// value (a manual override wins). Custom targets (`custom.NAME`) are
/// evaluated but not written: the staged writer only writes managed
/// fields.
pub fn run_tagging(
    program: &[Statement],
    fields: &mut BTreeMap<String, Vec<String>>,
    protected: &std::collections::BTreeSet<String>,
    refresh: &Refresh,
    budget: &mut Budget,
) -> Result<(), ScriptFault> {
    for statement in program {
        budget.step(1, 1)?;
        let variables = refresh(fields);
        match statement {
            Statement::If(condition, body, alternate) => {
                let branch = if evaluate(condition, &variables, budget)?.truthy() {
                    body
                } else {
                    alternate
                };
                run_tagging(branch, fields, protected, refresh, budget)?;
            }
            Statement::Delete(Target { name: target, .. }) => {
                if !protected.contains(target) && !target.starts_with("custom.") {
                    fields.insert(target.clone(), Vec::new());
                }
            }
            Statement::Set(Target { name: target, .. }, expr)
            | Statement::Append(Target { name: target, .. }, expr) => {
                let value = evaluate(expr, &variables, budget)?;
                if protected.contains(target) || target.starts_with("custom.") {
                    continue;
                }
                let mut values = if matches!(statement, Statement::Append(..)) {
                    fields.get(target).cloned().unwrap_or_default()
                } else {
                    Vec::new()
                };
                for item in value.values() {
                    if !values.contains(&item) {
                        values.push(item);
                    }
                }
                if values.len() > MAX_LIST_VALUES {
                    return fault("Tagging statement produced too many values.", 1, 1);
                }
                fields.insert(target.clone(), values);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, Value)]) -> Variables {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), value.clone()))
            .collect()
    }

    #[test]
    fn picard_template_renders_and_cleans() {
        let source = crate::management::settings::PICARD_MULTI_DISC_NAMING_SOURCE;
        let variables = vars(&[
            ("albumartist", Value::Text("AC/DC".into())),
            ("album", Value::Text("Back in Black".into())),
            ("year", Value::Text("1980".into())),
            ("album_disambiguation", Value::Text(String::new())),
            ("medium_format", Value::Text("CD".into())),
            ("medium_number", Value::Int(2)),
            ("track", Value::Int(7)),
            ("title", Value::Text("What: Now?".into())),
            ("ext", Value::Text("flac".into())),
        ]);
        let path = render_path(
            source,
            &variables,
            &PathCompatibilitySettings::default(),
            "/m",
        )
        .expect("renders");
        assert_eq!(
            path,
            "AC_DC/Back in Black (1980)/CD 02/07 - What_ Now_.flac"
        );
    }

    #[test]
    fn empty_component_and_bad_format_are_reported() {
        let compat = PathCompatibilitySettings::default();
        let empty = render_path("{album}/{title}", &Variables::new(), &compat, "/m");
        assert!(empty.is_err());
        let bad = render_path(
            "{track:02d}",
            &vars(&[("track", Value::Text("x".into()))]),
            &compat,
            "/m",
        )
        .expect_err("not a number");
        assert!(bad.message.contains("whole number"));
    }

    #[test]
    fn a_dots_value_is_replaced_never_followed() {
        let dots = vars(&[("album", Value::Text("..".into()))]);
        let mut compat = PathCompatibilitySettings::default();
        let path = render_path("{album}/x", &dots, &compat, "/m").expect("renders");
        assert_eq!(path, "_/x");
        compat.windows_compatible = false;
        assert!(render_path("{album}/x", &dots, &compat, "/m").is_err());
    }

    #[test]
    fn tagging_program_sets_appends_and_branches() {
        let program = compile_tagging(
            "if eq(album, \"X\"):\n  set title = upper(title)\nelse:\n  delete title\nend\nappend genre = \"Rock\"\nset album = \"Y\"",
        )
        .expect("compiles");
        let mut fields = BTreeMap::from([
            ("album".to_owned(), vec!["X".to_owned()]),
            ("title".to_owned(), vec!["song".to_owned()]),
            ("genre".to_owned(), vec!["Pop".to_owned()]),
        ]);
        let protected = std::collections::BTreeSet::from(["album".to_owned()]);
        let refresh = |fields: &BTreeMap<String, Vec<String>>| {
            fields
                .iter()
                .map(|(name, values)| (name.clone(), Value::Text(values.join("; "))))
                .collect()
        };
        run_tagging(
            &program,
            &mut fields,
            &protected,
            &refresh,
            &mut Budget::default(),
        )
        .expect("runs");
        assert_eq!(fields["title"], vec!["SONG"]);
        assert_eq!(fields["genre"], vec!["Pop", "Rock"]);
        assert_eq!(fields["album"], vec!["X"]);
    }
}
