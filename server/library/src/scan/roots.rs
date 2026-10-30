//! Library roots: the registry, policy resolution, and the stream seam.
//!
//! The root registry. Playback and library downloads read it through
//! `stream::local_files`, which turns a catalog track's root id and
//! relative path into a file confined to the configured roots.
//! [`StreamRootSeam`] joins a caller-supplied key under a root instead;
//! production no longer calls it, since local stream keys are track ids.
//!
//! The policy surface: roots, their per-subpath rules, an enabled flag,
//! and a revision string. A path takes the policy of the deepest rule
//! that covers it, else its root's policy.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json;

use super::models::{EffectivePolicy, ScanScope};

/// One subpath rule inside a root: every path under `relative_path`
/// takes `policy` unless a deeper rule covers it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyRule {
    pub id: String,
    pub relative_path: String,
    pub policy: EffectivePolicy,
}

/// One library root: a stable id, a filesystem path, a policy, and the
/// subpath rules that override it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryRoot {
    pub id: String,
    pub path: PathBuf,
    pub policy: EffectivePolicy,
    pub rules: Vec<PolicyRule>,
}

impl LibraryRoot {
    pub fn new(id: &str, path: PathBuf, policy: EffectivePolicy) -> Self {
        Self {
            id: id.to_owned(),
            path,
            policy,
            rules: Vec::new(),
        }
    }

    pub fn with_rules(mut self, rules: Vec<PolicyRule>) -> Self {
        self.rules = rules;
        self
    }

    /// Policy for a path under this root: the deepest covering rule
    /// wins, else the root policy.
    pub fn policy_for(&self, path: &Path) -> EffectivePolicy {
        let Ok(relative) = path.strip_prefix(&self.path) else {
            return self.policy;
        };
        self.rules
            .iter()
            .filter(|rule| relative.starts_with(Path::new(&rule.relative_path)))
            .max_by_key(|rule| Path::new(&rule.relative_path).components().count())
            .map(|rule| rule.policy)
            .unwrap_or(self.policy)
    }
}

/// Registry of library roots plus the enabled flag. The fingerprint is an
/// opaque hash of the roots, their rules and the flag: any change to them
/// bumps it, which supersedes running scans through the coordinator
/// checkpoint. It is not the settings' policy revision the pages show.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RootRegistry {
    roots: Vec<LibraryRoot>,
    enabled: bool,
    fingerprint: String,
}

