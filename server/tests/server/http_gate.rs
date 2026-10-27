//! The one way in for errors and extractors. Every native error answers
//! through `ApiError`, bodies through the kernel's two extractors, and
//! nothing outside the kernel and the request boundary builds an envelope
//! or an error id. These scans fail the day an old way comes back.

use regex::Regex;

use crate::common::sources::{Source, workspace_sources};

/// One kind of code that may live only in the listed places.
struct Rule {
    what: &'static str,
    /// The folder under `server/` the rule covers; every source when empty.
    under: &'static str,
    pattern: &'static str,
    /// (file or folder under `server/`, reason it may match).
    allowed: &'static [(&'static str, &'static str)],
}

const RULES: &[Rule] = &[
    Rule {
        what: "an IntoResponse impl (only ApiError answers errors)",
        under: "",
        pattern: r"impl\s*(?:<[^>]*>)?\s*(?:[\w:]+::)?IntoResponse\s+for\s+(\w+)",
        allowed: &[(
            "kernel/src/error.rs",
            "ApiError, the one error type that becomes a response",
        )],
    },
    Rule {
        what: "a ValidJson or ValidQuery definition",
        under: "",
        pattern: r"\b(?:struct|enum|type)\s+Valid\w*(?:Json|Query)\b",
        allowed: &[("kernel/src/extract.rs", "the two shared extractors")],
    },
    Rule {
        what: "an error envelope written out",
        under: "",
        pattern: r"\b(?:ErrorBody|ErrorEnvelope)\s*\{",
        allowed: &[("kernel/src/error.rs", "the envelope's home")],
    },
    Rule {
        what: "a raw Query or Json extractor",
        under: "",
        pattern: r"(?:^|[^:]):\s*(?:axum::(?:extract::)?)?(?:Query|Json)\s*<",
        allowed: &[(
            "compat/src/",
            "Subsonic and Jellyfin answer in their own protocol shapes",
        )],
    },
    Rule {
        what: "the literal error_id",
        under: "",
        pattern: r"error_id",
        allowed: &[
            (
                "kernel/src/error.rs",
                "the envelope names the request id as details.error_id",
            ),
            (
                "src/middleware.rs",
                "the request boundary writes every 5xx body",
            ),
        ],
    },
    Rule {
        what: "a memory or fake twin under auth",
        under: "auth/src/",
        pattern: r"\bstruct\s+((?:Memory|Fake)\w*)",
        allowed: &[],
    },
];

/// The file's code with comment lines blanked, so docs may name the rules.
fn code_of(source: &Source) -> String {
    source
        .text
        .lines()
        .map(|line| {
            if line.trim_start().starts_with("//") {
                ""
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn errors_and_extractors_have_one_way_in() {
    let sources = workspace_sources();
    assert!(sources.len() > 100, "the scan found the sources");
    let mut found = Vec::new();
    for rule in RULES {
        let pattern = Regex::new(rule.pattern).unwrap();
        for source in &sources {
            let relative = source.path.as_str();
            if !relative.starts_with(rule.under) {
                continue;
            }
            let code = code_of(source);
            for hit in pattern.find_iter(&code) {
                let allowed = rule
                    .allowed
                    .iter()
                    .any(|(place, _)| relative.starts_with(place));
                if !allowed {
                    let line = code[..hit.start()].lines().count().max(1);
                    found.push(format!(
                        "{relative}:{line}: {} ({})",
                        rule.what,
                        hit.as_str().trim()
                    ));
                }
            }
        }
    }
    assert!(
        found.is_empty(),
        "outside the one way in:\n{}",
        found.join("\n")
    );

    // The allowlisted files hold only the one thing each is allowed.
    let only = |file: &str, pattern: &str, expected: &[&str]| {
        let source = sources
            .iter()
            .find(|source| source.path == file)
            .expect("the allowlisted file exists");
        let code = code_of(source);
        let names: Vec<String> = Regex::new(pattern)
            .unwrap()
            .captures_iter(&code)
            .map(|caps| caps[1].to_owned())
            .collect();
        assert_eq!(names, expected, "{file}");
    };
    only(
        "kernel/src/error.rs",
        r"impl\s*(?:<[^>]*>)?\s*(?:[\w:]+::)?IntoResponse\s+for\s+(\w+)",
        &["ApiError"],
    );
}
