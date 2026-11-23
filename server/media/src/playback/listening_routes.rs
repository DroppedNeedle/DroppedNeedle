//! The listening-settings routes: each user's scrobble preferences and
//! ListenBrainz link.
//!
//! A ListenBrainz link is verified before it is stored, and only a verified
//! pair is stored. A saved visibility reaches the live presence feed at
//! once, and the personal-mix toggle feeds the requests side's approval
//! queue through [`MixGrants`]. A ListenBrainz export file the person
//! uploads lands in the cache folder and is read by the history import
//! job ([`crate::history`]).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::{
    Json,
    extract::{
        DefaultBodyLimit, FromRef, Multipart, State,
        multipart::{MultipartError, MultipartRejection},
    },
    http::{HeaderMap, StatusCode, header},
};
use tokio::io::AsyncWriteExt as _;

use droppedneedle_auth::{Role, User};
use droppedneedle_kernel::db::DbError;
use droppedneedle_kernel::error::{ApiError, Unavailable};
use droppedneedle_kernel::extract::ValidJson;
use droppedneedle_kernel::jobs::queue::JobError;
use droppedneedle_kernel::mount::{Routes, routes};
use droppedneedle_plugins::models::StatusMessage;
use droppedneedle_providers::listenbrainz::ListenBrainzVerifier;

use super::listening::{
    BoxFuture, LinkSaveError, ListenBrainzLink, ListeningStore, PrefsPatch, ScrobblePrefs,
};
use super::models::{
    LinkStatus, ListenBrainzConnectRequest, MixGrantState, ScrobblePreferences,
    ScrobblePreferencesUpdate,
};
use super::presence::PresenceRegistry;
use super::sqlite::off_worker;
use crate::history::archive::Export;
use crate::history::store::remove;
use crate::history::{HistoryStore, ListeningHistory, ListeningHistoryUpload};

/// The largest listening history upload accepted, in bytes.
const MAX_HISTORY_UPLOAD_BYTES: usize = 512 * (1 << 20);

/// The personal-mix standing grant, which the requests side keeps. A
/// trait because that side is built above this crate.
pub trait MixGrants: Send + Sync {
    /// The auto-request toggle changed value.
    fn on_toggle<'a>(&'a self, user_id: &'a str, role: Role, enabled: bool) -> BoxFuture<'a, ()>;
    /// Grant state for the preferences answer.
    fn state<'a>(
        &'a self,
        user_id: &'a str,
        role: Role,
        toggle_on: bool,
    ) -> BoxFuture<'a, MixGrantState>;
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
#[derive(Debug)]
pub enum ConnectError {
    /// No username was given. Usernames drive every per-user read; without
    /// one a "connected" account yields silently empty discovery.
    UsernameRequired,
    /// The upstream is rate-limiting.
    RateLimited,
    /// The credential did not check out.
    Rejected(String),
    /// The link failed to persist.
    StoreFailed(LinkSaveError),
}

/// A database failure as an answer: busy asks the caller to try again,
/// anything else is the fixed internal error.
fn store_error(error: DbError) -> ApiError {
    match error {
        DbError::Busy { .. } | DbError::LaneClosed => {
            ApiError::unavailable(Unavailable::ServerBusy, error)
        }
        other => ApiError::internal(other),
    }
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
        .save_listenbrainz(user_id, username, token)
        .await
        .map_err(ConnectError::StoreFailed)?;
    Ok(ListenBrainzLink {
        username: username.to_owned(),
    })
}

/// Save one user's preferences. A save that does not land changes nothing
/// else; a saved visibility applies to the live feed at once; only a real
/// change of the auto-request toggle reaches the grant, so sending an
/// unchanged full object back never queues a grant again.
pub async fn update_prefs(
    deps: &ListeningDeps,
    user_id: &str,
    role: Role,
    patch: &PrefsPatch,
) -> Result<ScrobblePrefs, DbError> {
    let before = deps.store.prefs(user_id).await;
    deps.store.update_prefs(user_id, patch).await?;
    if let Some(visibility) = patch.now_playing_visibility {
        deps.presence.set_visibility(user_id, visibility.as_str());
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
    tag = "listening",
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
    tag = "listening",
    path = "/api/v3/me/scrobble-preferences",
    request_body = ScrobblePreferencesUpdate,
    responses(
        (status = 200, description = "Scrobble preferences", body = ScrobblePreferences),
        (status = 400, description = "Unknown enum value"),
        (status = 401, description = "Not authenticated"),
        (status = 503, description = "The server is busy; try again"),
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
        Err(error) => Err(store_error(error)),
    }
}

/// Link the caller's ListenBrainz account. The credential verifies first;
/// only a verified pair stores.
#[utoipa::path(
    put,
    tag = "listening",
    path = "/api/v3/me/connections/listenbrainz",
    request_body = ListenBrainzConnectRequest,
    responses(
        (status = 200, description = "Link status", body = LinkStatus),
        (status = 400, description = "Username required or credential rejected"),
        (status = 401, description = "Not authenticated"),
        (status = 429, description = "ListenBrainz is rate-limiting"),
        (status = 503, description = "The server is busy; try again"),
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
        Err(ConnectError::StoreFailed(LinkSaveError::Db(error))) => Err(store_error(error)),
        Err(ConnectError::StoreFailed(error)) => Err(ApiError::internal(error)),
    }
}

