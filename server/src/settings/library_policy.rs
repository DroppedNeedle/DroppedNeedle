//! Library policy: normalize-and-validate, content revisions, and the
//! pure logic behind the policy routes (tree, impact, apply preview,
//! restorable roots, path mapping).
//!
//! Ports v2's `LibraryPolicyResolver` (root/rule normalization, warnings,
//! SHA-256 content revision) plus the pure parts of v2's library policy
//! services. Catalog rows come in through the [`LibraryPolicyCatalog`]
//! read port; the SQLite adapter is `settings::library_catalog` and the
//! orchestration (blocking hops, revision checks, saves) is
//! `settings::library_policy_service`.
//!
//! v3 keeps no pending-policy state: a save applies at once and the scan
//! engine reads the roots on its next tick, so every preview reads the
//! saved settings only.
//!
//! The revision hash is byte-compatible with v2 (`sort_keys`, compact
//! separators, ASCII-escaped JSON over the same payload), so a migrated
//! config keeps its revisions. Golden vectors minted from v2 pin this in
//! the settings tests.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Component, Path, PathBuf};

use futures_util::future::BoxFuture;
use sha2::{Digest, Sha256};

use super::error::SettingsError;
use super::models::{
    LibraryPathMappingItem, LibraryPathMappingReport, LibraryPolicyApplyPreviewResponse,
    LibraryPolicyImpactResponse, LibraryPolicyTreeNode, LibraryPolicyTreeResponse,
    LibraryRestorableRoot, LibrarySettingsResponse, PathMappingError, PathMappingSource,
    PolicyNodeKind,
};
use crate::runtime_config::Masked;
use crate::runtime_config::secret_sections::{
    IdentificationPolicy, LibraryPathRule, LibraryRoot, TypedLibrary,
};

/// Canonical JSON: sorted keys, compact separators, ASCII-escaped
/// strings, byte-identical to Python's
/// `json.dumps(value, sort_keys=True, separators=(",", ":"))` for the
/// shapes hashed here (ASCII keys, ints, bools, null, short floats).
pub fn canonical_json(value: &serde_json::Value) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &serde_json::Value, out: &mut String) {
    match value {
        serde_json::Value::Null => out.push_str("null"),
        serde_json::Value::Bool(true) => out.push_str("true"),
        serde_json::Value::Bool(false) => out.push_str("false"),
        serde_json::Value::Number(number) => out.push_str(&number.to_string()),
        serde_json::Value::String(text) => write_ascii_string(text, out),
        serde_json::Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        serde_json::Value::Object(map) => {
            out.push('{');
            // serde_json Map is a BTreeMap: keys already in byte order,
            // which matches code-point order for UTF-8.
            for (index, (key, item)) in map.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_ascii_string(key, out);
                out.push(':');
                write_canonical(item, out);
            }
            out.push('}');
        }
    }
}

/// ASCII-escape one string the way `ensure_ascii=True` does: printable
/// ASCII passes through; `"`, `\`, and everything outside 0x20-0x7E
/// escapes (`\b \f \n \r \t` short forms, `\uXXXX` lowercase hex with
/// surrogate pairs past the BMP).
fn write_ascii_string(text: &str, out: &mut String) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            ch if ('\u{20}'..='\u{7e}').contains(&ch) => out.push(ch),
            ch => {
                let code = ch as u32;
                if code > 0xffff {
                    let base = code - 0x1_0000;
                    out.push_str(&format!(
                        "\\u{:04x}\\u{:04x}",
                        0xd800 + (base >> 10),
                        0xdc00 + (base & 0x3ff)
                    ));
                } else {
                    out.push_str(&format!("\\u{code:04x}"));
                }
            }
        }
    }
    out.push('"');
}

