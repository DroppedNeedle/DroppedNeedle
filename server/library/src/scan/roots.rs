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
    /// wins, else the root policy. Rules match as [`rule_covers`] says.
    pub fn policy_for(&self, path: &Path) -> EffectivePolicy {
        let Ok(relative) = path.strip_prefix(&self.path) else {
            return self.policy;
        };
        let parts: Vec<String> = relative
            .components()
            .filter_map(|component| match component {
                Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect();
        deepest_rule(
            self.rules
                .iter()
                .map(|rule| (rule.relative_path.as_str(), rule.policy)),
            &parts,
            |policy| *policy,
        )
        .unwrap_or(self.policy)
    }
}

/// Whether a rule at `rule_path` (`/`-separated, relative to its root)
/// covers a path whose components below the root are `parts`: each of
/// the rule's components names the path's component at that depth,
/// exactly or once both are folded as collision keys fold them (Unicode
/// form and letter case), so a rule `Live` covers `live/a.flac` but not
/// `Lively/a.flac`. `Some(true)` when every component matched exactly.
pub fn rule_covers(rule_path: &str, parts: &[String]) -> Option<bool> {
    let rule: Vec<&str> = rule_path
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    if rule.is_empty() || rule.len() > parts.len() {
        return None;
    }
    let mut exact = true;
    for (wanted, part) in rule.iter().zip(parts) {
        if *wanted == part.as_str() {
            continue;
        }
        if fold(wanted) != fold(part) {
            return None;
        }
        exact = false;
    }
    Some(exact)
}

/// The value of the deepest rule among `rules` that covers `parts`; at
/// one depth a rule that matches exactly beats one that matches only once
/// folded, and two that tie take the stricter policy.
pub fn deepest_rule<'a, T>(
    rules: impl IntoIterator<Item = (&'a str, T)>,
    parts: &[String],
    policy_of: impl Fn(&T) -> EffectivePolicy,
) -> Option<T> {
    let mut best: Option<((usize, bool), T)> = None;
    for (path, value) in rules {
        let Some(exact) = rule_covers(path, parts) else {
            continue;
        };
        let rank = (
            path.split('/').filter(|part| !part.is_empty()).count(),
            exact,
        );
        let wins = match &best {
            None => true,
            Some((kept, held)) => {
                rank > *kept
                    || (rank == *kept
                        && strictness(policy_of(&value)) > strictness(policy_of(held)))
            }
        };
        if wins {
            best = Some((rank, value));
        }
    }
    best.map(|(_, value)| value)
}

/// How far a policy keeps files out: Excluded, then Local metadata, then
/// Automatic.
fn strictness(policy: EffectivePolicy) -> u8 {
    match policy {
        EffectivePolicy::Excluded => 2,
        EffectivePolicy::LocalMetadata => 1,
        EffectivePolicy::Automatic => 0,
    }
}

/// Whether the scope at `ancestor` covers the scope at `path`, both relative
/// to a root (`.` is the root). Folders match as [`rule_covers`] says.
pub fn scope_covers(ancestor: &str, path: &str) -> bool {
    if ancestor == "." {
        return true;
    }
    let parts: Vec<String> = path
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .map(str::to_owned)
        .collect();
    rule_covers(ancestor, &parts).is_some()
}

/// How long one folder listing may hold up the thread asking: scopes are
/// worked out on the scan and watcher threads, and a share can hang.
const LIST_DEADLINE: std::time::Duration = std::time::Duration::from_secs(2);
/// Listings that may run at once. One that hangs is left to end, so a hung
/// share holds these up and later asks fail at once instead of adding a thread.
const MAX_LISTINGS: usize = 4;
static LISTINGS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The real folders directly in `at`. A folder that is not there holds none.
fn list_folders(at: &Path) -> Result<Vec<String>, String> {
    use std::sync::atomic::Ordering;

    if LISTINGS.fetch_add(1, Ordering::SeqCst) >= MAX_LISTINGS {
        LISTINGS.fetch_sub(1, Ordering::SeqCst);
        return Err("earlier listings have not finished".to_owned());
    }
    let (send, answer) = std::sync::mpsc::channel();
    let folder = at.to_path_buf();
    let started = std::thread::Builder::new()
        .name("library-rule-listing".to_owned())
        .spawn(move || {
            let listed = read_folders(&folder);
            LISTINGS.fetch_sub(1, Ordering::SeqCst);
            // The asker may have stopped waiting; nobody needs the answer then.
            let _ = send.send(listed);
        });
    if let Err(error) = started {
        LISTINGS.fetch_sub(1, Ordering::SeqCst);
        return Err(error.to_string());
    }
    match answer.recv_timeout(LIST_DEADLINE) {
        Ok(listed) => listed.map_err(|error| error.to_string()),
        Err(_) => Err("the folder did not answer in time".to_owned()),
    }
}

/// Any error, on the folder or on one entry, fails the whole listing: part of
/// a listing cannot say which spelling exists.
fn read_folders(at: &Path) -> std::io::Result<Vec<String>> {
    let entries = match std::fs::read_dir(at) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        other => other?,
    };
    let mut folders = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_dir()
            && let Ok(name) = entry.file_name().into_string()
        {
            folders.push(name);
        }
    }
    Ok(folders)
}

