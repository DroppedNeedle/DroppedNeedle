//! Quality upgrades: the albums below the quality cutoff, and asking for a
//! better copy of an album or a single track.
//!
//! An upgrade is an ordinary download with origin `upgrade`. When it lands,
//! a file replaces the library's copy only when it is strictly better, and
//! the old copy goes to the recycle bin. Asking for an album or track that
//! already meets the cutoff, or while upgrades are switched off, queues
//! nothing.

use std::sync::Arc;

use crate::dispatch::{
    DispatchOrigin, DispatchOutcome, DispatchRequest, DispatchTarget, DownloadDispatch,
};
use crate::flows::stores::{UpgradeItem, UpgradePolicy, UpgradeWorklist};
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::sections::DownloadPolicy;

/// Upgrade failures, mapped to HTTP statuses by the handlers.
#[derive(Debug, thiserror::Error)]
pub enum UpgradeError {
    /// The catalog or the download queue could not be reached.
    #[error("upgrades unavailable: {0}")]
    Unavailable(String),
}

/// The worklist as the page shows it.
#[derive(Debug, Clone)]
pub struct CutoffView {
    /// Empty while upgrades are switched off.
    pub items: Vec<UpgradeItem>,
    pub cutoff: String,
    pub upgrade_allowed: bool,
}

/// What an upgrade ask did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpgradeOutcome {
    /// A download was queued.
    Queued(String),
    /// Nothing to do: the copy meets the cutoff, upgrades are off, or the
    /// album is already downloading.
    Satisfied,
}

/// What to upgrade.
#[derive(Debug, Clone)]
pub struct UpgradeAsk {
    pub user_id: String,
    /// The album or the track.
    pub target: DispatchTarget,
    pub artist: String,
    pub title: String,
}

/// Upgrades over the catalog worklist and the download dispatch.
#[derive(Clone)]
pub struct Upgrades {
    worklist: UpgradeWorklist,
    policy: Live<DownloadPolicy>,
    dispatch: Arc<DownloadDispatch>,
}

impl Upgrades {
    pub fn new(
        worklist: UpgradeWorklist,
        policy: Live<DownloadPolicy>,
        dispatch: Arc<DownloadDispatch>,
    ) -> Self {
        Self {
            worklist,
            policy,
            dispatch,
        }
    }

    /// The upgrade policy as saved right now; upgrades off when it does
    /// not read.
    fn policy(&self) -> UpgradePolicy {
        UpgradePolicy::from_policy(&self.policy.read().unwrap_or_default())
    }

    /// Albums below the cutoff, worst first. Empty while upgrades are off:
    /// the list is an upgrade surface, not a quality report.
    pub async fn cutoff_unmet(&self) -> Result<CutoffView, UpgradeError> {
        let policy = self.policy();
        let items = if policy.upgrade_allowed {
            self.worklist
                .list_cutoff_unmet(&policy.cutoff)
                .await
                .map_err(|error| UpgradeError::Unavailable(error.to_string()))?
        } else {
            Vec::new()
        };
        Ok(CutoffView {
            items,
            cutoff: policy.cutoff,
            upgrade_allowed: policy.upgrade_allowed,
        })
    }

    /// Queue a better copy of an album or a track.
    pub async fn request(&self, ask: UpgradeAsk) -> Result<UpgradeOutcome, UpgradeError> {
        let policy = self.policy();
        if !policy.upgrade_allowed {
            return Ok(UpgradeOutcome::Satisfied);
        }
        let meets = match &ask.target {
            DispatchTarget::Album { release_group_mbid } => {
                self.worklist
                    .album_meets_cutoff(release_group_mbid, &policy.cutoff)
                    .await
            }
            DispatchTarget::Track { recording_mbid, .. } => {
                self.worklist
                    .recording_meets_cutoff(recording_mbid, &policy.cutoff)
                    .await
            }
        }
        .map_err(|error| UpgradeError::Unavailable(error.to_string()))?;
        if meets {
            return Ok(UpgradeOutcome::Satisfied);
        }
        let request = DispatchRequest {
            user_id: ask.user_id,
            target: ask.target,
            artist_name: ask.artist,
            title: ask.title,
            origin: DispatchOrigin::Upgrade,
            release_mbid: None,
            wanted: Vec::new(),
            planned_album: None,
            idempotency_key: None,
        };
        match self.dispatch.dispatch(&request).await {
            Ok(DispatchOutcome::Dispatched { task_id }) => Ok(UpgradeOutcome::Queued(task_id)),
            Ok(DispatchOutcome::Active { .. }) => Ok(UpgradeOutcome::Satisfied),
            Err(cause) => Err(UpgradeError::Unavailable(cause.to_string())),
        }
    }
}
