//! "Get missing tracks": fetch what the album's chosen edition lacks and
//! upgrade the edition's tracks below the quality cutoff.
//!
//! v2 sent one track request per missing track. Here the library's edition
//! plan names the missing positions, and one album download on the chosen
//! release fetches just those files from the best whole-album source, the
//! way a person would search by hand. Owned tracks below the cutoff queue
//! one album upgrade, which replaces a file only with a strictly better
//! one. Existing files are never retagged.

use super::RequestsService;
use crate::acquire::downloads::upgrades::{UpgradeAsk, UpgradeOutcome};
use crate::acquire::edition::local_names;
use crate::acquire::flows::seams::DispatchKind;
use crate::acquire::requests::auth::Principal;
use crate::acquire::requests::dispatch::{
    DispatchError, DispatchOrigin, DispatchOutcome, DispatchRequest, DispatchTaskState,
};
use crate::acquire::requests::error::RequestsError;
use crate::acquire::requests::ledger::{EditionMark, now_epoch, validate_mbid};
use crate::acquire::requests::models::EditionAcquireResponse;
use crate::library::operations::models::OperationError;
use crate::library::operations::plan::EditionPlan;

/// What the edition still needs, from the library's plan.
struct Gap {
    release_mbid: String,
    total_tracks: u32,
    missing: Vec<(u32, u32)>,
    kept_track_ids: Vec<String>,
}

impl From<EditionPlan> for Gap {
    fn from(plan: EditionPlan) -> Self {
        Self {
            total_tracks: u32::try_from(plan.tracks.len()).unwrap_or(u32::MAX),
            missing: plan
                .missing()
                .map(|track| (track.disc, track.position))
                .collect(),
            kept_track_ids: plan
                .kept()
                .map(|file| file.local_track_id.clone())
                .collect(),
            release_mbid: plan.release_mbid,
        }
    }
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

impl RequestsService {
    /// Fill the chosen edition's missing tracks and upgrade its owned
    /// tracks below the cutoff (curator only, as in v2).
    pub async fn acquire_edition(
        &self,
        principal: &Principal,
        album_id: &str,
    ) -> Result<EditionAcquireResponse, RequestsError> {
        principal.require_curator()?;
        let mbid = validate_mbid(album_id)?;
        let now = now_epoch();
        if let Some(mark) = self.editions.get(&mbid).await? {
            let state = self
                .dispatch
                .task_state(&mark.task_id)
                .await
                .map_err(super::dispatch_fault)?;
            if state == DispatchTaskState::Active {
                return Ok(EditionAcquireResponse {
                    status: "already_in_progress".to_owned(),
                    message: "Missing tracks for this edition are already downloading.".to_owned(),
                    task_id: Some(mark.task_id),
                    ..EditionAcquireResponse::default()
                });
            }
            self.editions.clear(&mbid).await?;
        }
        self.quota
            .check_storage_admission(&principal.user_id, principal.role, DispatchOrigin::Edition)
            .await?;
        // The search needs the album's names; the plan says which edition
        // and which of its tracks.
        let Some((artist_name, title)) = local_names(self.library.pool(), &mbid)
            .await
            .map_err(|error| RequestsError::internal(&error))?
        else {
            return Err(RequestsError::InvalidInput {
                message: "This album is not in your library yet. Request the album instead."
                    .to_owned(),
            });
        };
        let gap = self.gap(&mbid).await?;
        let upgrades = match &gap {
            Some(gap) => self.below_cutoff(&gap.kept_track_ids).await,
            None => 0,
        };
        let upgrade_task_id = if upgrades > 0 {
            self.queue_upgrade(principal, &mbid, &artist_name, &title)
                .await
        } else {
            None
        };
        let mut response = EditionAcquireResponse {
            release_mbid: gap.as_ref().map(|gap| gap.release_mbid.clone()),
            total_tracks: gap.as_ref().map(|gap| gap.total_tracks),
            requested: gap.as_ref().map(|gap| count(gap.missing.len())),
            upgrades: gap.as_ref().map(|_| upgrades),
            upgrade_task_id,
            ..EditionAcquireResponse::default()
        };
        if gap.as_ref().is_some_and(|gap| gap.missing.is_empty()) {
            response.status = "already_complete".to_owned();
            response.message = if response.upgrade_task_id.is_some() {
                format!("Every track of this edition is here; {upgrades} will be upgraded.")
            } else {
                "Every track of this edition is already in your library.".to_owned()
            };
            return Ok(response);
        }
        let release_mbid = match &gap {
            Some(gap) => Some(gap.release_mbid.clone()),
            None => self.chosen_release(&mbid).await,
        };
        let request = DispatchRequest {
            user_id: principal.user_id.clone(),
            kind: "edition".to_owned(),
            key: mbid.clone(),
            artist_name,
            title,
            origin: DispatchOrigin::Edition,
            release_mbid,
            wanted: gap
                .as_ref()
                .map(|gap| gap.missing.clone())
                .unwrap_or_default(),
            // No stable per-decision key exists (marks clear on terminal,
            // so a key would pin re-acquires to the old task); the
            // in-progress mark above owns double-submit dedup.
            idempotency_key: None,
        };
        match self.dispatch.dispatch(&request).await {
            Err(DispatchError::Validation(message)) => Err(RequestsError::InvalidInput { message }),
            Err(DispatchError::Failed(cause)) => Err(RequestsError::internal(&format_args!(
                "edition dispatch failed: {cause}"
            ))),
            Ok(DispatchOutcome::AlreadyInLibrary) => {
                response.status = "already_complete".to_owned();
                response.message =
                    "Every track of this edition is already in your library.".to_owned();
                Ok(response)
            }
            Ok(DispatchOutcome::Dispatched { task_id }) => {
                self.editions
                    .set(EditionMark {
                        key: mbid,
                        task_id: task_id.clone(),
                        started_at: now,
                    })
                    .await?;
                response.status = "started".to_owned();
                response.message = match response.requested {
                    Some(1) => "Downloading the 1 missing track of this edition.".to_owned(),
                    Some(n) => format!("Downloading the {n} missing tracks of this edition."),
                    None => "Downloading this edition.".to_owned(),
                };
                response.task_id = Some(task_id);
                Ok(response)
            }
        }
    }

