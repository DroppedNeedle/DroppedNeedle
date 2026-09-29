//! Raw wrapper around the slskd 0.25.1 REST API. No business logic.
//!
//! Ported from `backend/repositories/slskd/slskd_client.py`. The transport
//! is injected, never acquired here (v2 AUD-12). Non-2xx responses raise
//! [`SlskdError`] (429 -> rate-limited, v2 AUD-10). The discrete calls
//! retry the 429 "only one concurrent operation" with backoff (v2 C3 — v2
//! retries it because `RateLimitedError` is an `ExternalServiceError`);
//! the search-poll helpers are NOT retried, because the repository's poll
//! loop owns its own deadline.

use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use super::error::SlskdError;
use super::http::{HttpFault, HttpReply, SlskdHttp};
use super::models::{
    SlskdEnqueueResponse, SlskdOptions, SlskdSearchResponse, SlskdTransfer, SlskdUserSearchResponse,
};

/// How many times a discrete call retries a 429 before surfacing it.
const RATE_LIMIT_ATTEMPTS: u32 = 4;
/// Base backoff between 429 retries; doubled per attempt.
const RATE_LIMIT_BACKOFF: Duration = Duration::from_millis(250);

/// Thin slskd REST client over an injected [`SlskdHttp`] transport.
#[derive(Debug, Clone)]
pub struct SlskdClient<T: SlskdHttp> {
    http: T,
}

impl<T: SlskdHttp> SlskdClient<T> {
    #[must_use]
    pub fn new(http: T) -> Self {
        Self { http }
    }

    fn check(reply: &HttpReply) -> Result<(), SlskdError> {
        if (200..300).contains(&reply.status) {
            return Ok(());
        }
        Err(SlskdError::for_status(reply.status, &reply.body))
    }

    fn decode<B: DeserializeOwned>(reply: &HttpReply) -> Result<B, SlskdError> {
        serde_json::from_slice(&reply.body).map_err(|err| SlskdError::Decode(err.to_string()))
    }

    /// Run a discrete call, retrying 429s with backoff (v2 C3 / `with_retry`).
    /// Transport faults are returned as-is; the caller owns broader policy.
    async fn with_rate_limit_retry<F, Fut>(&self, call: F) -> Result<HttpReply, SlskdError>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<HttpReply, HttpFault>>,
    {
        let mut backoff = RATE_LIMIT_BACKOFF;
        for attempt in 1..=RATE_LIMIT_ATTEMPTS {
            let reply = call()
                .await
                .map_err(|fault| SlskdError::Transport(fault.0))?;
            if reply.status != 429 || attempt == RATE_LIMIT_ATTEMPTS {
                return Ok(reply);
            }
            tokio::time::sleep(backoff).await;
            backoff = backoff.saturating_mul(2);
        }
        unreachable!("loop always returns on the final attempt");
    }

    /// GET /api/v0/application; returns raw JSON (version/server state).
    pub async fn health_check(&self) -> Result<Map<String, Value>, SlskdError> {
        let reply = self
            .with_rate_limit_retry(|| self.http.get("/application", &[]))
            .await?;
        Self::check(&reply)?;
        Self::decode(&reply)
    }

    /// GET /api/v0/options; the repository reads `directories.downloads` to
    /// tell the user the exact path slskd saves to (v2: for the
    /// downloads-mount diagnostic, not a hot path).
    pub async fn get_options(&self) -> Result<SlskdOptions, SlskdError> {
        let reply = self
            .with_rate_limit_retry(|| self.http.get("/options", &[]))
            .await?;
        Self::check(&reply)?;
        Self::decode(&reply)
    }

    /// POST /api/v0/transfers/downloads/{username}.
    ///
    /// The body is a PLAIN JSON array `[{filename, size}]` (no options
    /// envelope, no destination/externalId; v2 C1). Returns 201
    /// `{Enqueued, Failed}`, not a batch GUID.
    pub async fn enqueue(
        &self,
        username: &str,
        files: &[(String, i64)],
    ) -> Result<SlskdEnqueueResponse, SlskdError> {
        let payload: Vec<Value> = files
            .iter()
            .map(|(filename, size)| serde_json::json!({"filename": filename, "size": size}))
            .collect();
        let body = serde_json::to_vec(&payload)
            .map_err(|error| SlskdError::Transport(format!("enqueue payload encode: {error}")))?;
        let path = format!("/transfers/downloads/{username}");
        let reply = self
            .with_rate_limit_retry(|| self.http.post(&path, body.clone()))
            .await?;
        Self::check(&reply)?;
        Self::decode(&reply)
    }

