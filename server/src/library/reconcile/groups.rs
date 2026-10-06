//! The grouping rules: pure functions over the loaded inputs.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use sha2::{Digest as _, Sha256};

use super::models::{
    Candidate, Dismissal, Group, GroupInputs, GroupPage, GroupState, Member, ReconcileError,
};
use super::reasons;

/// Largest page of groups, as in v2.
pub const PAGE_MAX: usize = 100;

/// Every group, open ones from same-name records and resolved ones from
/// past automatic merges, sorted by name.
pub fn build(inputs: &GroupInputs) -> Vec<Group> {
    let dismissed: HashMap<(&str, &str), &Dismissal> = inputs
        .dismissals
        .iter()
        .map(|row| ((row.left_id.as_str(), row.right_id.as_str()), row))
        .collect();
    let ambiguous: HashSet<&str> = inputs
        .ambiguous_artist_ids
        .iter()
        .map(String::as_str)
        .collect();

    let mut by_name: BTreeMap<&str, Vec<&Candidate>> = BTreeMap::new();
    for candidate in &inputs.candidates {
        by_name
            .entry(candidate.folded_name.as_str())
            .or_default()
            .push(candidate);
    }

    let mut groups = Vec::new();
    for mut members in by_name.into_values() {
        if members.len() < 2 {
            continue;
        }
        members.sort_by(|a, b| {
            a.created_at
                .total_cmp(&b.created_at)
                .then_with(|| a.member.id.cmp(&b.member.id))
        });
        if all_pairs_dismissed(&members, &dismissed) {
            continue;
        }
        groups.push(open_group(&members, &ambiguous));
    }
    groups.extend(inputs.merges.iter().filter_map(|merge| {
        let ids = std::iter::once(&merge.survivor_id).chain(&merge.retired_ids);
        let members: Vec<Member> = ids
            .filter_map(|id| inputs.merged_artists.get(id).cloned())
            .collect();
        if members.len() < 2 {
            return None;
        }
        let display_name = members
            .iter()
            .find(|member| member.id == merge.survivor_id)
            .unwrap_or(&members[0])
            .name
            .clone();
        let affected = members
            .iter()
            .map(|member| {
                inputs
                    .merged_reference_totals
                    .get(&member.id)
                    .copied()
                    .unwrap_or(0)
            })
            .sum();
        Some(Group {
            id: format!("resolved:{}", merge.id),
            display_name,
            state: GroupState::ResolvedAutomatically,
            members,
            provider_mbids: merge.provider_mbid.iter().cloned().collect(),
            recommended_survivor_id: Some(merge.survivor_id.clone()),
            affected_reference_count: affected,
            reason_code: merge.reason_code.clone(),
            reason: reasons::for_code(&merge.reason_code),
            resolved_at: Some(merge.created_at),
        })
    }));
    groups.sort_by(|a, b| sort_key(a).cmp(&sort_key(b)));
    groups
}

/// True when every pair in the group was marked distinct at the revisions
/// the members have now. A changed member or a new record reopens it.
fn all_pairs_dismissed(
    members: &[&Candidate],
    dismissed: &HashMap<(&str, &str), &Dismissal>,
) -> bool {
    members.iter().enumerate().all(|(index, left)| {
        members[index + 1..].iter().all(|right| {
            let (low, high) = if left.member.id < right.member.id {
                (&left.member, &right.member)
            } else {
                (&right.member, &left.member)
            };
            dismissed
                .get(&(low.id.as_str(), high.id.as_str()))
                .is_some_and(|row| {
                    row.left_revision == low.row_revision && row.right_revision == high.row_revision
                })
        })
    })
}

