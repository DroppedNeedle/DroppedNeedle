//! The one way a provider request goes out.
//!
//! An [`Executor`] belongs to one row of the rate table and sends every
//! request of that row: it paces through the row's limiter at the
//! caller's priority, sends with a deadline, reads the body under a cap,
//! classifies the answer, retries what may be retried inside a budget,
//! turns `Retry-After` into a cooldown on the limiter, counts every
//! request it sends on the row, and records the outcome once for service
//! health and the request's degradation notes.
//! What differs per provider is a [`Spec`] beside its client.
//!
//! Transport errors carry only a [`TransportKind`]: the URL, which can
//! hold a key in its path or query, never reaches an error or a log line.

#[cfg(any(test, feature = "test-support"))]
use std::{
    collections::{HashMap, VecDeque},
    sync::Mutex,
};
use std::{sync::Arc, time::Duration};

use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::http_client::{BodyError, HttpClientFactory, read_capped};
use futures_util::future::BoxFuture;
use serde::de::DeserializeOwned;
use tokio::time::Instant;

pub use reqwest::Url;

use crate::calls::CallOutcome;
use crate::degradation::{IntegrationStatus, record_current};
use crate::error::{PayloadKind, ProviderError, TransportKind, retry_delay_from_headers};
use crate::health::{self, ServiceHealth};
use crate::limiter::{RateLimiter, Refused, RequestPriority};

/// Longest a page's call waits for each request it sends, a cooldown and
/// the queue ahead of it included; the bound is per request, so a retry
/// gets its own. Past it the call fails at once and the page answers from
/// what it has. An image fetch is turned down only by a cooldown longer
/// than this.
pub const USER_COOLDOWN_WAIT: Duration = Duration::from_millis(2500);

/// How long a provider rests after a run of failed calls.
pub const FAILURE_COOLDOWN: Duration = Duration::from_secs(30);

/// Failed calls in a row before the provider rests, unless its health
/// entry waits for more.
const FAILURE_RUN: usize = 5;

/// Longest log or error text kept from a decoder.
const MAX_MESSAGE: usize = 200;

/// The headers an answer keeps; the rest are dropped at the wire.
const KEPT_HEADERS: [&str; 5] = [
    "content-type",
    "location",
    "retry-after",
    "ratelimit-reset",
    "rate-limit-reset",
];

/// The request method.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Method {
    /// GET.
    Get,
    /// POST.
    Post,
}

/// What a request carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Body {
    /// Nothing.
    Empty,
    /// URL-encoded form pairs.
    Form(Vec<(String, String)>),
    /// A JSON document.
    Json(Vec<u8>),
}