impl RootRegistry {
    pub fn new(roots: Vec<LibraryRoot>, enabled: bool, fingerprint: &str) -> Self {
        Self {
            roots,
            enabled,
            fingerprint: fingerprint.to_owned(),
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    pub fn roots(&self) -> &[LibraryRoot] {
        &self.roots
    }

    pub fn resolve(&self, root_id: &str) -> Option<&LibraryRoot> {
        self.roots.iter().find(|root| root.id == root_id)
    }

    /// Root paths keyed by root id, the map the coordinator hands the walker.
    pub fn root_paths(&self) -> HashMap<String, PathBuf> {
        self.roots
            .iter()
            .map(|root| (root.id.clone(), root.path.clone()))
            .collect()
    }

    /// Whole-root scopes for every non-excluded root (v2
    /// `LibraryAutomaticScanScheduler.scheduled_scopes`, root half). The
    /// excluded-root rule half lives in the scheduler and resolves against
    /// this same registry.
    pub fn scheduled_root_scopes(&self) -> Vec<ScanScope> {
        self.roots
            .iter()
            .filter(|root| root.policy != EffectivePolicy::Excluded)
            .map(|root| {
                let mut scope =
                    ScanScope::root(&root.id, &root.path.to_string_lossy(), &self.fingerprint);
                scope.effective_policy = root.policy;
                scope
            })
            .collect()
    }
}

/// One root's files that another root indexes instead: their real folders
/// overlap (the same folder, or one holds the other).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overlap {
    /// The root whose walk leaves the shared files alone.
    pub yields: String,
    /// The real folder of `yields`.
    pub from: PathBuf,
    /// The root the shared files are indexed under, with its policy and
    /// rules.
    pub to: String,
    /// The real folder of `to`, which `yields` does not walk into.
    pub folder: PathBuf,
}

impl Overlap {
    /// Where a file of `yields` at `relative` is indexed instead: the
    /// path below `to`, when the file lies in the shared folder.
    pub fn home(&self, relative: &str) -> Option<String> {
        let real = self.from.join(relative);
        let below = real.strip_prefix(&self.folder).ok()?;
        let below = below.to_str()?;
        (!below.is_empty()).then(|| below.to_owned())
    }
}

/// Every pair of roots whose real folders overlap, and which of the two
/// indexes the shared files, so each file is indexed once, under the
/// winner's policy and rules. A root not excluded at the shared folder
/// (by its own policy or its deepest rule covering that folder) wins over
/// one excluded there, as an excluded folder is only walked for its
/// included subfolders. Then the root that takes file changes wins (a root
/// reached through a link into another is
/// [`left_out`](crate::publish::paths::left_out) and takes none), so
/// automatic management keeps the files; otherwise the first saved wins.
/// Blocking: it resolves every root on disk.
pub fn overlaps(roots: &[LibraryRoot]) -> Vec<Overlap> {
    use crate::publish::paths::{ResolvedRoot, left_out};
    let resolved: Vec<ResolvedRoot> = roots
        .iter()
        .map(|root| ResolvedRoot::of(&root.id, &root.path))
        .collect();
    let left_out = left_out(&resolved);
    // A root's rules are written against its configured path, so the
    // shared folder is read there.
    let rank = |index: usize, shared: &Path| {
        let excluded = resolved[index]
            .real
            .as_deref()
            .and_then(|real| shared.strip_prefix(real).ok())
            .map(|below| roots[index].path.join(below))
            .is_none_or(|at| roots[index].policy_for(&at) == EffectivePolicy::Excluded);
        (excluded, left_out.contains(&resolved[index].id), index)
    };
    let mut found = Vec::new();
    for (mine, root) in resolved.iter().enumerate() {
        let Some(my_folder) = &root.real else {
            continue;
        };
        for (theirs, other) in resolved.iter().enumerate() {
            let Some(their_folder) = &other.real else {
                continue;
            };
            if theirs == mine
                || !(my_folder.starts_with(their_folder) || their_folder.starts_with(my_folder))
            {
                continue;
            }
            let shared = if my_folder.starts_with(their_folder) {
                my_folder
            } else {
                their_folder
            };
            if rank(theirs, shared) < rank(mine, shared) {
                found.push(Overlap {
                    yields: root.id.clone(),
                    from: my_folder.clone(),
                    to: other.id.clone(),
                    folder: their_folder.clone(),
                });
            }
        }
    }
    found
}

/// The [`overlaps`] in which `root_id` yields its shared files.
/// Blocking: it resolves every root on disk.
pub fn yielded_by(roots: &[LibraryRoot], root_id: &str) -> Vec<Overlap> {
    overlaps(roots)
        .into_iter()
        .filter(|overlap| overlap.yields == root_id)
        .collect()
}

/// Where a file of a yielding root at `relative` is indexed instead: the
/// winning root and the path below it.
pub fn yielded_home(yielded: &[Overlap], relative: &str) -> Option<(String, String)> {
    yielded.iter().find_map(|overlap| {
        overlap
            .home(relative)
            .map(|path| (overlap.to.clone(), path))
    })
}

/// Policy resolver over the registry: longest-prefix root match, then the
/// deepest covering rule inside it. Mirrors the v2
/// `LibraryPolicyResolver.resolve` contract (returns `None` for paths
/// under no root).
#[derive(Debug, Clone)]
pub struct PolicyResolver {
    registry: RootRegistry,
}

impl PolicyResolver {
    pub fn new(registry: RootRegistry) -> Self {
        Self { registry }
    }

    pub fn registry(&self) -> &RootRegistry {
        &self.registry
    }

    pub fn fingerprint(&self) -> &str {
        self.registry.fingerprint()
    }

    pub fn enabled(&self) -> bool {
        self.registry.enabled()
    }

    pub fn resolve(&self, path: &Path) -> Option<EffectivePolicy> {
        self.registry
            .roots()
            .iter()
            .filter(|root| path.starts_with(&root.path))
            .max_by_key(|root| root.path.components().count())
            .map(|root| root.policy_for(path))
    }
}

/// Why a stream key failed to resolve against the roots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootSeamError {
    UnknownRoot { root_id: String },
    Forbidden,
}