fn open_group(members: &[&Candidate], ambiguous: &HashSet<&str>) -> Group {
    let direct: BTreeSet<&str> = members
        .iter()
        .filter_map(|c| c.member.provider_mbid.as_deref())
        .collect();
    let provider_mbids: BTreeSet<&str> = members
        .iter()
        .flat_map(|c| {
            c.member
                .provider_mbid
                .as_deref()
                .into_iter()
                .chain(c.proof_mbids.iter().map(String::as_str))
        })
        .filter(|mbid| !mbid.is_empty())
        .collect();
    let (state, reason) = if direct.len() > 1 {
        (
            GroupState::ProviderConflict,
            reasons::CONFLICTING_PROVIDER_IDENTITIES,
        )
    } else if members
        .iter()
        .any(|c| ambiguous.contains(c.member.id.as_str()))
    {
        (
            GroupState::AmbiguousCreditStructure,
            reasons::AMBIGUOUS_CREDIT_STRUCTURE,
        )
    } else if provider_mbids.len() > 1 {
        (
            GroupState::ProviderConflict,
            reasons::CONFLICTING_PROVIDER_IDENTITIES,
        )
    } else if !provider_mbids.is_empty() {
        (
            GroupState::WaitingForIdentity,
            reasons::INCOMPLETE_PROVIDER_PROOF,
        )
    } else {
        (
            GroupState::SameNameOnly,
            reasons::NAME_MATCH_WITHOUT_PROVIDER_PROOF,
        )
    };
    // The record that already carries a MusicBrainz identity survives a
    // merge; with only proofs to go on, the oldest record does.
    let survivor = members
        .iter()
        .find(|c| c.member.provider_mbid.is_some())
        .or_else(|| (!provider_mbids.is_empty()).then_some(&members[0]))
        .map(|c| c.member.id.clone());
    let members: Vec<Member> = members.iter().map(|c| c.member.clone()).collect();
    Group {
        id: group_id(&members),
        display_name: members[0].name.clone(),
        state,
        affected_reference_count: members.iter().map(|m| m.counts.affected()).sum(),
        provider_mbids: provider_mbids.into_iter().map(str::to_owned).collect(),
        recommended_survivor_id: survivor,
        members,
        reason_code: reason.code.to_owned(),
        reason,
        resolved_at: None,
    }
}

/// A stable id from the member ids, so the same records give the same
/// group across reads.
fn group_id(members: &[Member]) -> String {
    let mut ids: Vec<&str> = members.iter().map(|m| m.id.as_str()).collect();
    ids.sort_unstable();
    let digest = Sha256::digest(ids.join(":").as_bytes());
    let hex: String = digest[..16].iter().map(|b| format!("{b:02x}")).collect();
    format!("artists:{hex}")
}

fn sort_key(group: &Group) -> (String, &str) {
    (group.display_name.to_lowercase(), group.id.as_str())
}

/// The cursor after `group`: its sort key, so a page boundary survives
/// groups disappearing between reads.
fn cursor_for(group: &Group) -> String {
    let (name, id) = sort_key(group);
    format!("{name}|{id}")
}