/// What happens when a provider answers with a redirect.
#[derive(Debug, Clone, Copy)]
pub enum Redirects {
    /// A 3xx is [`ProviderError::Redirect`].
    Refuse,
    /// The 3xx comes back as a [`Reply`] for the client to judge, with its
    /// `location` header.
    Return,
    /// Followed while its rule allows, without another token.
    Follow(&'static RedirectRule),
}

/// Which hops a followed redirect may take.
#[derive(Debug)]
pub struct RedirectRule {
    /// Hops followed at most per request.
    pub max_hops: usize,
    /// Where a hop from the first URL toward the second goes: the URL to
    /// fetch, or `None` to refuse the hop.
    pub next: fn(&Url, &Url) -> Option<Url>,
    /// Whether a hop target is another service than the provider (such as
    /// the archive's storage nodes). A target's failure then neither counts
    /// toward the provider's failure rest nor marks it down; a wait it
    /// names still cools the row.
    pub foreign: fn(&Url) -> bool,
}

/// [`RedirectRule::foreign`] for rules whose targets all belong to the
/// provider.
#[must_use]
pub fn never_foreign(_: &Url) -> bool {
    false
}

/// One outgoing request. Built only from a parsed base URL, with path
/// values added as segments, never joined as text.
#[derive(Clone)]
pub struct Request {
    method: Method,
    url: Url,
    headers: Vec<(&'static str, String)>,
    body: Body,
    idempotent: bool,
    max_body: Option<usize>,
    redirects: Redirects,
}

impl std::fmt::Debug for Request {
    // The path or query can hold a key, so only the method and host show.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Request")
            .field("method", &self.method)
            .field("host", &self.url.host_str())
            .finish_non_exhaustive()
    }
}

impl Request {
    /// A GET of `segments` under `base`, with `query` appended.
    ///
    /// # Errors
    ///
    /// [`NotABase`] when `base` cannot carry a path.
    pub fn get(base: &Url, segments: &[&str], query: &[(&str, &str)]) -> Result<Self, NotABase> {
        let mut url = under(base, segments)?;
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query.iter().copied());
        }
        Ok(Self::new(Method::Get, url, Body::Empty))
    }

    /// A GET of `url` exactly as given: an address a provider's own data
    /// named, already checked by the caller.
    #[must_use]
    pub(crate) fn get_url(url: Url) -> Self {
        Self::new(Method::Get, url, Body::Empty)
    }

    /// A POST of form pairs to `segments` under `base`. Not idempotent
    /// unless marked so.
    ///
    /// # Errors
    ///
    /// [`NotABase`] when `base` cannot carry a path.
    pub fn post_form(
        base: &Url,
        segments: &[&str],
        form: Vec<(String, String)>,
    ) -> Result<Self, NotABase> {
        Ok(Self::new(
            Method::Post,
            under(base, segments)?,
            Body::Form(form),
        ))
    }

    /// A POST of a JSON document to `segments` under `base`. Not
    /// idempotent unless marked so.
    ///
    /// # Errors
    ///
    /// [`NotABase`] when `base` cannot carry a path.
    pub fn post_json(base: &Url, segments: &[&str], json: Vec<u8>) -> Result<Self, NotABase> {
        Ok(Self::new(
            Method::Post,
            under(base, segments)?,
            Body::Json(json),
        ))
    }

    fn new(method: Method, url: Url, body: Body) -> Self {
        Self {
            method,
            url,
            headers: Vec::new(),
            body,
            idempotent: method == Method::Get,
            max_body: None,
            redirects: Redirects::Refuse,
        }
    }

    /// Add one header.
    #[must_use]
    pub fn header(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.headers.push((name, value.into()));
        self
    }

    /// Mark whether sending twice is safe. A read sent as POST says so.
    #[must_use]
    pub fn idempotent(mut self, idempotent: bool) -> Self {
        self.idempotent = idempotent;
        self
    }

    /// Read at most `cap` bytes of the answer; only ever lowers the
    /// provider's cap.
    #[must_use]
    pub fn max_body(mut self, cap: usize) -> Self {
        self.max_body = Some(cap);
        self
    }

    /// End the path with a slash, for a route the provider only serves
    /// that way.
    #[must_use]
    pub fn trailing_slash(mut self) -> Self {
        if let Ok(mut path) = self.url.path_segments_mut() {
            path.pop_if_empty().push("");
        }
        self
    }

    /// How a redirect is handled.
    #[must_use]
    pub fn redirects(mut self, redirects: Redirects) -> Self {
        self.redirects = redirects;
        self
    }
}

/// A base address that cannot carry a path (`mailto:`, `data:`), so no
/// request can be built under it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the address cannot carry a path")]
pub struct NotABase;

/// `base` with `segments` pushed onto its path.
fn under(base: &Url, segments: &[&str]) -> Result<Url, NotABase> {
    let mut url = base.clone();
    url.set_query(None);
    url.set_fragment(None);
    {
        let mut path = url.path_segments_mut().map_err(|()| NotABase)?;
        path.pop_if_empty();
        path.extend(segments.iter().filter(|segment| !segment.is_empty()));
    }
    Ok(url)
}

