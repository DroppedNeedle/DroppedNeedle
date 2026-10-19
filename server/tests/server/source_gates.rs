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

use crate::common::sources::{Source, workspace_sources};

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
    place(
        "catalog/src/reads/mod.rs",
        "an in-memory pool for the reads' test bundle, until the catalog reads have tests on real stores",
    ),
    place(
        "catalog/src/reads/library/perf_tests.rs",
        "migration and plan-recording fixtures of the read benchmarks, until the catalog reads have tests on real stores",
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
            let code = code_of(source);
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
        let code = code_of(&source);
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

/// Where long-lived tasks start: the loop supervisor (`kernel/src/jobs/`)
/// and the database runtime's own threads (`kernel/src/db/`), which stop
/// with `DbRuntime::shutdown`. Any other spawn in shipping code is listed
/// below with its count in that file and why it is not a loop: work a
/// request or a loop starts and waits for, or that logs its own end.
const SPAWN_PLACES: &[&str] = &["kernel/src/jobs/", "kernel/src/db/"];

const AWAITED: &str = "blocking file or store work, awaited by the request or loop that runs it";
const UPLOAD_CLEANUP: &str = "removes unused uploads when the request's guard drops";
const DROP_IMPORT_STEP: &str = "blocking file work, awaited by the drop import that runs it";
const DROP_IMPORT: &str = "a drop import a request starts; it logs its own end";
const SPOTIFY_IMPORT: &str = "a Spotify import a request starts; it logs its own end";
const MIX_REFRESH: &str = "a personal-mix refresh a request starts; it logs its own end";
const SEARCH: &str = "an acquire search a request starts; it logs its own end";
const WORKER_CHILD: &str = "a landing or review the download worker waits for";
const REQUEST: &str = "blocking file or store work, awaited by the request that runs it";
const CATALOG_WARM: &str =
    "a background catalog warm a read starts, once per key; it logs its own end";
const DISCOVER_BUILD: &str = "a discover or home build a visit starts; it logs its own end";
const FAN_OUT: &str = "enrichment calls fanned out and joined by their caller";
const BENCHMARKS: &str = "the read benchmarks, a test-only module";
const IMAGE_WORK: &str = "image work off the async workers, awaited by the request";
const COVER_FLIGHT: &str =
    "a cover fetch that still lands in the cache when its request stops waiting";
const TEST_TEMPLATE: &str = "the test template migrates on its own thread, once per test process";
const LOOP_STEP: &str =
    "a blocking step awaited inside a supervised loop, its stop checked between steps";
const SCAN_WALK: &str = "scan walk tasks, owned and joined by the scan's drive";
const TRANSCODE: &str = "ffmpeg's stderr reader and exit waiter, bound to one transcode";
const PLUGIN_EVENTS: &str = "plugin event deliveries, each bounded by its call timeout";
const PLUGIN_REPLACED: &str =
    "stops a replaced plugin runtime off the request path; plugin lifecycle";
const PLUGIN_CHILD: &str =
    "the plugin child's I/O pumps and host-call answers, ended with its pipes";
const CACHE_HOOK: &str =
    "cache invalidation after a ListenBrainz link change, off the request path";
const BOOT_STEP: &str = "boot-time file work, awaited before serving";
const HTTP_SERVER: &str = "the HTTP server, joined by serve at the shutdown deadline";
const OFFLINE_TOOL: &str = "the offline tool's file work, awaited";

const SPAWN_SITES: &[(&str, usize, &str)] = &[
    ("acquire/src/dispatch.rs", 1, AWAITED),
    ("acquire/src/downloads/held.rs", 1, AWAITED),
    ("acquire/src/drop_import/http.rs", 1, UPLOAD_CLEANUP),
    ("acquire/src/drop_import/mod.rs", 7, DROP_IMPORT_STEP),
    ("acquire/src/drop_import/mod.rs", 1, DROP_IMPORT),
    ("acquire/src/imports/jobs.rs", 1, SPOTIFY_IMPORT),
    ("acquire/src/landing/hold.rs", 1, AWAITED),
    ("acquire/src/landing/library.rs", 4, AWAITED),
    ("acquire/src/landing/mod.rs", 3, AWAITED),
    ("acquire/src/library_removal.rs", 1, AWAITED),
    ("acquire/src/probes.rs", 1, AWAITED),
    ("acquire/src/requests/discovery.rs", 1, AWAITED),
    ("acquire/src/requests/mix.rs", 1, MIX_REFRESH),
    ("acquire/src/requests/service/edition.rs", 1, AWAITED),
    ("acquire/src/search_jobs/mod.rs", 1, SEARCH),
    ("acquire/src/slskd/repository.rs", 3, AWAITED),
    ("acquire/src/target/mod.rs", 1, AWAITED),
    ("acquire/src/usenet/sabnzbd.rs", 6, AWAITED),
    ("acquire/src/wiring.rs", 2, SPOTIFY_IMPORT),
    ("acquire/src/worker/landings.rs", 2, WORKER_CHILD),
    ("acquire/src/worker/review.rs", 1, WORKER_CHILD),
    ("acquire/src/worker.rs", 3, AWAITED),
    ("auth/src/avatars.rs", 2, REQUEST),
    ("auth/src/session/login.rs", 1, REQUEST),
    ("auth/src/users/hibp.rs", 1, REQUEST),
    ("auth/src/users/services.rs", 2, REQUEST),
    ("catalog/src/reads/catalog/artist.rs", 1, CATALOG_WARM),
    ("catalog/src/reads/catalog/discovery.rs", 1, CATALOG_WARM),
    (
        "catalog/src/reads/discover/adapters/content.rs",
        1,
        DISCOVER_BUILD,
    ),
    (
        "catalog/src/reads/discover/adapters/home/genre.rs",
        1,
        DISCOVER_BUILD,
    ),
    (
        "catalog/src/reads/discover/adapters/home/mod.rs",
        1,
        DISCOVER_BUILD,
    ),
    (
        "catalog/src/reads/discover/adapters/page/mod.rs",
        2,
        DISCOVER_BUILD,
    ),
    (
        "catalog/src/reads/discover/adapters/queue/mod.rs",
        1,
        DISCOVER_BUILD,
    ),
    ("catalog/src/reads/enrichment/mod.rs", 1, FAN_OUT),
    ("catalog/src/reads/library/file_lyrics.rs", 1, REQUEST),
    ("catalog/src/reads/library/perf_tests.rs", 5, BENCHMARKS),
    ("catalog/src/reads/library/tags.rs", 1, REQUEST),
    ("catalog/src/reads/platform/artwork/cache.rs", 1, IMAGE_WORK),
    ("catalog/src/reads/platform/artwork/local.rs", 1, IMAGE_WORK),
    ("catalog/src/reads/platform/artwork/mod.rs", 2, COVER_FLIGHT),
    ("catalog/src/reads/platform/artwork/mod.rs", 2, IMAGE_WORK),
    ("compat/src/adapters/library.rs", 1, REQUEST),
    ("compat/src/adapters/subsonic_store.rs", 1, REQUEST),
    ("kernel/src/runtime_config/store.rs", 4, AWAITED),
    ("kernel/src/schema/template.rs", 1, TEST_TEMPLATE),
    ("library/src/adapters.rs", 1, LOOP_STEP),
    ("library/src/http/corrections.rs", 1, REQUEST),
    ("library/src/http/edition.rs", 2, REQUEST),
    ("library/src/http/handlers.rs", 5, REQUEST),
    ("library/src/http/management.rs", 1, REQUEST),
    ("library/src/http/management_tools.rs", 1, REQUEST),
    ("library/src/http/mutations.rs", 1, REQUEST),
    ("library/src/http/operations.rs", 1, REQUEST),
    ("library/src/http/reconcile.rs", 1, REQUEST),
    ("library/src/http/repairs.rs", 1, REQUEST),
    ("library/src/http/reviews.rs", 2, REQUEST),
    ("library/src/http/scan.rs", 1, REQUEST),
    ("library/src/identify/sources.rs", 2, LOOP_STEP),
    ("library/src/identify/stores.rs", 2, LOOP_STEP),
    ("library/src/loops.rs", 3, LOOP_STEP),
    ("library/src/management/mod.rs", 2, LOOP_STEP),
    ("library/src/operations/mod.rs", 2, LOOP_STEP),
    ("library/src/operations/service.rs", 9, LOOP_STEP),
    ("library/src/repairs/mod.rs", 1, LOOP_STEP),
    ("library/src/scan/coordinator.rs", 1, LOOP_STEP),
    ("library/src/scan/pool.rs", 1, LOOP_STEP),
    ("library/src/scan/walk.rs", 3, SCAN_WALK),
    ("library/src/wiring.rs", 2, LOOP_STEP),
    ("media/src/stream/download.rs", 3, REQUEST),
    ("media/src/stream/gateway.rs", 3, REQUEST),
    ("media/src/stream/local_files.rs", 1, REQUEST),
    ("media/src/stream/plugin_url.rs", 2, REQUEST),
    ("media/src/stream/routes.rs", 1, REQUEST),
    ("media/src/stream/transcode.rs", 4, TRANSCODE),
    ("plugins/src/capabilities/events.rs", 2, PLUGIN_EVENTS),
    ("plugins/src/capabilities/library_files.rs", 1, REQUEST),
    ("plugins/src/capabilities/stream.rs", 1, REQUEST),
    ("plugins/src/handlers.rs", 3, REQUEST),
    ("plugins/src/host.rs", 1, PLUGIN_REPLACED),
    ("plugins/src/process.rs", 5, PLUGIN_CHILD),
    ("plugins/src/wiring.rs", 1, CACHE_HOOK),
    ("providers/src/youtube.rs", 1, REQUEST),
    ("src/admin/handlers.rs", 1, REQUEST),
    ("src/bootstrap/key_check.rs", 1, BOOT_STEP),
    ("src/bootstrap/mod.rs", 1, BOOT_STEP),
    ("src/bootstrap/platform.rs", 1, BOOT_STEP),
    ("src/bootstrap/serve.rs", 1, HTTP_SERVER),
    ("src/settings/library_policy_service.rs", 1, REQUEST),
    ("src/settings/services.rs", 3, REQUEST),
    ("src/settings/wiring.rs", 1, REQUEST),
    ("tool/src/import/carry/avatars.rs", 2, OFFLINE_TOOL),
    ("tool/src/import/carry/bundle.rs", 1, OFFLINE_TOOL),
    ("tool/src/import/carry/held.rs", 2, OFFLINE_TOOL),
    ("tool/src/tooling/reset_key.rs", 1, OFFLINE_TOOL),
    ("tool/src/tooling/upgrade.rs", 1, OFFLINE_TOOL),
];

/// The code before the file's unit tests, comment lines blanked.
fn shipping_code(source: &Source) -> String {
    let code = code_of(source);
    let lines: Vec<&str> = code.lines().collect();
    let tests = lines.windows(2).position(|pair| {
        pair[0].trim() == "#[cfg(test)]" && pair[1].trim_start().starts_with("mod ")
    });
    lines[..tests.unwrap_or(lines.len())].join("\n")
}

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
        let spawns = pattern.find_iter(&shipping_code(&source)).count();
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
        let code = shipping_code(&source);
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
        let code = shipping_code(&source);
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
        let code = code_of(&source);
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
