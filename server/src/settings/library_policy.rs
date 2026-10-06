//! Library policy resolution: normalize-and-validate, content
//! revisions, the policy tree, and impact previews.
//!
//! Ports v2's `LibraryPolicyResolver` (root/rule normalization, warnings,
//! SHA-256 content revision) plus the pure parts of
//! `LibraryPolicyService` (transition scopes, collapse, tree, impact).
//! Catalog counts ride behind the [`LibraryPolicyCatalog`] port: the
//! production implementation counts `local_tracks` rows under each
//! scope. v2 split indexed vs indexed-plus-excluded rows; v3 stores no
//! excluded rows, so both counts read the rows under the scope.
//!
//! The revision hash is byte-compatible with v2 (`sort_keys`, compact
//! separators, ASCII-escaped JSON over the same payload), so a migrated
//! config keeps its revisions. Golden vectors minted from v2 pin this in
//! the settings tests.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use futures_util::future::BoxFuture;
use sha2::{Digest, Sha256};

use super::error::SettingsError;
use super::models::{
    LibraryPolicyImpactResponse, LibraryPolicyTreeNode, LibraryPolicyTreeResponse,
    LibrarySettingsResponse,
};
use crate::ids::IdGenerator;
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
            if canonical == *other_path
                || canonical.starts_with(other_path)
                || other_path.starts_with(&canonical)
            {
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
        rules.sort_by_key(|rule| rule.relative_path.matches('/').count());
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
    use std::collections::BTreeMap;
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
    let mut unique: std::collections::BTreeMap<(String, String), TransitionScope> =
        std::collections::BTreeMap::new();
    for scope in scopes {
        if whole_roots.contains(scope.root_id.as_str()) && scope.relative_path != "." {
            continue;
        }
        unique.insert((scope.root_id.clone(), scope.relative_path.clone()), scope);
    }
    unique.into_values().collect()
}

/// Scope counts: (indexed, on-disk) rows per (root, relative path).
pub type ScopeCounts = std::collections::HashMap<(String, String), (i64, i64)>;

/// Catalog counts port: rows under each (root, relative path) scope.
pub trait LibraryPolicyCatalog: Send + Sync {
    /// Count rows under each scope. Returns (indexed, on_disk) per scope;
    /// missing scopes count zero.
    fn scope_counts<'a>(
        &'a self,
        scopes: &'a [(String, String)],
    ) -> BoxFuture<'a, Result<ScopeCounts, String>>;
    /// Whether the catalog holds any tracks (the remove-every-root guard).
    fn has_tracks<'a>(&'a self) -> BoxFuture<'a, Result<bool, String>>;
}

/// Production catalog counts over `local_tracks`.
pub struct SqliteLibraryPolicyCatalog {
    /// Reader pool.
    pub pool: sqlx::SqlitePool,
}

impl LibraryPolicyCatalog for SqliteLibraryPolicyCatalog {
    fn scope_counts<'a>(
        &'a self,
        scopes: &'a [(String, String)],
    ) -> BoxFuture<'a, Result<std::collections::HashMap<(String, String), (i64, i64)>, String>>
    {
        Box::pin(async move {
            // v3 stores no excluded rows: indexed and on-disk read the
            // same rows-under-scope count.
            let mut out = std::collections::HashMap::new();
            let mut unique: BTreeSet<&(String, String)> = BTreeSet::new();
            unique.extend(scopes.iter());
            for (root_id, relative) in unique {
                let prefix = relative.trim_matches('/').to_owned();
                let count: i64 = if prefix.is_empty() || prefix == "." {
                    sqlx::query_scalar("SELECT COUNT(*) FROM local_tracks WHERE root_id = ?1")
                        .bind(root_id)
                        .fetch_one(&self.pool)
                        .await
                        .map_err(|cause| cause.to_string())?
                } else {
                    let escaped: String = prefix
                        .chars()
                        .flat_map(|ch| {
                            if ch == '%' || ch == '_' {
                                vec!['\\', ch]
                            } else {
                                vec![ch]
                            }
                        })
                        .collect();
                    let like = format!("{escaped}/%");
                    sqlx::query_scalar(
                        "SELECT COUNT(*) FROM local_tracks WHERE root_id = ?1
                         AND (relative_path = ?2 OR relative_path LIKE ?3 ESCAPE '\\')",
                    )
                    .bind(root_id)
                    .bind(&prefix)
                    .bind(&like)
                    .fetch_one(&self.pool)
                    .await
                    .map_err(|cause| cause.to_string())?
                };
                out.insert((root_id.clone(), relative.clone()), (count, count));
            }
            Ok(out)
        })
    }

    fn has_tracks<'a>(&'a self) -> BoxFuture<'a, Result<bool, String>> {
        Box::pin(async move {
            let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM local_tracks")
                .fetch_one(&self.pool)
                .await
                .map_err(|cause| cause.to_string())?;
            Ok(count > 0)
        })
    }
}