/// One provider's behaviour on the wire: a `const` beside its client.
#[derive(Debug)]
pub struct Spec {
    /// The provider name errors carry.
    pub source: &'static str,
    /// The service-health key failures count toward.
    pub health_key: &'static str,
    /// Deadline per attempt, covering the headers and the body.
    pub timeout: Duration,
    /// Largest answer read.
    pub max_body: usize,
    /// The cooldown after a rate limit that named no wait.
    pub retry_after_default: Backoff,
    /// The longest wait a provider may ask for.
    pub retry_after_cap: Duration,
    /// Turns an answer into success or a typed failure;
    /// [`standard_classify`] unless the provider needs its own rules.
    pub classify: fn(&Answer<'_>) -> Result<(), ProviderError>,
}

/// A cooldown chosen when the provider named none.
#[derive(Debug, Clone, Copy)]
pub enum Backoff {
    /// Always this long.
    Fixed(Duration),
    /// Doubling from `base` with each headerless limit in a row, up to
    /// `max`, with jitter.
    Exponential {
        /// The first wait.
        base: Duration,
        /// The longest wait.
        max: Duration,
    },
}

/// One answer as the classifier sees it, with the kernel clock's time for
/// reading dates in its headers.
#[derive(Debug)]
pub struct Answer<'a> {
    /// What came back.
    pub reply: &'a Reply,
    /// When it came back, as unix seconds from the kernel clock.
    pub now_unix: i64,
}

impl Answer<'_> {
    /// The wait the answer asks for, uncapped.
    #[must_use]
    pub fn retry_after(&self) -> Option<Duration> {
        retry_delay_from_headers(
            self.reply
                .headers
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str())),
            self.now_unix,
        )
    }
}

/// The standard status rules: 2xx succeeds, 401, 403, 404, other 4xx,
/// 429, 503 and other 5xx each map to their own variant, and 429 and 503
/// carry the wait the answer names.
pub fn standard_classify(answer: &Answer<'_>) -> Result<(), ProviderError> {
    let provider = answer.reply.provider;
    let status = answer.reply.status;
    Err(match status {
        200..=299 => return Ok(()),
        401 => ProviderError::Unauthorized { provider },
        403 => ProviderError::Forbidden { provider },
        404 => ProviderError::NotFound { provider },
        429 => ProviderError::RateLimited {
            provider,
            retry_after: answer.retry_after(),
        },
        503 => ProviderError::Unavailable {
            provider,
            retry_after: answer.retry_after(),
        },
        300..=399 => ProviderError::Redirect { provider, status },
        400..=499 => ProviderError::BadRequest { provider, status },
        _ => ProviderError::Server { provider, status },
    })
}

/// A provider's answer: the status, the headers that matter, the body.
#[derive(Debug, Clone)]
pub struct Reply {
    provider: &'static str,
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Reply {
    /// The status code.
    #[must_use]
    pub fn status(&self) -> u16 {
        self.status
    }

    /// One kept header, by lowercase name.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// The `content-type` header.
    #[must_use]
    pub fn content_type(&self) -> Option<&str> {
        self.header("content-type")
    }

    /// The body.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.body
    }

    /// Decode the body as `T`.
    ///
    /// # Errors
    ///
    /// [`ProviderError::Payload`] with [`PayloadKind::Decode`] when it does
    /// not decode.
    pub fn json<T: DeserializeOwned>(&self) -> Result<T, ProviderError> {
        serde_json::from_slice(&self.body).map_err(|error| ProviderError::Payload {
            provider: self.provider,
            kind: PayloadKind::Decode,
            message: short(&error.to_string()),
        })
    }
}

/// `text` cut to a log-safe length.
#[must_use]
pub fn short(text: &str) -> String {
    text.chars().take(MAX_MESSAGE).collect()
}

/// What the wire hands back for one sent request.
#[derive(Debug, Clone)]
pub struct WireAnswer {
    /// The status code.
    pub status: u16,
    /// Response headers; the executor keeps the ones it reads.
    pub headers: Vec<(String, String)>,
    /// The body, already read under the cap.
    pub body: Vec<u8>,
}

/// Why the wire produced no answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireError {
    /// Nothing usable came back.
    Transport(TransportKind),
    /// The body passed the cap and was not read.
    TooLarge,
}

/// One request ready for the wire.
pub struct Prepared<'a> {
    /// The provider, for the wire's own log line.
    pub provider: &'static str,
    /// The request.
    pub request: &'a Request,
    /// Where it goes: the request's URL, or a followed hop.
    pub url: &'a Url,
    /// The cap the body is read under.
    pub max_body: usize,
}

