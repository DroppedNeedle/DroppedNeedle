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

/// The library's own connections, which go once the library is given a
/// `Db`.
const LIBRARY_UNTIL_DB: &str = "the library's own connections, until the library moves onto Db";

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
    place("library/src/contrib/sqlite.rs", LIBRARY_UNTIL_DB),
    place("library/src/identify/sqlite.rs", LIBRARY_UNTIL_DB),
    place("library/src/manage.rs", LIBRARY_UNTIL_DB),
    place("library/src/publish/publisher.rs", LIBRARY_UNTIL_DB),
    place("library/src/publish/recovery.rs", LIBRARY_UNTIL_DB),
    place(
        "media/src/playback/sqlite.rs",
        "playback's own connection, until playback moves onto Db",
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

const TRANSACTIONS: &[Place] = &[
    DB,
    TOOL,
    place("library/src/contrib/sqlite.rs", LIBRARY_UNTIL_DB),
    place("library/src/identify/sqlite.rs", LIBRARY_UNTIL_DB),
    place("library/src/publish/journal.rs", LIBRARY_UNTIL_DB),
    place("library/src/publish/publisher.rs", LIBRARY_UNTIL_DB),
    place("library/src/publish/recovery.rs", LIBRARY_UNTIL_DB),
];

const RULES: &[Rule] = &[
    Rule {
        what: "opens a SQLite connection or pool, or begins a sqlx transaction",
        patterns: &[
            r"Connection::open",
            r"\bopen_connection\b",
            r"\bopen_journal_connection\b",
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
