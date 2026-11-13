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
//! handle in any parameter (a generic bound by a trait other than a
//! conversion or a marker counts as one), and an `Option` of a handle,
//! owned or by reference. A handle is a trait object, an `impl` of
//! anything but a conversion trait, an `Arc`, a `Live` settings handle,
//! the `Platform` or `Db`, or a workspace type whose every definition
//! holds one of those (a closure alone does not make one); type aliases
//! are read as what they name, however their path is written.
//!
//! The few process-wide caches and run-time handles that stay are listed
//! by file (or by file and name, for the second pass, one site per
//! entry), each with its reason, and every listed entry must still match
//! its rule.

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
/// parameter or field), each with its reason. An entry covers one site: a
/// file with two sites of one name lists it twice.
const LATE_ALLOWED: &[(&str, &str, &str)] = &[
    (
        "acquire/src/probes.rs",
        "sabnzbd",
        "one health pass's SABnzbd queue, there only while the section sets one up",
    ),
    (
        "acquire/src/probes.rs",
        "slskd",
        "one health pass's slskd client, there only while the section sets one up",
    ),
    (
        "acquire/src/slskd/client.rs",
        "with_rate_limit_retry",
        "runs one call, retrying 429s; the closure is that call, not a builder",
    ),
    (
        "auth/src/session/stated.rs",
        "config",
        "no address list in upgrade mode, which runs before the settings are open",
    ),
    (
        "catalog/src/artwork/mod.rs",
        "cache",
        "what the cover cache knows, nothing until its index is loaded",
    ),
    (
        "catalog/src/discover/youtube.rs",
        "client",
        "the YouTube client, none while its quota file cannot be read, which keeps search off",
    ),
    (
        "catalog/src/discover/youtube.rs",
        "client",
        "the same client, handed to the constructor",
    ),
    (
        "compat/src/jellyfin/ids/mod.rs",
        "exceptions",
        "the fill loop's exception list, none until its first pass loads it",
    ),
    (
        "compat/src/subsonic/value.rs",
        "stream",
        "a streamed audio body, there only for a stream answer",
    ),
    (
        "kernel/src/db/backup.rs",
        "progress",
        "a caller's progress report for one backup",
    ),
    (
        "kernel/src/db/backup.rs",
        "progress",
        "the same report, passed on to the staging step",
    ),
    (
        "kernel/src/db/backup.rs",
        "progress",
        "the same report, passed on to the page copy",
    ),
    (
        "kernel/src/db/checkpoint.rs",
        "with_analyze",
        "added in the one build after the relief thread's copy is taken, so that thread holds no Db",
    ),
    (
        "kernel/src/db/checkpoint.rs",
        "analyze",
        "the field with_analyze fills; the relief thread's copy has none",
    ),
    (
        "kernel/src/db/writer.rs",
        "connection",
        "the write connection while no turn holds it, taken by each turn and closed at shutdown",
    ),
    (
        "kernel/src/db/writer.rs",
        "runner",
        "the async-write thread's channel, dropped at shutdown so the thread ends",
    ),
    (
        "kernel/src/db/writer.rs",
        "connection",
        "the connection a turn holds, put back when the turn drops",
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
        "library/src/publish/bin_contents.rs",
        "sandbox",
        "the same, for one page of the bin",
    ),
    (
        "library/src/publish/bin_contents.rs",
        "sandbox",
        "the same, for the bin's summary",
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
        "library/src/publish/session.rs",
        "cell",
        "the open publisher, none until a publish opens the session",
    ),
    (
        "library/src/publish/session.rs",
        "session",
        "the open session's sandbox, none once it closes",
    ),
    (
        "library/src/test_support/mod.rs",
        "hook",
        "the setter of a test's hook between two publish steps",
    ),
    (
        "library/src/test_support/mod.rs",
        "hook",
        "the setter of a test's other hook between two publish steps",
    ),
    (
        "library/src/test_support/mod.rs",
        "before_place",
        "a test's hook, armed on the open publisher; tests only",
    ),
    (
        "library/src/test_support/mod.rs",
        "before_set_aside",
        "a test's hook, armed on the open publisher; tests only",
    ),
    (
        "plugins/src/host.rs",
        "runtime",
        "a plugin's running connection, there only while it is enabled",
    ),
    (
        "providers/src/cache.rs",
        "outcome",
        "the answer a single-flight lookup hands its waiters, none if it panicked; not a port",
    ),
    (
        "src/app.rs",
        "state",
        "the API document is built from the routes with no app state behind them",
    ),
    (
        "src/bootstrap/jobs.rs",
        "scripted",
        "the scripted test job kind, in test builds only",
    ),
    (
        "src/security_headers.rs",
        "settings",
        "no settings in upgrade mode, which serves the fixed policy before the settings are open",
    ),
    (
        "src/settings/library_policy_service.rs",
        "with_saved",
        "runs one closure on the blocking pool; not a builder",
    ),
    (
        "src/upgrade_mode/mod.rs",
        "mode",
        "a run's place, taken when the run records how it ended and given back on drop otherwise",
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
    let wrong = allowed_once(late_bindings(&workspace_sources()));
    assert!(
        wrong.is_empty(),
        "take the handle as a constructor argument instead, or list a run-time choice with its reason:\n{}",
        wrong.join("\n")
    );
}

/// What is wrong with `found` against [`LATE_ALLOWED`]: a site no entry
/// lists, a site past the number of entries under its file and name, and
/// an entry that no longer matches.
fn allowed_once(found: Vec<Late>) -> Vec<String> {
    let mut used = vec![false; LATE_ALLOWED.len()];
    let mut wrong = Vec::new();
    for late in found {
        let names = |(path, name, _): &(&str, &str, &str)| late.path == *path && late.name == *name;
        match (0..LATE_ALLOWED.len()).find(|&at| !used[at] && names(&LATE_ALLOWED[at])) {
            Some(at) => used[at] = true,
            None if LATE_ALLOWED.iter().any(names) => wrong.push(format!(
                "{}:{}: {} ({}); the entry for {} covers one site, and this is another",
                late.path, late.line, late.name, late.what, late.name
            )),
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
    wrong
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
pub struct Folders { store: Box<dyn FolderStore> }
pub struct Hook { run: Box<dyn FnMut(u32)> }
pub struct Gateway { library: Option<CoverWarm>, hook: Option<Hook>, limits: Option<Limits> }
pub struct Stop { flag: Arc<AtomicBool> }
pub struct Guarded { stop: Arc<AtomicBool>, port: Arc<dyn SearchPort> }
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
    pub fn with_nested<T: Into<Vec<u8>>>(mut self, warm: CoverWarm) -> Self { self }
    pub fn with_hooked<F: Fn(&Path) -> bool>(mut self, warm: CoverWarm) -> Self { self }
    pub fn with_port<P: Send + SearchPort>(mut self, port: P) -> Self { self }
    pub fn with_where<P>(mut self, port: P) -> Self where P: SearchPort { self }
    pub fn with_label<S: Into<String> + Send>(mut self, label: S) -> Self { self }
    pub fn with_folders(mut self, folders: Folders) -> Self { self }
    pub fn with_hook(mut self, hook: Hook) -> Self { self }
}
fn enrich(enrichment: Option<&ProductionEnrichment>, settings: Option<&Live<Search>>) {}
fn place(name: Option<&str>, hook: Option<&dyn Fn(&Path)>, limits: Option<&Limits>) {}
fn report(status: Option<&Status>, reader: Option<&Reader>) {}
fn wake(wake_up: Option<crate::effects::DownloadWake>, every: Option<Duration>) {}
fn bound(stop: Option<Stop>, permits: Option<Arc<Semaphore>>, left: Option<Arc<AtomicUsize>>) {}
fn guard(guarded: Option<Guarded>, budget: Option<(Instant, Arc<AtomicBool>)>) {}
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
            "guarded",
            "library",
            "reader",
            "settings",
            "wake",
            "wake_up",
            "with_cover_warm",
            "with_download_wake",
            "with_folders",
            "with_hooked",
            "with_job",
            "with_lookup",
            "with_media_refresh",
            "with_nested",
            "with_port",
            "with_store",
            "with_where",
        ]
    );
}

