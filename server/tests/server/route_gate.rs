//! Nothing registers a route except the mount API in
//! `kernel/src/mount.rs`: documented routes through `Routes`, the compat
//! shims through `CompatRoutes`. A `.route(`, `.route_service(`, `.nest(`,
//! `.nest_service(`, `.fallback(` or `.fallback_service(` anywhere else in
//! the shipping source, called as a method or as `Router::route(..)`,
//! fails unless its file is listed below with the reason and the number of
//! calls it holds. The walk leaves out the in-repo mock servers in `testkit/`,
//! which stand in for outside services and are never mounted by the app.

use regex::Regex;

use crate::common::sources::workspace_sources;

/// The one place routes are registered.
const MOUNT: &str = "kernel/src/mount.rs";

/// A file allowed to register routes itself: how many calls, and why. A
/// file may have one row per reason; its rows add up.
struct Allowed {
    path: &'static str,
    calls: usize,
    why: &'static str,
}

const ALLOWED: &[Allowed] = &[
    Allowed {
        path: "src/app.rs",
        calls: 7,
        why: "the `/__test__` hooks: built only with test-support and served only when the test config asks",
    },
    Allowed {
        path: "src/app.rs",
        calls: 5,
        why: "the pre-routing layers its module docs describe: the base-path strip and its outside-base 404, the debug CORS wrap, the compat dispatch, and the web UI's fallback for a GET no route took",
    },
    Allowed {
        path: "catalog/src/artwork/covers_debug.rs",
        calls: 1,
        why: "the covers debug tooling: mounted only under cfg(debug_assertions), never documented",
    },
    Allowed {
        path: "media/src/stream/routes.rs",
        calls: 1,
        why: "test-support only: the stream route over a fake engine, for its slice tests",
    },
    Allowed {
        path: "src/upgrade_mode/mod.rs",
        calls: 1,
        why: "upgrade mode's web UI fallback: the server waiting for its upgrade serves the upgrade page, not the app",
    },
];

#[test]
fn routes_are_registered_only_through_the_mount() {
    let call = Regex::new(
        r"(?:\.|::)(?:route|route_service|nest|nest_service|fallback|fallback_service)\s*\(",
    )
    .unwrap();
    let mut wrong = Vec::new();
    let mut seen: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for source in workspace_sources() {
        if source.path == MOUNT {
            continue;
        }
        let calls = source
            .text
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .map(|line| call.find_iter(line).count())
            .sum::<usize>();
        if calls == 0 {
            continue;
        }
        match ALLOWED.iter().find(|entry| entry.path == source.path) {
            Some(entry) => {
                seen.insert(entry.path, calls);
            }
            None => wrong.push(format!(
                "{}: {calls} route registration(s) outside the mount API",
                source.path
            )),
        }
    }
    let mut allowed: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for entry in ALLOWED {
        *allowed.entry(entry.path).or_default() += entry.calls;
    }
    for (path, want) in allowed {
        let calls = seen.get(path).copied().unwrap_or(0);
        if calls != want {
            let why: Vec<&str> = ALLOWED
                .iter()
                .filter(|entry| entry.path == path)
                .map(|entry| entry.why)
                .collect();
            wrong.push(format!(
                "{path}: {calls} call(s), allowed {want} ({})",
                why.join("; ")
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "register routes through kernel/src/mount.rs: {wrong:#?}"
    );
}
