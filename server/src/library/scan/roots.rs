//! Library roots: the registry, policy resolution, and the stream seam.
//!
//! The root registry. The stream gateway takes the registry through
//! `Gateway::with_library_roots` and resolves bare local keys under the
//! [`StreamRootSeam`]'s primary music root; per-root
//! [`StreamRootSeam::resolve_key`] is unused because playback keys carry no
//! root id.
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

/// Registry of library roots plus the enabled flag. The policy revision is
/// an opaque string: any settings change bumps it, which supersedes running
/// scans through the coordinator checkpoint (v2 `policy_revision`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RootRegistry {
    roots: Vec<LibraryRoot>,
    enabled: bool,
    policy_revision: String,
}

impl RootRegistry {
    pub fn new(roots: Vec<LibraryRoot>, enabled: bool, policy_revision: &str) -> Self {
        Self {
            roots,
            enabled,
            policy_revision: policy_revision.to_owned(),
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn policy_revision(&self) -> &str {
        &self.policy_revision
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
                let mut scope = ScanScope::root(
                    &root.id,
                    &root.path.to_string_lossy(),
                    &self.policy_revision,
                );
                scope.effective_policy = root.policy;
                scope
            })
            .collect()
    }
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

    pub fn policy_revision(&self) -> &str {
        self.registry.policy_revision()
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

/// Root-resolution seam for the stream gateway.
///
/// With library roots wired (`Gateway::with_library_roots`), the gateway's
/// `sandboxed_path(key)` joins a bare key under
/// [`StreamRootSeam::primary_music_root`]. Per-root resolution through
/// [`StreamRootSeam::resolve_key`] is unused: playback keys carry no root
/// id yet, so the gateway cannot pick a root per key. Sandboxing mirrors `sandboxed_path` exactly: component
/// screening first, then canonicalize both sides so a symlink inside the
/// root cannot point outside it; unresolvable paths skip the prefix check
/// and fall through to the read, which reports them.
///
/// Do not construct paths by joining untrusted keys anywhere else; every
/// local-key join funnels through here.
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

    /// First non-excluded root, for callers that still take a single path
    /// (the `<root>/music` stand-in). Returns `None` when no usable
    /// root is configured, which callers must read as "local reads 404".
    pub fn primary_music_root(&self) -> Option<PathBuf> {
        self.registry
            .roots()
            .iter()
            .find(|root| root.policy != EffectivePolicy::Excluded)
            .map(|root| root.path.clone())
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