/// Every folder on the disk a rule's path matches, then the rule's own
/// spelling when the disk does not have it. A rule `Live` matches `Live/` and
/// `live/` alike ([`rule_covers`]), so each is a scope of its own, and the
/// rule's spelling stays one while its folder goes by another: rows a
/// case-only rename left there are judged only by a scope at their path.
/// Links are never read through. When a folder on the way cannot be listed
/// only the rule's spelling is given, never a guess at a twin.
pub fn on_disk_rule_paths(root: &Path, rule_path: &str) -> Vec<String> {
    let wanted: Vec<&str> = rule_path
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    let as_written = wanted.join("/");
    let mut found: Vec<(std::path::PathBuf, Vec<String>)> = vec![(root.to_path_buf(), Vec::new())];
    for part in &wanted {
        let key = fold(part);
        let mut next = Vec::new();
        for (at, spelled) in &found {
            let names = match list_folders(at) {
                Ok(names) => names,
                Err(error) => {
                    tracing::warn!(
                        folder = %at.display(),
                        rule = rule_path,
                        %error,
                        "a library rule's folder could not be listed; using the rule as written"
                    );
                    return vec![as_written];
                }
            };
            for name in names.into_iter().filter(|name| fold(name) == key) {
                let mut spelled = spelled.clone();
                spelled.push(name.clone());
                next.push((at.join(&name), spelled));
            }
        }
        found = next;
    }
    let mut paths: Vec<String> = found
        .into_iter()
        .map(|(_, spelled)| spelled.join("/"))
        .collect();
    paths.sort();
    if !paths.contains(&as_written) {
        paths.push(as_written);
    }
    paths
}