/// SHA-256 hex of the canonical JSON encoding.
pub fn stable_hash(value: &serde_json::Value) -> String {
    let mut hasher = Sha256::new();
    hasher.update(canonical_json(value).as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Lexically clean an absolute path (normpath equivalent): collapse
/// `.`/`..`/duplicate separators without touching the filesystem.
pub fn clean_absolute(path: &str) -> Option<PathBuf> {
    let candidate = Path::new(path.trim());
    if !candidate.is_absolute() {
        return None;
    }
    let mut clean = PathBuf::new();
    for component in candidate.components() {
        match component {
            Component::RootDir => clean.push(Component::RootDir),
            Component::CurDir => {}
            Component::ParentDir => {
                clean.pop();
            }
            Component::Normal(part) => clean.push(part),
            Component::Prefix(prefix) => clean.push(prefix.as_os_str()),
        }
    }
    Some(clean)
}

/// Normalize one root path: absolute after lexical cleaning.
fn normalise_root_path(path: &str) -> Result<PathBuf, SettingsError> {
    clean_absolute(path).ok_or_else(|| SettingsError::InvalidInput {
        message: format!("Library root paths must be absolute: {path}"),
    })
}

/// Normalize one rule path: relative, forward slashes, no `.`/`..`.
fn normalise_rule_path(path: &str) -> Result<String, SettingsError> {
    let candidate = path.trim();
    if candidate.is_empty() {
        return Err(SettingsError::InvalidInput {
            message: "A policy rule needs a directory path.".to_owned(),
        });
    }
    if candidate.contains('\\') {
        return Err(SettingsError::InvalidInput {
            message: "Policy rule paths must use forward slashes.".to_owned(),
        });
    }
    let mut parts: Vec<&str> = Vec::new();
    for part in candidate.split('/') {
        if part.is_empty() || part == "." || part == ".." || part.ends_with(':') {
            return Err(SettingsError::InvalidInput {
                message: format!("Policy rule paths must stay inside their library root: {path}"),
            });
        }
        parts.push(part);
    }
    if candidate.starts_with('/') {
        return Err(SettingsError::InvalidInput {
            message: format!("Policy rule paths must stay inside their library root: {path}"),
        });
    }
    Ok(parts.join("/"))
}

/// Whether two cleaned root paths collide: the same directory, or one
/// inside the other. Roots may not overlap.
fn paths_overlap(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

/// Normalized settings plus warnings and the content revision.
pub struct ResolvedLibraryPolicy {
    /// Normalized settings.
    pub settings: TypedLibrary,
    /// Non-blocking warnings.
    pub warnings: Vec<String>,
    /// Content revision.
    pub policy_revision: String,
}

/// Normalize-and-validate library settings, then hash the revision.
/// Mirrors v2's resolver: unique root ids, unique casefolded labels,
/// no root inside another root or inside staging, unique rule ids per
/// root, rules ordered by depth, warnings for unavailable paths.
pub fn resolve(settings: &TypedLibrary) -> Result<ResolvedLibraryPolicy, SettingsError> {
    let mut root_ids = BTreeSet::new();
    let mut labels = BTreeSet::new();
    let mut canonical_paths: Vec<(String, PathBuf)> = Vec::new();
    let mut roots: Vec<LibraryRoot> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let staging = if settings.staging_path.trim().is_empty() {
        None
    } else {
        Some(normalise_root_path(&settings.staging_path)?)
    };

    for root in &settings.library_roots {
        let root_id = root.id.trim().to_owned();
        if root_id.is_empty() {
            return Err(SettingsError::InvalidInput {
                message: "Library root ids must be non-blank.".to_owned(),
            });
        }
        if !root_ids.insert(root_id.clone()) {
            return Err(SettingsError::InvalidInput {
                message: format!("Duplicate library root id: {root_id}"),
            });
        }
        let label = root.label.trim().to_owned();
        if label.is_empty() {
            return Err(SettingsError::InvalidInput {
                message: "Library root labels must be non-blank.".to_owned(),
            });
        }
        let folded = label.to_lowercase();
        if !labels.insert(folded) {
            return Err(SettingsError::InvalidInput {
                message: format!("Duplicate library root label: {label}"),
            });
        }
        let canonical = normalise_root_path(&root.path)?;
        for (other_id, other_path) in &canonical_paths {
            if paths_overlap(&canonical, other_path) {
                return Err(SettingsError::InvalidInput {
                    message: format!("Library root {root_id} overlaps another root ({other_id})."),
                });
            }
        }
        if let Some(staging_path) = &staging
            && (canonical == *staging_path || canonical.starts_with(staging_path))
        {
            return Err(SettingsError::InvalidInput {
                message: format!("Library root {root_id} must not live inside staging."),
            });
        }
        canonical_paths.push((root_id.clone(), canonical.clone()));

        let mut rule_ids = BTreeSet::new();
        let mut rule_paths = BTreeSet::new();
        let mut rules: Vec<LibraryPathRule> = Vec::new();
        for rule in &root.rules {
            let rule_id = rule.id.trim().to_owned();
            if rule_id.is_empty() {
                return Err(SettingsError::InvalidInput {
                    message: "Policy rule ids must be non-blank.".to_owned(),
                });
            }
            if !rule_ids.insert(rule_id.clone()) {
                return Err(SettingsError::InvalidInput {
                    message: format!("Duplicate policy rule id: {rule_id}"),
                });
            }
            let relative = normalise_rule_path(&rule.relative_path)?;
            if !rule_paths.insert(relative.clone()) {
                return Err(SettingsError::InvalidInput {
                    message: format!("Library root {label} has more than one rule for {relative}."),
                });
            }
            if !canonical.join(&relative).exists() {
                warnings.push(format!(
                    "Policy path {relative} under {label} is not currently available."
                ));
            }
            rules.push(LibraryPathRule {
                id: rule_id,
                relative_path: relative,
                policy: rule.policy,
            });
        }
        // Shallow rules first, then by path, so deeper rules win and the
        // revision does not depend on the order rules were entered.
        rules.sort_by(|a, b| {
            (a.relative_path.matches('/').count(), &a.relative_path)
                .cmp(&(b.relative_path.matches('/').count(), &b.relative_path))
        });
        if !canonical.exists() {
            warnings.push(format!("Library root {label} is not currently available."));
        }
        roots.push(LibraryRoot {
            id: root_id,
            path: canonical.to_string_lossy().into_owned(),
            label,
            policy: root.policy,
            rules,
        });
    }

    let normalized = TypedLibrary {
        library_roots: roots,
        staging_path: settings.staging_path.clone(),
        naming_template: settings.naming_template.clone(),
        acoustid_api_key: settings.acoustid_api_key.clone(),
        enabled: settings.enabled,
    };
    let policy_revision = revision(&normalized);
    Ok(ResolvedLibraryPolicy {
        settings: normalized,
        warnings,
        policy_revision,
    })
}

/// 409 unless `expected` is the stored settings' revision.
pub fn check_revision(stored: &TypedLibrary, expected: &str) -> Result<(), SettingsError> {
    if expected != revision(stored) {
        return Err(SettingsError::StaleRevision {
            message: "Library settings changed since this page loaded. Refresh and retry."
                .to_owned(),
        });
    }
    Ok(())
}

/// 400 when a proposal drops every root while the catalog holds tracks:
/// those tracks would be left with no root.
pub fn guard_last_root(
    proposal: &TypedLibrary,
    catalog_has_tracks: bool,
) -> Result<(), SettingsError> {
    if proposal.library_roots.is_empty() && catalog_has_tracks {
        return Err(SettingsError::InvalidInput {
            message: "Removing every library root would orphan the existing catalog. \
                      Keep at least one root, or set its policy to Excluded instead."
                .to_owned(),
        });
    }
    Ok(())
}

/// Content revision over roots plus rules (enabled, staging, naming,
/// and keys are excluded, exactly like v2).
pub fn revision(settings: &TypedLibrary) -> String {
    let payload = serde_json::json!({
        "roots": settings.library_roots.iter().map(|root| {
            serde_json::json!({
                "id": root.id,
                "path": root.path,
                "policy": policy_name(root.policy),
                "rules": root.rules.iter().map(|rule| {
                    serde_json::json!({
                        "id": rule.id,
                        "relative_path": rule.relative_path,
                        "policy": policy_name(rule.policy),
                    })
                }).collect::<Vec<_>>(),
            })
        }).collect::<Vec<_>>(),
    });
    stable_hash(&payload)
}

fn policy_name(policy: IdentificationPolicy) -> &'static str {
    match policy {
        IdentificationPolicy::LocalMetadata => "local_metadata",
        IdentificationPolicy::Automatic => "automatic",
        IdentificationPolicy::Excluded => "excluded",
    }
}

/// One transition scope: a root or rule whose effective policy changed
/// between the stored and candidate settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitionScope {
    /// Root id.
    pub root_id: String,
    /// Scope id (root id or rule id).
    pub scope_id: String,
    /// Relative path (`.` for a whole root).
    pub relative_path: String,
    /// Effective policy under the candidate.
    pub effective_policy: IdentificationPolicy,
}

/// Resolve one absolute path to its effective policy under `settings`.
/// None when the path sits under no root; ambiguous matches are
/// impossible (normalization rejects overlapping roots).
pub fn resolve_path(
    settings: &TypedLibrary,
    path: &Path,
) -> Option<(String, String, IdentificationPolicy)> {
    let mut matches: Vec<(&LibraryRoot, PathBuf)> = Vec::new();
    for root in &settings.library_roots {
        let root_path = PathBuf::from(&root.path);
        if path.starts_with(&root_path) {
            matches.push((root, root_path));
        }
    }
    if matches.len() != 1 {
        return None;
    }
    let (root, root_path) = matches.into_iter().next()?;
    let relative = path.strip_prefix(&root_path).ok()?;
    let relative_parts: Vec<String> = relative
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    let mut selected: Option<&LibraryPathRule> = None;
    for rule in &root.rules {
        let parts: Vec<&str> = rule.relative_path.split('/').collect();
        if relative_parts.len() >= parts.len()
            && relative_parts[..parts.len()]
                .iter()
                .zip(parts.iter())
                .all(|(a, b)| a == b)
        {
            selected = Some(rule);
        }
    }
    let relative_path = if relative_parts.is_empty() {
        ".".to_owned()
    } else {
        relative_parts.join("/")
    };
    Some((
        root.id.clone(),
        relative_path,
        selected.map(|rule| rule.policy).unwrap_or(root.policy),
    ))
}

/// Diff stored vs candidate settings into transition scopes, then
/// collapse (a whole-root scope swallows its rule scopes; dedupe by
/// root + relative path, sorted).
pub fn transition_scopes(
    current: &ResolvedLibraryPolicy,
    proposed: &ResolvedLibraryPolicy,
) -> Vec<TransitionScope> {
    let current_roots: BTreeMap<&str, &LibraryRoot> = current
        .settings
        .library_roots
        .iter()
        .map(|root| (root.id.as_str(), root))
        .collect();
    let proposed_roots: BTreeMap<&str, &LibraryRoot> = proposed
        .settings
        .library_roots
        .iter()
        .map(|root| (root.id.as_str(), root))
        .collect();
    let mut root_ids: BTreeSet<&str> = BTreeSet::new();
    root_ids.extend(current_roots.keys().copied());
    root_ids.extend(proposed_roots.keys().copied());
    let mut scopes: Vec<TransitionScope> = Vec::new();
    for root_id in root_ids {
        let old_root = current_roots.get(root_id).copied();
        let new_root = proposed_roots.get(root_id).copied();
        match (old_root, new_root) {
            (_, None) => {
                scopes.push(TransitionScope {
                    root_id: root_id.to_owned(),
                    scope_id: root_id.to_owned(),
                    relative_path: ".".to_owned(),
                    effective_policy: IdentificationPolicy::Excluded,
                });
            }
            (None, Some(new)) => {
                scopes.push(TransitionScope {
                    root_id: root_id.to_owned(),
                    scope_id: root_id.to_owned(),
                    relative_path: ".".to_owned(),
                    effective_policy: new.policy,
                });
            }
            (Some(old), Some(new)) => {
                if old.path != new.path || old.policy != new.policy {
                    scopes.push(TransitionScope {
                        root_id: root_id.to_owned(),
                        scope_id: root_id.to_owned(),
                        relative_path: ".".to_owned(),
                        effective_policy: new.policy,
                    });
                    continue;
                }
                let old_rules: BTreeMap<&str, &LibraryPathRule> = old
                    .rules
                    .iter()
                    .map(|rule| (rule.id.as_str(), rule))
                    .collect();
                let new_rules: BTreeMap<&str, &LibraryPathRule> = new
                    .rules
                    .iter()
                    .map(|rule| (rule.id.as_str(), rule))
                    .collect();
                let mut rule_ids: BTreeSet<&str> = BTreeSet::new();
                rule_ids.extend(old_rules.keys().copied());
                rule_ids.extend(new_rules.keys().copied());
                for rule_id in rule_ids {
                    let old_rule = old_rules.get(rule_id).copied();
                    let new_rule = new_rules.get(rule_id).copied();
                    if old_rule == new_rule {
                        continue;
                    }
                    if let Some(old) = old_rule
                        && (new_rule.is_none()
                            || old.relative_path
                                != new_rule
                                    .map(|rule| rule.relative_path.as_str())
                                    .unwrap_or(""))
                    {
                        let candidate = PathBuf::from(&new.path).join(&old.relative_path);
                        let policy = resolve_path(&proposed.settings, &candidate)
                            .map(|(_, _, policy)| policy)
                            .unwrap_or(new.policy);
                        scopes.push(TransitionScope {
                            root_id: root_id.to_owned(),
                            scope_id: rule_id.to_owned(),
                            relative_path: old.relative_path.clone(),
                            effective_policy: policy,
                        });
                    }
                    if let Some(new) = new_rule {
                        scopes.push(TransitionScope {
                            root_id: root_id.to_owned(),
                            scope_id: rule_id.to_owned(),
                            relative_path: new.relative_path.clone(),
                            effective_policy: new.policy,
                        });
                    }
                }
            }
        }
    }
    collapse_scopes(scopes)
}

/// Collapse transition scopes: whole-root scopes swallow rule scopes;
/// dedupe by (root, relative path); sorted.
pub fn collapse_scopes(scopes: Vec<TransitionScope>) -> Vec<TransitionScope> {
    let whole_roots: BTreeSet<String> = scopes
        .iter()
        .filter(|scope| scope.relative_path == ".")
        .map(|scope| scope.root_id.clone())
        .collect();
    let mut unique: BTreeMap<(String, String), TransitionScope> = BTreeMap::new();
    for scope in scopes {
        if whole_roots.contains(scope.root_id.as_str()) && scope.relative_path != "." {
            continue;
        }
        unique.insert((scope.root_id.clone(), scope.relative_path.clone()), scope);
    }
    unique.into_values().collect()
}

/// Scope counts: (indexed, on-disk) catalog files per (root, relative
/// path). On-disk counts indexed plus excluded files, like v2.
pub type ScopeCounts = HashMap<(String, String), (i64, i64)>;

/// Catalog files under a set of scopes, each file counted once however
/// many scopes cover it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScopeTotals {
    /// Indexed files.
    pub indexed: i64,
    /// Indexed plus excluded files.
    pub on_disk: i64,
    /// Every catalog row, missing files included.
    pub all: i64,
}

