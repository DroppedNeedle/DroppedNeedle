//! Every stored settings field has a reader. A field nothing reads is a
//! setting that saves and shows but changes nothing.
//!
//! For each section on the kernel's section list, the fields come from the
//! section's schema in the API document (nested objects, list items and
//! map values included), or from its serialized default when it has no
//! schema. A field has a reader when its name is read as `.name` in the
//! shipping source of a workspace crate, or appears as a word in the web
//! app, outside the places that only define, carry or edit settings: the
//! section definitions, the settings routes and services, the mask, the v2
//! import and export, the settings forms and queries, the generated
//! contract, type files and tests.
//!
//! The check is by name, not by use: a generic name such as `enabled`,
//! `url` or `api_key` passes when anything reads a field of that name. It
//! catches a field whose name nothing reads, not every unread field.
//!
//! A field read only through a method on its section is listed in
//! [`READ_THROUGH`] with the method, which must be called.
//!
//! [`PENDING`] lists fields whose reader has not landed yet. A row that
//! covers no unread field fails, so the list only shrinks; it must be
//! empty before release.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use droppedneedle::docs::ApiDoc;
use droppedneedle_kernel::runtime_config::registry::{self, Listed, Visitor};
use droppedneedle_kernel::runtime_config::secret_sections::SecretSection;
use droppedneedle_kernel::runtime_config::sections::{PlainSection, Section};
use regex::Regex;
use serde_json::Value;
use utoipa::OpenApi as _;

use crate::common::sources::workspace_sources;

/// Fields with no reader yet: a path prefix (section key, then fields;
/// `[]` steps into list items, `{}` into map values) and why.
const PENDING: &[(&str, &str)] = &[(
    "library_management.profiles[]",
    "not every Library Management profile option is honoured yet",
)];

/// Fields read only through a method on their section, with the method.
/// A row whose method nothing calls counts as unread.
const READ_THROUGH: &[(&str, &str)] = &[("security_settings.frame_ancestors", "framing_sources")];

/// Rust source that defines, carries or edits settings rather than acting
/// on them, as path prefixes from `server/`.
const RUST_NOT_READERS: &[&str] = &[
    "kernel/src/runtime_config/sections/",
    "kernel/src/runtime_config/secret_sections/",
    "kernel/src/runtime_config/mask.rs",
    "src/settings/",
    "tool/src/import/",
    "tool/src/export/",
];

/// Web app source that shows, edits or types settings rather than acting
/// on them, as path prefixes from `frontend/src/`.
const WEB_NOT_READERS: &[&str] = &[
    "lib/components/settings/",
    "routes/settings/",
    "lib/queries/settings/",
    "lib/api/v3/",
    "lib/test/",
];

/// Sections stored under a schema name other than their type's.
const SCHEMA_NAMES: &[(&str, &str)] = &[
    ("TypedLibrary", "LibrarySettings"),
    ("LibraryManagement", "LibraryManagementSettings"),
];