/// Whether the scope at `relative` (below `root`) is on the disk only under
/// another spelling: its exact path is not a chain of real folders, and a real
/// folder that differs only in case exists at its full depth and holds a file.
/// A twin that is empty, or that stops short of the full path, says nothing:
/// the scope's own folder may just be away. The caller bounds the time. Any
/// error fails it: part of a listing proves nothing.
pub(super) fn renamed_to_a_twin(root: &Path, relative: &str) -> std::io::Result<bool> {
    let mut found = vec![(root.to_path_buf(), true)];
    for part in relative.split('/').filter(|part| !part.is_empty()) {
        let key = fold(part);
        let mut next = Vec::new();
        for (at, exact) in &found {
            for name in read_folders(at)? {
                if fold(&name) == key {
                    next.push((at.join(&name), *exact && name == part));
                }
            }
        }
        found = next;
    }
    if found.iter().any(|(_, exact)| *exact) {
        return Ok(false);
    }
    for (twin, _) in &found {
        if holds_a_file(twin)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Whether a regular file lies anywhere below `folder`, links not followed.
fn holds_a_file(folder: &Path) -> std::io::Result<bool> {
    let mut stack = vec![folder.to_path_buf()];
    while let Some(at) = stack.pop() {
        for entry in std::fs::read_dir(&at)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_file() {
                return Ok(true);
            }
            if kind.is_dir() {
                stack.push(entry.path());
            }
        }
    }
    Ok(false)
}

/// A name folded the way the publisher's collision keys fold it.
fn fold(name: &str) -> crate::publish::paths::CollisionKey {
    crate::publish::paths::collision_key("", name)
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

    /// Whole-root scopes for every non-excluded root. The
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
    /// The folders `to` itself yields to other roots.
    pub onward: Vec<PathBuf>,
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
                    onward: Vec::new(),
                });
            }
        }
    }
    let yielded_folders = |root: &str| -> Vec<PathBuf> {
        found
            .iter()
            .filter(|overlap| overlap.yields == root)
            .map(|overlap| overlap.folder.clone())
            .collect()
    };
    let onward: Vec<Vec<PathBuf>> = found
        .iter()
        .map(|overlap| yielded_folders(&overlap.to))
        .collect();
    for (overlap, folders) in found.iter_mut().zip(onward) {
        overlap.onward = folders;
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
/// winning root and the path below it. Among nested folders that is the
/// root that does not itself yield the file, as only that root has a row
/// for it. Should every winner yield it (each root losing a different
/// shared folder), the first answers, so the file still counts as skipped
/// and its row stays.
pub fn yielded_home(yielded: &[Overlap], relative: &str) -> Option<(String, String)> {
    let keeps = |overlap: &&Overlap| {
        let real = overlap.from.join(relative);
        !overlap.onward.iter().any(|folder| real.starts_with(folder))
    };
    let holding = || {
        yielded
            .iter()
            .filter(|overlap| overlap.home(relative).is_some())
    };
    let overlap = holding().find(keeps).or_else(|| holding().next())?;
    Some((overlap.to.clone(), overlap.home(relative)?))
}

/// Policy resolver over the registry: longest-prefix root match, then the
/// deepest covering rule inside it. Returns `None` for paths under no
/// root.
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

    /// A rule matches every real folder that differs only in case, then
    /// keeps its own spelling.
    #[test]
    fn a_rule_finds_each_spelling_at_every_depth() {
        let dir =
            droppedneedle_kernel::test_support::ScratchDir::new("rule-spellings").expect("scratch");
        std::fs::create_dir_all(dir.path().join("Shut").join("live")).expect("twin");
        assert_eq!(
            on_disk_rule_paths(dir.path(), "shut/Live"),
            ["Shut/live", "shut/Live"]
        );
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

    /// A rule covers whole path components, ignoring letter case and
    /// Unicode form; at one depth the rule that matches exactly wins, and
    /// of two stored rules that fold together the stricter policy wins,
    /// whichever is stored first.
    #[test]
    fn rules_match_folded_components() {
        let rule = |id: &str, path: &str, policy| PolicyRule {
            id: id.to_owned(),
            relative_path: path.to_owned(),
            policy,
        };
        let root = LibraryRoot::new("a", PathBuf::from("/music/a"), EffectivePolicy::Automatic)
            .with_rules(vec![
                rule("live", "Live", EffectivePolicy::Excluded),
                rule("cafe", "Cafe\u{301}/Disc", EffectivePolicy::LocalMetadata),
                rule("folded", "MIXED", EffectivePolicy::Excluded),
                rule("exact", "Mixed", EffectivePolicy::LocalMetadata),
            ]);
        let at = |relative: &str| root.policy_for(&Path::new("/music/a").join(relative));
        assert_eq!(at("Live/x.flac"), EffectivePolicy::Excluded);
        assert_eq!(at("live/x.flac"), EffectivePolicy::Excluded);
        assert_eq!(at("LIVE/deeper/x.flac"), EffectivePolicy::Excluded);
        assert_eq!(at("Lively/x.flac"), EffectivePolicy::Automatic);
        assert_eq!(at("x/live/x.flac"), EffectivePolicy::Automatic);
        assert_eq!(at("caf\u{e9}/disc/x.flac"), EffectivePolicy::LocalMetadata);
        assert_eq!(at("Mixed/x.flac"), EffectivePolicy::LocalMetadata);
        assert_eq!(at("MIXED/x.flac"), EffectivePolicy::Excluded);

        for (order, rules) in [
            (
                "automatic first",
                vec![
                    rule("automatic", "Live", EffectivePolicy::Automatic),
                    rule("excluded", "LIVE", EffectivePolicy::Excluded),
                ],
            ),
            (
                "excluded first",
                vec![
                    rule("excluded", "LIVE", EffectivePolicy::Excluded),
                    rule("automatic", "Live", EffectivePolicy::Automatic),
                ],
            ),
        ] {
            let root = LibraryRoot::new("a", PathBuf::from("/music/a"), EffectivePolicy::Automatic)
                .with_rules(rules);
            assert_eq!(
                root.policy_for(&Path::new("/music/a").join("live/x.flac")),
                EffectivePolicy::Excluded,
                "{order}"
            );
        }
    }

    /// Three nested folders, where the first saved overlap's winner itself
    /// yields the file: the file's home is the root that keeps it. The
    /// middle root is excluded at its own folder but includes `inner`.
    #[test]
    fn a_nested_file_goes_home_to_the_root_that_keeps_it() {
        let dir = droppedneedle_kernel::test_support::ScratchDir::new("nested-roots").expect("dir");
        let outer = dir.path().join("outer");
        let middle = outer.join("middle");
        let inner = middle.join("inner");
        std::fs::create_dir_all(&inner).unwrap();
        let roots = vec![
            LibraryRoot::new("middle", middle, EffectivePolicy::Excluded).with_rules(vec![
                PolicyRule {
                    id: "rule".to_owned(),
                    relative_path: "inner".to_owned(),
                    policy: EffectivePolicy::Automatic,
                },
            ]),
            LibraryRoot::new("outer", outer, EffectivePolicy::Automatic),
            LibraryRoot::new("inner", inner, EffectivePolicy::Automatic),
        ];
        assert_eq!(
            yielded_home(&yielded_by(&roots, "inner"), "a.flac"),
            Some(("outer".to_owned(), "middle/inner/a.flac".to_owned()))
        );
    }

    #[test]
    fn fingerprint_is_stable_and_sensitive() {
        let roots = registry().roots;
        let first = fingerprint_roots(&roots, true);
        assert_eq!(first, fingerprint_roots(&roots, true));
        assert_ne!(first, fingerprint_roots(&roots, false));
    }
}
