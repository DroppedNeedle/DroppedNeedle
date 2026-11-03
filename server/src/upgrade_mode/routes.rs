//! The upgrade page's routes, served only in upgrade mode. The normal app
//! never mounts them, so there they answer 404.
//!
//! The state read and sign-in are public; every other route takes the
//! upgrade session ([`UpgradeSession`]). An unsigned caller sees the state
//! value, the 503s and the 401, nothing else: counts, names, addresses,
//! paths and failure text are behind the session.

use std::net::SocketAddr;

use axum::{
    Json,
    extract::{ConnectInfo, FromRequestParts, State},
    http::{HeaderMap, StatusCode, Uri, request::Parts},
    response::{IntoResponse as _, Response},
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use droppedneedle_auth::{
    routes::native::PeerAddr,
    session::{
        client_ip::client_ip,
        cookies::{UPGRADE_COOKIE, push_set_cookie, secure_for_sign_in},
        rate_limit::{
            AUTH_FAILURE_CLASS, LOGIN_CLASS, LOGIN_USERNAME_CLASS, address_key, rate_limited,
        },
        stated::StatedAddresses,
    },
};
use droppedneedle_kernel::{
    error::{ApiError, Unavailable},
    extract::ValidJson,
    mount::{OwnCredential, Routes, routes},
};

use super::{
    ContinueRefused, Refused, UpgradeLayout, UpgradeMode, UpgradeRun, runner::Ending, runner::Task,
    session_max_age, sign_in::CheckError,
};
use crate::bootstrap::upgrade_progress::{Address, CallbackFeature, CheckResult, FailedLine};

/// The one answer to a sign-in that did not pass, whatever was wrong.
const SIGN_IN_REFUSED: &str = "That sign-in did not work";

/// The upgrade page's routes.
pub fn routes() -> Routes<UpgradeMode> {
    Routes::new()
        .public(routes!(upgrade_state))
        .public(routes!(upgrade_sign_in))
        .own_credential(OwnCredential::UpgradeSession, routes!(upgrade_preflight))
        .own_credential(OwnCredential::UpgradeSession, routes!(upgrade_start))
        .own_credential(OwnCredential::UpgradeSession, routes!(upgrade_fresh))
        .own_credential(OwnCredential::UpgradeSession, routes!(upgrade_progress))
        .own_credential(OwnCredential::UpgradeSession, routes!(upgrade_undo))
        .own_credential(OwnCredential::UpgradeSession, routes!(upgrade_continue))
}

/// The routes' entries in the API document.
pub fn document() -> utoipa::openapi::OpenApi {
    routes().openapi()
}

/// A caller signed in to the upgrade page: its cookie names a session this
/// upgrade mode started. Never a `Principal`, and no v3 session passes.
pub struct UpgradeSession;

impl FromRequestParts<UpgradeMode> for UpgradeSession {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        mode: &UpgradeMode,
    ) -> Result<Self, Self::Rejection> {
        if UPGRADE_COOKIE
            .read(&parts.headers)
            .is_some_and(|token| mode.sign_in().holds(&token))
        {
            return Ok(Self);
        }
        let peer = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|info| info.0);
        let outcome = mode
            .limits()
            .check(AUTH_FAILURE_CLASS, &caller_key(mode, peer, &parts.headers));
        Err(if outcome.allowed {
            ApiError::unauthorized_without_challenge("Sign in to the upgrade page first")
        } else {
            rate_limited(AUTH_FAILURE_CLASS, outcome.retry_after_secs)
        })
    }
}

/// The rate limiter's key for the client behind `peer`.
fn caller_key(mode: &UpgradeMode, peer: Option<SocketAddr>, headers: &HeaderMap) -> String {
    match peer {
        Some(peer) => address_key(client_ip(peer, headers, mode.trusted_proxies())),
        None => "ip:unknown".to_owned(),
    }
}

/// Where the upgrade stands, for anyone.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct UpgradeStateBody {
    /// Where the upgrade stands.
    pub layout: UpgradeLayout,
    /// Whether this caller's upgrade sign-in is valid.
    pub signed_in: bool,
}

