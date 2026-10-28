//! Personal-mix auto-request approvals and the mix refresh.

use super::RequestsService;
use crate::requests::error::RequestsError;
use crate::requests::ledger::ApprovalState;
use crate::requests::models::{
    PersonalMixApprovalItem, PersonalMixApprovalsResponse, RefreshResponse,
};
use droppedneedle_auth::{Admin, Principal};

/// Reviewer stamp for approval decisions: acting id plus display name.
fn reviewer(principal: &Principal) -> (&str, Option<String>) {
    (
        principal.user_id(),
        Some(principal.username_display().to_owned()),
    )
}

/// How an admin's decision on a personal-mix grant ended. The routes
/// word the reply ([`crate::requests::http::mix_reply`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MixDecided {
    /// No grant or ask matched.
    NoMatch,
    /// The auto-request was approved.
    Approved,
    /// The auto-request was rejected.
    Rejected,
    /// The grant was revoked.
    Revoked,
}

impl RequestsService {
    /// Pending personal-mix approvals (admin).
    pub async fn mix_approvals(
        &self,
        _admin: &Admin,
    ) -> Result<PersonalMixApprovalsResponse, RequestsError> {
        let items = self
            .mixes
            .pending()
            .await?
            .into_iter()
            .map(|row| PersonalMixApprovalItem {
                user_id: row.user_id,
                user_name: row.user_name,
                requested_at: row.requested_at,
            })
            .collect::<Vec<_>>();
        Ok(PersonalMixApprovalsResponse {
            count: items.len() as u32,
            items,
        })
    }

    /// Approve one mix auto-request grant (admin).
    pub async fn approve_mix(
        &self,
        admin: &Admin,
        user_id: &str,
    ) -> Result<MixDecided, RequestsError> {
        if !self
            .mixes
            .decide(
                user_id,
                ApprovalState::Approved,
                reviewer(admin),
                self.now_epoch(),
            )
            .await?
        {
            return Ok(MixDecided::NoMatch);
        }
        Ok(MixDecided::Approved)
    }

    /// Reject one mix auto-request ask (admin).
    pub async fn reject_mix(
        &self,
        admin: &Admin,
        user_id: &str,
    ) -> Result<MixDecided, RequestsError> {
        if !self
            .mixes
            .decide(
                user_id,
                ApprovalState::Rejected,
                reviewer(admin),
                self.now_epoch(),
            )
            .await?
        {
            return Ok(MixDecided::NoMatch);
        }
        // A rejection also turns the user's toggle off.
        if let Some(mixer) = &self.mixer {
            mixer.clear_intent(user_id).await;
        }
        Ok(MixDecided::Rejected)
    }

    /// Revoke one mix auto-request grant (admin).
    pub async fn revoke_mix(
        &self,
        admin: &Admin,
        user_id: &str,
    ) -> Result<MixDecided, RequestsError> {
        if !self
            .mixes
            .revoke(user_id, reviewer(admin), self.now_epoch())
            .await?
        {
            return Ok(MixDecided::NoMatch);
        }
        if let Some(mixer) = &self.mixer {
            mixer.clear_intent(user_id).await;
        }
        Ok(MixDecided::Revoked)
    }

    /// Refresh one user's personal mix. The build runs in the background
    /// (a cold build waits on ListenBrainz pacing for minutes) and lands as
    /// a `personal_mix_refreshed` event. While it runs, repeat calls answer
    /// `already_running` instead of stacking builds.
    pub async fn refresh_personal_mix(
        &self,
        principal: &Principal,
    ) -> Result<RefreshResponse, RequestsError> {
        let Some(mixer) = &self.mixer else {
            return Err(RequestsError::Conflict {
                message: "Weekly Mix is not available on this server.".to_owned(),
            });
        };
        if !mixer.is_linked(principal.user_id()).await {
            return Err(RequestsError::InvalidInput {
                message: "Connect ListenBrainz first to build Your Weekly Mix".to_owned(),
            });
        }
        if !self.mixes.refresh_start(principal.user_id())? {
            return Ok(RefreshResponse {
                status: "already_running".to_owned(),
            });
        }
        mixer.spawn_refresh(principal.user_id().to_owned());
        Ok(RefreshResponse {
            status: "started".to_owned(),
        })
    }
}