    /// GET /api/v0/transfers/downloads/{username}, flattened to transfers.
    /// A 404 means "no such user bucket" and yields an empty list (v2).
    pub async fn get_downloads(&self, username: &str) -> Result<Vec<SlskdTransfer>, SlskdError> {
        let path = format!("/transfers/downloads/{username}");
        let reply = self
            .with_rate_limit_retry(|| self.http.get(&path, &[]))
            .await?;
        if reply.status == 404 {
            return Ok(Vec::new());
        }
        Self::check(&reply)?;
        let payload: Value = Self::decode(&reply)?;
        Ok(flatten_transfers(&payload))
    }

    /// GET /api/v0/transfers/downloads (every peer), username preserved per
    /// transfer — the flatten loses it, so it is carried down from the
    /// per-user block (v2). For the downloads-mount diagnostic, not the
    /// per-task poll.
    pub async fn get_all_downloads(&self) -> Result<Vec<SlskdTransfer>, SlskdError> {
        let reply = self
            .with_rate_limit_retry(|| self.http.get("/transfers/downloads", &[]))
            .await?;
        if reply.status == 404 {
            return Ok(Vec::new());
        }
        Self::check(&reply)?;
        let payload: Value = Self::decode(&reply)?;
        let mut out = Vec::new();
        if let Value::Array(blocks) = &payload {
            for block in blocks {
                let username = block
                    .get("username")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                for mut transfer in flatten_transfers(block) {
                    if transfer.username.is_empty() {
                        transfer.username = username.to_owned();
                    }
                    out.push(transfer);
                }
            }
        }
        Ok(out)
    }

    /// DELETE /api/v0/transfers/downloads/{username}/{id}?remove=true.
    /// Serves both cancellation of an in-flight transfer and post-import
    /// removal of a completed transfer record (v2 DEC-1). A 404 means the
    /// record is already gone and yields `false` (v2).
    pub async fn cancel_transfer(
        &self,
        username: &str,
        transfer_id: &str,
    ) -> Result<bool, SlskdError> {
        let path = format!("/transfers/downloads/{username}/{transfer_id}");
        let reply = self
            .with_rate_limit_retry(|| self.http.delete(&path, &[("remove", "true")]))
            .await?;
        if reply.status == 404 {
            return Ok(false);
        }
        Self::check(&reply)?;
        Ok(matches!(reply.status, 200 | 204))
    }

    // Search poll: no retry wrapper on the state/response reads, the
    // repository owns the deadline (v2). The START keeps the 429 wrapper:
    // slskd allows one concurrent search and answers 429 while another
    // client holds it, so an unretried start drops the whole rung.

    /// POST /api/v0/searches. `searchTimeout` is MILLISECONDS (v2: verified).
    pub async fn start_search(
        &self,
        search_text: &str,
        timeout: Duration,
    ) -> Result<SlskdSearchResponse, SlskdError> {
        let body = serde_json::json!({
            "searchText": search_text,
            "searchTimeout": timeout.as_millis().min(i64::MAX as u128) as i64,
        });
        let bytes = serde_json::to_vec(&body)
            .map_err(|error| SlskdError::Transport(format!("search payload encode: {error}")))?;
        let reply = self
            .with_rate_limit_retry(|| self.http.post("/searches", bytes.clone()))
            .await?;
        Self::check(&reply)?;
        Self::decode(&reply)
    }

    /// GET /api/v0/searches/{id}.
    pub async fn get_search_state(
        &self,
        search_id: &str,
    ) -> Result<SlskdSearchResponse, SlskdError> {
        let path = format!("/searches/{search_id}");
        let reply = self
            .http
            .get(&path, &[])
            .await
            .map_err(|fault| SlskdError::Transport(fault.0))?;
        Self::check(&reply)?;
        Self::decode(&reply)
    }

    /// GET /api/v0/searches/{id}/responses.
    pub async fn get_search_responses(
        &self,
        search_id: &str,
    ) -> Result<Vec<SlskdUserSearchResponse>, SlskdError> {
        let path = format!("/searches/{search_id}/responses");
        let reply = self
            .http
            .get(&path, &[])
            .await
            .map_err(|fault| SlskdError::Transport(fault.0))?;
        Self::check(&reply)?;
        Self::decode(&reply)
    }
}

/// Walk slskd's per-user transfers tree and collect transfer dicts. Robust
/// to the exact nesting: any object carrying both `id` and `filename` is a
/// transfer (v2 `_flatten_transfers`).
fn flatten_transfers(payload: &Value) -> Vec<SlskdTransfer> {
    fn walk(node: &Value, out: &mut Vec<SlskdTransfer>) {
        match node {
            Value::Object(map) => {
                if map.contains_key("id")
                    && map.contains_key("filename")
                    && let Ok(transfer) =
                        serde_json::from_value::<SlskdTransfer>(Value::Object(map.clone()))
                {
                    out.push(transfer);
                    return;
                }
                for value in map.values() {
                    walk(value, out);
                }
            }
            Value::Array(items) => {
                for item in items {
                    walk(item, out);
                }
            }
            _ => {}
        }
    }

    let mut out = Vec::new();
    walk(payload, &mut out);
    out
}