/// Where the upgrade stands, and whether this caller is signed in to the
/// upgrade page.
#[utoipa::path(
    get,
    tag = "upgrade",
    path = "/api/v3/upgrade/state",
    responses((status = 200, description = "Where the upgrade stands", body = UpgradeStateBody))
)]
pub async fn upgrade_state(
    State(mode): State<UpgradeMode>,
    headers: HeaderMap,
) -> Json<UpgradeStateBody> {
    let signed_in = UPGRADE_COOKIE
        .read(&headers)
        .is_some_and(|token| mode.sign_in().holds(&token));
    Json(UpgradeStateBody {
        layout: mode.layout(),
        signed_in,
    })
}

/// How to sign in to the upgrade page.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum UpgradeSignInBody {
    /// A v2 admin's username and password.
    Password {
        /// The v2 username.
        username: String,
        /// The v2 password.
        password: String,
    },
    /// The one-time code from the server's log.
    Code {
        /// The code, with or without its dashes.
        code: String,
    },
}

/// Sign in to the upgrade page with a v2 admin's password or the one-time
/// code from the server's log. Attempts are limited per address, and
/// passwords per username too.
#[utoipa::path(
    post,
    tag = "upgrade",
    path = "/api/v3/upgrade/sign-in",
    request_body = UpgradeSignInBody,
    responses(
        (status = 200, description = "Signed in; the upgrade cookie is set", body = UpgradeStateBody),
        (status = 401, description = "The sign-in did not pass"),
        (status = 429, description = "Too many attempts from this address or for this username")
    )
)]
pub async fn upgrade_sign_in(
    State(mode): State<UpgradeMode>,
    uri: Uri,
    headers: HeaderMap,
    peer: PeerAddr,
    ValidJson(body): ValidJson<UpgradeSignInBody>,
) -> Result<Response, ApiError> {
    let address = mode
        .limits()
        .check(LOGIN_CLASS, &caller_key(&mode, peer.0, &headers));
    if !address.allowed {
        return Err(rate_limited(LOGIN_CLASS, address.retry_after_secs));
    }
    let passed = match body {
        UpgradeSignInBody::Password { username, password } => {
            let budget = mode.limits().check_login(&username);
            if !budget.allowed {
                return Err(rate_limited(LOGIN_USERNAME_CLASS, budget.retry_after_secs));
            }
            mode.sign_in().password(&username, password).await
        }
        UpgradeSignInBody::Code { code } => mode.sign_in().code(&code),
    }
    .map_err(check_failed)?;
    if !passed {
        return Err(ApiError::unauthorized_without_challenge(SIGN_IN_REFUSED));
    }
    let token = mode.sign_in().start_session().map_err(check_failed)?;
    let mark = secure_for_sign_in(
        uri.scheme_str().unwrap_or("http"),
        &headers,
        mode.trusted_proxies().is_trusted(peer.0),
        &StatedAddresses::default(),
    );
    let mut response = Json(UpgradeStateBody {
        layout: mode.layout(),
        signed_in: true,
    })
    .into_response();
    for line in UPGRADE_COOKIE.set(&token, &cookie_path(&mode), session_max_age(), mark) {
        push_set_cookie(response.headers_mut(), &line);
    }
    Ok(response)
}

/// The upgrade cookie's path: the upgrade routes alone.
fn cookie_path(mode: &UpgradeMode) -> String {
    format!("{}/api/v3/upgrade", mode.base_path().trim_end_matches('/'))
}

fn check_failed(error: CheckError) -> ApiError {
    match error {
        CheckError::Busy => ApiError::unavailable(Unavailable::ServerBusy, "password checks busy"),
        CheckError::Random => ApiError::internal("the system random source failed"),
    }
}

/// What the pre-flight found: the check, or why it could not run.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct UpgradePreflightBody {
    /// The check's findings: counts, blockers with their fixes, and the
    /// address changes to acknowledge.
    pub check: Option<CheckResult>,
    /// Why the check could not run.
    pub failure: Option<FailedLine>,
}

