//! Nothing is wired after construction. Each area is built by one function
//! from `&Platform` and the ports it uses, and a port another area provides
//! is a constructor argument, never set on a built bundle. This gate reads
//! the shipping source of every workspace crate as text, comment lines and
//! unit test modules aside, and fails on the shapes a late binding takes: a
//! `OnceLock` slot, a `*Slot` type, a handle behind a lock waiting to be
//! filled, a `set_*` or `attach_*` method that takes a shared handle
//! through `&self`, a static that holds state, the warning a slot logs when
//! it is filled twice, and a port held or passed as an `Option` a test
//! bundle leaves empty.
//!
//! A second pass reads signatures whole: a `with_*` method that takes a
//! handle in any parameter, an `Option<&..>` of a handle, and an `Option`
//! of a type alias that names a port. A handle is a trait object, an
//! `impl` of anything but a conversion trait, an `Arc`, a `Live` settings
//! handle, the `Platform` or `Db`, or a workspace type whose every
//! definition holds one of those; type aliases are read as what they name.
//!
//! The few process-wide caches and run-time handles that stay are listed
//! by file (or by file and name, for the second pass), each with its
//! reason, and every listed entry must still match its rule.

use std::collections::{HashMap, HashSet};

use regex::Regex;

use crate::common::sources::{Source, workspace_sources};

struct Rule {
    what: &'static str,
    patterns: &'static [&'static str],
    allowed: &'static [(&'static str, &'static str)],
}