/// One root id the catalog holds rows for, with one sample row to
/// recover the root path from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogRoot {
    /// Root id on the rows.
    pub root_id: String,
    /// Absolute path of the sample row.
    pub sample_file_path: String,
    /// Root-relative path of the same row.
    pub sample_relative_path: String,
    /// Rows under the root.
    pub track_count: i64,
}

/// One catalog track path, for the path-mapping dry run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogPath {
    /// Track id.
    pub track_id: String,
    /// Absolute file path.
    pub file_path: String,
}

/// Read port over the library catalog. Settings reads catalog rows only
/// through this; the production adapter is `settings::library_catalog`.
pub trait LibraryPolicyCatalog: Send + Sync {
    /// Counts per scope. Every requested scope is in the result.
    fn scope_counts<'a>(
        &'a self,
        scopes: &'a [(String, String)],
    ) -> BoxFuture<'a, Result<ScopeCounts, String>>;
    /// Totals across scopes, each file counted once.
    fn scope_totals<'a>(
        &'a self,
        scopes: &'a [(String, String)],
    ) -> BoxFuture<'a, Result<ScopeTotals, String>>;
    /// Whether the catalog holds any tracks.
    fn has_tracks<'a>(&'a self) -> BoxFuture<'a, Result<bool, String>>;
    /// Every root id the catalog holds rows for, sorted by id.
    fn catalog_roots<'a>(&'a self) -> BoxFuture<'a, Result<Vec<CatalogRoot>, String>>;
    /// Every track path, ordered by track id.
    fn track_paths<'a>(&'a self) -> BoxFuture<'a, Result<Vec<CatalogPath>, String>>;
}

