//! The listening-settings routes: each user's scrobble preferences and
//! ListenBrainz link.
//!
//! A ListenBrainz link is verified before it is stored, and only a verified
//! pair is stored. A saved visibility reaches the live presence feed at
//! once, and the personal-mix toggle feeds the requests side's approval
//! queue through [`MixGrants`].

use std::sync::Arc;

use axum::{
    Json,
    extract::{FromRef, State},
};

use droppedneedle_auth::{Role, User};
use droppedneedle_kernel::error::ApiError;
use droppedneedle_kernel::extract::ValidJson;
use droppedneedle_kernel::mount::{Routes, routes};
use droppedneedle_plugins::models::StatusMessage;
use droppedneedle_providers::listenbrainz::ListenBrainzVerifier;

use super::listening::{
    BoxFuture, ListenBrainzLink, ListenBrainzLinkStore as _, ListeningStore,
    NOW_PLAYING_VISIBILITIES, PRIMARY_SOURCES, PrefsPatch, ScrobblePrefs,
};
use super::models::{
    LinkStatus, ListenBrainzConnectRequest, ScrobblePreferences, ScrobblePreferencesUpdate,
};
use super::presence::PresenceRegistry;

/// The personal-mix standing grant, which the requests side keeps. A
/// trait because that side is built above this crate.
pub trait MixGrants: Send + Sync {
    /// The auto-request toggle changed value.
    fn on_toggle<'a>(&'a self, user_id: &'a str, role: Role, enabled: bool) -> BoxFuture<'a, ()>;
    /// Grant state for the preferences answer (`none`, `pending`,
    /// `approved`, `rejected` or `revoked`).
    fn state<'a>(&'a self, user_id: &'a str, role: Role, toggle_on: bool) -> BoxFuture<'a, String>;
}

/// What the listening-settings routes need.
#[derive(Clone)]
pub struct ListeningDeps {
    /// Preferences and links.
    pub store: ListeningStore,
    /// Checks a ListenBrainz credential before it is stored.
    pub verifier: Arc<dyn ListenBrainzVerifier>,
    /// The personal-mix standing grant.
    pub mix: Arc<dyn MixGrants>,
    /// The live presence feed a saved visibility applies to.
    pub presence: PresenceRegistry,
}

/// Every way a ListenBrainz connect can fail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectError {
    /// No username was given. Usernames drive every per-user read; without
    /// one a "connected" account yields silently empty discovery.
    UsernameRequired,
    /// The upstream is rate-limiting.
    RateLimited,
    /// The credential did not check out.
    Rejected(String),
    /// The link failed to persist. The message is log-only.
    StoreFailed(String),
}

/// Every way a preferences save can fail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrefsError {
    /// An enum field named an unknown value.
    InvalidValue(String),
}

/// Connect one user's ListenBrainz account: verify first, store only on
/// success. The username is required even with a token.
pub async fn connect_listenbrainz(
    deps: &ListeningDeps,
    user_id: &str,
    username: &str,
    token: &str,
) -> Result<ListenBrainzLink, ConnectError> {
    if username.trim().is_empty() {
        return Err(ConnectError::UsernameRequired);
    }
    let outcome = deps.verifier.verify(username, token).await;
    if outcome.rate_limited {
        return Err(ConnectError::RateLimited);
    }
    if !outcome.valid {
        return Err(ConnectError::Rejected(outcome.message));
    }
    deps.store
        .save(user_id, username, token)
        .await
        .map_err(|reason| {
            tracing::warn!("listenbrainz link save failed: {reason}");
            ConnectError::StoreFailed(reason)
        })?;
    Ok(ListenBrainzLink {
        username: username.to_owned(),
    })
}

/// Save one user's preferences. Enum fields are checked before anything
/// is written; a saved visibility applies to the live feed at once; only
/// a real change of the auto-request toggle reaches the grant, so sending
/// an unchanged full object back never queues a grant again.
pub async fn update_prefs(
    deps: &ListeningDeps,
    user_id: &str,
    role: Role,
    patch: &PrefsPatch,
) -> Result<ScrobblePrefs, PrefsError> {
    if let Some(source) = &patch.primary_music_source
        && !PRIMARY_SOURCES.contains(&source.as_str())
    {
        return Err(PrefsError::InvalidValue(format!(
            "primary_music_source must be one of {}",
            PRIMARY_SOURCES.join(", ")
        )));
    }
    if let Some(visibility) = &patch.now_playing_visibility
        && !NOW_PLAYING_VISIBILITIES.contains(&visibility.as_str())
    {
        return Err(PrefsError::InvalidValue(format!(
            "now_playing_visibility must be one of {}",
            NOW_PLAYING_VISIBILITIES.join(", ")
        )));
    }
    let before = deps.store.prefs(user_id).await;
    deps.store.update_prefs(user_id, patch).await;
    if let Some(visibility) = &patch.now_playing_visibility {
        deps.presence.set_visibility(user_id, visibility);
    }
    if let Some(enabled) = patch.auto_request_personal_mix
        && enabled != before.auto_request_personal_mix
    {
        deps.mix.on_toggle(user_id, role, enabled).await;
    }
    Ok(deps.store.prefs(user_id).await)
}

