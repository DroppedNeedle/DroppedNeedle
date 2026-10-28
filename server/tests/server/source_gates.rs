//! One way to the database. Areas are given a `Db`, and the code that opens
//! SQLite, runs transactions by hand or reaches past the writer lane lives
//! in `kernel/src/db/` and the few other places listed here. This gate
//! reads the shipping source of every workspace crate as text, comment
//! lines aside, and fails the day one of those comes back somewhere else.
//!
//! Each allowed place names its reason. A temporary one names the work
//! that empties it, and every place must still match its rule, so that work
//! deletes the entry as it goes.

use regex::Regex;

use crate::common::sources::workspace_sources;

/// A place under `server/`, a file or a folder ending in `/`.
struct Place {
    path: &'static str,
    why: &'static str,
    /// Kept although nothing in it matches yet.
    reserved: bool,
}

const fn place(path: &'static str, why: &'static str) -> Place {
    Place {
        path,
        why,
        reserved: false,
    }
}

struct Rule {
    what: &'static str,
    patterns: &'static [&'static str],
    allowed: &'static [Place],
}

const DB: Place = place(
    "kernel/src/db/",
    "the database runtime: the factory, the lane, the read pools, backup and their briefs",
);
/// The database runtime, for a rule nothing in it breaks today.
const DB_RESERVED: Place = Place {
    reserved: true,
    ..DB
};
const TOOL: Place = place(
    "tool/src/",
    "the offline tool, which runs with the server stopped and owns the file",
);

const OPENS: &[Place] = &[
    DB,
    place(
        "kernel/src/schema/template.rs",
        "the migrated test template, built once per test process",
    ),
    place(
        "tool/src/tooling/",
        "the offline tool's upgrade, fixture and recovery steps",
    ),
    place(
        "tool/src/export/",
        "the offline export reads the v2 database and writes its bundle",
    ),
    place(
        "tool/src/import/pipeline.rs",
        "the offline import runs its rows in one sqlx transaction on the pool the upgrade hands it",
    ),
    place(
        "src/admin/backups.rs",
        "backup and restore checks, read-only: integrity, stamp, tables",
    ),
    place(
        "src/bootstrap/data_dirs.rs",
        "the read-only stamp probe of an existing database before the runtime opens it, and its tests",
    ),
];

const TRANSACTIONS: &[Place] = &[DB, TOOL];