/// Build the policy tree: one node per root plus one per rule, counts
/// left empty. Checks each path on disk, so run it off the async workers.
pub fn policy_tree(resolved: &ResolvedLibraryPolicy) -> LibraryPolicyTreeResponse {
    let roots = resolved
        .settings
        .library_roots
        .iter()
        .map(|root| {
            let root_path = PathBuf::from(&root.path);
            let children = root
                .rules
                .iter()
                .map(|rule| LibraryPolicyTreeNode {
                    id: rule.id.clone(),
                    kind: PolicyNodeKind::Rule,
                    label: rule
                        .relative_path
                        .rsplit('/')
                        .next()
                        .unwrap_or(&rule.relative_path)
                        .to_owned(),
                    path: rule.relative_path.clone(),
                    policy: rule.policy,
                    inherited_from_id: Some(rule.id.clone()),
                    available: root_path.join(&rule.relative_path).exists(),
                    indexed_file_count: None,
                    on_disk_file_count: None,
                    children: Vec::new(),
                })
                .collect();
            LibraryPolicyTreeNode {
                id: root.id.clone(),
                kind: PolicyNodeKind::Root,
                label: root.label.clone(),
                path: root.path.clone(),
                policy: root.policy,
                inherited_from_id: Some(root.id.clone()),
                available: root_path.exists(),
                indexed_file_count: None,
                on_disk_file_count: None,
                children,
            }
        })
        .collect();
    LibraryPolicyTreeResponse {
        policy_revision: resolved.policy_revision.clone(),
        roots,
        warnings: resolved.warnings.clone(),
    }
}

