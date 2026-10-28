//! Raw wrapper around the slskd 0.25.1 REST API. No business logic.
//!
//! The transport is injected, never acquired here. Non-2xx responses map
//! into [`ClientError`]. The discrete calls retry the 429 "only one
//! concurrent operation", waiting out
//! the server's `Retry-After` when it sends one; the search-poll helpers
//! are not retried, because the repository's poll loop owns its own
//! deadline.

use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use super::http::{HttpFault, HttpReply, SlskdHttp};
use super::models::{
    SlskdEnqueueResponse, SlskdOptions, SlskdSearchResponse, SlskdTransfer,
    SlskdUserSearchResponse, SlskdUserTransfers,
};
use crate::client_error::{ClientError, Service};

/// How many times a discrete call retries a 429 before surfacing it.
const RATE_LIMIT_ATTEMPTS: u32 = 4;
/// Base backoff between 429 retries when slskd names no wait; doubled per
/// attempt.
const RATE_LIMIT_BACKOFF: Duration = Duration::from_millis(250);
/// The longest `Retry-After` a call waits out itself. A longer one ends
/// the call at once with the wait in the error, for the caller to honour.
const MAX_RETRY_AFTER_WAIT: Duration = Duration::from_secs(30);

fn transport(fault: HttpFault) -> ClientError {
    ClientError::transport(Service::Slskd, fault.0)
}

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

    fn check(reply: &HttpReply) -> Result<(), ClientError> {
        if (200..300).contains(&reply.status) {
            return Ok(());
        }
        Err(ClientError::for_status(
            Service::Slskd,
            reply.status,
            reply.retry_after,
            &reply.body,
        ))
    }

    fn decode<B: DeserializeOwned>(reply: &HttpReply) -> Result<B, ClientError> {
        serde_json::from_slice(&reply.body)
            .map_err(|err| ClientError::decode(Service::Slskd, err.to_string()))
    }

    /// Run a discrete call, retrying 429s: after the
    /// server's `Retry-After` when it names one, else with doubling
    /// backoff. Transport faults are returned as-is; the caller owns
    /// broader policy.
    async fn with_rate_limit_retry<F, Fut>(&self, call: F) -> Result<HttpReply, ClientError>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<HttpReply, HttpFault>>,
    {
        let mut backoff = RATE_LIMIT_BACKOFF;
        for attempt in 1..=RATE_LIMIT_ATTEMPTS {
            let reply = call().await.map_err(transport)?;
            if reply.status != 429 || attempt == RATE_LIMIT_ATTEMPTS {
                return Ok(reply);
            }
            let wait = match reply.retry_after {
                Some(wait) if wait > MAX_RETRY_AFTER_WAIT => return Ok(reply),
                Some(wait) => wait,
                None => backoff,
            };
            tokio::time::sleep(wait).await;
            backoff = backoff.saturating_mul(2);
        }
        unreachable!("loop always returns on the final attempt");
    }

    /// GET /api/v0/application; returns raw JSON (version/server state).
    pub async fn health_check(&self) -> Result<Map<String, Value>, ClientError> {
        let reply = self
            .with_rate_limit_retry(|| self.http.get("/application", &[]))
            .await?;
        Self::check(&reply)?;
        Self::decode(&reply)
    }

    /// GET /api/v0/options; the repository reads `directories.downloads` to
    /// tell the user the exact path slskd saves to (for the
    /// downloads-mount diagnostic, not a hot path).
    pub async fn get_options(&self) -> Result<SlskdOptions, ClientError> {
        let reply = self
            .with_rate_limit_retry(|| self.http.get("/options", &[]))
            .await?;
        Self::check(&reply)?;
        Self::decode(&reply)
    }

    /// POST /api/v0/transfers/downloads/{username}.
    ///
    /// The body is a plain JSON array `[{filename, size}]` (no options
    /// envelope, no destination/externalId). Returns 201
    /// `{Enqueued, Failed}`, not a batch GUID.
    pub async fn enqueue(
        &self,
        username: &str,
        files: &[(String, i64)],
    ) -> Result<SlskdEnqueueResponse, ClientError> {
        let payload: Vec<Value> = files
            .iter()
            .map(|(filename, size)| serde_json::json!({"filename": filename, "size": size}))
            .collect();
        let body = serde_json::to_vec(&payload).map_err(|error| ClientError::BadRequest {
            service: Service::Slskd,
            detail: format!("enqueue payload encode: {error}"),
        })?;
        let path = format!("/transfers/downloads/{username}");
        let reply = self
            .with_rate_limit_retry(|| self.http.post(&path, body.clone()))
            .await?;
        Self::check(&reply)?;
        Self::decode(&reply)
    }

    /// GET /api/v0/transfers/downloads/{username}, flattened to transfers.
    /// A 404 means "no such user bucket" and yields an empty list.
    pub async fn get_downloads(&self, username: &str) -> Result<Vec<SlskdTransfer>, ClientError> {
        let path = format!("/transfers/downloads/{username}");
        let reply = self
            .with_rate_limit_retry(|| self.http.get(&path, &[]))
            .await?;
        if reply.status == 404 {
            return Ok(Vec::new());
        }
        Self::check(&reply)?;
        let payload: SlskdUserTransfers = Self::decode(&reply)?;
        Ok(payload.into_transfers())
    }

    /// GET /api/v0/transfers/downloads (every peer), username preserved per
    /// transfer: the flatten loses it, so it is carried down from the
    /// per-user block. For the downloads-mount diagnostic, not the
    /// per-task poll.
    pub async fn get_all_downloads(&self) -> Result<Vec<SlskdTransfer>, ClientError> {
        let reply = self
            .with_rate_limit_retry(|| self.http.get("/transfers/downloads", &[]))
            .await?;
        if reply.status == 404 {
            return Ok(Vec::new());
        }
        Self::check(&reply)?;
        let blocks: Vec<SlskdUserTransfers> = Self::decode(&reply)?;
        Ok(blocks
            .into_iter()
            .flat_map(SlskdUserTransfers::into_transfers)
            .collect())
    }

    /// DELETE /api/v0/transfers/downloads/{username}/{id}?remove=true.
    /// Serves both cancellation of an in-flight transfer and post-import
    /// removal of a completed transfer record. A 404 means the
    /// record is already gone and yields `false`.
    pub async fn cancel_transfer(
        &self,
        username: &str,
        transfer_id: &str,
    ) -> Result<bool, ClientError> {
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
    // repository owns the deadline. The start keeps the 429 wrapper:
    // slskd allows one concurrent search and answers 429 while another
    // client holds it, so an unretried start drops the whole rung.

    /// POST /api/v0/searches. `searchTimeout` is in milliseconds (verified).
    pub async fn start_search(
        &self,
        search_text: &str,
        timeout: Duration,
    ) -> Result<SlskdSearchResponse, ClientError> {
        let body = serde_json::json!({
            "searchText": search_text,
            "searchTimeout": timeout.as_millis().min(i64::MAX as u128) as i64,
        });
        let bytes = serde_json::to_vec(&body).map_err(|error| ClientError::BadRequest {
            service: Service::Slskd,
            detail: format!("search payload encode: {error}"),
        })?;
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
    ) -> Result<SlskdSearchResponse, ClientError> {
        let path = format!("/searches/{search_id}");
        let reply = self.http.get(&path, &[]).await.map_err(transport)?;
        Self::check(&reply)?;
        Self::decode(&reply)
    }

    /// GET /api/v0/searches/{id}/responses.
    pub async fn get_search_responses(
        &self,
        search_id: &str,
    ) -> Result<Vec<SlskdUserSearchResponse>, ClientError> {
        let path = format!("/searches/{search_id}/responses");
        let reply = self.http.get(&path, &[]).await.map_err(transport)?;
        Self::check(&reply)?;
        Self::decode(&reply)
    }
}