/// Run every check the upgrade runs before it moves anything, changing
/// nothing, and wait for it.
#[utoipa::path(
    post,
    tag = "upgrade",
    path = "/api/v3/upgrade/preflight",
    responses(
        (status = 200, description = "What the check found", body = UpgradePreflightBody),
        (status = 401, description = "Not signed in to the upgrade page"),
        (status = 409, description = "A run is under way, the folders already hold v3 data, or an upgrade did not finish")
    )
)]
pub async fn upgrade_preflight(
    State(mode): State<UpgradeMode>,
    _session: UpgradeSession,
) -> Result<Json<UpgradePreflightBody>, ApiError> {
    match mode.layout() {
        UpgradeLayout::Finished => {
            return Err(ApiError::conflict(
                "The folders already hold v3 data. Continue to open the app.",
            ));
        }
        // The check runs nothing there: finishing the upgrade puts the v2
        // files back first, and a pre-flight then runs on them.
        UpgradeLayout::Interrupted => {
            return Err(ApiError::conflict(
                "An upgrade did not finish. Finish it or go back to v2 first.",
            ));
        }
        UpgradeLayout::NeedsUpgrade => {}
    }
    let ending = mode.check().await.map_err(busy)?;
    Ok(Json(match ending {
        Ending::Checked(check) => UpgradePreflightBody {
            check: Some(check),
            failure: None,
        },
        Ending::Failed(failure) => UpgradePreflightBody {
            check: None,
            failure: Some(failure),
        },
        Ending::Finished(_) => UpgradePreflightBody {
            check: None,
            failure: None,
        },
    }))
}

/// An address change the caller has made at the outside service, as the
/// pre-flight listed it.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct AcknowledgedWarning {
    /// The feature the change concerns.
    pub feature: CallbackFeature,
    /// The address v2 registered, as the pre-flight gave it.
    pub old_address: Option<Address>,
    /// The address v3 uses, as the pre-flight gave it.
    pub new_address: Option<Address>,
}

/// What start needs.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct UpgradeStartBody {
    /// Every address change the latest pre-flight listed.
    #[serde(default)]
    pub acknowledged: Vec<AcknowledgedWarning>,
}

/// The latest run and where the upgrade stands.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct UpgradeProgressBody {
    /// Where the upgrade stands.
    pub layout: UpgradeLayout,
    /// The latest upgrade, fresh start or undo; none yet.
    pub run: Option<UpgradeRun>,
}

/// Start the upgrade. On v2 data it needs a pre-flight with nothing to fix
/// and every address change it listed acknowledged. An interrupted upgrade
/// is finished: the tool puts the v2 files back first, then upgrades.
#[utoipa::path(
    post,
    tag = "upgrade",
    path = "/api/v3/upgrade/start",
    request_body = UpgradeStartBody,
    responses(
        (status = 202, description = "The upgrade started", body = UpgradeProgressBody),
        (status = 401, description = "Not signed in to the upgrade page"),
        (status = 409, description = "No pre-flight, something to fix, an address change not acknowledged, a run under way, or nothing to upgrade")
    )
)]
pub async fn upgrade_start(
    State(mode): State<UpgradeMode>,
    _session: UpgradeSession,
    ValidJson(body): ValidJson<UpgradeStartBody>,
) -> Result<Response, ApiError> {
    let task = match mode.layout() {
        UpgradeLayout::Finished => {
            return Err(ApiError::conflict(
                "The folders already hold v3 data. Continue to open the app.",
            ));
        }
        // The tool rolls the unfinished run back, then checks and stops on
        // addresses not yet acknowledged; a pre-flight follows that.
        UpgradeLayout::Interrupted => Task::Upgrade {
            callbacks_changed: false,
        },
        UpgradeLayout::NeedsUpgrade => {
            let Some(check) = mode.preflight() else {
                return Err(ApiError::conflict("Run the pre-flight first."));
            };
            if !check.blockers.is_empty() {
                return Err(ApiError::conflict(
                    "The pre-flight found something to fix first.",
                ));
            }
            if !acknowledges(&body.acknowledged, &check) {
                return Err(ApiError::conflict(
                    "Acknowledge each address change the pre-flight lists.",
                ));
            }
            Task::Upgrade {
                callbacks_changed: !check.warnings.is_empty(),
            }
        }
    };
    started(&mode, task)
}

/// Whether `acknowledged` names exactly the address changes `check` lists.
fn acknowledges(acknowledged: &[AcknowledgedWarning], check: &CheckResult) -> bool {
    let same = |seen: &AcknowledgedWarning,
                listed: &crate::bootstrap::upgrade_progress::Warning| {
        seen.feature == listed.feature
            && seen.old_address == listed.old_address
            && seen.new_address == listed.new_address
    };
    acknowledged.len() == check.warnings.len()
        && check
            .warnings
            .iter()
            .all(|listed| acknowledged.iter().any(|seen| same(seen, listed)))
        && acknowledged
            .iter()
            .all(|seen| check.warnings.iter().any(|listed| same(seen, listed)))
}

/// Start fresh's confirmation.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct UpgradeFreshBody {
    /// Must be true: nothing comes across from v2.
    pub confirm: bool,
}

