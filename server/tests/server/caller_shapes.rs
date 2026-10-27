//! One way to say who is calling. The session gate puts one
//! `droppedneedle_auth::Principal` in the request, and a handler takes
//! `User`, `Curator` or `Admin`. This gate reads the shipping source of
//! every workspace crate, comments included, and fails the day a second
//! role enum, principal struct, caller extractor, per-request user read,
//! principal read straight from the request or raw role check comes back.
//! Each allowed entry names its reason.

use regex::Regex;

use crate::common::sources::{Source, workspace_sources};

/// Role enums: the account role, and the people frame of a tag field (which
/// pairs of a credit a field owns), allowed by folder so the tags code can
/// move it.
fn role_enum_allowed(path: &str) -> bool {
    path == "auth/src/users/roles.rs" || path.starts_with("library/src/tags/")
}

/// Request extractors by file and type. `User`, `Curator` and `Admin` are
/// the only ones that say who is calling.
const EXTRACTORS: &[(&str, &str, &str)] = &[
    ("auth/src/users/roles.rs", "User", "any signed-in caller"),
    (
        "auth/src/users/roles.rs",
        "Curator",
        "an admin or a trusted user",
    ),
    ("auth/src/users/roles.rs", "Admin", "an admin"),
    (
        "auth/src/routes/native.rs",
        "PeerAddr",
        "the client address, not a caller",
    ),
    ("kernel/src/extract.rs", "ValidJson", "a request body"),
    ("kernel/src/extract.rs", "ValidQuery", "a query string"),
    (
        "catalog/src/reads/platform/wrapped.rs",
        "WrappedKey",
        "a Wrapped share link's own key, not a session",
    ),
    (
        "compat/src/jellyfin/router.rs",
        "Authed",
        "a Jellyfin app password, outside the session gate",
    ),
];

/// Reads of the user store's row outside auth and compat. Each reads
/// another user's row, never the caller's.
const USER_ROW_READS: &[(&str, &str)] = &[
    ("src/admin/quota.rs", "the quota's target user"),
    (
        "catalog/src/reads/platform/listenbrainz_wrapped.rs",
        "display names for Wrapped",
    ),
];

/// Names of the removed caller shapes. None may come back anywhere. Each
/// is split so that a search of the tree finds none, this file included.
const GONE: &[&str] = &[
    concat!("fn translate", "_principal"),
    concat!("x-slice", "-principal"),
    concat!("PRINCIPAL", "_HEADER"),
    concat!("Auth", "Context"),
    concat!("Batch", "Caller"),
    concat!("Current", "Session"),
];

fn line_of(source: &Source, offset: usize) -> usize {
    source.text[..offset].matches('\n').count() + 1
}

#[test]
fn one_principal_and_three_caller_extractors() {
    let sources = workspace_sources();
    assert!(
        sources
            .iter()
            .any(|source| source.path.starts_with("acquire/src/target/")),
        "the walk reaches every module, `target` included"
    );
    let role_enum = Regex::new(r"\benum\s+Role\b").unwrap();
    let principal_struct = Regex::new(r"\bstruct\s+Principal\b").unwrap();
    let principal_literal = Regex::new(r"\bPrincipal\s*\{").unwrap();
    let extractor = Regex::new(
        r"\bimpl\b[^{;]*?\b(?:OptionalFromRequestParts|OptionalFromRequest|FromRequestParts|FromRequest)\b[^{;]*?\bfor\s+(?:[A-Za-z_]\w*::)*([A-Za-z_]\w*)",
    )
    .unwrap();
    let principal_from_request = Regex::new(
        r"\bExtension\s*<\s*(?:\w+::)*Principal\s*>|\bextensions\(\)\s*\.get::<\s*(?:\w+::)*Principal\s*>",
    )
    .unwrap();
    let raw_role_check = Regex::new(r"\brole\(\)\s*\.is_admin\(\)").unwrap();

    let mut wrong = Vec::new();
    let mut extractors_seen = Vec::new();
    for source in &sources {
        let path = source.path.as_str();
        let in_auth = path.starts_with("auth/src/");
        let in_compat = path.starts_with("compat/src/");

        for found in role_enum.find_iter(&source.text) {
            if !role_enum_allowed(path) {
                let line = line_of(source, found.start());
                wrong.push(format!("{path}:{line}: a second role enum"));
            }
        }
        if !in_compat && !path.starts_with("auth/src/compat_auth/") {
            for found in principal_struct.find_iter(&source.text) {
                if path != "auth/src/users/roles.rs" {
                    let line = line_of(source, found.start());
                    wrong.push(format!("{path}:{line}: a second principal struct"));
                }
            }
        }
        if !in_auth {
            for found in principal_literal.find_iter(&source.text) {
                let before = source.text[..found.start()]
                    .rsplit('\n')
                    .next()
                    .unwrap_or_default()
                    .trim_end();
                let declares = ["->", "impl", "struct", "for", "enum", "trait"]
                    .iter()
                    .any(|word| before.ends_with(word));
                if !declares {
                    let line = line_of(source, found.start());
                    wrong.push(format!(
                        "{path}:{line}: a `Principal` built by literal outside auth; \
                         work done for a user uses `Principal::acting_for`"
                    ));
                }
            }
        }
        if !in_auth {
            for found in principal_from_request.find_iter(&source.text) {
                let line = line_of(source, found.start());
                wrong.push(format!(
                    "{path}:{line}: the principal read from the request outside auth; \
                     a handler takes `User`, `Curator` or `Admin`"
                ));
            }
        }
        for found in raw_role_check.find_iter(&source.text) {
            let line = line_of(source, found.start());
            wrong.push(format!(
                "{path}:{line}: `role().is_admin()`; owner-or-admin is `Principal::is_admin`, \
                 which also checks the session"
            ));
        }
        for found in extractor.captures_iter(&source.text) {
            let name = &found[1];
            let line = line_of(source, found.get(0).map_or(0, |m| m.start()));
            if EXTRACTORS
                .iter()
                .any(|(file, kind, _)| *file == path && *kind == name)
            {
                extractors_seen.push(name.to_owned());
            } else {
                wrong.push(format!(
                    "{path}:{line}: extractor `{name}`; a caller is proven by `User`, \
                     `Curator` or `Admin` only (anything else that reads no caller \
                     goes in EXTRACTORS with its reason)"
                ));
            }
        }
        if !in_auth
            && !in_compat
            && source.text.contains("get_by_id(")
            && !USER_ROW_READS.iter().any(|(file, _)| *file == path)
        {
            wrong.push(format!(
                "{path}: reads a user row; the caller's comes with the principal"
            ));
        }
        for name in GONE {
            if let Some(offset) = source.text.find(name) {
                let line = line_of(source, offset);
                wrong.push(format!("{path}:{line}: `{name}` is gone; use `Principal`"));
            }
        }
    }
    extractors_seen.sort();
    let mut expected: Vec<String> = EXTRACTORS
        .iter()
        .map(|(_, kind, _)| (*kind).to_owned())
        .collect();
    expected.sort();
    assert_eq!(
        extractors_seen, expected,
        "every allowed extractor is still found, so the match still works"
    );
    assert!(wrong.is_empty(), "caller shapes:\n{}", wrong.join("\n"));
}
