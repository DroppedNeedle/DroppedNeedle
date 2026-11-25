//! Raw SQL lives in `sql.rs` files. The statements those files declare are
//! prepared against the schema by `persist_schema` in the server tests.

use regex::Regex;

use crate::common::sources::{is_sql_file, split_literals, workspace_sources};

/// A literal allowed outside `sql.rs` for good, and why.
struct AllowedLiteral {
    path: &'static str,
    starts_with: &'static str,
    /// The literal is `starts_with` and nothing more, as an HTTP method's
    /// name is, so a statement in the same file that starts with that word
    /// is still refused.
    whole: bool,
    why: &'static str,
}

const ALLOWED_LITERALS: &[AllowedLiteral] = &[
    AllowedLiteral {
        path: "kernel/src/db/analyze.rs",
        starts_with: "CREATE TABLE db_planner_stats_reload (x); DROP TABLE",
        whole: false,
        why: "bumps the schema cookie so readers reload the planner statistics; the marker \
              table is never in the schema, so there is nothing to prepare it against",
    },
    AllowedLiteral {
        path: "compat/src/jellyfin/router.rs",
        starts_with: "DELETE",
        whole: true,
        why: "the HTTP method's name in the Jellyfin route table, not a statement",
    },
    AllowedLiteral {
        path: "compat/src/shared/ratelimit.rs",
        starts_with: "DELETE",
        whole: true,
        why: "the HTTP method's name when classing a request as a mutation, not a statement",
    },
    AllowedLiteral {
        path: "plugins/src/manifest.rs",
        starts_with: "DELETE",
        whole: true,
        why: "the HTTP method's name in a plugin's route list, not a statement",
    },
    AllowedLiteral {
        path: "plugins/src/capabilities/routes.rs",
        starts_with: "DELETE",
        whole: true,
        why: "an HTTP method a plugin route may declare, not a statement",
    },
    AllowedLiteral {
        path: "src/bootstrap/data_dirs.rs",
        starts_with: "PRAGMA ",
        whole: false,
        why: "the stamp probe reads a file's version before the runtime opens it, and its \
              tests put a probed file in WAL mode; a pragma prepares only in kernel/src/db/",
    },
    AllowedLiteral {
        path: "src/bootstrap/data_dirs.rs",
        starts_with: "CREATE TABLE ",
        whole: false,
        why: "the stamp probe's tests build a small file of each kind it tells apart",
    },
    AllowedLiteral {
        path: "src/admin/backups.rs",
        starts_with: "PRAGMA ",
        whole: false,
        why: "the backup checks read a backup file's integrity and version, a file the \
              server never opens as its database",
    },
    AllowedLiteral {
        path: "tool/src/export/sql.rs",
        starts_with: "PRAGMA wal_checkpoint(TRUNCATE)",
        whole: true,
        why: "checkpoints the upgrade's private recovered copy of a v2 database, a file no \
              server opens; v2's own files are never written",
    },
];

/// The export's bundle builder. Each statement is made at run time from the
/// columns the v2 database being exported has (an install of any age), into
/// bundle tables made the same way, so there is no fixed text to prepare;
/// its fixed reads are in `tool/src/export/sql.rs`.
const EXPORT_BUNDLE: &[&str] = &[
    "tool/src/export/artist_keys.rs",
    "tool/src/export/bundle.rs",
    "tool/src/export/library_keys.rs",
    "tool/src/export/sections/",
];

/// Source that may hold raw SQL for good: unit test files and test folders
/// (a unit test that needs raw SQL lives in one), the builder of the v2
/// fixture database, which writes the v2 schema, and [`EXPORT_BUNDLE`].
/// `server/tests/`, `server/testkit/`, `server/gates/` and
/// `server/migrations/` are never walked.
fn exempt(path: &str) -> bool {
    path.ends_with("/tests.rs")
        || path.ends_with("_tests.rs")
        || path.contains("/tests/")
        || path == "tool/src/test_support/fixture.rs"
        || EXPORT_BUNDLE.iter().any(|place| path.starts_with(place))
}

/// Raw SQL lives only in `sql.rs` files. A string literal that starts with
/// a statement verb, or holds ` FROM `, ` JOIN `, ` WHERE `, ` VALUES ` or
/// ` ORDER BY ` in capitals, fails anywhere else. One that starts with
/// `PRAGMA`, `ANALYZE`, `VACUUM` or transaction control fails outside
/// `kernel/src/db/`, which owns connection set-up and transactions. A table
/// name read from the database is quoted by
/// `db::statement::quoted_identifier`.
#[test]
fn raw_sql_stays_in_sql_files() {
    let verb = Regex::new(r"^\s*(?:SELECT|INSERT|UPDATE|DELETE|REPLACE|WITH|CREATE|DROP|ALTER)\b")
        .unwrap();
    let fragment = Regex::new(r"(?:^|\s)(?:FROM|JOIN|WHERE|VALUES|ORDER\s+BY)(?:\s|$)").unwrap();
    let control =
        Regex::new(r"^\s*(?:PRAGMA|ANALYZE|VACUUM|BEGIN|COMMIT|ROLLBACK|SAVEPOINT|RELEASE)\b")
            .unwrap();
    let sources = workspace_sources();
    let mut allowed_used = vec![false; ALLOWED_LITERALS.len()];
    let mut wrong = Vec::new();
    for source in &sources {
        if exempt(&source.path) {
            continue;
        }
        for (line, value) in split_literals(&source.text).1 {
            let raw = if control.is_match(&value) {
                !source.path.starts_with("kernel/src/db/")
            } else {
                !is_sql_file(&source.path) && (verb.is_match(&value) || fragment.is_match(&value))
            };
            if !raw {
                continue;
            }
            if let Some(at) = ALLOWED_LITERALS.iter().position(|allowed| {
                allowed.path == source.path
                    && if allowed.whole {
                        value == allowed.starts_with
                    } else {
                        value.starts_with(allowed.starts_with)
                    }
            }) {
                allowed_used[at] = true;
                continue;
            }
            let first: String = value.trim().chars().take(60).collect();
            wrong.push(format!(
                "{}:{line}: raw SQL outside sql.rs: {first}",
                source.path
            ));
        }
    }
    for (allowed, used) in ALLOWED_LITERALS.iter().zip(allowed_used) {
        if !used {
            wrong.push(format!(
                "{}: allowed literal no longer there; remove the entry ({})",
                allowed.path, allowed.why
            ));
        }
    }
    assert!(wrong.is_empty(), "raw SQL:\n{}", wrong.join("\n"));
}
