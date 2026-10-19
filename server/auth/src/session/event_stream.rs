//! `GET /api/v3/events/stream`: the tab's one live connection.
//!
//! A Server-Sent Events stream, multiplexed by event name the way v2's
//! `/api/v1/events/stream` was: it opens with a `retry:` frame (the
//! browser waits that long before reconnecting), replays the retained
//! state, then follows live events. An idle stream gets a comment every
//! [`KEEPALIVE`] so proxies do not cut it. The session gate admits the
//! request; after that the stream rechecks its session every
//! [`SESSION_RECHECK`] and ends once it is revoked or expired, which v2
//! never did. Responses carry `X-Accel-Buffering: no` so nginx passes
//! frames straight through, and the compression layer skips
//! `text/event-stream`, so nothing buffers the stream.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::State,
    http::{HeaderMap, HeaderValue},
    response::{
        IntoResponse, Response,
        sse::{Event as SseEvent, Sse},
    },
};
use futures_util::future::BoxFuture;
use tokio::time::Instant;

use super::{
    extract::extract,
    store::{SessionStore, now_unix},
    tokens::hash_token,
};
use crate::users::roles::User;
use droppedneedle_kernel::events::{EventHub, Subscription};
use droppedneedle_kernel::mount::{Routes, routes};

/// Reconnect delay the stream asks browsers for (v2 `_MUX_RETRY_MS`).
pub const RETRY: Duration = Duration::from_secs(5);

/// Idle gap before a keep-alive comment (v2 `_MUX_KEEPALIVE_SECONDS`).
pub const KEEPALIVE: Duration = Duration::from_secs(30);

/// How often an open stream checks its session is still valid.
pub const SESSION_RECHECK: Duration = Duration::from_secs(60);

/// What a session recheck found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    /// Still valid.
    Valid,
    /// Revoked, expired or gone: end the stream.
    Gone,
    /// The lookup failed; keep the stream and try again later.
    Unknown,
}

/// Looks a session up by its token hash.
pub type SessionCheck = Arc<dyn Fn(String) -> BoxFuture<'static, SessionStatus> + Send + Sync>;

/// A session check over the session store the gate uses.
pub fn session_check<S: SessionStore>(store: S) -> SessionCheck {
    Arc::new(move |token_hash: String| {
        let store = store.clone();
        Box::pin(async move {
            match store.lookup_valid(&token_hash, now_unix()).await {
                Ok(Some(_)) => SessionStatus::Valid,
                Ok(None) => SessionStatus::Gone,
                Err(error) => {
                    tracing::warn!(%error, "event stream session recheck failed");
                    SessionStatus::Unknown
                }
            }
        })
    })
}

/// State behind the stream route.
#[derive(Clone)]
pub struct StreamRoute {
    hub: EventHub,
    sessions: SessionCheck,
}

impl StreamRoute {
    /// Events from `hub`, each stream rechecking its session through
    /// `sessions`.
    pub fn new(hub: EventHub, sessions: SessionCheck) -> Self {
        Self { hub, sessions }
    }
}

/// The stream route, for any signed-in user.
pub fn routes() -> Routes<StreamRoute> {
    Routes::new().user(routes!(stream))
}

/// Live events for the signed-in user.
///
/// Event names: `activity.changed` and `snapshot` (now playing) go to
/// everyone; `wanted_new_candidates`, `wanted_auto_dispatched`,
/// `wanted_fulfilled`, `auto_download_enqueued`, `request_imported`,
/// `playlist_imported`, `drop_import_updated`,
/// `personal_mix_refreshed`, `concerts_new` and `search_job_updated` go
/// only to the user they concern. Each `data:` line is one JSON payload (see the `ActivityChanged`,
/// `NowPlayingSnapshot`, `WantedNotice`, `AutoDownloadEnqueued`,
/// `RequestImported`, `PlaylistImported`, `DropImportUpdated`,
/// `PersonalMixRefreshed`, `ConcertsNew` and
/// `SearchJobUpdated` schemas).
#[utoipa::path(
    get,
    path = "/api/v3/events/stream",
    operation_id = "events_stream",
    tag = "events",
    responses(
        (status = 200, description = "Server-Sent Events stream", content_type = "text/event-stream", body = String),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn stream(
    State(route): State<StreamRoute>,
    User(caller): User,
    headers: HeaderMap,
) -> Response {
    let token_hash = extract(&headers).map(|(raw, _)| hash_token(&raw));
    let state = Stream {
        subscription: route.hub.subscribe(caller.user_id()),
        sessions: route.sessions,
        token_hash,
        opened: false,
        recheck_at: Instant::now() + SESSION_RECHECK,
    };
    let body = futures_util::stream::unfold(state, |mut state| async move {
        state
            .next_event()
            .await
            .map(|event| (Ok::<_, Infallible>(event), state))
    });
    let mut response = Sse::new(body).into_response();
    response
        .headers_mut()
        .insert("x-accel-buffering", HeaderValue::from_static("no"));
    response
}

/// One open stream.
struct Stream {
    subscription: Subscription,
    sessions: SessionCheck,
    token_hash: Option<String>,
    opened: bool,
    recheck_at: Instant,
}

impl Stream {
    /// The next SSE event to write, or `None` to end the stream.
    async fn next_event(&mut self) -> Option<SseEvent> {
        if !self.opened {
            self.opened = true;
            return Some(SseEvent::default().retry(RETRY));
        }
        if Instant::now() >= self.recheck_at {
            self.recheck_at = Instant::now() + SESSION_RECHECK;
            if !self.session_alive().await {
                return None;
            }
        }
        match tokio::time::timeout(KEEPALIVE, self.subscription.next()).await {
            Ok(Some(frame)) => {
                let mut event = SseEvent::default().event(frame.name()).data(frame.data());
                if let Some(id) = frame.id() {
                    event = event.id(id);
                }
                Some(event)
            }
            Ok(None) => None,
            Err(_idle) => Some(SseEvent::default().comment("keepalive")),
        }
    }

    async fn session_alive(&self) -> bool {
        let Some(token_hash) = self.token_hash.clone() else {
            return true;
        };
        (self.sessions)(token_hash).await != SessionStatus::Gone
    }
}
