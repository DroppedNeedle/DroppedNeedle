//! One way to say who is calling. The session gate puts one
//! `droppedneedle_auth::Principal` in the request, and a handler takes
//! `User`, `Curator` or `Admin`. This gate reads the shipping source of
//! every workspace crate, comments included, and fails the day a second
//! role enum, principal struct, caller extractor, per-request user read,
//! principal read straight from the request or raw role check comes back.
//! Each allowed entry names its reason. It also holds every documented
//! signed-in operation to the caller its group names.

use std::collections::BTreeMap;

use droppedneedle::docs::ApiDoc;
use regex::Regex;
use utoipa::OpenApi as _;

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

/// Every operation the document mounts in the user, curator or admin group
/// is served by a handler that takes that group's caller: `User`, `Curator`
/// or `Admin`. The group gates the route; the extractor says in the handler
/// who it serves, and the two must agree. The handler is found by its
/// `#[utoipa::path]`, under its `operation_id` or, when it names none, its
/// function name.
#[test]
fn every_signed_in_operation_takes_its_groups_caller() {
    let doc = serde_json::to_value(ApiDoc::openapi()).expect("document serializes");
    let mut wanted: BTreeMap<String, (&str, String)> = BTreeMap::new();
    for (path, item) in doc["paths"].as_object().expect("document has paths") {
        for (method, operation) in item.as_object().expect("path item is an object") {
            let Some(requirements) = operation["security"].as_array() else {
                continue;
            };
            let caller = requirements
                .iter()
                .filter_map(|requirement| requirement["session"].as_array())
                .flatten()
                .find_map(|scope| match scope.as_str() {
                    Some("user") => Some("User"),
                    Some("curator") => Some("Curator"),
                    Some("admin") => Some("Admin"),
                    _ => None,
                });
            let Some(caller) = caller else {
                continue;
            };
            let id = operation["operationId"]
                .as_str()
                .unwrap_or_else(|| panic!("{method} {path} has no operation id"));
            wanted.insert(id.to_owned(), (caller, format!("{method} {path}")));
        }
    }
    assert!(
        wanted.len() > 100,
        "the document's signed-in operations are found"
    );

    let mut handlers: Handlers = BTreeMap::new();
    let operation_id = Regex::new(r#"\boperation_id\s*=\s*"([^"]+)""#).unwrap();
    let function = Regex::new(r"\bfn\s+(\$?[A-Za-z_]\w*)").unwrap();
    for source in workspace_sources() {
        let code = source.code();
        if source.path == SETTINGS_REGISTRY {
            settings_registry_callers(&code, &mut handlers);
        }
        let mut from = 0;
        while let Some(found) = code[from..].find("#[utoipa::path(") {
            let start = from + found;
            let open = start + "#[utoipa::path".len();
            let close = balanced(&code, open);
            let attribute = &code[open..close];
            let name = function
                .captures(&code[close..])
                .unwrap_or_else(|| panic!("{}: a path with no handler", source.path));
            let name_end = close + name.get(0).map_or(0, |m| m.end());
            let params_open = name_end + code[name_end..].find('(').unwrap_or(0);
            let params_close = balanced(&code, params_open);
            from = params_close;
            if name[1].starts_with('$') {
                continue;
            }
            let id = operation_id
                .captures(attribute)
                .map_or_else(|| name[1].to_owned(), |id| id[1].to_owned());
            let line = code[..start].matches('\n').count() + 1;
            handlers.entry(id).or_default().push((
                source.path.clone(),
                line,
                callers(&code[params_open + 1..params_close - 1]),
            ));
        }
    }

    let mut wrong = Vec::new();
    for (id, (caller, route)) in &wanted {
        match handlers.get(id).map(Vec::as_slice) {
            Some([(path, line, takes)]) => {
                if takes.as_slice() != [*caller] {
                    let takes = if takes.is_empty() {
                        "no caller".to_owned()
                    } else {
                        takes.join(", ")
                    };
                    wrong.push(format!(
                        "{path}:{line}: `{id}` ({route}) takes {takes}; its group wants `{caller}`"
                    ));
                }
            }
            Some(found) => wrong.push(format!(
                "`{id}` ({route}): {} handlers carry this operation id",
                found.len()
            )),
            None => wrong.push(format!(
                "`{id}` ({route}): no `#[utoipa::path]` handler found"
            )),
        }
    }
    assert!(
        wrong.is_empty(),
        "handlers whose caller does not match their group:\n{}",
        wrong.join("\n")
    );
}

/// Each operation id's handlers: file, line and the callers they take.
type Handlers = BTreeMap<String, Vec<(String, usize, Vec<String>)>>;

/// The settings registry, whose macro writes a get and a put handler per
/// row.
const SETTINGS_REGISTRY: &str = "src/settings/registry.rs";

/// The handlers `section_routes!` writes. Each row's `read` and `write`
/// token both picks the handler's caller and names the group it is mounted
/// in, so the macro is checked to still use it for both, and each row's
/// operations take the caller its tokens name.
fn settings_registry_callers(code: &str, handlers: &mut Handlers) {
    for shape in [
        "_caller: section_routes!(@caller $read)",
        "_caller: section_routes!(@caller $write)",
        ".$read(routes!($get))",
        ".$write(routes!($put))",
        "(@caller admin) => { Admin };",
        "(@caller user) => { User };",
    ] {
        assert!(
            code.contains(shape),
            "{SETTINGS_REGISTRY}: `section_routes!` no longer has `{shape}`; \
             teach this gate its new shape"
        );
    }
    let row = Regex::new(r"read (\w+), write (\w+) \{\s*get (\w+):[^;]*;\s*put (\w+):").unwrap();
    let caller = |token: &str| match token {
        "admin" => "Admin",
        "user" => "User",
        other => panic!("{SETTINGS_REGISTRY}: unknown caller token `{other}`"),
    };
    let mut rows = 0;
    for found in row.captures_iter(code) {
        let line = code[..found.get(0).map_or(0, |m| m.start())]
            .matches('\n')
            .count()
            + 1;
        for (id, token) in [(&found[3], &found[1]), (&found[4], &found[2])] {
            handlers.entry(id.to_owned()).or_default().push((
                SETTINGS_REGISTRY.to_owned(),
                line,
                vec![caller(token).to_owned()],
            ));
        }
        rows += 1;
    }
    assert!(
        rows > 10,
        "{SETTINGS_REGISTRY}: the registry rows are found"
    );
}

/// The offset just past the `)` that closes the `(` at `open`.
fn balanced(code: &str, open: usize) -> usize {
    let mut depth = 0usize;
    for (at, ch) in code[open..].char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return open + at + 1;
                }
            }
            _ => {}
        }
    }
    panic!("unbalanced parentheses from offset {open}");
}

/// The caller extractors among a handler's parameters: each parameter whose
/// type is exactly `User`, `Curator` or `Admin`, a path prefix allowed.
fn callers(params: &str) -> Vec<String> {
    let mut pieces = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    for (at, ch) in params.char_indices() {
        match ch {
            '(' | '<' | '[' => depth += 1,
            ')' | '>' | ']' => depth -= 1,
            ',' if depth == 0 => {
                pieces.push(&params[start..at]);
                start = at + 1;
            }
            _ => {}
        }
    }
    pieces.push(&params[start..]);
    pieces
        .into_iter()
        .filter_map(|piece| {
            let ty = match piece.rsplit_once("):") {
                Some((_, ty)) => ty,
                None => piece.split_once(':')?.1,
            };
            let ty = ty.trim().rsplit("::").next().unwrap_or_default();
            ["User", "Curator", "Admin"]
                .contains(&ty)
                .then(|| ty.to_owned())
        })
        .collect()
}