    /// The library's plan for the album's chosen edition. `None` when it
    /// cannot be read right now (MusicBrainz down, library not wired): the
    /// whole album is fetched then, and the landing skips what is owned.
    async fn gap(&self, release_group_mbid: &str) -> Result<Option<Gap>, RequestsError> {
        let Some(plans) = self.edition_plans.get() else {
            return Ok(None);
        };
        match plans.group_plan(release_group_mbid).await {
            Ok(Some(plan)) => Ok(Some(plan.into())),
            Ok(None) => Ok(None),
            Err(OperationError::Unavailable(cause)) => {
                tracing::warn!(release_group_mbid, %cause, "edition plan unavailable; fetching the whole album");
                Ok(None)
            }
            Err(OperationError::Store(cause)) => Err(RequestsError::internal(&cause)),
            Err(
                OperationError::NotFound(reason)
                | OperationError::Invalid(reason)
                | OperationError::Conflict(reason),
            ) => Err(RequestsError::InvalidInput {
                message: format!("{} {}", reason.message, reason.action),
            }),
        }
    }

    /// Owned edition tracks below the cutoff; 0 while upgrades are off or
    /// the qualities cannot be read.
    async fn below_cutoff(&self, track_ids: &[String]) -> u32 {
        let Some(upgrades) = self.upgrades.get() else {
            return 0;
        };
        match upgrades.tracks_below_cutoff(track_ids).await {
            Ok(below) => count(below.unwrap_or(0)),
            Err(error) => {
                tracing::warn!(%error, "track qualities unreadable; no upgrade queued");
                0
            }
        }
    }

    async fn queue_upgrade(
        &self,
        principal: &Principal,
        release_group_mbid: &str,
        artist: &str,
        title: &str,
    ) -> Option<String> {
        let upgrades = self.upgrades.get()?;
        let ask = UpgradeAsk {
            user_id: principal.user_id.clone(),
            kind: DispatchKind::Album,
            mbid: release_group_mbid.to_owned(),
            artist: artist.to_owned(),
            title: title.to_owned(),
        };
        match upgrades.request(ask).await {
            Ok(UpgradeOutcome::Queued(task_id)) => Some(task_id),
            Ok(UpgradeOutcome::Satisfied) => None,
            Err(error) => {
                tracing::warn!(release_group_mbid, %error, "edition upgrade not queued");
                None
            }
        }
    }
}
