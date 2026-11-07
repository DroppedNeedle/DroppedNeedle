//! The custom CSS check: what in a sheet is refused, and where.
//!
//! The content-security policy lets a picture load from any host and does
//! not govern a constructed stylesheet, so this check is the only thing
//! keeping a person's or the site's CSS from fetching anything. It is the
//! check that counts: the web UI runs a copy for quick feedback, and both
//! read the one rules file, `frontend/src/lib/stores/customCssRules.json`,
//! whose cases they must both pass.
//!
//! - A backslash is refused outright: an escape can spell any name
//!   (`u\72l(` is `url(`).
//! - `@import` and `@font-face` are refused in any case.
//! - Every run of name characters (`a-z`, `0-9`, `_`, `-`) right before a
//!   `(` must be a function on the allowlist. A comment or space between a
//!   name and its `(` makes no function in CSS, and a longer run than the
//!   real name is still not on the list. Strings and comments are not
//!   skipped, so a name there is refused too.
//!
//! Matching is ASCII only, as JavaScript's case-insensitive match without
//! the `u` flag: `ſrc(` reads as `rc(`, never `src(`. Length counts UTF-16
//! units, as JavaScript's `length` does.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// The rules both checks read.
const RULES: &str = include_str!("../../../frontend/src/lib/stores/customCssRules.json");

#[derive(Deserialize)]
struct RulesFile {
    allowed_functions: Vec<String>,
    limits: Limits,
}

#[derive(Deserialize)]
struct Limits {
    user: usize,
    site: usize,
}

/// The rules, decoded once by [`CssRules::compiled`] and passed to every
/// check.
#[derive(Debug)]
pub struct CssRules {
    allowed_functions: HashSet<String>,
    user_limit: usize,
    site_limit: usize,
}

impl CssRules {
    /// The rules as compiled in. Rules that do not decode allow nothing:
    /// every function is refused and every non-empty sheet is past its
    /// limit (the test over their cases fails first).
    #[must_use]
    pub fn compiled() -> Self {
        match serde_json::from_str::<RulesFile>(RULES) {
            Ok(file) => Self {
                allowed_functions: file.allowed_functions.into_iter().collect(),
                user_limit: file.limits.user,
                site_limit: file.limits.site,
            },
            Err(error) => {
                tracing::error!(
                    %error,
                    "the custom CSS rules do not decode; every sheet is refused"
                );
                Self {
                    allowed_functions: HashSet::new(),
                    user_limit: 0,
                    site_limit: 0,
                }
            }
        }
    }
}

/// Whose sheet is checked, which sets its length limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Limit {
    /// A person's own CSS.
    User,
    /// The admin's site CSS.
    Site,
}

impl Limit {
    /// The most UTF-16 units a sheet may hold under `rules`.
    #[must_use]
    pub fn max(self, rules: &CssRules) -> usize {
        match self {
            Self::User => rules.user_limit,
            Self::Site => rules.site_limit,
        }
    }
}

/// One refused part of a sheet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CssProblem {
    /// What is refused: `backslash`, `import`, `font_face`, `too_long`, or
    /// `function:<name>` with the name in lower case.
    pub what: String,
    /// The 1-based line it first appears on; 0 for `too_long`.
    pub line: u32,
}

/// The code of a sheet past its length limit.
pub const TOO_LONG: &str = "too_long";

/// What in `css` is refused, each problem once, at the line it first
/// appears on: a backslash, then `@import`, then `@font-face`, then each
/// function off the allowlist in the order met. A sheet past its limit
/// answers `too_long` alone. Empty means the sheet may be applied.
#[must_use]
pub fn problems(css: &str, limit: Limit, rules: &CssRules) -> Vec<CssProblem> {
    if css.encode_utf16().count() > limit.max(rules) {
        return vec![CssProblem {
            what: TOO_LONG.to_owned(),
            line: 0,
        }];
    }
    let bytes = css.as_bytes();
    let lines = LineStarts::of(bytes);
    let mut found: Vec<CssProblem> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut refuse = |what: String, at: usize| {
        if seen.insert(what.clone()) {
            found.push(CssProblem {
                what,
                line: lines.line_of(at),
            });
        }
    };
    if let Some(at) = bytes.iter().position(|byte| *byte == b'\\') {
        refuse("backslash".to_owned(), at);
    }
    if let Some(at) = find_ascii_caseless(bytes, b"@import") {
        refuse("import".to_owned(), at);
    }
    if let Some(at) = find_ascii_caseless(bytes, b"@font-face") {
        refuse("font_face".to_owned(), at);
    }
    for (at, byte) in bytes.iter().enumerate() {
        if *byte != b'(' {
            continue;
        }
        let start = bytes[..at]
            .iter()
            .rposition(|byte| !is_name_byte(*byte))
            .map_or(0, |before| before + 1);
        if start == at {
            continue;
        }
        let name = String::from_utf8_lossy(&bytes[start..at]).to_ascii_lowercase();
        if !rules.allowed_functions.contains(&name) {
            refuse(format!("function:{name}"), start);
        }
    }
    found
}

/// A character of a name the check reads before a `(`.
fn is_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-'
}

/// Where `needle` (ASCII, lower case) first appears in `haystack`, ASCII
/// letters matched in either case.
fn find_ascii_caseless(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle))
}

/// The byte offset each line starts at. A line ends at `\n`, `\r\n`, `\r`
/// or `\f`, as CSS reads them.
struct LineStarts(Vec<usize>);

impl LineStarts {
    fn of(bytes: &[u8]) -> Self {
        let mut starts = vec![0];
        let mut at = 0;
        while at < bytes.len() {
            match bytes[at] {
                b'\r' if bytes.get(at + 1) == Some(&b'\n') => {
                    at += 2;
                    starts.push(at);
                }
                b'\n' | b'\r' | b'\x0c' => {
                    at += 1;
                    starts.push(at);
                }
                _ => at += 1,
            }
        }
        Self(starts)
    }

    /// The 1-based line holding byte `at`.
    fn line_of(&self, at: usize) -> u32 {
        let line = self.0.partition_point(|start| *start <= at);
        u32::try_from(line).unwrap_or(u32::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    struct Case {
        css: String,
        #[serde(default)]
        repeat: Option<usize>,
        limit: Limit,
        problems: Vec<CssProblem>,
    }

    #[derive(Deserialize)]
    struct CaseFile {
        allowed_functions: Vec<String>,
        limits: Limits,
        cases: Vec<Case>,
    }

    /// Every case in the shared rules file, which the web UI's copy of the
    /// check passes too.
    #[test]
    fn every_shared_case_answers_its_problems() {
        let file: CaseFile = serde_json::from_str(RULES).unwrap();
        assert!(!file.cases.is_empty() && !file.allowed_functions.is_empty());
        assert!(file.limits.user > 0 && file.limits.site > file.limits.user);
        let rules = CssRules::compiled();
        let mut wrong = Vec::new();
        for case in file.cases {
            let css = case.css.repeat(case.repeat.unwrap_or(1));
            let found = problems(&css, case.limit, &rules);
            if found != case.problems {
                let shown: String = case.css.chars().take(60).collect();
                wrong.push(format!(
                    "{shown:?} x{:?} ({:?}): wanted {:?}, got {found:?}",
                    case.repeat, case.limit, case.problems
                ));
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }
}
