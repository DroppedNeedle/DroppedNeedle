//! Skiddle API client (Upcoming Events, the UK/IE depth source).
//!
//! Port of v2's Skiddle repository and models. Every request goes through
//! the executor at the `skiddle` rate row, which retries, rests on a rate
//! limit and types each failure as a [`ProviderError`]. Every answer carries
//! Skiddle's error code: a non-zero code, or none, is a payload failure that
//! keeps only the code. The API key travels as the `api_key` query pair, so
//! it is never in an error or a log line.
//!
//! The wire shapes, their quirks and the recordings they were checked
//! against are in `skiddle/models.rs`.

use std::time::Duration;

use crate::RequestPriority;
use crate::client::{decode, parse_base, payload, unusable_base};
use crate::error::{MAX_RETRY_AFTER, PayloadKind, ProviderError};
use crate::executor::{Backoff, Executor, Reply, Request, Spec, Url, standard_classify};

mod models;

use models::{Answer, ArtistsAnswer, EventsAnswer, KeyCheck};
pub use models::{SkiddleArtist, SkiddleEvent, SkiddleEventArtist, SkiddleVenue};

/// Provider key used in [ProviderError] and logs, and its rate row.
pub const SOURCE: &str = "skiddle";

/// Production endpoint.
pub const SKIDDLE_API_URL: &str = "https://www.skiddle.com/api/v1";
/// Largest Skiddle answer read.
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

/// Skiddle on the wire: the standard rules.
pub static SPEC: Spec = Spec {
    source: SOURCE,
    health_key: SOURCE,
    timeout: Duration::from_secs(15),
    max_body: MAX_BODY_BYTES,
    retry_after_default: Backoff::Fixed(Duration::from_secs(5)),
    retry_after_cap: MAX_RETRY_AFTER,
    classify: standard_classify,
};

/// The Skiddle client, built once with the providers. The key is an
/// argument of each call, read from settings by the caller.
#[derive(Debug, Clone)]
pub struct SkiddleClient {
    executor: Executor,
    base: Option<Url>,
}

impl SkiddleClient {
    /// The client sending through `executor` to the API root at
    /// `base_url`.
    #[must_use]
    pub fn new(executor: Executor, base_url: &str) -> Self {
        Self {
            executor,
            base: parse_base(base_url, SOURCE),
        }
    }

    /// How much longer the bucket rests, for a caller that will not wait
    /// that long.
    #[must_use]
    pub fn cooling_for(&self) -> Duration {
        self.executor.limiter().cooling_for()
    }

    /// Whether the bucket rests after a run of failed calls (an outage).
    #[must_use]
    pub fn resting_after_failures(&self) -> bool {
        self.executor.resting_after_failures()
    }

    /// Acts matching `name`; `[]` means Skiddle knows no such act.
    ///
    /// # Errors
    ///
    /// The typed failure of the call or of its answer.
    pub async fn search_artists(
        &self,
        key: &str,
        name: &str,
        priority: RequestPriority,
    ) -> Result<Vec<SkiddleArtist>, ProviderError> {
        let reply = self
            .get(key, &["artists"], &[("name", name)], priority)
            .await?;
        let answer: ArtistsAnswer = decode(&reply)?;
        results(answer)
    }

    /// Upcoming events tagged with one Skiddle artist id (`a=` filter).
    ///
    /// # Errors
    ///
    /// The typed failure of the call or of its answer.
    pub async fn events_for_artist(
        &self,
        key: &str,
        artist_id: &str,
        priority: RequestPriority,
    ) -> Result<Vec<SkiddleEvent>, ProviderError> {
        let reply = self
            .get(
                key,
                &["events", "search"],
                &[("a", artist_id), ("description", "1")],
                priority,
            )
            .await?;
        let answer: EventsAnswer = decode(&reply)?;
        results(answer)
    }

    /// Check that `key` is accepted: one small events search, sent once,
    /// whose answer must carry Skiddle's `error` code 0. An answer without
    /// the code is not proof the key works.
    ///
    /// # Errors
    ///
    /// The typed failure of the call or of its answer.
    pub async fn check_key(
        &self,
        key: &str,
        priority: RequestPriority,
    ) -> Result<(), ProviderError> {
        let base = self.base.as_ref().ok_or_else(|| unusable_base(SOURCE))?;
        let request = Request::get(
            base,
            &["events", "search"],
            &[("limit", "1"), ("api_key", key)],
        )
        .map_err(|_| unusable_base(SOURCE))?
        .trailing_slash()
        .idempotent(false);
        let reply = self.executor.send(request, priority).await?;
        let answer: KeyCheck = decode(&reply)?;
        check_code(answer.error)
    }

    /// One GET of `segments` under the root, with the key as a query pair.
    /// Skiddle serves these routes with a trailing slash.
    async fn get(
        &self,
        key: &str,
        segments: &[&str],
        params: &[(&str, &str)],
        priority: RequestPriority,
    ) -> Result<Reply, ProviderError> {
        let base = self.base.as_ref().ok_or_else(|| unusable_base(SOURCE))?;
        let mut query = params.to_vec();
        query.push(("api_key", key));
        let request = Request::get(base, segments, &query)
            .map_err(|_| unusable_base(SOURCE))?
            .trailing_slash();
        self.executor.send(request, priority).await
    }
}

/// Skiddle's error code, which every answer carries. Zero is success. Any
/// other code is Skiddle's own failure, and only the code is kept: the
/// message is the provider's text and is not ours to log. An answer without
/// a code is not proof of anything.
fn check_code(error: Option<i64>) -> Result<(), ProviderError> {
    match error {
        Some(0) => Ok(()),
        Some(code) => Err(contract(&format!("skiddle answered error {code}"))),
        None => Err(contract("skiddle answered no error code")),
    }
}

/// The results of a search answer. A success with no results list is a
/// broken answer, not an empty one.
fn results<T>(answer: Answer<T>) -> Result<Vec<T>, ProviderError> {
    check_code(answer.error)?;
    answer
        .results
        .ok_or_else(|| contract("skiddle answered no results"))
}

fn contract(message: &str) -> ProviderError {
    payload(SOURCE, PayloadKind::Contract, message)
}
