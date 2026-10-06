//! The reconciliation service: what the HTTP handlers call. Every method
//! blocks on SQLite, so callers run it off the async workers.

use std::collections::BTreeMap;
use std::sync::Arc;

use rusqlite::TransactionBehavior;

use super::groups::{self, build};
use super::models::{Group, GroupDetail, GroupPage, GroupState, Progress, ReconcileError};
use super::{reasons, store};
use crate::library::clock::now_unix;
use crate::library::identify::sqlite::SqliteIdentifyStore;
use crate::library::wiring::LibrarySetup;

#[derive(Clone)]
pub struct Reconcile {
    store: Arc<SqliteIdentifyStore>,
}

impl Reconcile {
    pub fn new(setup: &LibrarySetup) -> Self {
        Self {
            store: setup.identify_store.clone(),
        }
    }

    fn groups(&self) -> Result<Vec<Group>, ReconcileError> {
        let inputs = self.store.with_connection(|conn| store::inputs(conn))?;
        Ok(build(&inputs))
    }

    fn group(&self, group_id: &str) -> Result<Group, ReconcileError> {
        self.groups()?
            .into_iter()
            .find(|group| group.id == group_id)
            .ok_or(ReconcileError::NotFound(reasons::GROUP_NOT_FOUND))
    }

    /// Where the reconciliation pass stands, with open groups by state.
    pub fn progress(&self) -> Result<Progress, ReconcileError> {
        let (job, merged) = self.store.with_connection(|conn| store::progress(conn))?;
        let groups = self.groups()?;
        let count = |state: GroupState| groups.iter().filter(|g| g.state == state).count();
        Ok(Progress {
            state: job
                .as_ref()
                .map_or_else(|| "idle".to_owned(), |job| job.state.clone()),
            completed_count: job.as_ref().map_or(0, |job| job.completed_count),
            expected_count: job.as_ref().map_or(0, |job| job.expected_count),
            automatically_resolved_count: merged,
            waiting_for_identity_count: count(GroupState::WaitingForIdentity),
            genuine_review_count: groups.iter().filter(|g| g.state.needs_review()).count(),
            provider_conflict_count: count(GroupState::ProviderConflict),
            ambiguous_credit_structure_count: count(GroupState::AmbiguousCreditStructure),
            same_name_only_count: count(GroupState::SameNameOnly),
            operation_job_id: job.map(|job| job.id),
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
            .store
            .with_connection(|conn| store::references(conn, &ids))?;
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
        let now = now_unix();
        self.store.with_connection(|conn| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let pairs = store::dismiss(&tx, &ids, expected, actor_user_id, now)?;
            tx.commit()?;
            Ok(pairs)
        })
    }
}