#[test]
fn every_stored_settings_field_has_a_reader() {
    let doc = serde_json::to_value(ApiDoc::openapi()).expect("document serializes");
    let mut fields = Fields {
        schemas: &doc["components"]["schemas"],
        paths: BTreeSet::new(),
    };
    registry::for_each(&mut fields);
    assert!(fields.paths.len() > 100, "the walk finds the stored fields");

    let rust = rust_reads();
    let web = web_words();
    let unread: Vec<&String> = fields
        .paths
        .iter()
        .filter(|path| {
            let name = leaf(path);
            let through = READ_THROUGH
                .iter()
                .any(|(field, method)| field == path && rust.contains(*method));
            !through && !rust.contains(name) && !web.contains(name)
        })
        .collect();

    let mut failures = Vec::new();
    for path in &unread {
        if !PENDING.iter().any(|(prefix, _)| covers(prefix, path)) {
            failures.push(format!("{path} has no reader"));
        }
    }
    for (prefix, why) in PENDING {
        if !unread.iter().any(|path| covers(prefix, path)) {
            failures.push(format!(
                "pending `{prefix}` ({why}) covers no unread field: remove it"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Every field path of every section on the list.
struct Fields<'a> {
    schemas: &'a Value,
    paths: BTreeSet<String>,
}

impl Fields<'_> {
    fn add<S: Section>(&mut self) {
        let type_name = std::any::type_name::<S>()
            .rsplit("::")
            .next()
            .unwrap_or_default();
        let name = SCHEMA_NAMES
            .iter()
            .find(|(ty, _)| *ty == type_name)
            .map_or(type_name, |(_, schema)| schema);
        let mut seen = HashSet::new();
        match self.schemas.get(name) {
            Some(schema) => self.walk_schema(schema, S::KEY, &mut seen),
            None => {
                let value = serde_json::to_value(S::default()).expect("default serializes");
                self.walk_value(&value, S::KEY);
            }
        }
    }

    fn walk_schema(&mut self, schema: &Value, at: &str, seen: &mut HashSet<String>) {
        if let Some(target) = schema["$ref"].as_str() {
            let name = target.rsplit('/').next().unwrap_or_default();
            if seen.insert(name.to_owned()) {
                let target = &self.schemas[name];
                self.walk_schema(target, at, seen);
                seen.remove(name);
            }
            return;
        }
        if let Some(properties) = schema["properties"].as_object() {
            for (field, child) in properties {
                let path = format!("{at}.{field}");
                self.paths.insert(path.clone());
                self.walk_schema(child, &path, seen);
            }
        }
        for key in ["oneOf", "anyOf", "allOf"] {
            for child in schema[key].as_array().into_iter().flatten() {
                self.walk_schema(child, at, seen);
            }
        }
        if schema["items"].is_object() {
            self.walk_schema(&schema["items"], &format!("{at}[]"), seen);
        }
        if schema["additionalProperties"].is_object() {
            self.walk_schema(&schema["additionalProperties"], &format!("{at}{{}}"), seen);
        }
    }

    fn walk_value(&mut self, value: &Value, at: &str) {
        if let Value::Object(map) = value {
            for (field, child) in map {
                let path = format!("{at}.{field}");
                self.paths.insert(path.clone());
                self.walk_value(child, &path);
            }
        }
    }
}

impl Visitor for Fields<'_> {
    fn plain<S: PlainSection + Listed>(&mut self) {
        self.add::<S>();
    }

    fn secret<S: SecretSection + Listed>(&mut self) {
        self.add::<S>();
    }

    fn indexers(&mut self) {
        let mut seen = HashSet::new();
        let schema = self.schemas["NewznabIndexer"].clone();
        self.walk_schema(&schema, "indexers[]", &mut seen);
    }

    /// Plugin settings are keys a plugin's manifest declares; the plugin
    /// reads them.
    fn plugins(&mut self) {}
}

/// The last field name of a path.
fn leaf(path: &str) -> &str {
    path.rsplit('.')
        .next()
        .unwrap_or(path)
        .trim_end_matches("[]")
        .trim_end_matches("{}")
}

/// `prefix` is the path or one of its parents.
fn covers(prefix: &str, path: &str) -> bool {
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with(['.', '[', '{']))
}

/// Every name read as `.name` in shipping Rust outside [`RUST_NOT_READERS`].
fn rust_reads() -> HashSet<String> {
    let access = Regex::new(r"\.([a-z_][a-z0-9_]*)\b").expect("pattern compiles");
    let mut names = HashSet::new();
    for source in workspace_sources() {
        if RUST_NOT_READERS
            .iter()
            .any(|prefix| source.path.starts_with(prefix))
        {
            continue;
        }
        for found in access.captures_iter(&source.shipping_code()) {
            names.insert(found[1].to_owned());
        }
    }
    names
}

/// Every word in the web app's source outside [`WEB_NOT_READERS`], type
/// files and specs.
fn web_words() -> HashSet<String> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../frontend/src");
    let word = Regex::new(r"[A-Za-z_][A-Za-z0-9_]*").expect("pattern compiles");
    let mut names = HashSet::new();
    let mut pending = vec![root.clone()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).expect("web source lists") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                pending.push(path);
            } else if web_reader(&root, &path) {
                let text = std::fs::read_to_string(&path).expect("web source reads");
                for found in word.find_iter(&text) {
                    names.insert(found.as_str().to_owned());
                }
            }
        }
    }
    assert!(names.len() > 1000, "the web app's source is walked");
    names
}

fn web_reader(root: &Path, path: &Path) -> bool {
    let relative = path
        .strip_prefix(root)
        .expect("under the web source")
        .to_string_lossy()
        .replace('\\', "/");
    let file = relative.rsplit('/').next().unwrap_or_default();
    let code = [".ts", ".svelte", ".js"]
        .iter()
        .any(|ext| file.ends_with(ext));
    code && file != "types.ts"
        && !file.contains(".spec.")
        && !file.contains(".test.")
        && !WEB_NOT_READERS
            .iter()
            .any(|prefix| relative.starts_with(prefix))
}