/// Build the policy tree: one node per root plus one per rule, counts
/// filled from the catalog port.
pub async fn policy_tree(
    resolved: &ResolvedLibraryPolicy,
    catalog: &dyn LibraryPolicyCatalog,
    ids: &dyn IdGenerator,
) -> Result<LibraryPolicyTreeResponse, SettingsError> {
    let mut scopes: Vec<(String, String)> = Vec::new();
    for root in &resolved.settings.library_roots {
        scopes.push((root.id.clone(), ".".to_owned()));
        for rule in &root.rules {
            scopes.push((root.id.clone(), rule.relative_path.clone()));
        }
    }
    let counts = catalog
        .scope_counts(&scopes)
        .await
        .map_err(|cause| SettingsError::internal(&cause, ids))?;
    let mut roots = Vec::new();
    for root in &resolved.settings.library_roots {
        let root_path = PathBuf::from(&root.path);
        let mut children = Vec::new();
        for rule in &root.rules {
            let (indexed, on_disk) = counts
                .get(&(root.id.clone(), rule.relative_path.clone()))
                .copied()
                .unwrap_or((0, 0));
            let label = rule
                .relative_path
                .rsplit('/')
                .next()
                .unwrap_or(&rule.relative_path)
                .to_owned();
            children.push(LibraryPolicyTreeNode {
                id: rule.id.clone(),
                kind: "rule".to_owned(),
                label,
                path: rule.relative_path.clone(),
                policy: rule.policy,
                inherited_from_id: Some(rule.id.clone()),
                available: root_path.join(&rule.relative_path).exists(),
                indexed_file_count: Some(indexed),
                on_disk_file_count: Some(on_disk),
                children: Vec::new(),
            });
        }
        let (indexed, on_disk) = counts
            .get(&(root.id.clone(), ".".to_owned()))
            .copied()
            .unwrap_or((0, 0));
        roots.push(LibraryPolicyTreeNode {
            id: root.id.clone(),
            kind: "root".to_owned(),
            label: root.label.clone(),
            path: root.path.clone(),
            policy: root.policy,
            inherited_from_id: Some(root.id.clone()),
            available: root_path.exists(),
            indexed_file_count: Some(indexed),
            on_disk_file_count: Some(on_disk),
            children,
        });
    }
    Ok(LibraryPolicyTreeResponse {
        policy_revision: resolved.policy_revision.clone(),
        roots,
        warnings: resolved.warnings.clone(),
    })
}

/// Preview the impact of candidate settings against the stored ones.
/// Without the pending-policy machinery (a library-engine follow-up),
/// reconciliation projects the applied state; the revision diff, scope
/// ids, and warnings are exact.
pub async fn preview_impact(
    current: &ResolvedLibraryPolicy,
    candidate: &TypedLibrary,
    expected_policy_revision: Option<&str>,
    catalog: &dyn LibraryPolicyCatalog,
    ids: &dyn IdGenerator,
) -> Result<LibraryPolicyImpactResponse, SettingsError> {
    let proposed = resolve(candidate)?;
    let scopes = transition_scopes(current, &proposed);
    let mut affected: Vec<String> = scopes.iter().map(|scope| scope.scope_id.clone()).collect();
    affected.sort();
    affected.dedup();
    let pairs: Vec<(String, String)> = scopes
        .iter()
        .map(|scope| (scope.root_id.clone(), scope.relative_path.clone()))
        .collect();
    let counts = catalog
        .scope_counts(&pairs)
        .await
        .map_err(|cause| SettingsError::internal(&cause, ids))?;
    let (mut indexed, mut on_disk) = (0_i64, 0_i64);
    for pair in &pairs {
        if let Some((index, disk)) = counts.get(pair) {
            indexed += index;
            on_disk += disk;
        }
    }
    Ok(LibraryPolicyImpactResponse {
        current_policy_revision: current.policy_revision.clone(),
        proposed_policy_revision: proposed.policy_revision.clone(),
        stale: expected_policy_revision.is_some_and(|expected| expected != current.policy_revision),
        reconciliation_required: !affected.is_empty(),
        affected_scope_ids: affected,
        indexed_file_count: Some(indexed),
        on_disk_file_count: Some(on_disk),
        content_will_become_unavailable: scopes
            .iter()
            .any(|scope| scope.effective_policy == IdentificationPolicy::Excluded),
        queued_work_will_be_cancelled: scopes
            .iter()
            .any(|scope| scope.effective_policy != IdentificationPolicy::Automatic),
        warnings: proposed.warnings,
    })
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