impl std::fmt::Display for RootSeamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RootSeamError::UnknownRoot { .. } => write!(f, "unknown library root"),
            RootSeamError::Forbidden => write!(f, "path escapes the library root"),
        }
    }
}

impl std::error::Error for RootSeamError {}

/// Joins a caller-supplied key under one root, refusing escapes.
///
/// Production resolves local files by catalog track id through
/// `stream::local_files` and does not call this. Sandboxing: component
/// screening first, then canonicalize both sides so a symlink inside the
/// root cannot point outside it; unresolvable paths skip the prefix check
/// and fall through to the read, which reports them.
#[derive(Debug, Clone)]
pub struct StreamRootSeam {
    registry: RootRegistry,
}

impl StreamRootSeam {
    pub fn new(registry: RootRegistry) -> Self {
        Self { registry }
    }

    /// Join `key` under root `root_id`, refusing anything that escapes it.
    pub fn resolve_key(&self, root_id: &str, key: &str) -> Result<PathBuf, RootSeamError> {
        let root = self
            .registry
            .resolve(root_id)
            .ok_or_else(|| RootSeamError::UnknownRoot {
                root_id: root_id.to_owned(),
            })?;
        if key.is_empty() || Path::new(key).is_absolute() {
            return Err(RootSeamError::Forbidden);
        }
        let mut path = root.path.clone();
        for component in Path::new(key).components() {
            match component {
                Component::Normal(part) => path.push(part),
                _ => return Err(RootSeamError::Forbidden),
            }
        }
        if let (Ok(canonical_root), Ok(resolved)) = (root.path.canonicalize(), path.canonicalize())
            && !resolved.starts_with(&canonical_root)
        {
            return Err(RootSeamError::Forbidden);
        }
        Ok(path)
    }
}

fn policy_name(policy: EffectivePolicy) -> &'static str {
    match policy {
        EffectivePolicy::LocalMetadata => "local_metadata",
        EffectivePolicy::Automatic => "automatic",
        EffectivePolicy::Excluded => "excluded",
    }
}

/// Stable policy-revision fingerprint for a root list, so settings saves
/// that change nothing do not supersede running scans.
pub fn fingerprint_roots(roots: &[LibraryRoot], enabled: bool) -> String {
    let canonical = serde_json::json!({
        "enabled": enabled,
        "roots": roots.iter().map(|root| serde_json::json!({
            "id": root.id,
            "path": root.path.to_string_lossy(),
            "policy": policy_name(root.policy),
            "rules": root.rules.iter().map(|rule| serde_json::json!({
                "id": rule.id,
                "relative_path": rule.relative_path,
                "policy": policy_name(rule.policy),
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    });
    let encoded = serde_json::to_string(&canonical).unwrap_or_default();
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in encoded.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> RootRegistry {
        RootRegistry::new(
            vec![
                LibraryRoot::new("a", PathBuf::from("/music/a"), EffectivePolicy::Automatic),
                LibraryRoot::new("x", PathBuf::from("/music/x"), EffectivePolicy::Excluded),
            ],
            true,
            "rev-1",
        )
    }

    #[test]
    fn scheduled_scopes_skip_excluded_roots() {
        let scopes = registry().scheduled_root_scopes();
        assert_eq!(scopes.len(), 1);
        assert_eq!(scopes[0].root_id, "a");
        assert_eq!(scopes[0].relative_path, ".");
    }

    #[test]
    fn seam_rejects_traversal_and_absolute_keys() {
        let seam = StreamRootSeam::new(registry());
        assert_eq!(
            seam.resolve_key("a", "../etc/passwd"),
            Err(RootSeamError::Forbidden)
        );
        assert_eq!(
            seam.resolve_key("a", "/etc/passwd"),
            Err(RootSeamError::Forbidden)
        );
        assert_eq!(seam.resolve_key("a", ""), Err(RootSeamError::Forbidden));
        assert!(matches!(
            seam.resolve_key("nope", "a.flac"),
            Err(RootSeamError::UnknownRoot { .. })
        ));
    }

    #[test]
    fn fingerprint_is_stable_and_sensitive() {
        let roots = registry().roots;
        let first = fingerprint_roots(&roots, true);
        assert_eq!(first, fingerprint_roots(&roots, true));
        assert_ne!(first, fingerprint_roots(&roots, false));
    }
}