/// One page of groups. The state counts cover every group, before the
/// state and search filters, as in v2.
pub fn page(
    groups: Vec<Group>,
    limit: usize,
    cursor: Option<&str>,
    state: Option<GroupState>,
    search: Option<&str>,
) -> Result<GroupPage, ReconcileError> {
    if !(1..=PAGE_MAX).contains(&limit) {
        return Err(ReconcileError::Invalid(reasons::LIMIT_INVALID));
    }
    let after = cursor
        .map(|raw| {
            raw.rsplit_once('|')
                .map(|(name, id)| (name.to_owned(), id.to_owned()))
                .ok_or(ReconcileError::Invalid(reasons::CURSOR_INVALID))
        })
        .transpose()?;
    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    for group in &groups {
        *counts.entry(group.state.as_str()).or_default() += 1;
    }
    let needle = search
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_lowercase);
    let filtered: Vec<Group> = groups
        .into_iter()
        .filter(|group| state.is_none_or(|state| group.state == state))
        .filter(|group| {
            needle.as_deref().is_none_or(|needle| {
                group.display_name.to_lowercase().contains(needle)
                    || group
                        .members
                        .iter()
                        .any(|member| member.name.to_lowercase().contains(needle))
            })
        })
        .collect();
    let total = filtered.len();
    let mut rest: Vec<Group> = filtered
        .into_iter()
        .filter(|group| {
            after.as_ref().is_none_or(|(name, id)| {
                let (group_name, group_id) = sort_key(group);
                (group_name.as_str(), group_id) > (name.as_str(), id.as_str())
            })
        })
        .collect();
    let has_more = rest.len() > limit;
    rest.truncate(limit);
    let next_cursor = if has_more {
        rest.last().map(cursor_for)
    } else {
        None
    };
    Ok(GroupPage {
        items: rest,
        next_cursor,
        has_more,
        total,
        counts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::reconcile::models::ReferenceCounts;

    fn candidate(id: &str, created_at: f64, mbid: Option<&str>, proofs: &[&str]) -> Candidate {
        Candidate {
            member: Member {
                id: id.into(),
                name: "Low".into(),
                sort_name: None,
                row_revision: 1,
                provider_mbid: mbid.map(Into::into),
                counts: ReferenceCounts::default(),
            },
            folded_name: "low".into(),
            created_at,
            proof_mbids: proofs.iter().map(|p| (*p).to_owned()).collect(),
        }
    }

    fn state_of(candidates: Vec<Candidate>) -> (GroupState, Option<String>) {
        let groups = build(&GroupInputs {
            candidates,
            ..GroupInputs::default()
        });
        assert_eq!(groups.len(), 1);
        (groups[0].state, groups[0].recommended_survivor_id.clone())
    }

    #[test]
    fn classifies_groups_like_v2() {
        let conflict = state_of(vec![
            candidate("a", 1.0, Some("m1"), &[]),
            candidate("b", 2.0, Some("m2"), &[]),
        ]);
        assert_eq!(conflict.0, GroupState::ProviderConflict);
        let proof_conflict = state_of(vec![
            candidate("a", 1.0, None, &["m1"]),
            candidate("b", 2.0, None, &["m2"]),
        ]);
        assert_eq!(proof_conflict.0, GroupState::ProviderConflict);
        let waiting = state_of(vec![
            candidate("a", 1.0, None, &[]),
            candidate("b", 2.0, Some("m1"), &["m1"]),
        ]);
        assert_eq!(
            waiting,
            (GroupState::WaitingForIdentity, Some("b".to_owned()))
        );
        let name_only = state_of(vec![
            candidate("a", 1.0, None, &[]),
            candidate("b", 2.0, None, &[]),
        ]);
        assert_eq!(name_only, (GroupState::SameNameOnly, None));
    }

    #[test]
    fn dismissal_holds_only_at_the_dismissed_revisions() {
        let mut inputs = GroupInputs {
            candidates: vec![
                candidate("a", 1.0, None, &[]),
                candidate("b", 2.0, None, &[]),
            ],
            dismissals: vec![Dismissal {
                left_id: "a".into(),
                right_id: "b".into(),
                left_revision: 1,
                right_revision: 1,
            }],
            ..GroupInputs::default()
        };
        assert!(build(&inputs).is_empty());
        inputs.candidates[1].member.row_revision = 2;
        assert_eq!(build(&inputs).len(), 1);
    }

    #[test]
    fn cursor_survives_a_group_disappearing() {
        let groups = |names: &[&str]| -> Vec<Group> {
            names
                .iter()
                .map(|name| {
                    let mut a = candidate(&format!("{name}-a"), 1.0, None, &[]);
                    let mut b = candidate(&format!("{name}-b"), 2.0, None, &[]);
                    for c in [&mut a, &mut b] {
                        c.folded_name = (*name).into();
                        c.member.name = (*name).into();
                    }
                    open_group(&[&a, &b], &HashSet::new())
                })
                .collect()
        };
        let first = page(groups(&["a", "b", "c"]), 1, None, None, None).unwrap();
        let cursor = first.next_cursor.unwrap();
        // "b" went away between reads; the next page starts at "c".
        let next = page(groups(&["a", "c"]), 1, Some(&cursor), None, None).unwrap();
        assert_eq!(next.items[0].display_name, "c");
        assert!(!next.has_more);
    }
}