/// The (root id, relative path) scope behind a tree node: `.` for a
/// root, the rule path for a rule.
fn node_scope(root_id: &str, node: &LibraryPolicyTreeNode) -> (String, String) {
    match node.kind {
        PolicyNodeKind::Root => (root_id.to_owned(), ".".to_owned()),
        PolicyNodeKind::Rule => (root_id.to_owned(), node.path.clone()),
    }
}

/// Every scope the tree shows.
pub fn tree_scopes(tree: &LibraryPolicyTreeResponse) -> Vec<(String, String)> {
    tree.roots
        .iter()
        .flat_map(|root| {
            std::iter::once(node_scope(&root.id, root)).chain(
                root.children
                    .iter()
                    .map(|child| node_scope(&root.id, child)),
            )
        })
        .collect()
}

/// Fill the tree's file counts from the catalog.
pub fn fill_tree_counts(tree: &mut LibraryPolicyTreeResponse, counts: &ScopeCounts) {
    fn fill(node: &mut LibraryPolicyTreeNode, root_id: &str, counts: &ScopeCounts) {
        let (indexed, on_disk) = counts
            .get(&node_scope(root_id, node))
            .copied()
            .unwrap_or((0, 0));
        node.indexed_file_count = Some(indexed);
        node.on_disk_file_count = Some(on_disk);
    }
    for root in &mut tree.roots {
        let root_id = root.id.clone();
        fill(root, &root_id, counts);
        for child in &mut root.children {
            fill(child, &root_id, counts);
        }
    }
}