/// Remove the caller's ListenBrainz link.
#[utoipa::path(
    delete,
    tag = "listening",
    path = "/api/v3/me/connections/listenbrainz",
    responses(
        (status = 200, description = "Disconnect receipt", body = StatusMessage),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "No link"),
        (status = 503, description = "The server is busy; try again"),
    )
)]
pub async fn disconnect_listenbrainz_route(
    State(deps): State<ListeningDeps>,
    User(user): User,
) -> Result<Json<StatusMessage>, ApiError> {
    if deps
        .store
        .delete_listenbrainz(user.user_id())
        .await
        .map_err(store_error)?
    {
        Ok(Json(StatusMessage {
            status: "ok".to_owned(),
            message: "Disconnected listenbrainz".to_owned(),
        }))
    } else {
        Err(ApiError::not_found())
    }
}

/// A job write refused: busy asks the caller to try again.
fn job_error(error: JobError) -> ApiError {
    match error {
        JobError::Db(error) => store_error(error),
        other => ApiError::internal(other),
    }
}

/// A form part that stopped part way: over the limit is 413, anything else
/// a cut-off upload.
fn unreadable_upload(error: MultipartError) -> ApiError {
    if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
        ApiError::too_large()
    } else {
        ApiError::invalid_input("The upload was cut off or malformed.")
    }
}

/// An upload's file until its import owns it. A refused or dropped upload
/// leaves nothing behind.
struct Landing(Option<PathBuf>);

impl Drop for Landing {
    fn drop(&mut self) {
        if let Some(path) = self.0.take()
            && let Err(error) = std::fs::remove_file(&path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(%error, "an unused listening history upload was not deleted");
        }
    }
}

/// Stream the first `file` part to `path`. False when the form has none.
async fn receive(mut multipart: Multipart, path: &Path) -> Result<bool, ApiError> {
    while let Some(mut field) = multipart.next_field().await.map_err(unreadable_upload)? {
        if field.name() != Some("file") {
            continue;
        }
        let mut file = tokio::fs::File::create(path)
            .await
            .map_err(ApiError::internal)?;
        while let Some(chunk) = field.chunk().await.map_err(unreadable_upload)? {
            file.write_all(&chunk).await.map_err(ApiError::internal)?;
        }
        file.sync_all().await.map_err(ApiError::internal)?;
        return Ok(true);
    }
    Ok(false)
}

/// Import a ListenBrainz export ZIP: the file the person downloaded from
/// ListenBrainz, up to 512 MB. It is read in the background; the answer
/// says where the caller's history stands. A new upload replaces one still
/// being read, and a listen already kept is never added twice.
#[utoipa::path(
    post,
    tag = "listening",
    path = "/api/v3/me/listening-history/upload",
    request_body(content = ListeningHistoryUpload, content_type = "multipart/form-data"),
    responses(
        (status = 202, description = "The file is being read", body = ListeningHistory),
        (status = 400, description = "No file, or not a ListenBrainz export"),
        (status = 401, description = "Not authenticated"),
        (status = 413, description = "Over 512 MB"),
        (status = 503, description = "The server is busy; try again"),
    )
)]
pub async fn upload_listening_history(
    State(history): State<Arc<HistoryStore>>,
    User(user): User,
    headers: HeaderMap,
    multipart: Result<Multipart, MultipartRejection>,
) -> Result<(StatusCode, Json<ListeningHistory>), ApiError> {
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    if declared.is_some_and(|length| length > MAX_HISTORY_UPLOAD_BYTES as u64) {
        return Err(ApiError::too_large());
    }
    let multipart =
        multipart.map_err(|_| ApiError::invalid_input("Send the file as a multipart form."))?;
    tokio::fs::create_dir_all(history.dir())
        .await
        .map_err(ApiError::internal)?;
    let (import_id, path) = history.new_upload(user.user_id());
    let mut landing = Landing(Some(path.clone()));
    if !receive(multipart, &path).await? {
        return Err(ApiError::invalid_input(
            "Choose a ListenBrainz export file.",
        ));
    }
    let months = off_worker(|| Export::open(&path).map(|export| export.months().len()));
    if !months.is_ok_and(|months| months > 0) {
        return Err(ApiError::invalid_input(
            "That file is not a ListenBrainz export.",
        ));
    }
    // Not deleted on a drop from here on: a request dropped while the import
    // starts may have started it. A file no import names is swept later.
    landing.0 = None;
    if let Err(error) = history.start_upload(user.user_id(), &import_id).await {
        remove(&path).await;
        return Err(job_error(error));
    }
    history.remove_unread(user.user_id()).await;
    tracing::info!(bytes = declared, "a listening history upload received");
    let status = history.status(user.user_id()).await.map_err(store_error)?;
    Ok((StatusCode::ACCEPTED, Json(status.into())))
}

/// Where the caller's listening history stands: their import, the listens
/// kept from ListenBrainz and their plays here.
#[utoipa::path(
    get,
    tag = "listening",
    path = "/api/v3/me/listening-history",
    responses(
        (status = 200, description = "Listening history", body = ListeningHistory),
        (status = 401, description = "Not authenticated"),
        (status = 503, description = "The server is busy; try again"),
    )
)]
pub async fn get_listening_history(
    State(history): State<Arc<HistoryStore>>,
    User(user): User,
) -> Result<Json<ListeningHistory>, ApiError> {
    let status = history.status(user.user_id()).await.map_err(store_error)?;
    Ok(Json(status.into()))
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
            connect_listenbrainz_route,
            disconnect_listenbrainz_route
        ))
}

/// The listening history routes, for any signed-in user. The upload takes
/// a body up to 512 MB.
pub fn history_routes<S>() -> Routes<S>
where
    S: Clone + Send + Sync + 'static,
    Arc<HistoryStore>: FromRef<S>,
{
    let (schemas, paths, upload) = routes!(upload_listening_history);
    let upload = upload.layer(DefaultBodyLimit::max(MAX_HISTORY_UPLOAD_BYTES));
    Routes::new()
        .user((schemas, paths, upload))
        .user(routes!(get_listening_history))
}
