//! The architecture test. An area may use areas in its own layer or in a
//! layer below it. A `crate::` path into a higher layer is a backward
//! import: the crate split cannot cut it into a crate. The allowed list sits
//! beside this file in `backward_imports.txt` and is empty, so any backward
//! import fails the test.
//!
//! The crates split out of this one (kernel, providers, auth, library,
//! plugins, media, catalog, acquire) sit below it, so they are not listed
//! here: Cargo itself refuses an import from them back up into this crate.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

/// Areas from the bottom layer up. An area may use areas in its own layer or
/// below it. Areas in one layer become one crate once the split lands.
const LAYERS: &[&[&str]] = &[
    &["compat"],
    // App: router, startup, admin, settings routes, docs, web and the crate
    // roots.
    &[
        "admin",
        "app",
        "bootstrap",
        "docs",
        "handlers",
        "lib",
        "main",
        "middleware",
        "security_headers",
        "settings",
        "state",
        "web",
    ],
    // Tool: export, import, the maintenance helpers and the tool binary.
    &["bin", "export", "import", "tooling"],
];

/// Names the crate root re-exports, mapped to the area that defines them.
const ROOT_REEXPORTS: &[(&str, &str)] = &[
    ("AppState", "state"),
    ("create_app", "app"),
    ("create_app_with_web", "app"),
];

/// A source file and the area it reaches up to.
type Key = (String, String);

#[test]
fn no_new_backward_import_between_areas() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let layers = layer_of_each_area();
    let mut problems = Vec::new();
    check_areas(&src, &layers, &mut problems);
    let found = backward_uses(&src, &layers, &mut problems);
    compare(&found, &listed_counts(), &mut problems);
    assert!(
        problems.is_empty(),
        "backward imports changed:\n{}\n\nFix a new one by moving the shared \
         type down a layer, or by passing the value in. A count in \
         backward_imports.txt only goes up with the owner's sign-off.",
        problems.join("\n")
    );
}

fn layer_of_each_area() -> BTreeMap<&'static str, usize> {
    let mut layers = BTreeMap::new();
    for (rank, group) in LAYERS.iter().enumerate() {
        for area in group.iter() {
            assert!(
                layers.insert(*area, rank).is_none(),
                "{area} is in two layers"
            );
        }
    }
    layers
}

/// Every top-level area on disk has a layer, and every layer is on disk.
fn check_areas(src: &Path, layers: &BTreeMap<&'static str, usize>, problems: &mut Vec<String>) {
    let mut on_disk = BTreeSet::new();
    for entry in fs::read_dir(src).expect("read server/src") {
        let path = entry.expect("read server/src entry").path();
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        if path.is_dir() || path.extension().is_some_and(|e| e == "rs") {
            on_disk.insert(stem.to_string());
        }
    }
    for area in &on_disk {
        if !layers.contains_key(area.as_str()) {
            problems.push(format!("src/{area} has no layer; add it to LAYERS"));
        }
    }
    for area in layers.keys() {
        if !on_disk.contains(*area) {
            problems.push(format!("LAYERS names {area}, which is not in src"));
        }
    }
}

/// Every `crate::` path into a higher layer, by file and target area, with
/// the lines it sits on.
fn backward_uses(
    src: &Path,
    layers: &BTreeMap<&'static str, usize>,
    problems: &mut Vec<String>,
) -> BTreeMap<Key, Vec<usize>> {
    let mut files = Vec::new();
    source_files(src, Path::new(""), &mut files);
    let mut found: BTreeMap<Key, Vec<usize>> = BTreeMap::new();
    for rel in files {
        let file = rel.to_string_lossy().into_owned();
        let area = area_of(&rel);
        let text = fs::read_to_string(src.join(&rel)).expect("read a source file");
        let text = blank_line_comments(&text);
        for (line, root) in crate_paths(&text) {
            let Some(target) = target_area(root, layers) else {
                problems.push(format!(
                    "src/{file}:{line}: crate::{root} is neither an area nor a root re-export; \
                     add it to LAYERS or ROOT_REEXPORTS"
                ));
                continue;
            };
            let (Some(&from), Some(&to)) = (layers.get(area.as_str()), layers.get(target)) else {
                continue;
            };
            if to > from {
                found
                    .entry((file.clone(), target.to_string()))
                    .or_default()
                    .push(line);
            }
        }
    }
    found
}