impl std::fmt::Debug for Prepared<'_> {
    // The path or query can hold a key, so only the provider and host show.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Prepared")
            .field("provider", &self.provider)
            .field("host", &self.url.host_str())
            .finish_non_exhaustive()
    }
}

/// The outside HTTP service. Production sends with reqwest; tests script
/// the answers.
pub trait Wire: Send + Sync {
    /// Send one request, following no redirects, and read its body under
    /// the cap.
    fn send<'a>(&'a self, prepared: Prepared<'a>) -> BoxFuture<'a, Result<WireAnswer, WireError>>;
}

/// The production wire: the factory's client that never follows a
/// redirect, since redirects are the executor's job.
#[derive(Debug, Clone)]
pub struct ReqwestWire {
    client: reqwest::Client,
}

impl ReqwestWire {
    /// Send through the factory's no-redirect client.
    #[must_use]
    pub fn new(http: &HttpClientFactory) -> Self {
        Self {
            client: http.no_redirect().clone(),
        }
    }
}

impl Wire for ReqwestWire {
    fn send<'a>(&'a self, prepared: Prepared<'a>) -> BoxFuture<'a, Result<WireAnswer, WireError>> {
        Box::pin(async move {
            let method = match prepared.request.method {
                Method::Get => reqwest::Method::GET,
                Method::Post => reqwest::Method::POST,
            };
            let mut outgoing = self.client.request(method, prepared.url.clone());
            for (name, value) in &prepared.request.headers {
                outgoing = outgoing.header(*name, value.as_str());
            }
            outgoing = match &prepared.request.body {
                Body::Empty => outgoing,
                Body::Form(pairs) => outgoing.form(pairs),
                Body::Json(json) => outgoing
                    .header("content-type", "application/json")
                    .body(json.clone()),
            };
            let response = outgoing.send().await.map_err(|error| {
                let kind = if error.is_timeout() {
                    TransportKind::Timeout
                } else if error.is_connect() {
                    TransportKind::Connect
                } else {
                    TransportKind::Other
                };
                tracing::debug!(
                    provider = prepared.provider,
                    cause = droppedneedle_kernel::http_client::transport_error(error),
                    "provider request failed"
                );
                WireError::Transport(kind)
            })?;
            let status = response.status().as_u16();
            let headers = response
                .headers()
                .iter()
                .filter_map(|(name, value)| {
                    Some((name.as_str().to_owned(), value.to_str().ok()?.to_owned()))
                })
                .collect();
            let body =
                read_capped(response, prepared.max_body)
                    .await
                    .map_err(|error| match error {
                        BodyError::TooLarge => WireError::TooLarge,
                        BodyError::Transport(cause) => {
                            tracing::debug!(
                                provider = prepared.provider,
                                cause,
                                "provider answer broke off"
                            );
                            WireError::Transport(TransportKind::Other)
                        }
                    })?;
            Ok(WireAnswer {
                status,
                headers,
                body,
            })
        })
    }
}

/// How many tries a call gets and how long it may take, by priority.
struct Budget {
    attempts: u32,
    total: Duration,
}

impl Budget {
    fn for_priority(priority: RequestPriority) -> Self {
        if priority.is_user_facing() {
            Self {
                attempts: 2,
                total: USER_COOLDOWN_WAIT,
            }
        } else {
            Self {
                attempts: 3,
                total: Duration::from_secs(30),
            }
        }
    }
}

/// Sends one rate row's requests. Built once inside the providers and
/// shared by every caller of that row.
#[derive(Clone)]
pub struct Executor {
    spec: &'static Spec,
    limiter: Arc<RateLimiter>,
    wire: Arc<dyn Wire>,
    health: Arc<ServiceHealth>,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for Executor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Executor")
            .field("source", &self.spec.source)
            .finish_non_exhaustive()
    }
}

