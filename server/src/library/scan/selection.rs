//! Scope selection for scans someone asks for by hand.
//!
//! A caller names roots and path rules by id; this turns those ids into
//! the scopes a run walks (v2 `_selected_scopes`). No ids means every
//! root. A rule whose root is also selected, or a rule inside another
//! selected rule, adds nothing: the wider scope already walks it.

use std::collections::BTreeSet;

use super::models::{ScanScope, scope_covers_path};
use super::roots::RootRegistry;

/// Some selected ids name no root or rule the scan engine knows. Holds
/// those ids, sorted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownScopes(pub Vec<String>);

/// Scopes for `scope_ids` over `registry`, ordered by root then depth,
/// with covered scopes dropped. Every scope carries the registry's
/// policy revision, so the run supersedes cleanly if the policy moves.
pub fn select_scopes(
    registry: &RootRegistry,
    scope_ids: &[String],
) -> Result<Vec<ScanScope>, UnknownScopes> {
    let selected: BTreeSet<&str> = scope_ids.iter().map(String::as_str).collect();
    let mut matched: BTreeSet<&str> = BTreeSet::new();
    let mut candidates: Vec<ScanScope> = Vec::new();
    for root in registry.roots() {
        let root_path = root.path.to_string_lossy();
        if selected.is_empty() || selected.contains(root.id.as_str()) {
            matched.insert(root.id.as_str());
            let mut scope = ScanScope::root(&root.id, &root_path, registry.policy_revision());
            scope.effective_policy = root.policy;
            candidates.push(scope);
        }
        for rule in &root.rules {
            if !selected.contains(rule.id.as_str()) {
                continue;
            }
            matched.insert(rule.id.as_str());
            candidates.push(ScanScope {
                root_id: root.id.clone(),
                scope_id: Some(rule.id.clone()),
                relative_path: rule.relative_path.clone(),
                root_path: Some(root_path.clone().into_owned()),
                effective_policy: rule.policy,
                policy_revision: registry.policy_revision().to_owned(),
                estimated_count: None,
            });
        }
    }
    if !selected.is_empty() && matched != selected {
        return Err(UnknownScopes(
            selected
                .difference(&matched)
                .map(|id| (*id).to_owned())
                .collect(),
        ));
    }
    candidates.sort_by(|left, right| {
        (
            left.root_id.as_str(),
            depth(&left.relative_path),
            &left.relative_path,
        )
            .cmp(&(
                right.root_id.as_str(),
                depth(&right.relative_path),
                &right.relative_path,
            ))
    });
    let mut scopes: Vec<ScanScope> = Vec::new();
    for candidate in candidates {
        let covered = scopes.iter().any(|kept| {
            kept.root_id == candidate.root_id
                && scope_covers_path(&kept.relative_path, &candidate.relative_path)
        });
        if !covered {
            scopes.push(candidate);
        }
    }
    Ok(scopes)
}

/// Path depth in components; the root scope `.` sorts first.
fn depth(relative_path: &str) -> usize {
    if relative_path == "." {
        0
    } else {
        relative_path
            .split('/')
            .filter(|part| !part.is_empty())
            .count()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::library::scan::models::EffectivePolicy;
    use crate::library::scan::roots::{LibraryRoot, PolicyRule};

    fn rule(id: &str, path: &str) -> PolicyRule {
        PolicyRule {
            id: id.to_owned(),
            relative_path: path.to_owned(),
            policy: EffectivePolicy::LocalMetadata,
        }
    }

    fn registry() -> RootRegistry {
        RootRegistry::new(
            vec![
                LibraryRoot::new("a", PathBuf::from("/music/a"), EffectivePolicy::Automatic)
                    .with_rules(vec![rule("a-live", "live"), rule("a-live-90s", "live/90s")]),
                LibraryRoot::new("b", PathBuf::from("/music/b"), EffectivePolicy::Excluded)
                    .with_rules(vec![rule("b-keep", "keep")]),
            ],
            true,
            "rev-1",
        )
    }

    fn picked(ids: &[&str]) -> Result<Vec<(String, String)>, UnknownScopes> {
        let ids: Vec<String> = ids.iter().map(|id| (*id).to_owned()).collect();
        select_scopes(&registry(), &ids).map(|scopes| {
            scopes
                .into_iter()
                .map(|scope| (scope.root_id, scope.relative_path))
                .collect()
        })
    }

    #[test]
    fn selection_matches_v2() {
        // No ids: every root walks whole, excluded ones too.
        assert_eq!(
            picked(&[]),
            Ok(vec![
                ("a".to_owned(), ".".to_owned()),
                ("b".to_owned(), ".".to_owned())
            ])
        );
        // A nested rule adds nothing under its selected parent rule, and
        // a rule adds nothing under its selected root.
        assert_eq!(
            picked(&["a-live-90s", "a-live", "b", "b-keep"]),
            Ok(vec![
                ("a".to_owned(), "live".to_owned()),
                ("b".to_owned(), ".".to_owned())
            ])
        );
        assert_eq!(
            picked(&["a-live", "gone"]),
            Err(UnknownScopes(vec!["gone".to_owned()]))
        );
    }
}