/// An entry of the allowlist covers the one site it was written for: a
/// second site of the same name in the same file is not allowed by it.
#[test]
fn an_entry_allows_one_site() {
    let (path, name, _) = LATE_ALLOWED[0];
    let site = |line| Late {
        path: path.to_owned(),
        line,
        name: name.to_owned(),
        what: "bait".to_owned(),
    };
    let wrong = allowed_once(vec![site(1), site(2)]);
    let second: Vec<&String> = wrong
        .iter()
        .filter(|message| message.contains("covers one site"))
        .collect();
    assert_eq!(second.len(), 1, "{wrong:?}");
    assert!(second[0].starts_with(&format!("{path}:2:")), "{second:?}");
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
        kinds.optionals(path, code, &mut found);
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

/// Marker and value traits a generic's bound may name, beside the
/// conversions, and still leave its type a plain value.
const MARKERS: &[&str] = &[
    "Send",
    "Sync",
    "Sized",
    "Unpin",
    "Clone",
    "Copy",
    "Debug",
    "Default",
    "PartialEq",
    "Eq",
    "PartialOrd",
    "Ord",
    "Hash",
];

/// The closure traits: a type that holds only a closure holds a run-time
/// choice, not a port.
const CLOSURES: &[&str] = &["Fn", "FnMut", "FnOnce"];

/// What an `Arc` may hold and still be plain data: text, paths, JSON, a
/// shared flag or counter, and a cap on calls in flight.
const SHARED_DATA: &[&str] = &[
    "str",
    "[",
    "Path",
    "OsStr",
    "Value",
    "serde_json::Value",
    "AtomicBool",
    "AtomicUsize",
    "AtomicU64",
    "Semaphore",
];

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
    /// The trait a `dyn` names.
    dyn_trait: Regex,
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
            dyn_trait: Regex::new(r"\bdyn\s+(?:\w+::)*(\w+)").unwrap(),
            held_marker: Regex::new(
                r"\bLive<|\bDb\b|\bPlatform\b|\breqwest::Client\b|\b(?:Mutex|RwLock)<|\bwatch::|\bmpsc::",
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

    /// Whether a type body holds a handle: a trait object other than a
    /// closure, a marker, a shared `Arc`, or a type already known to be
    /// one.
    fn holds_handle(&self, body: &str) -> bool {
        self.dyn_trait
            .captures_iter(body)
            .any(|named| !CLOSURES.contains(&&named[1]))
            || self.held_marker.is_match(body)
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

    /// `with_*` methods that take a handle in any parameter. A generic
    /// bound by a trait other than a conversion or a marker, in the
    /// generics or a `where` clause, is a handle; other generics are plain
    /// values.
    fn with_methods(&self, path: &str, code: &str, found: &mut Vec<Late>) {
        let method = Regex::new(r"\bfn\s+(with_\w*)\s*").unwrap();
        let receiver = Regex::new(r"^(?:&\s*(?:'\w+\s+)?)?(?:mut\s+)?self$").unwrap();
        for at in method.captures_iter(code) {
            let mut open = at.get(0).unwrap().end();
            let mut bounds = String::new();
            if code[open..].starts_with('<') {
                let Some(close) = closing(code, open) else {
                    continue;
                };
                bounds = code[open + 1..close].to_owned();
                let rest = &code[close + 1..];
                open = close + 1 + rest.len() - rest.trim_start().len();
            }
            if !code[open..].starts_with('(') {
                continue;
            }
            let Some(close) = closing(code, open) else {
                continue;
            };
            let params = split_top(&code[open + 1..close]);
            if !params.first().is_some_and(|first| receiver.is_match(first)) {
                continue;
            }
            if let Some(clause) = where_clause(&code[close + 1..]) {
                bounds = format!("{bounds}, {clause}");
            }
            let (plain, bound) = generics(&bounds);
            if let Some(why) = params[1..].iter().find_map(|param| {
                let (_, ty) = param.split_once(':')?;
                bound
                    .iter()
                    .find(|(name, _)| self.word.find_iter(ty).any(|word| word.as_str() == name))
                    .map(|(name, by)| format!("{name}, a generic bound by {by}"))
                    .or_else(|| self.handle(ty, &plain))
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

    /// Parameters, fields and bindings whose `Option` holds a handle, owned
    /// or by reference, aliases read as what they name. An optional
    /// closure is a run-time choice, not a port.
    fn optionals(&self, path: &str, code: &str, found: &mut Vec<Late>) {
        let optional = Regex::new(r"\b(\w+)\s*:\s*Option\s*<").unwrap();
        let reference = Regex::new(r"^&\s*(?:'\w+\s+)?(?:mut\s+)?").unwrap();
        let closure = Regex::new(r"^dyn\s+Fn(?:Mut|Once)?\b").unwrap();
        for at in optional.captures_iter(code) {
            let open = at.get(0).unwrap().end() - 1;
            let Some(close) = closing(code, open) else {
                continue;
            };
            let inner = code[open + 1..close].trim();
            let (held, ty) = match reference.find(inner) {
                Some(reference) => ("an Option<&..>", &inner[reference.end()..]),
                None => ("an Option", inner),
            };
            if closure.is_match(ty) {
                continue;
            }
            if let Some(why) = self.handle(ty, &[]) {
                found.push(Late {
                    path: path.to_owned(),
                    line: line_of(code, open),
                    name: at[1].to_owned(),
                    what: format!("{held} of {why}, which a bundle may leave empty"),
                });
            }
        }
    }
}

/// The `where` clause of the signature whose parameters end at the start
/// of `rest`, if it has one.
fn where_clause(rest: &str) -> Option<&str> {
    let head = &rest[..rest.find(['{', ';']).unwrap_or(rest.len())];
    let at = Regex::new(r"\bwhere\b").unwrap().find(head)?;
    Some(&head[at.end()..])
}

/// A function's type parameters, from its generics and `where` clause:
/// the plain ones, and each one bound by a trait that is neither a
/// conversion nor a marker, with that trait.
fn generics(list: &str) -> (Vec<String>, Vec<(String, String)>) {
    let parameter = Regex::new(r"^(?:const\s+)?([A-Z]\w*)\s*(?::([\s\S]*))?$").unwrap();
    let named = Regex::new(r"^\??\s*(?:\w+::)*(\w+)").unwrap();
    let mut plain = Vec::new();
    let mut bound: Vec<(String, String)> = Vec::new();
    for generic in split_top(list) {
        let Some(at) = parameter.captures(&generic) else {
            continue;
        };
        let port = at.get(2).and_then(|bounds| {
            split_at_top(bounds.as_str(), '+').iter().find_map(|each| {
                let trait_name = named.captures(each)?[1].to_owned();
                let plain = CONVERSIONS.contains(&trait_name.as_str())
                    || MARKERS.contains(&trait_name.as_str());
                (!plain).then_some(trait_name)
            })
        });
        match port {
            Some(port) => bound.push((at[1].to_owned(), port)),
            None => plain.push(at[1].to_owned()),
        }
    }
    plain.retain(|name| !bound.iter().any(|(known, _)| known == name));
    (plain, bound)
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
    split_at_top(list, ',')
}

/// `list` split at each top-level `separator`, each part trimmed.
fn split_at_top(list: &str, separator: char) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut part = String::new();
    let mut previous = ' ';
    for c in list.chars() {
        match c {
            '(' | '[' | '{' | '<' => depth += 1,
            '>' if previous == '-' => {}
            ')' | ']' | '}' | '>' => depth = depth.saturating_sub(1),
            c if c == separator && depth == 0 => {
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