impl Executor {
    /// An executor for `spec`, pacing through `limiter`.
    #[must_use]
    pub fn new(
        spec: &'static Spec,
        limiter: Arc<RateLimiter>,
        wire: Arc<dyn Wire>,
        health: Arc<ServiceHealth>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            spec,
            limiter,
            wire,
            health,
            clock,
        }
    }

    /// The limiter this executor paces through.
    #[must_use]
    pub fn limiter(&self) -> &RateLimiter {
        &self.limiter
    }

    /// Send `request` at `priority`.
    ///
    /// # Errors
    ///
    /// The typed failure of the last attempt, or
    /// [`ProviderError::RateLimited`] at once, with the wait, when a page's
    /// call would not get a request within [`USER_COOLDOWN_WAIT`], or an
    /// image fetch meets a longer cooldown.
    pub async fn send(
        &self,
        request: Request,
        priority: RequestPriority,
    ) -> Result<Reply, ProviderError> {
        // An image fetch is not bounded here: its callers bound the
        // person's wait themselves and keep fetching, so it is turned down
        // only during a long cooldown.
        let cooling = self.limiter.cooling_for();
        if priority == RequestPriority::ImageFetch && cooling > USER_COOLDOWN_WAIT {
            return Err(self.decline(cooling));
        }
        let budget = Budget::for_priority(priority);
        let mut first_sent: Option<Instant> = None;
        let mut last_error: Option<ProviderError> = None;
        let mut off_host = false;
        let mut attempt = 1;
        loop {
            // A page's request never queues longer than this.
            let within = (priority == RequestPriority::UserInitiated).then_some(USER_COOLDOWN_WAIT);
            let slot = match within {
                Some(within) => self.limiter.acquire_slot_within(priority, within).await,
                None => self
                    .limiter
                    .acquire_slot(priority)
                    .await
                    .map_err(Refused::from),
            };
            let slot = match slot {
                Ok(slot) => slot,
                Err(Refused::TooLong { wait }) => {
                    return Err(match last_error {
                        Some(error) if off_host => self.finish_off_host(error),
                        Some(error) => self.finish(error),
                        None => self.decline(wait),
                    });
                }
                Err(Refused::OverCapacity(_)) => {
                    return Err(ProviderError::Invalid {
                        provider: self.spec.source,
                        reason: "the rate row grants no requests",
                    });
                }
            };
            let sent = *first_sent.get_or_insert_with(Instant::now);
            let (outcome, foreign) = self.attempt(&request, priority).await;
            // The slot and the token are never held through a sleep.
            drop(slot);
            let error = match outcome {
                Ok(reply) => {
                    self.limiter.clear_headerless_limits();
                    self.answered(None);
                    return Ok(reply);
                }
                Err(error) => {
                    off_host = foreign;
                    let error = self.cap_wait(error);
                    if let Some(delay) = self.cooldown_after(&error) {
                        self.limiter.cool_down(delay);
                    }
                    error
                }
            };
            let wait = if request.idempotent && error.is_retriable() && attempt < budget.attempts {
                let wait = error
                    .retry_after()
                    .unwrap_or_else(|| backoff(attempt))
                    .max(self.limiter.cooling_for());
                (sent.elapsed() + wait < budget.total).then_some(wait)
            } else {
                None
            };
            let Some(wait) = wait else {
                return Err(if off_host {
                    self.finish_off_host(error)
                } else {
                    self.finish(error)
                });
            };
            last_error = Some(error);
            tokio::time::sleep(wait).await;
            attempt += 1;
        }
    }

    /// Turn down a person's call whose turn is `wait` away, without
    /// sending. A local decline is quiet: it is no failure of the provider.
    fn decline(&self, wait: Duration) -> ProviderError {
        let declined = ProviderError::RateLimited {
            provider: self.spec.source,
            retry_after: Some(wait),
        };
        tracing::debug!(provider = self.spec.source, error = %declined, "provider call declined");
        record_current(self.spec.source, IntegrationStatus::Error, false);
        declined
    }

    /// One attempt, redirects included, inside the spec's deadline, counted
    /// on the row with the body bytes it read.
    async fn attempt(
        &self,
        request: &Request,
        priority: RequestPriority,
    ) -> (Result<Reply, ProviderError>, bool) {
        let mut body_bytes = 0;
        let mut foreign = false;
        let outcome = tokio::time::timeout(
            self.spec.timeout,
            self.exchange(request, &mut body_bytes, &mut foreign),
        )
        .await
        .unwrap_or(Err(ProviderError::Transport {
            provider: self.spec.source,
            kind: TransportKind::Timeout,
        }));
        self.limiter
            .record_call(priority, CallOutcome::of(&outcome), body_bytes);
        (outcome, foreign)
    }

    async fn exchange(
        &self,
        request: &Request,
        body_bytes: &mut u64,
        foreign: &mut bool,
    ) -> Result<Reply, ProviderError> {
        let provider = self.spec.source;
        let max_body = request
            .max_body
            .map_or(self.spec.max_body, |cap| cap.min(self.spec.max_body));
        let mut url = request.url.clone();
        let mut hops = 0;
        loop {
            let answer = self
                .wire
                .send(Prepared {
                    provider,
                    request,
                    url: &url,
                    max_body,
                })
                .await;
            let raw = answer.map_err(|error| match error {
                WireError::Transport(kind) => ProviderError::Transport { provider, kind },
                WireError::TooLarge => ProviderError::Payload {
                    provider,
                    kind: PayloadKind::TooLarge,
                    message: format!("larger than {max_body} bytes"),
                },
            })?;
            *body_bytes = body_bytes.saturating_add(raw.body.len() as u64);
            let reply = Reply {
                provider,
                status: raw.status,
                headers: raw
                    .headers
                    .into_iter()
                    .filter(|(name, _)| kept(name))
                    .collect(),
                body: raw.body,
            };
            if (300..400).contains(&reply.status) {
                match request.redirects {
                    Redirects::Refuse => {
                        return Err(ProviderError::Redirect {
                            provider,
                            status: reply.status,
                        });
                    }
                    Redirects::Return => return Ok(reply),
                    Redirects::Follow(rule) => {
                        let Some(location) = reply
                            .header("location")
                            .and_then(|location| url.join(location).ok())
                        else {
                            return Err(contract(provider, "a redirect without a usable location"));
                        };
                        if hops >= rule.max_hops {
                            return Err(contract(provider, "too many redirects"));
                        }
                        let Some(next) = (rule.next)(&url, &location) else {
                            return Err(ProviderError::Redirect {
                                provider,
                                status: reply.status,
                            });
                        };
                        hops += 1;
                        *foreign = (rule.foreign)(&next);
                        url = next;
                        continue;
                    }
                }
            }
            let verdict = (self.spec.classify)(&Answer {
                reply: &reply,
                now_unix: self.clock.now_unix(),
            });
            return verdict.map(|()| reply);
        }
    }

    /// Hold every wait the provider asked for to the spec's cap.
    fn cap_wait(&self, error: ProviderError) -> ProviderError {
        let cap = |wait: Option<Duration>| wait.map(|wait| wait.min(self.spec.retry_after_cap));
        match error {
            ProviderError::RateLimited {
                provider,
                retry_after,
            } => ProviderError::RateLimited {
                provider,
                retry_after: cap(retry_after),
            },
            ProviderError::Unavailable {
                provider,
                retry_after,
            } => ProviderError::Unavailable {
                provider,
                retry_after: cap(retry_after),
            },
            other => other,
        }
    }

    /// The cooldown one failure sets on the limiter: the wait it names, or
    /// the spec's default for a rate limit that named none.
    fn cooldown_after(&self, error: &ProviderError) -> Option<Duration> {
        if let Some(wait) = error.retry_after() {
            return Some(wait);
        }
        if !matches!(error, ProviderError::RateLimited { .. }) {
            return None;
        }
        Some(match self.spec.retry_after_default {
            Backoff::Fixed(wait) => wait,
            Backoff::Exponential { base, max } => {
                let run = self.limiter.note_headerless_limit();
                let doubled = base.saturating_mul(1u32 << run.saturating_sub(1).min(10));
                doubled.min(max).mul_f64(0.5 + 0.5 * unit_random())
            }
        })
    }

    /// Whether a run of `run` failed calls rests the provider: five, or
    /// more where its health entry waits for more.
    fn run_reached(&self, run: u32) -> bool {
        let threshold = health::threshold(self.spec.health_key)
            .unwrap_or(FAILURE_RUN)
            .max(FAILURE_RUN);
        usize::try_from(run).unwrap_or(usize::MAX) >= threshold
    }

    /// Whether the provider is resting after a run of failed calls (an
    /// outage), rather than after a wait it asked for.
    #[must_use]
    pub fn resting_after_failures(&self) -> bool {
        !self.limiter.cooling_for().is_zero() && self.run_reached(self.limiter.failure_run())
    }

    /// Record a call the provider answered without a failure that counts
    /// against it.
    fn answered(&self, error: Option<&ProviderError>) {
        self.health.record_success(self.spec.health_key);
        self.limiter.note_answer();
        if let Some(error) = error {
            tracing::debug!(provider = self.spec.source, %error, "provider call refused");
        }
    }

    /// Hand back the last error of a call whose last hop went to a foreign
    /// target: a failure there is logged but recorded nothing against this
    /// provider; anything else is finished as usual.
    fn finish_off_host(&self, error: ProviderError) -> ProviderError {
        if !error.trips_breaker() {
            return self.finish(error);
        }
        tracing::warn!(provider = self.spec.source, %error, "provider redirect target failed");
        error
    }

    /// Record the final failure once, and hand it back.
    fn finish(&self, error: ProviderError) -> ProviderError {
        let provider = self.spec.source;
        if error.trips_breaker() {
            self.health
                .record_failure(self.spec.health_key, std::time::Instant::now());
            if self.run_reached(self.limiter.note_failure()) {
                self.limiter.cool_down(FAILURE_COOLDOWN);
            }
            tracing::warn!(provider, %error, "provider call failed");
        } else {
            self.answered(Some(&error));
        }
        if error.trips_breaker()
            || matches!(
                error,
                ProviderError::Payload { .. } | ProviderError::Disabled { .. }
            )
        {
            record_current(provider, IntegrationStatus::Error, error.is_deterministic());
        }
        error
    }
}

