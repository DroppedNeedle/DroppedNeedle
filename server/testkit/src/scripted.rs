//! Scripted error statuses for the mock servers.
//!
//! A mock wraps its router with [`ScriptedStatuses::layer`]; a test queues
//! statuses with [`ScriptedStatuses::answer_next`], and each queued one
//! answers the next request in place of the route, with a `Retry-After`
//! header when one was given. Every request through the layer is counted,
//! so a test can tell whether a client retried.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};

#[derive(Debug, Clone)]
struct Scripted {
    status: StatusCode,
    retry_after: Option<String>,
}

#[derive(Debug, Default)]
struct Inner {
    queue: VecDeque<Scripted>,
    requests: usize,
}

/// The queue of scripted statuses one mock answers with.
#[derive(Debug, Clone, Default)]
pub struct ScriptedStatuses {
    inner: Arc<Mutex<Inner>>,
}

impl ScriptedStatuses {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The next request answers `status`, with `Retry-After: retry_after`
    /// when given. Queued statuses answer in order.
    pub fn answer_next(&self, status: u16, retry_after: Option<&str>) {
        self.lock().queue.push_back(Scripted {
            status: StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            retry_after: retry_after.map(str::to_owned),
        });
    }

    /// Requests seen since the mock started.
    #[must_use]
    pub fn requests(&self) -> usize {
        self.lock().requests
    }

    /// Drop queued statuses and the request count.
    pub fn reset(&self) {
        let mut inner = self.lock();
        inner.queue.clear();
        inner.requests = 0;
    }

    /// Wrap `router` so queued statuses answer before its routes.
    pub fn layer(&self, router: Router) -> Router {
        router.layer(middleware::from_fn_with_state(self.clone(), answer))
    }
}

async fn answer(
    State(statuses): State<ScriptedStatuses>,
    request: Request,
    next: Next,
) -> Response {
    let scripted = {
        let mut inner = statuses.lock();
        inner.requests += 1;
        inner.queue.pop_front()
    };
    let Some(scripted) = scripted else {
        return next.run(request).await;
    };
    let mut response = (scripted.status, "scripted status").into_response();
    if let Some(value) = scripted
        .retry_after
        .and_then(|value| HeaderValue::from_str(&value).ok())
    {
        response.headers_mut().insert(header::RETRY_AFTER, value);
    }
    response
}