const TEMPLATE: (&str, &str) = (
    "kernel/src/test_support/template.rs",
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
        allowed: &[],
    },
    Rule {
        what: "a port held or passed as an Option a bundle may leave empty",
        patterns: &[
            r"\bOption<\s*(?:(?:std::)?sync::)?(?:Arc|Box)<\s*dyn\b",
            r"\bOption<\s*(?:\w+::)*\w*Port\s*>",
            r"\bOption<\s*(?:(?:std::)?sync::)?Arc<\s*(?:\w+::)*(?:PluginHost|LandingService|Targets|PersonalMixBuilder)\s*>",
        ],
        allowed: &[
            (
                "plugins/src/host.rs",
                "a plugin's running connection, opened when it starts and dropped when it stops",
            ),
            (
                "providers/src/cache.rs",
                "the answer a single-flight lookup hands its waiters, not a port",
            ),
        ],
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

/// Sites the second pass allows, by file and name (the method, or the
/// parameter or field), each with its reason.
const LATE_ALLOWED: &[(&str, &str, &str)] = &[
    (
        "acquire/src/usenet/sabnzbd.rs",
        "grab",
        "the release's own indexer, there only for a release found through one",
    ),
    (
        "catalog/src/artwork/mod.rs",
        "cache",
        "what the cover cache knows, nothing until its index is loaded",
    ),
    (
        "catalog/src/local/edition_tracks.rs",
        "releases",
        "the library reads' releases, which their test rigs leave out; to be made required",
    ),
    (
        "catalog/src/local/stores.rs",
        "policy",
        "the library reads' download policy, which their test rigs leave out; to be made required",
    ),
    (
        "catalog/src/search/service.rs",
        "with_remote",
        "MusicBrainz search, which the local search tests leave out; to be made required",
    ),
    (
        "kernel/src/db/backup.rs",
        "progress",
        "a caller's progress report for one backup",
    ),
    (
        "kernel/src/db/checkpoint.rs",
        "with_analyze",
        "added in the one build after the relief thread's copy is taken, so that thread holds no Db",
    ),
    (
        "kernel/src/db/read_pool.rs",
        "cancel",
        "a read's own cancel flag, for reads a caller can stop",
    ),
    (
        "kernel/src/jobs/precache.rs",
        "on_end",
        "what one run hands its outcome to, spent when it lands",
    ),
    (
        "library/src/publish/bin_contents.rs",
        "sandbox",
        "the bins as one listing can tell them, none while they cannot be told",
    ),
    (
        "library/src/publish/publisher.rs",
        "before_place",
        "a test's hook between two publish steps; production never arms it",
    ),
    (
        "library/src/publish/publisher.rs",
        "before_set_aside",
        "a test's hook between two publish steps; production never arms it",
    ),
    (
        "library/src/publish/publisher.rs",
        "hook",
        "the setter of a test's hook between two publish steps",
    ),
    (
        "library/src/publish/step.rs",
        "add_budget",
        "one publish's deadline for an added file",
    ),
    (
        "media/src/stream/gateway.rs",
        "with_settings",
        "the stream gateway's live settings, which its test rigs leave out; to be made required",
    ),
    (
        "media/src/stream/gateway.rs",
        "with_library",
        "the stream gateway's library reads, which its test rigs leave out; to be made required",
    ),
    (
        "media/src/stream/gateway.rs",
        "with_plugins",
        "the stream gateway's plugin sources, which its test rigs leave out; to be made required",
    ),
    (
        "src/app.rs",
        "state",
        "the API document is built from the routes with no app state behind them",
    ),
];

/// One late binding the second pass found.
struct Late {
    path: String,
    line: usize,
    name: String,
    what: String,
}

#[test]
fn no_handle_is_attached_or_left_optional() {
    let found = late_bindings(&workspace_sources());
    let mut used = vec![false; LATE_ALLOWED.len()];
    let mut wrong = Vec::new();
    for late in found {
        match LATE_ALLOWED
            .iter()
            .position(|(path, name, _)| late.path == *path && late.name == *name)
        {
            Some(at) => used[at] = true,
            None => wrong.push(format!(
                "{}:{}: {} ({})",
                late.path, late.line, late.name, late.what
            )),
        }
    }
    for ((path, name, why), used) in LATE_ALLOWED.iter().zip(used) {
        if !used {
            wrong.push(format!(
                "{path}: {name} is listed, but is no longer a late binding; remove the entry ({why})"
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "take the handle as a constructor argument instead, or list a run-time choice with its reason:\n{}",
        wrong.join("\n")
    );
}

/// Each shape the second pass exists for is found in a bait source, and
/// plain builders and run-time choices are not.
#[test]
fn the_second_pass_sees_each_shape() {
    let bait = Source {
        path: "bait/src/lib.rs".to_owned(),
        text: r#"
pub type DownloadWake = Arc<dyn Fn() + Send + Sync>;
pub struct CoverWarm { db: Db, queue: JobQueue }
pub struct ProductionEnrichment { search: Arc<dyn SearchCounts> }
pub struct Limits { per_minute: u32, names: Arc<str> }
pub struct Reader { files: CoverWarm }
pub enum Status { CoverWarm, Idle(u32) }
pub struct Effects { wake: Option<DownloadWake> }
impl Builder {
    pub fn with_cover_warm(mut self, warm: CoverWarm) -> Self { self }
    pub fn with_job(mut self, spec: JobSpec, handler: Arc<dyn JobHandler>) -> Self { self }
    pub fn with_media_refresh(mut self, platform: &Platform, every: Duration) -> Self { self }
    pub fn with_store(&mut self, warm: CoverWarm) {}
    pub fn with_download_wake(mut self, wake: DownloadWake) -> Self { self }
    pub fn with_lookup(mut self, lookup: impl AlbumLookup) -> Self { self }
    pub fn with_name(mut self, name: impl Into<String>) -> Self { self }
    pub fn with_jitter(mut self, jitter: Duration) -> Self { self }
    pub fn with_limits(mut self, limits: Limits) -> Self { self }
}
fn enrich(enrichment: Option<&ProductionEnrichment>, settings: Option<&Live<Search>>) {}
fn place(name: Option<&str>, hook: Option<&dyn Fn(&Path)>, limits: Option<&Limits>) {}
fn report(status: Option<&Status>, reader: Option<&Reader>) {}
"#
        .to_owned(),
    };
    let mut found: Vec<String> = late_bindings(&[bait])
        .into_iter()
        .map(|late| late.name)
        .collect();
    found.sort();
    assert_eq!(
        found,
        [
            "enrichment",
            "reader",
            "settings",
            "wake",
            "with_cover_warm",
            "with_download_wake",
            "with_job",
            "with_lookup",
            "with_media_refresh",
            "with_store",
        ]
    );
}

/// Every late binding in `sources`.
fn late_bindings(sources: &[Source]) -> Vec<Late> {
    let codes: Vec<(&str, String)> = sources
        .iter()
        .map(|source| (source.path.as_str(), source.shipping_code()))
        .collect();
    let kinds = Kinds::new(&codes);
    let mut found = Vec::new();
    for (path, code) in &codes {
        kinds.with_methods(path, code, &mut found);
        kinds.optional_refs(path, code, &mut found);
        kinds.optional_aliases(path, code, &mut found);
    }
    found
}

/// Traits an `impl` parameter may name and still be a plain value.
const CONVERSIONS: &[&str] = &[
    "Into",
    "AsRef",
    "IntoIterator",
    "Iterator",
    "ToString",
    "Display",
    "Borrow",
    "ToOwned",
];

/// What an `Arc` may hold and still be plain data.
const SHARED_DATA: &[&str] = &["str", "[", "Path", "OsStr", "Value", "serde_json::Value"];

/// What the second pass knows about the workspace's types.
struct Kinds {
    /// Each non-generic type alias, by name; a name defined two ways is
    /// left out.
    aliases: HashMap<String, String>,
    /// Workspace types every definition of which holds a handle.
    handles: HashSet<String>,
    word: Regex,
    arc: Regex,
    dyn_object: Regex,
    impl_trait: Regex,
    held_marker: Regex,
}

impl Kinds {
    fn new(codes: &[(&str, String)]) -> Self {
        let word = Regex::new(r"\b[A-Z]\w*").unwrap();
        let arc = Regex::new(r"\bArc<\s*(?:(?:std::)?sync::)?").unwrap();
        let alias =
            Regex::new(r"(?m)^(?:pub(?:\([^)]*\))?\s+)?type\s+(\w+)\s*=\s*([^;]+);").unwrap();
        let mut aliases = HashMap::new();
        let mut ambiguous = HashSet::new();
        for (_, code) in codes {
            for found in alias.captures_iter(code) {
                let definition = found[2].split_whitespace().collect::<Vec<_>>().join(" ");
                let name = found[1].to_owned();
                if aliases.get(&name).is_some_and(|known| *known != definition) {
                    ambiguous.insert(name.clone());
                }
                aliases.insert(name, definition);
            }
        }
        for name in ambiguous {
            aliases.remove(&name);
        }
        let mut kinds = Self {
            aliases,
            handles: HashSet::new(),
            word,
            arc,
            dyn_object: Regex::new(r"\bdyn\b").unwrap(),
            impl_trait: Regex::new(r"\bimpl\s+(?:\w+::)*(\w+)").unwrap(),
            held_marker: Regex::new(
                r"\bdyn\s+[^F\s]|\bLive<|\bDb\b|\bPlatform\b|\breqwest::Client\b|\b(?:Mutex|RwLock)<|\bwatch::|\bmpsc::",
            )
            .unwrap(),
        };
        let definition = Regex::new(r"\b(struct|enum)\s+(\w+)").unwrap();
        let variant = Regex::new(r"^(?:#\[[^\]]*\]\s*)*\w+").unwrap();
        let mut bodies: HashMap<String, Vec<String>> = HashMap::new();
        for (_, code) in codes {
            for found in definition.captures_iter(code) {
                let body = type_body(code, found.get(0).unwrap().end());
                // A variant's name is not a type it holds.
                let body = if &found[1] == "enum" {
                    split_top(body)
                        .iter()
                        .map(|item| variant.replace(item, "").into_owned())
                        .collect::<Vec<_>>()
                        .join(", ")
                } else {
                    body.to_owned()
                };
                bodies.entry(found[2].to_owned()).or_default().push(body);
            }
        }
        // A type holding a handle is one too: grow the set until it stops.
        loop {
            let grown: HashSet<String> = bodies
                .iter()
                .filter(|(_, bodies)| bodies.iter().all(|body| kinds.holds_handle(body)))
                .map(|(name, _)| name.clone())
                .collect();
            if grown.len() == kinds.handles.len() {
                break;
            }
            kinds.handles = grown;
        }
        kinds
    }

    /// Whether a type body holds a handle: a marker, a shared `Arc`, or a
    /// type already known to be one.
    fn holds_handle(&self, body: &str) -> bool {
        self.held_marker.is_match(body)
            || self.shares(body)
            || self
                .word
                .find_iter(body)
                .any(|word| self.handles.contains(word.as_str()))
    }

    /// Whether `text` holds an `Arc` of anything but plain data.
    fn shares(&self, text: &str) -> bool {
        self.arc.find_iter(text).any(|arc| {
            let inner = text[arc.end()..].trim_start();
            !SHARED_DATA.iter().any(|data| inner.starts_with(data))
        })
    }

    /// `ty` with every alias in it replaced by what it names.
    fn resolve(&self, ty: &str) -> String {
        let mut ty = ty.to_owned();
        for _ in 0..4 {
            let next = self
                .word
                .replace_all(&ty, |word: &regex::Captures| {
                    self.aliases
                        .get(&word[0])
                        .cloned()
                        .unwrap_or_else(|| word[0].to_owned())
                })
                .into_owned();
            if next == ty {
                break;
            }
            ty = next;
        }
        ty
    }

    /// Why `ty` is a handle, or `None` for a plain value. Names in
    /// `generics` are the function's own type parameters.
    fn handle(&self, ty: &str, generics: &[String]) -> Option<String> {
        let ty = self.resolve(ty);
        if self.dyn_object.is_match(&ty) {
            return Some("a trait object".to_owned());
        }
        if let Some(named) = self
            .impl_trait
            .captures_iter(&ty)
            .find(|named| !CONVERSIONS.contains(&&named[1]))
        {
            return Some(format!("an impl {}", &named[1]));
        }
        if self.shares(&ty) {
            return Some("a shared Arc".to_owned());
        }
        self.word
            .find_iter(&ty)
            .map(|word| word.as_str())
            .filter(|word| !generics.iter().any(|generic| generic == word))
            .find(|word| ["Live", "Platform", "Db"].contains(word) || self.handles.contains(*word))
            .map(|word| format!("the handle {word}"))
    }

    /// `with_*` methods that take a handle in any parameter.
    fn with_methods(&self, path: &str, code: &str, found: &mut Vec<Late>) {
        let method = Regex::new(r"\bfn\s+(with_\w*)\s*(<[^>(]*>)?\s*\(").unwrap();
        let receiver = Regex::new(r"^(?:&\s*(?:'\w+\s+)?)?(?:mut\s+)?self$").unwrap();
        for at in method.captures_iter(code) {
            let open = at.get(0).unwrap().end() - 1;
            let Some(close) = closing(code, open) else {
                continue;
            };
            let params = split_top(&code[open + 1..close]);
            if !params.first().is_some_and(|first| receiver.is_match(first)) {
                continue;
            }
            let generics: Vec<String> = at.get(2).map_or_else(Vec::new, |list| {
                self.word
                    .find_iter(list.as_str())
                    .map(|word| word.as_str().to_owned())
                    .collect()
            });
            if let Some(why) = params[1..].iter().find_map(|param| {
                let (_, ty) = param.split_once(':')?;
                self.handle(ty, &generics)
            }) {
                found.push(Late {
                    path: path.to_owned(),
                    line: line_of(code, open),
                    name: at[1].to_owned(),
                    what: format!("a with_ method that takes {why} after the build"),
                });
            }
        }
    }

    /// Parameters and fields of type `Option<&..>` of a handle. An
    /// optional closure is a run-time choice, not a port.
    fn optional_refs(&self, path: &str, code: &str, found: &mut Vec<Late>) {
        let optional = Regex::new(r"\b(\w+)\s*:\s*Option\s*<").unwrap();
        let reference = Regex::new(r"^&\s*(?:'\w+\s+)?(?:mut\s+)?").unwrap();
        let closure = Regex::new(r"^dyn\s+Fn(?:Mut|Once)?\b").unwrap();
        for at in optional.captures_iter(code) {
            let open = at.get(0).unwrap().end() - 1;
            let Some(close) = closing(code, open) else {
                continue;
            };
            let inner = code[open + 1..close].trim();
            let Some(reference) = reference.find(inner) else {
                continue;
            };
            let ty = &inner[reference.end()..];
            if closure.is_match(ty) {
                continue;
            }
            if let Some(why) = self.handle(ty, &[]) {
                found.push(Late {
                    path: path.to_owned(),
                    line: line_of(code, open),
                    name: at[1].to_owned(),
                    what: format!("an Option<&..> of {why}, which a bundle may leave empty"),
                });
            }
        }
    }

    /// Parameters and fields whose `Option` holds a type alias that names a
    /// port, which the text rules cannot see through.
    fn optional_aliases(&self, path: &str, code: &str, found: &mut Vec<Late>) {
        let optional = Regex::new(r"\b(\w+)\s*:\s*Option<\s*(\w+)\s*>").unwrap();
        let port = Regex::new(r"^(?:(?:std::)?sync::)?(?:Arc|Box)<\s*dyn\b").unwrap();
        for at in optional.captures_iter(code) {
            if self.aliases.contains_key(&at[2]) && port.is_match(&self.resolve(&at[2])) {
                found.push(Late {
                    path: path.to_owned(),
                    line: line_of(code, at.get(0).unwrap().start()),
                    name: at[1].to_owned(),
                    what: format!("an Option of {}, an alias of a port", &at[2]),
                });
            }
        }
    }
}

/// The body of the type whose name ends at `from`: what its braces or
/// parentheses hold, or nothing for a unit type.
fn type_body(code: &str, from: usize) -> &str {
    let mut angle = 0usize;
    for (offset, c) in code[from..].char_indices() {
        match c {
            '<' => angle += 1,
            '>' => angle = angle.saturating_sub(1),
            ';' if angle == 0 => return "",
            '{' | '(' if angle == 0 => {
                let open = from + offset;
                return closing(code, open).map_or("", |close| &code[open + 1..close]);
            }
            _ => {}
        }
    }
    ""
}

/// Where the bracket opened at `open` closes. The `>` of an `->` or `=>`
/// closes nothing.
fn closing(code: &str, open: usize) -> Option<usize> {
    let bytes = code.as_bytes();
    let mut depth = 0usize;
    for at in open..bytes.len() {
        match bytes[at] {
            b'(' | b'[' | b'{' | b'<' => depth += 1,
            b'>' if at > 0 && matches!(bytes[at - 1], b'-' | b'=') => {}
            b')' | b']' | b'}' | b'>' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(at);
                }
            }
            _ => {}
        }
    }
    None
}

/// `list` split at its top-level commas, each part trimmed.
fn split_top(list: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut part = String::new();
    let mut previous = ' ';
    for c in list.chars() {
        match c {
            '(' | '[' | '{' | '<' => depth += 1,
            '>' if previous == '-' => {}
            ')' | ']' | '}' | '>' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(part.trim().to_owned());
                part.clear();
                previous = c;
                continue;
            }
            _ => {}
        }
        part.push(c);
        previous = c;
    }
    if !part.trim().is_empty() {
        parts.push(part.trim().to_owned());
    }
    parts
}

fn line_of(code: &str, at: usize) -> usize {
    code[..at].matches('\n').count() + 1
}