fn contract(provider: &'static str, message: &str) -> ProviderError {
    ProviderError::Payload {
        provider,
        kind: PayloadKind::Contract,
        message: message.to_owned(),
    }
}

fn kept(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    KEPT_HEADERS.contains(&name.as_str()) || name.starts_with("x-ratelimit-")
}

/// Wait before attempt `attempt + 1`: half a second doubling, at most five
/// seconds, with jitter between half and one and a half times.
fn backoff(attempt: u32) -> Duration {
    let base = Duration::from_millis(500).saturating_mul(1u32 << attempt.saturating_sub(1).min(4));
    base.min(Duration::from_secs(5))
        .mul_f64(0.5 + unit_random())
}

/// A random number in `[0, 1)`; one half when the system has none to give.
fn unit_random() -> f64 {
    let mut bytes = [0u8; 8];
    if getrandom::fill(&mut bytes).is_err() {
        return 0.5;
    }
    (u64::from_le_bytes(bytes) >> 11) as f64 / (1u64 << 53) as f64
}

/// A scripted wire for tests: answers queue per method and URL (without
/// the query), the last one repeating, and every request is recorded. An
/// unscripted request gets no answer, as if nothing listened.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
pub struct ScriptedWire {
    script: Mutex<HashMap<(Method, String), VecDeque<Scripted>>>,
    sent: Mutex<Vec<Sent>>,
}

