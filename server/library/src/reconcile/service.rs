//! The reconciliation service: what the HTTP handlers call. Every method
//! blocks on the shared database, so callers run it off the async workers.

use std::collections::BTreeMap;

use super::groups::{self, build};
use super::models::{Group, GroupDetail, GroupPage, GroupState, Progress, ReconcileError};
use super::{reasons, store};
use crate::clock::LibraryClock as _;
use droppedneedle_kernel::db::{Db, Lane};

#[derive(Clone)]
pub struct Reconcile {
    db: Db,
    clock: std::sync::Arc<dyn droppedneedle_kernel::clock::Clock>,
}

impl Reconcile {
    pub fn new(db: Db, clock: std::sync::Arc<dyn droppedneedle_kernel::clock::Clock>) -> Self {
        Self { db, clock }
    }

    fn groups(&self) -> Result<Vec<Group>, ReconcileError> {
        let inputs = self.db.read_blocking("reconcile.inputs", store::inputs)?;
        Ok(build(&inputs))
    }

    fn group(&self, group_id: &str) -> Result<Group, ReconcileError> {
        self.groups()?
            .into_iter()
            .find(|group| group.id == group_id)
            .ok_or(ReconcileError::NotFound(reasons::GROUP_NOT_FOUND))
    }

    /// How many records were merged automatically, with open groups by
    /// state.
    pub fn progress(&self) -> Result<Progress, ReconcileError> {
        let merged = self
            .db
            .read_blocking("reconcile.progress", store::progress)?;
        let groups = self.groups()?;
        let count = |state: GroupState| groups.iter().filter(|g| g.state == state).count();
        Ok(Progress {
            automatically_resolved_count: merged,
            waiting_for_identity_count: count(GroupState::WaitingForIdentity),
            genuine_review_count: groups.iter().filter(|g| g.state.needs_review()).count(),
            provider_conflict_count: count(GroupState::ProviderConflict),
            ambiguous_credit_structure_count: count(GroupState::AmbiguousCreditStructure),
            same_name_only_count: count(GroupState::SameNameOnly),
        })
    }

    pub fn list(
        &self,
        limit: usize,
        cursor: Option<&str>,
        state: Option<GroupState>,
        search: Option<&str>,
    ) -> Result<GroupPage, ReconcileError> {
        groups::page(self.groups()?, limit, cursor, state, search)
    }

    pub fn detail(&self, group_id: &str) -> Result<GroupDetail, ReconcileError> {
        let group = self.group(group_id)?;
        let ids: Vec<String> = group.members.iter().map(|m| m.id.clone()).collect();
        let references = self
            .db
            .read_blocking("reconcile.references", |conn| store::references(conn, &ids))?;
        Ok(GroupDetail { group, references })
    }

    /// Mark a group's records as distinct people. `expected` must name
    /// every member at the revision the administrator saw.
    pub fn dismiss(
        &self,
        group_id: &str,
        expected: &BTreeMap<String, i64>,
        actor_user_id: &str,
    ) -> Result<usize, ReconcileError> {
        let group = self.group(group_id)?;
        if group.state == GroupState::ResolvedAutomatically {
            return Err(ReconcileError::Invalid(reasons::GROUP_RESOLVED));
        }
        let ids: Vec<String> = group.members.iter().map(|m| m.id.clone()).collect();
        if expected.len() != ids.len() || ids.iter().any(|id| !expected.contains_key(id)) {
            return Err(ReconcileError::Conflict(reasons::GROUP_STALE));
        }
        let now = self.clock.now_secs();
        self.db
            .write_blocking(Lane::Foreground, "reconcile.dismiss", |tx| {
                store::dismiss(tx, &ids, expected, actor_user_id, now)
            })
    }
}