/// Preview the impact of candidate settings against the saved ones.
/// Returns the response with counts left empty plus the affected scopes
/// to count.
pub fn preview_impact(
    current: &ResolvedLibraryPolicy,
    proposed: ResolvedLibraryPolicy,
    expected_policy_revision: Option<&str>,
) -> (LibraryPolicyImpactResponse, Vec<(String, String)>) {
    let scopes = transition_scopes(current, &proposed);
    let affected: Vec<String> = scopes
        .iter()
        .map(|scope| scope.scope_id.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let response = LibraryPolicyImpactResponse {
        current_policy_revision: current.policy_revision.clone(),
        proposed_policy_revision: proposed.policy_revision,
        stale: expected_policy_revision.is_some_and(|expected| expected != current.policy_revision),
        reconciliation_required: !affected.is_empty(),
        affected_scope_ids: affected,
        indexed_file_count: None,
        on_disk_file_count: None,
        content_will_become_unavailable: any_excluded(&scopes),
        queued_work_will_be_cancelled: scopes
            .iter()
            .any(|scope| scope.effective_policy != IdentificationPolicy::Automatic),
        warnings: proposed.warnings,
    };
    (response, scope_pairs(&scopes))
}

fn any_excluded(scopes: &[TransitionScope]) -> bool {
    scopes
        .iter()
        .any(|scope| scope.effective_policy == IdentificationPolicy::Excluded)
}

/// (root id, relative path) pairs for counting.
pub fn scope_pairs(scopes: &[TransitionScope]) -> Vec<(String, String)> {
    scopes
        .iter()
        .map(|scope| (scope.root_id.clone(), scope.relative_path.clone()))
        .collect()
}

/// The saved scopes a reconcile would cover: the named roots and rules,
/// or every root when none are named. A named root covers its rules.
/// Unknown ids are a 400.
pub fn apply_scopes(
    resolved: &ResolvedLibraryPolicy,
    scope_ids: &[String],
) -> Result<Vec<TransitionScope>, SettingsError> {
    let selected: BTreeSet<&str> = scope_ids.iter().map(String::as_str).collect();
    let mut scopes = Vec::new();
    for root in &resolved.settings.library_roots {
        if selected.is_empty() || selected.contains(root.id.as_str()) {
            scopes.push(TransitionScope {
                root_id: root.id.clone(),
                scope_id: root.id.clone(),
                relative_path: ".".to_owned(),
                effective_policy: root.policy,
            });
            continue;
        }
        for rule in &root.rules {
            if selected.contains(rule.id.as_str()) {
                scopes.push(TransitionScope {
                    root_id: root.id.clone(),
                    scope_id: rule.id.clone(),
                    relative_path: rule.relative_path.clone(),
                    effective_policy: rule.policy,
                });
            }
        }
    }
    if !selected.is_empty() && scopes.len() != selected.len() {
        return Err(SettingsError::InvalidInput {
            message: "One or more library policy scopes no longer exist.".to_owned(),
        });
    }
    Ok(scopes)
}

/// Build the apply preview for scopes picked by [`apply_scopes`]. Saves
/// never cancel queued work on v3, so that flag is always false.
pub fn apply_preview(
    resolved: &ResolvedLibraryPolicy,
    scope_ids: Vec<String>,
    scopes: &[TransitionScope],
    estimated_file_count: i64,
) -> LibraryPolicyApplyPreviewResponse {
    LibraryPolicyApplyPreviewResponse {
        policy_revision: resolved.policy_revision.clone(),
        scope_ids,
        estimated_file_count,
        content_will_become_unavailable: any_excluded(scopes),
        queued_work_was_cancelled_on_save: false,
    }
}

/// Recover a root's path from one of its rows: the file path minus the
/// root-relative path, cleaned. When the relative path is not plain
/// directory names or the file path does not end with it, fall back to
/// the file's directory, which the restore dialog lets the user correct.
/// None when the stored file path is not absolute.
fn recovered_root_path(root: &CatalogRoot) -> Option<PathBuf> {
    let file = Path::new(&root.sample_file_path);
    let relative = Path::new(&root.sample_relative_path);
    let plain = relative.components().next().is_some()
        && relative
            .components()
            .all(|component| matches!(component, Component::Normal(_)));
    let mut path = file.to_path_buf();
    if plain && file.ends_with(relative) {
        for _ in relative.components() {
            path.pop();
        }
    } else {
        path.pop();
    }
    clean_absolute(&path.to_string_lossy())
}

/// Roots the catalog holds rows for that the saved settings no longer
/// list, with their recovered paths. A root whose recovered path equals,
/// holds, or sits inside a configured root is left out: restoring it
/// would fail the overlap check, and its files already belong to that
/// root.
pub fn restorable_roots(
    settings: &TypedLibrary,
    catalog: &[CatalogRoot],
) -> Vec<LibraryRestorableRoot> {
    let configured_ids: BTreeSet<&str> = settings
        .library_roots
        .iter()
        .map(|root| root.id.as_str())
        .collect();
    let configured_paths: Vec<PathBuf> = settings
        .library_roots
        .iter()
        .filter_map(|root| clean_absolute(&root.path))
        .collect();
    catalog
        .iter()
        .filter(|root| !configured_ids.contains(root.root_id.as_str()))
        .filter_map(|root| {
            let path = recovered_root_path(root)?;
            if configured_paths
                .iter()
                .any(|configured| paths_overlap(&path, configured))
            {
                return None;
            }
            Some(LibraryRestorableRoot {
                root_id: root.root_id.clone(),
                path: path.to_string_lossy().into_owned(),
                indexed_file_count: root.track_count,
            })
        })
        .collect()
}

/// A label for a restored root: the directory name, numbered when a
/// root already uses it (casefolded, like the resolver's uniqueness
/// check).
fn restored_root_label(path: &str, used: &mut BTreeSet<String>) -> String {
    let base = Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "Library".to_owned());
    let mut label = base.clone();
    let mut number = 2;
    while used.contains(&label.to_lowercase()) {
        label = format!("{base} ({number})");
        number += 1;
    }
    used.insert(label.to_lowercase());
    label
}