/// One scripted outcome.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Clone)]
pub enum Scripted {
    /// An answer.
    Answer {
        /// The status.
        status: u16,
        /// The headers.
        headers: Vec<(String, String)>,
        /// The body.
        body: Vec<u8>,
    },
    /// No answer, for this reason.
    Transport(TransportKind),
    /// No answer, ever: the deadline decides.
    Hang,
}

/// One request the scripted wire saw.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Clone)]
pub struct Sent {
    /// The method.
    pub method: Method,
    /// The full URL, query included.
    pub url: Url,
    /// The headers.
    pub headers: Vec<(String, String)>,
    /// The body.
    pub body: Body,
}

#[cfg(any(test, feature = "test-support"))]
impl Sent {
    /// One query value.
    #[must_use]
    pub fn query(&self, key: &str) -> Option<String> {
        self.url
            .query_pairs()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.into_owned())
    }
}

#[cfg(any(test, feature = "test-support"))]
impl ScriptedWire {
    /// A wire with nothing scripted.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue `outcome` for `method` requests to `url`.
    pub fn on(&self, method: Method, url: &str, outcome: Scripted) {
        self.script
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .entry((method, url.to_owned()))
            .or_default()
            .push_back(outcome);
    }

    /// Queue an answer for GETs of `url`.
    pub fn answer(
        &self,
        url: &str,
        status: u16,
        headers: &[(&str, &str)],
        body: impl Into<Vec<u8>>,
    ) {
        self.on(
            Method::Get,
            url,
            Scripted::Answer {
                status,
                headers: headers
                    .iter()
                    .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                    .collect(),
                body: body.into(),
            },
        );
    }

