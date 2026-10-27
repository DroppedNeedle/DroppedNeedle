//! Nothing registers a route except the mount API in
//! `kernel/src/mount.rs`: documented routes through `Routes`, the compat
//! shims through `CompatRoutes`. A `.route(`, `.route_service(`, `.nest(`
//! or `.nest_service(` anywhere else in the shipping source fails, unless
//! its file is listed below with the reason and the number of calls it
//! holds. The walk leaves out the in-repo mock servers in `testkit/`,
//! which stand in for outside services and are never mounted by the app.

use regex::Regex;

use crate::common::sources::workspace_sources;

/// The one place routes are registered.
const MOUNT: &str = "kernel/src/mount.rs";

/// A file allowed to register routes itself: how many calls, and why.
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
        path: "catalog/src/reads/platform/covers_debug.rs",
        calls: 1,
        why: "the covers debug tooling: mounted only under cfg(debug_assertions), never documented",
    },
    Allowed {
        path: "auth/src/wiring.rs",
        calls: 1,
        why: "the OIDC callback at its v2 path: kept until the legacy callbacks are removed",
    },
    Allowed {
        path: "acquire/src/imports/handlers.rs",
        calls: 1,
        why: "the Spotify callback at its v1 path: kept until the legacy callbacks are removed",
    },
    Allowed {
        path: "library/src/http/contrib.rs",
        calls: 1,
        why: "the MusicBrainz contribution callback at its v1 path: kept until the legacy callbacks are removed",
    },
    Allowed {
        path: "media/src/stream/routes.rs",
        calls: 1,
        why: "test-support only: the stream route over a fake engine, for its slice tests",
    },
    Allowed {
        path: "media/src/remotes/playlist_sync.rs",
        calls: 1,
        why: "test-support only: playlist sync over fakes, for its slice tests",
    },
];

#[test]
fn routes_are_registered_only_through_the_mount() {
    let call = Regex::new(r"\.(?:route|route_service|nest|nest_service)\s*\(").unwrap();
    let mut wrong = Vec::new();
    let mut seen = vec![0; ALLOWED.len()];
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
        match ALLOWED.iter().position(|entry| entry.path == source.path) {
            Some(at) => seen[at] = calls,
            None => wrong.push(format!(
                "{}: {calls} route registration(s) outside the mount API",
                source.path
            )),
        }
    }
    for (entry, calls) in ALLOWED.iter().zip(seen) {
        if calls != entry.calls {
            wrong.push(format!(
                "{}: {calls} call(s), allowed {} ({})",
                entry.path, entry.calls, entry.why
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "register routes through kernel/src/mount.rs: {wrong:#?}"
    );
}