/// Start fresh: v2's files move into the `v2-backup` folders, nothing is
/// imported, and v3 starts empty. Going back to v2 still works.
#[utoipa::path(
    post,
    tag = "upgrade",
    path = "/api/v3/upgrade/fresh",
    request_body = UpgradeFreshBody,
    responses(
        (status = 202, description = "The fresh start began", body = UpgradeProgressBody),
        (status = 400, description = "Not confirmed"),
        (status = 401, description = "Not signed in to the upgrade page"),
        (status = 409, description = "A run is under way, or the folders already hold v3 data")
    )
)]
pub async fn upgrade_fresh(
    State(mode): State<UpgradeMode>,
    _session: UpgradeSession,
    ValidJson(body): ValidJson<UpgradeFreshBody>,
) -> Result<Response, ApiError> {
    if !body.confirm {
        return Err(ApiError::invalid_input(
            "Confirm the fresh start: nothing comes across from v2.",
        ));
    }
    if mode.layout() == UpgradeLayout::Finished {
        return Err(ApiError::conflict(
            "The folders already hold v3 data. Continue to open the app.",
        ));
    }
    started(&mode, Task::Fresh)
}

/// The latest upgrade, fresh start or undo: its step, how far along, and
/// its result or failure once it has ended.
#[utoipa::path(
    get,
    tag = "upgrade",
    path = "/api/v3/upgrade/progress",
    responses(
        (status = 200, description = "The latest run", body = UpgradeProgressBody),
        (status = 401, description = "Not signed in to the upgrade page")
    )
)]
pub async fn upgrade_progress(
    State(mode): State<UpgradeMode>,
    _session: UpgradeSession,
) -> Json<UpgradeProgressBody> {
    Json(progress_of(&mode))
}

/// Go back to v2: the v3 data is deleted and v2's files come back from the
/// backup. Also finishes going back from an interrupted upgrade.
#[utoipa::path(
    post,
    tag = "upgrade",
    path = "/api/v3/upgrade/undo",
    responses(
        (status = 202, description = "Going back to v2 began", body = UpgradeProgressBody),
        (status = 401, description = "Not signed in to the upgrade page"),
        (status = 409, description = "A run is under way, or the folders already hold v2 data")
    )
)]
pub async fn upgrade_undo(
    State(mode): State<UpgradeMode>,
    _session: UpgradeSession,
) -> Result<Response, ApiError> {
    if mode.layout() == UpgradeLayout::NeedsUpgrade {
        return Err(ApiError::conflict(
            "The folders already hold v2 data: there is nothing to undo.",
        ));
    }
    started(&mode, Task::Undo)
}

/// Open the app: the upgrade page stops and the normal app starts in its
/// place. The next request reaches the app, which shows its own sign-in.
#[utoipa::path(
    post,
    tag = "upgrade",
    path = "/api/v3/upgrade/continue",
    responses(
        (status = 202, description = "The app is starting"),
        (status = 401, description = "Not signed in to the upgrade page"),
        (status = 409, description = "A run is under way, the folders do not hold v3 data, or another program is using them")
    )
)]
pub async fn upgrade_continue(
    State(mode): State<UpgradeMode>,
    _session: UpgradeSession,
) -> Result<StatusCode, ApiError> {
    mode.accept_continue()
        .await
        .map_err(|refused| match refused {
            ContinueRefused::Busy => ApiError::conflict("Wait for the run to end."),
            ContinueRefused::NotV3 => ApiError::conflict("The folders do not hold v3 data yet."),
            ContinueRefused::Locked => ApiError::conflict(
                "Another program is using the data folders. Try again when it is done.",
            ),
        })?;
    Ok(StatusCode::ACCEPTED)
}

/// Start `task`: 202 with its progress, or 409 while another run is under
/// way.
fn started(mode: &UpgradeMode, task: Task) -> Result<Response, ApiError> {
    mode.start(task).map_err(busy)?;
    Ok((StatusCode::ACCEPTED, Json(progress_of(mode))).into_response())
}

fn progress_of(mode: &UpgradeMode) -> UpgradeProgressBody {
    UpgradeProgressBody {
        layout: mode.layout(),
        run: mode.last_run(),
    }
}

fn busy(refused: Refused) -> ApiError {
    match refused {
        Refused::Busy => ApiError::conflict("Another run is under way. Wait for it to end."),
    }
}