    /// Every request sent so far, in order.
    #[must_use]
    pub fn sent(&self) -> Vec<Sent> {
        self.sent
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }

    fn next(&self, method: Method, url: &Url) -> Option<Scripted> {
        let mut bare = url.clone();
        bare.set_query(None);
        let mut script = self
            .script
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let queue = script.get_mut(&(method, bare.to_string()))?;
        if queue.len() > 1 {
            queue.pop_front()
        } else {
            queue.front().cloned()
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Wire for ScriptedWire {
    fn send<'a>(&'a self, prepared: Prepared<'a>) -> BoxFuture<'a, Result<WireAnswer, WireError>> {
        Box::pin(async move {
            let request = prepared.request;
            self.sent
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .push(Sent {
                    method: request.method,
                    url: prepared.url.clone(),
                    headers: request
                        .headers
                        .iter()
                        .map(|(name, value)| ((*name).to_owned(), value.clone()))
                        .collect(),
                    body: request.body.clone(),
                });
            match self.next(request.method, prepared.url) {
                Some(Scripted::Answer {
                    status,
                    headers,
                    body,
                }) => {
                    if body.len() > prepared.max_body {
                        return Err(WireError::TooLarge);
                    }
                    Ok(WireAnswer {
                        status,
                        headers,
                        body,
                    })
                }
                Some(Scripted::Transport(kind)) => Err(WireError::Transport(kind)),
                Some(Scripted::Hang) => std::future::pending().await,
                None => {
                    tracing::warn!(host = ?prepared.url.host_str(), "unscripted provider request");
                    Err(WireError::Transport(TransportKind::Connect))
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    const CAP: usize = 64 * 1024;

    /// Answer one request with `head` and then `body_bytes` of filler.
    async fn serve_once(head: &'static str, body_bytes: usize) -> Url {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut raw = Vec::new();
            let mut chunk = [0u8; 1024];
            while !raw.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = socket.read(&mut chunk).await.unwrap();
                assert_ne!(read, 0, "client closed before sending headers");
                raw.extend_from_slice(&chunk[..read]);
            }
            // The client hangs up once it has seen too much, so a failed
            // write here is the refusal working.
            let _ = socket.write_all(head.as_bytes()).await;
            let _ = socket.write_all(&vec![b' '; body_bytes]).await;
        });
        Url::parse(&format!("http://{address}/cover")).unwrap()
    }

    async fn send(wire: &ReqwestWire, url: &Url) -> Result<WireAnswer, WireError> {
        let request = Request::get(url, &[], &[]).unwrap();
        wire.send(Prepared {
            provider: "test",
            request: &request,
            url,
            max_body: CAP,
        })
        .await
    }

    /// The real wire never buffers a body past its cap: a declared length
    /// over the cap and a body that streams past it are both refused.
    #[tokio::test]
    async fn a_body_over_the_cap_is_never_buffered_whole() {
        let wire = ReqwestWire::new(&HttpClientFactory::new().unwrap());

        let declared = serve_once("HTTP/1.1 200 OK\r\ncontent-length: 65537\r\n\r\n", 0).await;
        assert!(matches!(
            send(&wire, &declared).await,
            Err(WireError::TooLarge)
        ));

        let streamed = serve_once("HTTP/1.1 200 OK\r\nconnection: close\r\n\r\n", CAP + 4096).await;
        assert!(matches!(
            send(&wire, &streamed).await,
            Err(WireError::TooLarge)
        ));
    }
}