/// The settings with every removed root put back as an automatic root
/// with no rules. `paths` overrides a recovered path by root id.
pub fn with_restored_roots(
    mut settings: TypedLibrary,
    restorable: &[LibraryRestorableRoot],
    paths: &BTreeMap<String, String>,
) -> Result<TypedLibrary, SettingsError> {
    if restorable.is_empty() {
        return Err(SettingsError::InvalidInput {
            message: "There are no removed library roots to restore.".to_owned(),
        });
    }
    let mut used: BTreeSet<String> = settings
        .library_roots
        .iter()
        .map(|root| root.label.to_lowercase())
        .collect();
    for root in restorable {
        let submitted = paths
            .get(&root.root_id)
            .map(|path| path.trim())
            .filter(|path| !path.is_empty())
            .unwrap_or(&root.path);
        // Label from the cleaned path; a relative override stays as sent so
        // the resolver rejects it with its own message.
        let path = clean_absolute(submitted)
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| submitted.to_owned());
        let label = restored_root_label(&path, &mut used);
        settings.library_roots.push(LibraryRoot {
            id: root.root_id.clone(),
            path,
            label,
            policy: IdentificationPolicy::Automatic,
            rules: Vec::new(),
        });
    }
    Ok(settings)
}

/// Dry run: map every catalog path to a saved root. Paths are cleaned
/// lexically, not resolved on disk, so the run stays one pass over the
/// rows.
pub fn path_mapping(
    resolved: &ResolvedLibraryPolicy,
    sources: Vec<CatalogPath>,
) -> LibraryPathMappingReport {
    let roots: Vec<PathBuf> = resolved
        .settings
        .library_roots
        .iter()
        .map(|root| PathBuf::from(&root.path))
        .collect();
    let (mut mapped, mut ambiguous, mut out_of_root) = (0_i64, 0_i64, 0_i64);
    let source_count = i64::try_from(sources.len()).unwrap_or(i64::MAX);
    let items = sources
        .into_iter()
        .map(|source| {
            let candidate = clean_absolute(&source.file_path);
            let matches = candidate.as_ref().map_or(0, |path| {
                roots.iter().filter(|root| path.starts_with(root)).count()
            });
            let mapping = match (&candidate, matches) {
                (Some(path), 1) => resolve_path(&resolved.settings, path),
                _ => None,
            };
            let mut item = LibraryPathMappingItem {
                source_kind: PathMappingSource::LibraryFile,
                source_id: source.track_id,
                absolute_path: source.file_path,
                root_id: None,
                relative_path: None,
                error: None,
            };
            match mapping {
                Some((root_id, relative_path, _)) => {
                    mapped += 1;
                    item.root_id = Some(root_id);
                    item.relative_path = Some(relative_path);
                }
                None if matches > 1 => {
                    ambiguous += 1;
                    item.error = Some(PathMappingError::Ambiguous);
                }
                None => {
                    out_of_root += 1;
                    item.error = Some(PathMappingError::OutOfRoot);
                }
            }
            item
        })
        .collect();
    LibraryPathMappingReport {
        policy_revision: resolved.policy_revision.clone(),
        source_count,
        mapped_count: mapped,
        ambiguous_count: ambiguous,
        out_of_root_count: out_of_root,
        blocking: ambiguous > 0 || out_of_root > 0,
        items,
    }
}