/// The `.rs` files under `dir/rel`, as paths relative to `dir`.
fn source_files(dir: &Path, rel: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir.join(rel)).expect("read server/src folder") {
        let entry = entry.expect("read server/src entry");
        let child = rel.join(entry.file_name());
        if entry.path().is_dir() {
            source_files(dir, &child, out);
        } else if child.extension().is_some_and(|e| e == "rs") {
            out.push(child);
        }
    }
}

/// The area a file belongs to: its top folder, or its name for a root file.
fn area_of(rel: &Path) -> String {
    let part = if rel.components().count() > 1 {
        rel.iter().next()
    } else {
        rel.file_stem()
    };
    part.map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn target_area(root: &str, layers: &BTreeMap<&'static str, usize>) -> Option<&'static str> {
    if let Some((&area, _)) = layers.get_key_value(root) {
        return Some(area);
    }
    ROOT_REEXPORTS
        .iter()
        .find(|(name, _)| *name == root)
        .map(|(_, area)| *area)
}

/// Line comments are blanked, so a path in a comment does not count. Line
/// numbers stay the same.
fn blank_line_comments(text: &str) -> String {
    text.lines()
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

/// Each `crate::` path in `text` as (line, first segment). A grouped
/// `crate::{a::b, c}` gives one entry per item.
fn crate_paths(text: &str) -> Vec<(usize, &str)> {
    const PREFIX: &str = "crate::";
    let mut out = Vec::new();
    for (at, _) in text.match_indices(PREFIX) {
        // A quote before it makes it text in a string literal, such as an
        // API tag, not a path.
        if text[..at].ends_with(|c: char| c.is_alphanumeric() || c == '_' || c == '"') {
            continue;
        }
        let after = at + PREFIX.len();
        if let Some(body) = text[after..].strip_prefix('{') {
            for (offset, item) in group_items(body) {
                let spaced = item.trim_start();
                let start = after + 1 + offset + (item.len() - spaced.len());
                if let Some(root) = leading_ident(spaced).filter(|root| *root != "self") {
                    out.push((line_of(text, start), root));
                }
            }
        } else if let Some(root) = leading_ident(&text[after..]) {
            out.push((line_of(text, after), root));
        }
    }
    out
}

/// The top-level items of a `{ ... }` group body, each with its offset.
fn group_items(body: &str) -> Vec<(usize, &str)> {
    let mut items = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    for (i, c) in body.char_indices() {
        match c {
            '{' => depth += 1,
            '}' if depth == 0 => {
                items.push((start, &body[start..i]));
                break;
            }
            '}' => depth -= 1,
            ',' if depth == 0 => {
                items.push((start, &body[start..i]));
                start = i + 1;
            }
            _ => {}
        }
    }
    items
}

fn leading_ident(s: &str) -> Option<&str> {
    let end = s
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(s.len());
    (end > 0).then(|| &s[..end])
}

fn line_of(text: &str, at: usize) -> usize {
    text[..at].matches('\n').count() + 1
}

/// The counts in backward_imports.txt, by file and target area.
fn listed_counts() -> BTreeMap<Key, usize> {
    let mut listed = BTreeMap::new();
    for line in include_str!("backward_imports.txt").lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        let (file, target, count) = match fields.as_slice() {
            [file, target, count] => (*file, *target, *count),
            _ => panic!("bad line in backward_imports.txt: {line}"),
        };
        let count: usize = count.parse().expect("a count in backward_imports.txt");
        assert!(
            listed
                .insert((file.to_string(), target.to_string()), count)
                .is_none(),
            "{file} {target} is listed twice"
        );
    }
    listed
}

/// Reports every use beyond its listed count, and every listed count the code
/// no longer reaches, so the list stays exact.
fn compare(
    found: &BTreeMap<Key, Vec<usize>>,
    listed: &BTreeMap<Key, usize>,
    problems: &mut Vec<String>,
) {
    for ((file, target), lines) in found {
        let allowed = listed
            .get(&(file.clone(), target.clone()))
            .copied()
            .unwrap_or(0);
        if lines.len() > allowed {
            for line in lines {
                problems.push(format!(
                    "src/{file}:{line}: uses crate::{target}, a higher layer \
                     ({allowed} listed, {} found)",
                    lines.len()
                ));
            }
        }
    }
    for ((file, target), &allowed) in listed {
        let now = found
            .get(&(file.clone(), target.clone()))
            .map_or(0, Vec::len);
        if now < allowed {
            problems.push(format!(
                "src/{file}: crate::{target} is listed {allowed} times, found {now}; \
                 lower the count in backward_imports.txt"
            ));
        }
    }
}
