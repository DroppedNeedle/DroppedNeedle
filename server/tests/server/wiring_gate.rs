//! Nothing is wired after construction. Each area is built by one function
//! from `&Platform` and the ports it uses, and a port another area provides
//! is a constructor argument, never set on a built bundle. This gate reads
//! the shipping source of every workspace crate as text, comment lines and
//! unit test modules aside, and fails on the shapes a late binding takes: a
//! `OnceLock` slot, a `*Slot` type, a handle behind a lock waiting to be
//! filled, a `set_*` or `attach_*` method that takes a shared handle
//! through `&self`, a static that holds state, and the warning a slot
//! logs when it is filled twice.
//!
//! The few process-wide caches that stay are listed by file, each with its
//! reason, and every listed file must still match its rule.

use regex::Regex;

use crate::common::sources::workspace_sources;

struct Rule {
    what: &'static str,
    patterns: &'static [&'static str],
    allowed: &'static [(&'static str, &'static str)],
}

const TEMPLATE: (&str, &str) = (
    "kernel/src/schema/template.rs",
    "the migrated test template, built once per test process",
);
const OBSERVABILITY: (&str, &str) = (
    "kernel/src/observability.rs",
    "the tracing subscriber, installed once per process",
);
const PROBE: (&str, &str) = (
    "library/src/tags/probe.rs",
    "the audio codec registry, built once and never changed",
);
const GENRES: (&str, &str) = (
    "library/src/management/genre.rs",
    "the fixed genre vocabulary, parsed once",
);

const RULES: &[Rule] = &[
    Rule {
        what: "a OnceLock slot",
        patterns: &[
            r"\b(?:OnceLock|OnceCell)\s*<",
            r"\b(?:OnceLock|OnceCell)::(?:new|const_new|new_with)\b",
            r"\.get_or_init\(",
            r"(?s)\.set\([^;]*?\)\s*\.is_(?:err|ok)\(\)",
        ],
        allowed: &[
            TEMPLATE,
            OBSERVABILITY,
            PROBE,
            GENRES,
            (
                "kernel/src/runtime_config/store.rs",
                "a settings entry's decrypted value, cached for that one revision; a write replaces the entry whole",
            ),
        ],
    },
    Rule {
        what: "a static that holds state",
        patterns: &[
            r"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?static\s+\w+\s*:[^=]*\b(?:OnceLock|LazyLock|OnceCell|Mutex|RwLock)\b",
        ],
        allowed: &[
            TEMPLATE,
            OBSERVABILITY,
            PROBE,
            GENRES,
            (
                "library/src/tags/loudness.rs",
                "measured loudness by file content hash, a bounded cache of results that never change",
            ),
            (
                "library/src/tags/picard.rs",
                "a compiled regular expression",
            ),
        ],
    },
    Rule {
        what: "a handle held behind a lock to be filled later",
        patterns: &[r"(?:Mutex|RwLock)<\s*Option<\s*(?:\w+::)*(?:Arc|Box)<"],
        allowed: &[(
            "plugins/src/process.rs",
            "a running plugin's session, which the process opens and drops at run time; not a port",
        )],
    },
    Rule {
        what: "a slot type",
        patterns: &[r"\btype\s+\w+Slot\b"],
        allowed: &[],
    },
    Rule {
        what: "a setter that takes a shared handle after construction",
        patterns: &[
            r"(?s)\bfn\s+(?:set|attach)\w*\s*(?:<[^>]*>)?\s*\(\s*&self\s*,\s*\w+\s*:\s*(?:Option<\s*)?(?:(?:\w+::)*Arc<|Box<\s*dyn\b|&\s*(?:'\w+\s+)?dyn\b|impl\b)",
        ],
        allowed: &[],
    },
    Rule {
        what: "a slot's filled-twice warning",
        patterns: &[r"already (?:attached|set)\b|attached twice"],
        allowed: &[(
            "tool/src/import/pipeline.rs",
            "an import note that a user's recovery code was already in place, not wiring",
        )],
    },
];

#[test]
fn nothing_is_wired_after_construction() {
    let sources = workspace_sources();
    let mut wrong = Vec::new();
    for rule in RULES {
        let patterns: Vec<Regex> = rule
            .patterns
            .iter()
            .map(|pattern| Regex::new(pattern).unwrap())
            .collect();
        let mut used = vec![false; rule.allowed.len()];
        for source in &sources {
            let code = source.shipping_code();
            for pattern in &patterns {
                for found in pattern.find_iter(&code) {
                    match rule
                        .allowed
                        .iter()
                        .position(|(path, _)| source.path == *path)
                    {
                        Some(at) => used[at] = true,
                        None => {
                            let line = code[..found.start()].matches('\n').count() + 1;
                            let text = found.as_str().split_whitespace().collect::<Vec<_>>();
                            wrong.push(format!(
                                "{}:{line}: {} ({})",
                                source.path,
                                text.join(" "),
                                rule.what
                            ));
                        }
                    }
                }
            }
        }
        for ((path, why), used) in rule.allowed.iter().zip(used) {
            if !used {
                wrong.push(format!(
                    "{path}: listed for {}, but nothing there is one; remove the entry ({why})",
                    rule.what
                ));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "hand the port to the constructor instead, or list a process-wide cache with its reason:\n{}",
        wrong.join("\n")
    );
}