/// Build the GET view: normalized settings plus revision, the applied
/// reconciliation projection, and warnings. The policy was resolved from
/// a masked read, so the AcoustID key stays masked.
pub fn settings_response(resolved: Masked<ResolvedLibraryPolicy>) -> LibrarySettingsResponse {
    let policy_revision = resolved.policy_revision.clone();
    let warnings = resolved.warnings.clone();
    LibrarySettingsResponse {
        settings: resolved.map(|resolved| resolved.settings),
        policy_revision,
        reconciliation_required: false,
        reconciliation_state: "applied".to_owned(),
        pending_policy_revision: None,
        affected_scope_ids: Vec::new(),
        actions_applied: Vec::new(),
        warnings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog_root(id: &str, file: &str, relative: &str) -> CatalogRoot {
        CatalogRoot {
            root_id: id.to_owned(),
            sample_file_path: file.to_owned(),
            sample_relative_path: relative.to_owned(),
            track_count: 1,
        }
    }

    #[test]
    fn restorable_roots_skip_paths_a_configured_root_covers() {
        let settings = TypedLibrary {
            library_roots: vec![LibraryRoot {
                id: "new".to_owned(),
                path: "/music".to_owned(),
                label: "music".to_owned(),
                ..LibraryRoot::default()
            }],
            ..TypedLibrary::default()
        };
        let catalog = [
            catalog_root("same", "/music/A/one.flac", "A/one.flac"),
            catalog_root("inside", "/music/A/one.flac", "one.flac"),
            catalog_root("holds", "/one.flac", "one.flac"),
            catalog_root("apart", "/other/x/../B/two.flac", "B/two.flac"),
        ];
        let offered = restorable_roots(&settings, &catalog);
        assert_eq!(offered.len(), 1);
        assert_eq!(offered[0].root_id, "apart");
        assert_eq!(offered[0].path, "/other");
    }
}