const RULES: &[Rule] = &[
    Rule {
        what: "opens a SQLite connection or pool, or begins a sqlx transaction",
        patterns: &[
            r"Connection::open",
            r"\bopen_connection\b",
            r"SqlitePoolOptions",
            r"SqlitePool::connect",
            r"SqliteConnection::connect",
            r"SqliteConnectOptions[^;]*?\.connect\(",
            r"\.begin\(\)|\.begin_with\(",
        ],
        allowed: OPENS,
    },
    Rule {
        what: "controls a transaction by hand",
        patterns: &[
            r#"(?:r#*)?"\s*(?:BEGIN|COMMIT|END|ROLLBACK|SAVEPOINT|RELEASE)(?:[\s;"]|$)"#,
            r"\btransaction_with_behavior\b",
            r"\.transaction\(\)",
            r"\bunchecked_transaction\b",
            r"\.savepoint\(",
        ],
        allowed: TRANSACTIONS,
    },
    Rule {
        what: "sets a pragma, which would stay on the lane's connection for later writes",
        patterns: &[r"\bpragma_update\b", r"PRAGMA\s+[\w.]+\s*="],
        allowed: &[
            DB,
            TOOL,
            place(
                "src/bootstrap/data_dirs.rs",
                "its tests build a database in WAL mode to probe",
            ),
        ],
    },
    Rule {
        what: "installs a connection hook, which only the lane may set",
        patterns: &[r"\b(?:commit_hook|rollback_hook|progress_handler)\("],
        allowed: &[DB],
    },
    Rule {
        what: "attaches another database or vacuums",
        patterns: &[r#"\b(?:ATTACH|VACUUM)(?:[\s;"]|$)"#],
        allowed: &[DB_RESERVED, TOOL],
    },
    Rule {
        what: "keeps a SQLite connection, where a store or service holds the `Db` instead",
        patterns: &[
            // A named field, bare or inside any wrapper (`Arc<Mutex<..>>`,
            // `Option<..>`, `RefCell<..>`).
            r"^\s*(?:pub(?:\([^)]*\))?\s+)?\w+\s*:\s*(?:[\w:]+<\s*)*(?:rusqlite::)?Connection\s*[>,]",
            // A tuple struct holding one.
            r"\bstruct\s+\w+[^{;]*?\([^;]*?\bConnection\b",
        ],
        allowed: &[DB],
    },
    Rule {
        what: "leaks a value, which can pin the WAL with a read left open",
        patterns: &[r"\bmem::forget\(", r"\bBox::leak\("],
        allowed: &[DB_RESERVED],
    },
    Rule {
        what: "names the bulk class, which waits while the WAL is held",
        patterns: &[r"\bLane::Bulk\b"],
        allowed: &[
            DB,
            place(
                "library/src/scan/",
                "the scan's window commits, inventory pages and failure rows",
            ),
        ],
    },
];

#[test]
fn the_database_is_reached_one_way() {
    let sources = workspace_sources();
    assert!(
        sources
            .iter()
            .any(|source| source.path.starts_with("acquire/src/target/")),
        "the walk reaches every module, `target` included"
    );
    let mut wrong = Vec::new();
    for rule in RULES {
        let patterns: Vec<Regex> = rule
            .patterns
            .iter()
            .map(|pattern| Regex::new(&format!("(?ms){pattern}")).unwrap())
            .collect();
        let mut used = vec![false; rule.allowed.len()];
        for source in &sources {
            let code = source.code();
            for pattern in &patterns {
                for found in pattern.find_iter(&code) {
                    let place = rule
                        .allowed
                        .iter()
                        .position(|place| source.path.starts_with(place.path));
                    match place {
                        Some(at) => used[at] = true,
                        None => {
                            let line = code[..found.start()].matches('\n').count() + 1;
                            wrong.push(format!(
                                "{}:{line}: {} ({})",
                                source.path,
                                rule.what,
                                found.as_str().trim()
                            ));
                        }
                    }
                }
            }
        }
        for (place, used) in rule.allowed.iter().zip(used) {
            if !sources
                .iter()
                .any(|source| source.path.starts_with(place.path))
            {
                wrong.push(format!("{}: allowed, but it no longer exists", place.path));
            } else if !used && !place.reserved {
                wrong.push(format!(
                    "{}: allowed where code {}, but none there does; remove the entry ({})",
                    place.path, rule.what, place.why
                ));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "outside the one way to the database:\n{}",
        wrong.join("\n")
    );
}

/// Synchronous library code waits on a future only through
/// `droppedneedle_kernel::db::block_on`, which refuses inside a read or
/// write closure, so no closure holds the database across a network wait.
#[test]
fn the_library_waits_through_the_database_runtime() {
    let pattern = Regex::new(r"\.block_on\(").unwrap();
    let mut wrong = Vec::new();
    for source in workspace_sources() {
        if !source.path.starts_with("library/src/") {
            continue;
        }
        let code = source.code();
        for found in pattern.find_iter(&code) {
            let line = code[..found.start()].matches('\n').count() + 1;
            wrong.push(format!("{}:{line}", source.path));
        }
    }
    assert!(
        wrong.is_empty(),
        "library code waits on a future without droppedneedle_kernel::db::block_on:\n{}",
        wrong.join("\n")
    );
}

/// The tables the identity module (`library/src/identity/`) owns: an
/// album's match and its files' matches, its match state and its two undo
/// rows, and what only means something with a match: the queued placement
/// of its files, the active custom edition and its manifests, and the
/// records of files matched by hand.
const IDENTITY_TABLES: &[&str] = &[
    "local_album_external_identities",
    "local_track_external_identities",
    "library_album_match_state",
    "library_automatic_edition_undo",
    "library_edition_choice_undo",
    "library_edition_remap_queue",
    "library_custom_edition_active",
    "library_custom_edition_manifests",
    "library_custom_edition_tracks",
    "library_track_hand_matches",
];

const IDENTITY: Place = place(
    "library/src/identity/",
    "the identity module, which owns the tables",
);
const IDENTITY_TOOL: Place = place(
    "tool/src/",
    "the offline tool carries v2's matches and exports and imports whole tables, with the server stopped",
);

const IDENTITY_RULES: &[Rule] = &[
    Rule {
        what: "writes an identity table",
        patterns: &[],
        allowed: &[
            IDENTITY,
            IDENTITY_TOOL,
            place(
                "catalog/src/reads/library/perf_tests.rs",
                "the read benchmarks seed their own database",
            ),
        ],
    },
    Rule {
        what: "names an identity table alone, as a table loop does",
        patterns: &[],
        allowed: &[
            Place {
                reserved: true,
                ..IDENTITY
            },
            IDENTITY_TOOL,
        ],
    },
];

/// Album and track identities, match state, their undo rows and their
/// satellites are written only through `library/src/identity/`. Shipping
/// code outside it never names one of the tables right after `INSERT
/// INTO`, `UPDATE`, `DELETE FROM` or `REPLACE INTO`, nor as a string of its
/// own (the table loop a `format!` statement runs over). Migrations are
/// SQL files and not read here.
#[test]
fn only_the_identity_module_writes_identity_rows() {
    let tables = IDENTITY_TABLES.join("|");
    let patterns = [
        Regex::new(&format!(
            r#"(?s)\b(?:INSERT(?:\s+OR\s+\w+)?\s+INTO|UPDATE(?:\s+OR\s+\w+)?|DELETE\s+FROM|REPLACE\s+INTO)[\s\\]+(?:main\.)?(?:{tables})\b"#
        ))
        .unwrap(),
        Regex::new(&format!(r#""(?:{tables})""#)).unwrap(),
    ];
    let sources = workspace_sources();
    let mut wrong = Vec::new();
    for (rule, pattern) in IDENTITY_RULES.iter().zip(&patterns) {
        let mut used = vec![false; rule.allowed.len()];
        for source in &sources {
            let code = source.shipping_code();
            for found in pattern.find_iter(&code) {
                match rule
                    .allowed
                    .iter()
                    .position(|place| source.path.starts_with(place.path))
                {
                    Some(at) => used[at] = true,
                    None => {
                        let line = code[..found.start()].matches('\n').count() + 1;
                        wrong.push(format!(
                            "{}:{line}: {} ({})",
                            source.path,
                            rule.what,
                            found
                                .as_str()
                                .split_whitespace()
                                .collect::<Vec<_>>()
                                .join(" ")
                        ));
                    }
                }
            }
        }
        for (place, used) in rule.allowed.iter().zip(used) {
            if !used && !place.reserved {
                wrong.push(format!(
                    "{}: allowed where code {}, but none there does; remove the entry ({})",
                    place.path, rule.what, place.why
                ));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "write identity rows through library/src/identity/:\n{}",
        wrong.join("\n")
    );
}

/// What the identity module may use from the rest of the library crate,
/// each with its reason. Anything else it needs moves into the module, so
/// the commands never reach back into the areas that call them.
const IDENTITY_USES: &[(&str, &str)] = &[
    ("catalog_revision", "moves the catalog revision"),
    (
        "corrections::continuation",
        "carries a file's history onto the files a match placed; reads identity rows, writes none",
    ),
    (
        "corrections::release_join",
        "joins a replaced file to the one that took its place; reads identity rows, writes none",
    ),
    (
        "management::automatic::queue",
        "queues the album for automatic management, one statement",
    ),
    (
        "identify::queue",
        "offers the album to identification and reads its input revision",
    ),
    ("identify::models", "the identity rows' data types"),
    (
        "operations::decisions::CatalogAction",
        "the audit row, until the audit writer has a module of its own",
    ),
    (
        "operations::decisions::record_action",
        "the audit row, until the audit writer has a module of its own",
    ),
    (
        "operations::decisions::settle_reviews",
        "review writes, until reviews have a service of their own",
    ),
    ("matching::Release", "the stored release document's type"),
    (
        "publish::planner::ReleaseIdentity",
        "an import's journaled match, a data type",
    ),
];

/// The identity module uses only what [`IDENTITY_USES`] lists from the
/// rest of the crate, and every listed use is still made.
#[test]
fn the_identity_module_uses_only_what_it_lists() {
    let path = Regex::new(r"\bcrate::((?:\w+::)*)(\{[^}]*\}|\w+)").unwrap();
    let mut used = vec![false; IDENTITY_USES.len()];
    let mut wrong = Vec::new();
    for source in workspace_sources() {
        if !source.path.starts_with(IDENTITY.path) {
            continue;
        }
        let code = source.shipping_code();
        for found in path.captures_iter(&code) {
            let prefix = &found[1];
            let items: Vec<String> = match found[2].strip_prefix('{') {
                Some(group) => group
                    .trim_end_matches('}')
                    .split(',')
                    .map(str::trim)
                    .filter(|item| !item.is_empty() && *item != "self")
                    .map(|item| format!("{prefix}{item}"))
                    .collect(),
                None => vec![format!("{prefix}{}", &found[2])],
            };
            for item in items {
                if item == "identity" || item.starts_with("identity::") {
                    continue;
                }
                let allowed = IDENTITY_USES.iter().position(|(listed, _)| {
                    item == *listed || item.starts_with(&format!("{listed}::"))
                });
                match allowed {
                    Some(at) => used[at] = true,
                    None => {
                        let line = code[..found.get(0).unwrap().start()].matches('\n').count() + 1;
                        wrong.push(format!("{}:{line}: uses crate::{item}", source.path));
                    }
                }
            }
        }
    }
    for ((listed, why), used) in IDENTITY_USES.iter().zip(used) {
        if !used {
            wrong.push(format!(
                "crate::{listed}: listed, but the identity module no longer uses it; remove the entry ({why})"
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "the identity module reaches outside what it lists:\n{}",
        wrong.join("\n")
    );
}

/// Where long-lived tasks start: the loop supervisor (`kernel/src/jobs/`)
/// and the database's own threads (`kernel/src/db/`), which stop with
/// `DbRuntime::shutdown`, and the provider cache's writer, which stops with
/// `CacheDb::shutdown_within`. Any other spawn in shipping code is listed
/// below with its count in that file and why it is not a loop: work a
/// request or a loop starts and waits for, or that logs its own end.
const SPAWN_PLACES: &[&str] = &["kernel/src/jobs/", "kernel/src/db/"];

const AWAITED: &str = "blocking file or store work, awaited by the request or loop that runs it";
const UPLOAD_CLEANUP: &str = "removes unused uploads when the request's guard drops";
const DROP_IMPORT_STEP: &str = "blocking file work, awaited by the drop import that runs it";
const DROP_IMPORT: &str = "a drop import a request starts; it logs its own end";
const MIX_REFRESH: &str = "a personal-mix refresh a request starts; it logs its own end";
const SEARCH: &str = "an acquire search a request starts; it logs its own end";
const LIBRARY_CHECK: &str = "the library folder check, bounded by a timeout; one that hangs is left to end and none starts beside it";
const WORKER_CHILD: &str = "a landing or review the download worker waits for";
const REQUEST: &str = "blocking file or store work, awaited by the request that runs it";
const CATALOG_WARM: &str =
    "a background catalog warm a read starts, once per key; it logs its own end";
const DISCOVER_BUILD: &str = "a discover or home build a visit starts; it logs its own end";
const SESSION_TOUCH: &str = "a session's last-seen touch, queued off the request path, one per session; it logs its own failure";
const WARM_ENQUEUE: &str =
    "a page's image warm, queued off the request path; it logs its own failure";
const BENCHMARKS: &str = "the read benchmarks, a test-only module";
const IMAGE_WORK: &str = "image work off the async workers, awaited by the request";
const COVER_FLIGHT: &str =
    "a cover fetch that still lands in the cache when its request stops waiting";
const TEST_TEMPLATE: &str = "the test template migrates on its own thread, once per test process";
const LOOP_STEP: &str =
    "a blocking step awaited inside a supervised loop, its stop checked between steps";
const JOB_STEP: &str = "blocking file and store work, awaited by the queued job that runs it, its stop checked between rows";
const SCAN_WALK: &str = "scan walk tasks, owned and joined by the scan's drive";
const TRANSCODE: &str = "ffmpeg's output pump and stderr reader, bound to one transcode";
const PLUGIN_EVENTS: &str = "plugin event deliveries, each bounded by its call timeout";
const PLUGIN_REPLACED: &str =
    "stops a replaced plugin runtime off the request path; plugin lifecycle";
const PLUGIN_CHILD: &str =
    "the plugin child's I/O pumps and host-call answers, ended with its pipes";
const PROVIDER_FETCH: &str = "a provider fetch shared by its callers, which stores its answer when they stop waiting and logs a panic";
const BOOT_STEP: &str = "boot-time file work, awaited before serving";
const HTTP_SERVER: &str = "the HTTP server, joined by serve at the shutdown deadline";
const OFFLINE_TOOL: &str = "the offline tool's file work, awaited";

const SPAWN_SITES: &[(&str, usize, &str)] = &[
    ("acquire/src/drop_import/http.rs", 1, UPLOAD_CLEANUP),
    ("acquire/src/drop_import/mod.rs", 6, DROP_IMPORT_STEP),
    ("acquire/src/drop_import/mod.rs", 1, DROP_IMPORT),
    ("acquire/src/held/service.rs", 5, AWAITED),
    ("acquire/src/intake/dispose.rs", 1, AWAITED),
    ("acquire/src/landing/hold.rs", 1, AWAITED),
    ("acquire/src/landing/library.rs", 2, AWAITED),
    ("acquire/src/landing/mod.rs", 2, AWAITED),
    ("acquire/src/probes.rs", 2, AWAITED),
    ("acquire/src/requests/discovery.rs", 1, AWAITED),
    ("acquire/src/requests/mix.rs", 1, MIX_REFRESH),
    ("acquire/src/requests/service/edition.rs", 1, AWAITED),
    ("acquire/src/search_jobs/mod.rs", 1, SEARCH),
    ("acquire/src/slskd/repository.rs", 3, AWAITED),
    ("acquire/src/usenet/sabnzbd.rs", 6, AWAITED),
    ("acquire/src/downloads/worker/mod.rs", 1, LIBRARY_CHECK),
    ("acquire/src/downloads/worker/landings.rs", 2, WORKER_CHILD),
    ("acquire/src/downloads/worker/review.rs", 1, WORKER_CHILD),
    ("auth/src/avatars.rs", 2, REQUEST),
    ("auth/src/session/login.rs", 1, REQUEST),
    ("auth/src/sqlite/sessions.rs", 1, SESSION_TOUCH),
    ("auth/src/users/hibp.rs", 1, REQUEST),
    ("auth/src/users/services.rs", 2, REQUEST),
    ("catalog/src/reads/catalog/artist.rs", 1, CATALOG_WARM),
    ("catalog/src/reads/catalog/discovery.rs", 1, CATALOG_WARM),
    ("catalog/src/reads/discover/content.rs", 1, DISCOVER_BUILD),
    ("catalog/src/reads/discover/home/mod.rs", 1, DISCOVER_BUILD),
    ("catalog/src/reads/discover/page/mod.rs", 2, DISCOVER_BUILD),
    ("catalog/src/reads/discover/queue/mod.rs", 1, DISCOVER_BUILD),
    ("catalog/src/reads/library/file_lyrics.rs", 1, REQUEST),
    ("catalog/src/reads/library/perf_tests.rs", 1, BENCHMARKS),
    ("catalog/src/reads/library/tags.rs", 1, REQUEST),
    ("catalog/src/reads/platform/artwork/cache.rs", 1, IMAGE_WORK),
    ("catalog/src/reads/platform/artwork/local.rs", 2, IMAGE_WORK),
    ("catalog/src/reads/platform/artwork/mod.rs", 2, COVER_FLIGHT),
    ("catalog/src/reads/platform/artwork/mod.rs", 2, IMAGE_WORK),
    (
        "catalog/src/reads/platform/artwork/warm.rs",
        1,
        WARM_ENQUEUE,
    ),
    ("compat/src/adapters/library.rs", 1, REQUEST),
    ("compat/src/adapters/subsonic_store.rs", 1, REQUEST),
    ("kernel/src/runtime_config/store.rs", 4, AWAITED),
    ("kernel/src/schema/template.rs", 1, TEST_TEMPLATE),
    ("library/src/contrib/live.rs", 1, LOOP_STEP),
    ("library/src/http/mod.rs", 1, REQUEST),
    ("library/src/identify/sources.rs", 2, LOOP_STEP),
    ("library/src/identify/stores.rs", 2, LOOP_STEP),
    ("library/src/loops.rs", 4, LOOP_STEP),
    ("library/src/manage.rs", 1, LOOP_STEP),
    ("library/src/management/mod.rs", 2, LOOP_STEP),
    ("library/src/mutations.rs", 1, REQUEST),
    ("library/src/operations/mod.rs", 2, LOOP_STEP),
    ("library/src/operations/service.rs", 9, LOOP_STEP),
    ("library/src/recycle_bin.rs", 1, JOB_STEP),
    ("library/src/repairs/mod.rs", 1, LOOP_STEP),
    ("library/src/scan/coordinator.rs", 1, LOOP_STEP),
    ("library/src/scan/pool.rs", 1, LOOP_STEP),
    ("library/src/scan/walk.rs", 3, SCAN_WALK),
    ("library/src/service.rs", 1, LOOP_STEP),
    ("media/src/stream/download.rs", 2, REQUEST),
    ("media/src/stream/local_files.rs", 1, REQUEST),
    ("media/src/stream/plugin_url.rs", 2, REQUEST),
    ("media/src/stream/transcode.rs", 2, TRANSCODE),
    ("plugins/src/capabilities/events.rs", 2, PLUGIN_EVENTS),
    ("plugins/src/capabilities/library_files.rs", 1, REQUEST),
    ("plugins/src/capabilities/stream.rs", 1, REQUEST),
    ("plugins/src/handlers.rs", 3, REQUEST),
    ("plugins/src/host.rs", 1, PLUGIN_REPLACED),
    ("plugins/src/process.rs", 5, PLUGIN_CHILD),
    ("providers/src/cache.rs", 1, PROVIDER_FETCH),
    ("providers/src/youtube.rs", 1, REQUEST),
    ("src/admin/handlers.rs", 1, REQUEST),
    ("src/bootstrap/key_check.rs", 1, BOOT_STEP),
    ("src/bootstrap/mod.rs", 1, BOOT_STEP),
    ("src/bootstrap/platform.rs", 1, BOOT_STEP),
    ("src/bootstrap/serve.rs", 1, HTTP_SERVER),
    ("src/settings/effects.rs", 1, LOOP_STEP),
    ("src/settings/library_policy_service.rs", 1, REQUEST),
    ("src/settings/services.rs", 3, REQUEST),
    ("src/settings/wiring.rs", 2, REQUEST),
    ("tool/src/import/carry/avatars.rs", 2, OFFLINE_TOOL),
    ("tool/src/import/carry/bundle.rs", 1, OFFLINE_TOOL),
    ("tool/src/import/carry/held.rs", 2, OFFLINE_TOOL),
    ("tool/src/tooling/reset_key.rs", 1, OFFLINE_TOOL),
    ("tool/src/tooling/upgrade.rs", 1, OFFLINE_TOOL),
];

/// Every long-lived task runs on the supervisor, so it stops on the one
/// token and shows in the admin jobs view. A spawn anywhere else is one of
/// the listed sites, and a listed count that no longer matches fails too.
#[test]
fn every_spawn_is_supervised_or_listed() {
    let pattern = Regex::new(
        r"tokio::spawn\(|task::spawn\(|spawn_blocking\(|thread::spawn\(|\b\w*(?:runtime|handle)\.spawn\(",
    )
    .unwrap();
    let mut found: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for source in workspace_sources() {
        if SPAWN_PLACES
            .iter()
            .any(|place| source.path.starts_with(place))
        {
            continue;
        }
        let spawns = pattern.find_iter(&source.shipping_code()).count();
        if spawns > 0 {
            found.insert(source.path.clone(), spawns);
        }
    }
    let mut listed: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for (path, count, _) in SPAWN_SITES {
        *listed.entry((*path).to_owned()).or_default() += count;
    }
    let mut wrong = Vec::new();
    for (path, count) in &found {
        match listed.get(path) {
            Some(want) if want == count => {}
            Some(want) => wrong.push(format!("{path}: {count} spawns, {want} listed")),
            None => wrong.push(format!("{path}: {count} spawns, none listed")),
        }
    }
    for (path, want) in &listed {
        if !found.contains_key(path) {
            wrong.push(format!("{path}: {want} listed, none found"));
        }
    }
    assert!(
        wrong.is_empty(),
        "start long-lived work on the loop supervisor, or list the site with its reason:\n{}",
        wrong.join("\n")
    );
}

/// Where the settings store itself is held. Every other service reads its
/// sections through live handles (`Live<S>`), so a saved change reaches it
/// without a restart. Each place names why it needs the store.
const SETTINGS_STORE: &[Place] = &[
    place(
        "kernel/src/runtime_config/",
        "the store, its live handles and the sections",
    ),
    place(
        "kernel/src/platform.rs",
        "what every area is built from, which hands out the live handles while it builds",
    ),
    place(
        "src/settings/",
        "the settings service, which saves the sections the settings pages edit",
    ),
    place(
        "src/bootstrap/",
        "boot, which opens the store and hands each service its handles",
    ),
    place(
        "library/src/settings.rs",
        "adds a root and signs correction previews with the data key",
    ),
    place(
        "library/src/management/settings/service.rs",
        "Library Management's settings, which it migrates and saves",
    ),
    place(
        "acquire/src/wiring.rs",
        "acquire's composition point, which makes its live handles",
    ),
    place(
        "acquire/src/settings.rs",
        "the Lidarr and Spotify settings, which save through the store",
    ),
    place(
        "plugins/src/host.rs",
        "the plugin host, which stores each plugin's settings",
    ),
    place(
        "plugins/src/handlers.rs",
        "the plugin routes, which show each plugin's masked settings; plugin settings have no live handle",
    ),
    place(
        "auth/src/federated/",
        "the install ids, read from the internal state section through the store",
    ),
];

/// Services hold a live handle on each section they read, never the store
/// or a copy of a section taken through it.
#[test]
fn settings_are_read_through_live_handles() {
    let patterns = [
        Regex::new(r"Arc<(?:\w+::)*ConfigStore>").unwrap(),
        Regex::new(r"\b\w+:\s*(?:\w+::)*ConfigStore\s*[,)}]").unwrap(),
        Regex::new(r"\.get_raw::<").unwrap(),
    ];
    let mut used = vec![false; SETTINGS_STORE.len()];
    let mut wrong = Vec::new();
    for source in workspace_sources() {
        let code = source.shipping_code();
        for pattern in &patterns {
            for found in pattern.find_iter(&code) {
                match SETTINGS_STORE
                    .iter()
                    .position(|place| source.path.starts_with(place.path))
                {
                    Some(at) => used[at] = true,
                    None => {
                        let line = code[..found.start()].matches('\n').count() + 1;
                        wrong.push(format!("{}:{line}: {}", source.path, found.as_str()));
                    }
                }
            }
        }
    }
    for (place, used) in SETTINGS_STORE.iter().zip(used) {
        if !used {
            wrong.push(format!(
                "{}: allowed to hold the settings store, but none there does; remove the entry ({})",
                place.path, place.why
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "read settings through `ConfigStore::live`, or list the place with its reason:\n{}",
        wrong.join("\n")
    );
}

/// The places that may hold a `Platform` by value.
const PLATFORM_HOLDERS: &[Place] = &[
    place(
        "kernel/src/platform.rs",
        "the type itself and its one constructor",
    ),
    place(
        "src/bootstrap/",
        "boot, which opens the platform and builds every area from it",
    ),
];

/// Areas take `&Platform` while they are built and keep only what they
/// use. The platform carries the settings store, so a kept platform would
/// be a way past the live handles: no field, wrapper or value of it lives
/// anywhere else.
#[test]
fn the_platform_is_borrowed_never_kept() {
    let pattern = Regex::new(r"(&\s*(?:'\w+\s+)?(?:mut\s+)?)?(?:\w+::)*\bPlatform\b(::)?").unwrap();
    let mut used = vec![false; PLATFORM_HOLDERS.len()];
    let mut wrong = Vec::new();
    for source in workspace_sources() {
        let code = source.shipping_code();
        let mut in_use = false;
        for (at, line) in code.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("use ") || trimmed.starts_with("pub use ") {
                in_use = true;
            }
            if in_use {
                in_use = !line.contains(';');
                continue;
            }
            for found in pattern.captures_iter(line) {
                if found.get(1).is_some() || found.get(2).is_some() {
                    continue;
                }
                match PLATFORM_HOLDERS
                    .iter()
                    .position(|place| source.path.starts_with(place.path))
                {
                    Some(place) => used[place] = true,
                    None => wrong.push(format!("{}:{}: {}", source.path, at + 1, line.trim())),
                }
            }
        }
    }
    for (place, used) in PLATFORM_HOLDERS.iter().zip(used) {
        if !used {
            wrong.push(format!(
                "{}: allowed to hold the platform, but none there does; remove the entry ({})",
                place.path, place.why
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "take `&Platform` and keep only what you use, or list the place with its reason:\n{}",
        wrong.join("\n")
    );
}

/// One way into the download queue: the concrete `DownloadDispatch`. A
/// download is not an outside service, so no trait stands in front of it.
#[test]
fn downloads_start_through_one_concrete_dispatch() {
    let pattern = Regex::new(r"\btrait\s+\w*Dispatch\w*").unwrap();
    let mut wrong = Vec::new();
    for source in workspace_sources() {
        if !source.path.starts_with("acquire/src/") {
            continue;
        }
        let code = source.code();
        for found in pattern.find_iter(&code) {
            let line = code[..found.start()].matches('\n').count() + 1;
            wrong.push(format!("{}:{line}: {}", source.path, found.as_str()));
        }
    }
    assert!(
        wrong.is_empty(),
        "start downloads through `dispatch::DownloadDispatch`, not a trait:\n{}",
        wrong.join("\n")
    );
}

/// One stop: the supervisor's token. No loop keeps a stop flag, a sleep
/// seam or a jitter seam of its own.
#[test]
fn no_loop_has_its_own_stop_or_time_seam() {
    let pattern = Regex::new(
        r"watch::channel::<bool>|watch::channel\((?:false|true)\)|watch::(?:Sender|Receiver)<bool>|\bSleeper\b|\bJitter\b",
    )
    .unwrap();
    let mut wrong = Vec::new();
    for source in workspace_sources() {
        let code = source.code();
        for found in pattern.find_iter(&code) {
            let line = code[..found.start()].matches('\n').count() + 1;
            wrong.push(format!("{}:{line}: {}", source.path, found.as_str()));
        }
    }
    assert!(
        wrong.is_empty(),
        "stop through the supervisor's token and pace through its schedule:\n{}",
        wrong.join("\n")
    );
}

/// The one file that reads the system clock: the kernel's `Clock` and its
/// `SystemClock`.
const KERNEL_CLOCK: &str = "kernel/src/clock.rs";

/// The files that still read the system clock outside the kernel, each with
/// its reason, from `server/clock-outside-kernel.txt`.
fn clock_outside_kernel() -> Vec<(String, String)> {
    let list = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("clock-outside-kernel.txt"),
    )
    .expect("clock-outside-kernel.txt reads");
    list.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let (path, why) = line
                .split_once(char::is_whitespace)
                .expect("each clock-outside-kernel.txt line is a path and a reason");
            (path.to_owned(), why.trim().to_owned())
        })
        .collect()
}

/// Wall time comes from the kernel clock, so a test's `ManualClock` drives
/// every path it reaches. Outside `kernel/src/clock.rs`, a read of
/// `SystemTime::now()`, chrono's `Utc::now()` or `Local::now()`, or the time
/// crate's `now_utc()` or `now_local()` must be listed in
/// `server/clock-outside-kernel.txt`, and a listed file that stops reading
/// the clock loses its line. `Instant::now()` is a monotonic timer and stays
/// allowed.
#[test]
fn wall_time_comes_from_the_kernel_clock() {
    let reads =
        Regex::new(r"\b(?:SystemTime::now|Utc::now|Local::now)\b|\bnow_(?:utc|local)\b").unwrap();
    let listed = clock_outside_kernel();
    let sources = workspace_sources();
    let mut used = vec![false; listed.len()];
    let mut wrong = Vec::new();
    for source in &sources {
        if source.path == KERNEL_CLOCK {
            continue;
        }
        let code = source.shipping_code();
        let hits: Vec<String> = reads
            .find_iter(&code)
            .map(|found| {
                let line = code[..found.start()].matches('\n').count() + 1;
                format!("{}:{line}: {}", source.path, found.as_str())
            })
            .collect();
        match listed.iter().position(|(path, _)| *path == source.path) {
            Some(at) => used[at] = !hits.is_empty(),
            None => wrong.extend(hits),
        }
    }
    for ((path, _), used) in listed.iter().zip(used) {
        if !sources.iter().any(|source| source.path == *path) {
            wrong.push(format!(
                "{path}: listed in clock-outside-kernel.txt, but no longer exists"
            ));
        } else if !used {
            wrong.push(format!(
                "{path}: listed in clock-outside-kernel.txt, but no longer reads the clock; delete the line"
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "read wall time through the kernel clock, or list the file:\n{}",
        wrong.join("\n")
    );
}