async fn prefs_response(deps: &ListeningDeps, user_id: &str, role: Role) -> ScrobblePreferences {
    let prefs = deps.store.prefs(user_id).await;
    ScrobblePreferences {
        scrobble_to_lastfm: prefs.scrobble_to_lastfm,
        scrobble_to_listenbrainz: prefs.scrobble_to_listenbrainz,
        navidrome_handles_external_scrobbles: prefs.navidrome_handles_external_scrobbles,
        primary_music_source: prefs.primary_music_source,
        now_playing_visibility: prefs.now_playing_visibility,
        auto_request_personal_mix: prefs.auto_request_personal_mix,
        auto_request_state: deps
            .mix
            .state(user_id, role, prefs.auto_request_personal_mix)
            .await,
    }
}

fn linked(link: ListenBrainzLink) -> LinkStatus {
    LinkStatus {
        service: "listenbrainz".to_owned(),
        enabled: true,
        username: link.username,
    }
}

/// Read the caller's scrobble preferences.
#[utoipa::path(
    get,
    tag = "plugins",
    path = "/api/v3/me/scrobble-preferences",
    responses(
        (status = 200, description = "Scrobble preferences", body = ScrobblePreferences),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn get_scrobble_preferences(
    State(deps): State<ListeningDeps>,
    User(user): User,
) -> Json<ScrobblePreferences> {
    Json(prefs_response(&deps, user.user_id(), user.role()).await)
}

/// Update the caller's scrobble preferences. Absent fields keep their
/// stored values.
#[utoipa::path(
    put,
    tag = "plugins",
    path = "/api/v3/me/scrobble-preferences",
    request_body = ScrobblePreferencesUpdate,
    responses(
        (status = 200, description = "Scrobble preferences", body = ScrobblePreferences),
        (status = 400, description = "Unknown enum value"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn update_scrobble_preferences(
    State(deps): State<ListeningDeps>,
    User(user): User,
    ValidJson(body): ValidJson<ScrobblePreferencesUpdate>,
) -> Result<Json<ScrobblePreferences>, ApiError> {
    let patch = PrefsPatch {
        scrobble_to_lastfm: body.scrobble_to_lastfm,
        scrobble_to_listenbrainz: body.scrobble_to_listenbrainz,
        navidrome_handles_external_scrobbles: body.navidrome_handles_external_scrobbles,
        primary_music_source: body.primary_music_source,
        now_playing_visibility: body.now_playing_visibility,
        auto_request_personal_mix: body.auto_request_personal_mix,
    };
    match update_prefs(&deps, user.user_id(), user.role(), &patch).await {
        Ok(_) => Ok(Json(
            prefs_response(&deps, user.user_id(), user.role()).await,
        )),
        Err(PrefsError::InvalidValue(message)) => Err(ApiError::invalid_input(message)),
    }
}

/// Link the caller's ListenBrainz account. The credential verifies first;
/// only a verified pair stores.
#[utoipa::path(
    put,
    tag = "plugins",
    path = "/api/v3/me/connections/listenbrainz",
    request_body = ListenBrainzConnectRequest,
    responses(
        (status = 200, description = "Link status", body = LinkStatus),
        (status = 400, description = "Username required or credential rejected"),
        (status = 401, description = "Not authenticated"),
        (status = 429, description = "ListenBrainz is rate-limiting"),
    )
)]
pub async fn connect_listenbrainz_route(
    State(deps): State<ListeningDeps>,
    User(user): User,
    ValidJson(body): ValidJson<ListenBrainzConnectRequest>,
) -> Result<Json<LinkStatus>, ApiError> {
    match connect_listenbrainz(&deps, user.user_id(), &body.username, &body.user_token).await {
        Ok(link) => Ok(Json(linked(link))),
        Err(ConnectError::UsernameRequired) => Err(ApiError::invalid_input(
            "A ListenBrainz username is required",
        )),
        Err(ConnectError::RateLimited) => Err(ApiError::rate_limited(60)),
        Err(ConnectError::Rejected(message)) => Err(ApiError::invalid_input(message)),
        Err(ConnectError::StoreFailed(reason)) => Err(ApiError::internal(reason)),
    }
}

/// Read the caller's ListenBrainz link status. Unlinked reads as missing.
#[utoipa::path(
    get,
    tag = "plugins",
    path = "/api/v3/me/connections/listenbrainz",
    responses(
        (status = 200, description = "Link status", body = LinkStatus),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "No link"),
    )
)]
pub async fn listenbrainz_status(
    State(deps): State<ListeningDeps>,
    User(user): User,
) -> Result<Json<LinkStatus>, ApiError> {
    match deps.store.status(user.user_id()).await {
        Some(link) => Ok(Json(linked(link))),
        None => Err(ApiError::not_found()),
    }
}

/// Remove the caller's ListenBrainz link.
#[utoipa::path(
    delete,
    tag = "plugins",
    path = "/api/v3/me/connections/listenbrainz",
    responses(
        (status = 200, description = "Disconnect receipt", body = StatusMessage),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "No link"),
    )
)]
pub async fn disconnect_listenbrainz_route(
    State(deps): State<ListeningDeps>,
    User(user): User,
) -> Result<Json<StatusMessage>, ApiError> {
    if deps.store.delete(user.user_id()).await {
        Ok(Json(StatusMessage {
            status: "ok".to_owned(),
            message: "Disconnected listenbrainz".to_owned(),
        }))
    } else {
        Err(ApiError::not_found())
    }
}

/// Every listening-settings route, for any signed-in user.
pub fn routes<S>() -> Routes<S>
where
    S: Clone + Send + Sync + 'static,
    ListeningDeps: FromRef<S>,
{
    Routes::new()
        .user(routes!(
            get_scrobble_preferences,
            update_scrobble_preferences
        ))
        .user(routes!(
            listenbrainz_status,
            connect_listenbrainz_route,
            disconnect_listenbrainz_route
        ))
}
