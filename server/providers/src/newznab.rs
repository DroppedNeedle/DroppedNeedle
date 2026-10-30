//! Newznab indexers: caps and search, each indexer on an executor of its
//! own.
//!
//! An indexer is a service on the internet the user signed up to, so
//! every call goes through the executor: paced on the `newznab` row,
//! cooled down by a 429's `Retry-After` or a "request limit reached"
//! answer, and retried inside the caller's budget. Indexers publish no
//! common rate, so each one gets its own bucket of the row's shape, keyed
//! by its id, and keeps it (with its cooldown and its counters) across
//! settings saves.
//!
//! Most indexers also cap API hits and NZB grabs a day, and say how far
//! along they are in `<newznab:apilimits>` on their answers. The client
//! keeps the last counts it read and refuses a call before it is sent once
//! a cap is reached, with the wait until the cap frees up as an honest
//! rate limit. The counts are a rolling day: room comes back a day after
//! the oldest counted call (`apiOldestTime`, `grabOldestTime`). An indexer
//! that names a cap but no oldest time is asked again an hour after it was
//! seen full. A search also stops while grabs are spent, since none of
//! its releases could be fetched.
//!
//! The key is an `apikey` query parameter. It never reaches a log line or
//! an error: the executor shows only the host, and a failure carries only
//! its kind. Wire shapes are in `newznab/models.rs`, the XML reader in
//! `newznab/xml.rs`.

mod models;
mod xml;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use droppedneedle_kernel::clock::Clock;
use thiserror::Error;

use crate::client::{parse_base, unusable_base};
use crate::error::{PayloadKind, ProviderError};
use crate::executor::{
    Answer, Backoff, Executor, RedirectRule, Redirects, Request, Spec, Url, Wire, never_foreign,
    short, standard_classify,
};
use crate::health::ServiceHealth;
use crate::limiter::{RateLimiter, RatePolicy, RequestPriority};
use crate::rates::{self, Rate, RateRow};

use models::{ErrorAnswer, error_answer, parse_apilimits, parse_caps, parse_items, safe_parse};
pub use models::{
    NewznabApiLimits, NewznabCaps, NewznabCategory, NewznabSubcategory, UsenetRelease,
    days_from_civil, parse_rfc2822_date,
};

/// The provider name errors carry, and the rate row every indexer's
/// bucket is shaped from.
pub const SOURCE: &str = "newznab";
/// The most an indexer answer may weigh. A search page is at most a few
/// megabytes even from an indexer that allows 1000 results a page (about
/// 3 KB an item with extended attributes), and a caps document is tens of
/// kilobytes, so 10 MB refuses nothing real.
pub const MAX_BODY_BYTES: usize = 10 * 1024 * 1024;
/// The window an indexer's daily counters cover.
const DAY_SECS: i64 = 86_400;
/// How long a full counter that names no oldest time waits before the
/// indexer is asked again.
const UNSTATED_RESET_SECS: i64 = 3_600;
/// How far into an answer the `<error>` check looks before parsing it.
const ERROR_SNIFF_BYTES: usize = 512;

/// Newznab on the wire: 30 seconds an attempt, as indexers were given
/// before; an `<error>` naming the key or a reached limit is a refusal or
/// a rate limit even under HTTP 200. A limit that names no wait rests the
/// indexer five minutes, doubling while it repeats, up to an hour. A
/// stated wait is honoured up to a day, the longest a daily cap can hold.
pub static SPEC: Spec = Spec {
    source: SOURCE,
    health_key: SOURCE,
    timeout: Duration::from_secs(30),
    max_body: MAX_BODY_BYTES,
    retry_after_default: Backoff::ExponentialAtLeast {
        base: Duration::from_secs(300),
        max: Duration::from_secs(3_600),
    },
    retry_after_cap: Duration::from_secs(86_400),
    classify,
};

/// The status rules, then the `<error>` an indexer sends under 200.
fn classify(answer: &Answer<'_>) -> Result<(), ProviderError> {
    standard_classify(answer)?;
    let body = answer.reply.bytes();
    let head = String::from_utf8_lossy(&body[..body.len().min(ERROR_SNIFF_BYTES)]);
    if !head.contains("<error") {
        return Ok(());
    }
    let Ok(root) = safe_parse(body) else {
        return Ok(());
    };
    match error_answer(&root) {
        Some(ErrorAnswer::Key) => Err(ProviderError::Unauthorized { provider: SOURCE }),
        Some(ErrorAnswer::Limit) => Err(ProviderError::RateLimited {
            provider: SOURCE,
            retry_after: answer.retry_after(),
        }),
        Some(ErrorAnswer::Other { .. }) | None => Ok(()),
    }
}

/// Redirects stay on the indexer's host (an `http` address moving to
/// `https` included), never carrying credentials.
static HOPS: RedirectRule = RedirectRule {
    max_hops: 3,
    next: same_host,
    foreign: never_foreign,
};

fn same_host(from: &Url, to: &Url) -> Option<Url> {
    if !to.username().is_empty() || to.password().is_some() {
        return None;
    }
    if !matches!(to.scheme(), "http" | "https") || to.host_str() != from.host_str() {
        return None;
    }
    let mut next = to.clone();
    next.set_fragment(None);
    Some(next)
}

/// A Newznab failure: the executor's typed error, or an `<error>` answer
/// that is neither a refused key nor a reached limit.
#[derive(Debug, Clone, Error)]
pub enum NewznabError {
    /// The call failed the way any provider call can: a refused key is
    /// [`ProviderError::Unauthorized`], a reached cap or limit
    /// [`ProviderError::RateLimited`] with the wait when known.
    #[error(transparent)]
    Provider(#[from] ProviderError),
    /// `<error code= description=>` with any other code, such as 202 for
    /// a function the indexer lacks.
    #[error("{message}")]
    Api {
        /// Description text.
        message: String,
        /// Newznab error code.
        code: i32,
    },
}

impl NewznabError {
    /// Newznab error code, when the failure carries one.
    #[must_use]
    pub fn code(&self) -> Option<i32> {
        match self {
            Self::Api { code, .. } => Some(*code),
            Self::Provider(_) => None,
        }
    }
}

/// One daily counter as the indexer last reported it.
#[derive(Debug, Default)]
struct Counter {
    current: Option<i64>,
    max: Option<i64>,
    oldest: Option<i64>,
    /// When it was first seen full, for an indexer that names no oldest
    /// time.
    full_since: Option<i64>,
}

impl Counter {
    fn read(&mut self, current: Option<i64>, max: Option<i64>, oldest: Option<i64>, now: i64) {
        if current.is_some() {
            self.current = current;
        }
        if max.is_some() {
            self.max = max;
        }
        self.oldest = oldest;
        self.settle(now);
    }

    /// One more call counted before its answer says so.
    fn spend(&mut self, now: i64) {
        if let Some(current) = self.current.as_mut() {
            *current += 1;
        }
        self.settle(now);
    }

    fn settle(&mut self, now: i64) {
        if self.full() {
            self.full_since.get_or_insert(now);
        } else {
            self.full_since = None;
        }
    }

    fn full(&self) -> bool {
        matches!((self.current, self.max), (Some(current), Some(max)) if max > 0 && current >= max)
    }

    /// Seconds until the cap frees up, or `None` while there is room.
    fn wait(&self, now: i64) -> Option<i64> {
        if !self.full() {
            return None;
        }
        let reset = match (self.oldest, self.full_since) {
            (Some(oldest), _) => oldest + DAY_SECS,
            (None, Some(since)) => since + UNSTATED_RESET_SECS,
            (None, None) => return None,
        };
        (reset > now).then(|| reset - now)
    }
}

/// An indexer's API and grab counters.
#[derive(Debug, Default)]
struct Allowance {
    api: Counter,
    grab: Counter,
}

/// One indexer: its address, its executor and its counters.
#[derive(Debug)]
struct Indexer {
    url: String,
    executor: Executor,
    allowance: Mutex<Allowance>,
}

impl Indexer {
    fn allowance(&self) -> std::sync::MutexGuard<'_, Allowance> {
        self.allowance
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Every indexer the app has called, keyed by its id. Built once with the
/// providers; clones share the indexers.
#[derive(Clone)]
pub struct NewznabIndexers {
    row: Option<RateRow>,
    shared: Arc<RateLimiter>,
    wire: Arc<dyn Wire>,
    health: Arc<ServiceHealth>,
    clock: Arc<dyn Clock>,
    known: Arc<Mutex<HashMap<String, Arc<Indexer>>>>,
}

impl std::fmt::Debug for NewznabIndexers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NewznabIndexers").finish_non_exhaustive()
    }
}

impl NewznabIndexers {
    /// Indexers over `wire`, each paced like `row_limiter`, the `newznab`
    /// row's limiter (unpaced in tests that ask for it).
    #[must_use]
    pub(crate) fn new(
        row_limiter: Arc<RateLimiter>,
        wire: Arc<dyn Wire>,
        health: Arc<ServiceHealth>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let pace: RatePolicy = row_limiter.policy();
        let row = rates::row(SOURCE).map(|row| RateRow {
            per_second: Rate::Fixed(pace.per_second),
            burst: pace.burst,
            ..*row
        });
        Self {
            row,
            shared: row_limiter,
            wire,
            health,
            clock,
            known: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// The client for the indexer `id` at `url`. An indexer keeps its
    /// bucket, cooldown and counters while its id and address stay the
    /// same; a new address starts afresh. An indexer not saved yet (an
    /// empty id) is keyed by its address.
    #[must_use]
    pub fn client(&self, id: &str, name: &str, url: &str, api_key: &str) -> NewznabClient {
        let url = url.trim().trim_end_matches('/');
        let key = if id.is_empty() { url } else { id };
        let indexer = {
            let mut known = self
                .known
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match known.get(key) {
                Some(indexer) if indexer.url == url => Arc::clone(indexer),
                _ => {
                    let indexer = Arc::new(self.indexer(url));
                    known.insert(key.to_owned(), Arc::clone(&indexer));
                    indexer
                }
            }
        };
        NewznabClient {
            indexer,
            base: parse_base(url, SOURCE),
            api_key: api_key.to_owned(),
            id: id.to_owned(),
            name: if name.is_empty() {
                url.to_owned()
            } else {
                name.to_owned()
            },
            clock: Arc::clone(&self.clock),
        }
    }

    fn indexer(&self, url: &str) -> Indexer {
        let limiter = match &self.row {
            Some(row) => Arc::new(RateLimiter::from_row(row)),
            None => Arc::clone(&self.shared),
        };
        Indexer {
            url: url.to_owned(),
            executor: Executor::new(
                &SPEC,
                limiter,
                Arc::clone(&self.wire),
                Arc::clone(&self.health),
                Arc::clone(&self.clock),
            ),
            allowance: Mutex::new(Allowance::default()),
        }
    }
}

/// One indexer's client. Cheap to clone; clones share the indexer's
/// bucket and counters.
#[derive(Clone)]
pub struct NewznabClient {
    indexer: Arc<Indexer>,
    base: Option<Url>,
    api_key: String,
    id: String,
    name: String,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for NewznabClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NewznabClient")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("api_key", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl NewznabClient {
    /// Indexer id this client searches.
    #[must_use]
    pub fn indexer_id(&self) -> &str {
        &self.id
    }

    /// Indexer display name.
    #[must_use]
    pub fn indexer_name(&self) -> &str {
        &self.name
    }

    /// Parsed `t=caps`.
    ///
    /// # Errors
    ///
    /// The executor's typed failure, a rate limit while the API cap is
    /// reached, or [`NewznabError::Api`] for any other `<error>`.
    pub async fn caps(&self, priority: RequestPriority) -> Result<NewznabCaps, NewznabError> {
        let (root, _) = self.get(&[("t", "caps")], false, priority).await?;
        Ok(parse_caps(&root))
    }

    /// Free-text `t=search`: the always-available path (DrunkenSlug only
    /// supports this; `t=music` returns error 202).
    ///
    /// # Errors
    ///
    /// As [`NewznabClient::caps`], and a rate limit while grabs are spent.
    pub async fn search(
        &self,
        query: &str,
        categories: &[i32],
        offset: u32,
        limit: u32,
        priority: RequestPriority,
    ) -> Result<(Vec<UsenetRelease>, Option<NewznabApiLimits>), NewznabError> {
        let mut params = vec![
            ("t".to_owned(), "search".to_owned()),
            ("q".to_owned(), query.to_owned()),
        ];
        page_params(&mut params, categories, offset, limit);
        self.search_request(&params, priority).await
    }

    /// Structured `t=music`, used only when caps advertises audio-search
    /// with artist/album params. The caller falls back to `search` on a 202.
    ///
    /// # Errors
    ///
    /// As [`NewznabClient::search`].
    #[allow(clippy::too_many_arguments)]
    pub async fn music_search(
        &self,
        artist: &str,
        album: &str,
        categories: &[i32],
        year: Option<i32>,
        offset: u32,
        limit: u32,
        priority: RequestPriority,
    ) -> Result<(Vec<UsenetRelease>, Option<NewznabApiLimits>), NewznabError> {
        let mut params = vec![
            ("t".to_owned(), "music".to_owned()),
            ("artist".to_owned(), artist.to_owned()),
            ("album".to_owned(), album.to_owned()),
        ];
        if let Some(year) = year {
            params.push(("year".to_owned(), year.to_string()));
        }
        page_params(&mut params, categories, offset, limit);
        self.search_request(&params, priority).await
    }

    async fn search_request(
        &self,
        params: &[(String, String)],
        priority: RequestPriority,
    ) -> Result<(Vec<UsenetRelease>, Option<NewznabApiLimits>), NewznabError> {
        let refs: Vec<(&str, &str)> = params
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        let (root, url) = self.get(&refs, true, priority).await?;
        let releases = parse_items(&root, &self.id, &self.name, &url);
        Ok((releases, parse_apilimits(&root)))
    }

    /// The wait before the indexer may be called, while a cap it reported
    /// is reached: the API cap always, the grab cap for a search.
    fn capped(&self, search: bool) -> Option<Duration> {
        let now = self.clock.now_unix();
        let allowance = self.indexer.allowance();
        let api = allowance.api.wait(now);
        let grab = if search {
            allowance.grab.wait(now)
        } else {
            None
        };
        api.max(grab)
            .map(|wait| Duration::from_secs(u64::try_from(wait).unwrap_or(0)))
    }

    async fn get(
        &self,
        params: &[(&str, &str)],
        search: bool,
        priority: RequestPriority,
    ) -> Result<(xml::Element, Url), NewznabError> {
        let base = self.base.as_ref().ok_or_else(|| unusable_base(SOURCE))?;
        if let Some(wait) = self.capped(search) {
            tracing::debug!(
                indexer = self.name.as_str(),
                wait_secs = wait.as_secs(),
                "newznab indexer at its daily cap; not called"
            );
            return Err(ProviderError::RateLimited {
                provider: SOURCE,
                retry_after: Some(wait),
            }
            .into());
        }
        let mut query = params.to_vec();
        query.push(("apikey", &self.api_key));
        let request = Request::get(base, &[], &query)
            .map_err(|_| unusable_base(SOURCE))?
            .redirects(Redirects::Follow(&HOPS));
        self.indexer.allowance().api.spend(self.clock.now_unix());
        let reply = self.indexer.executor.send(request, priority).await?;
        let root = safe_parse(reply.bytes()).map_err(|message| ProviderError::Payload {
            provider: SOURCE,
            kind: PayloadKind::Decode,
            message: short(&message),
        })?;
        if let Some(limits) = parse_apilimits(&root) {
            self.read_limits(&limits);
        }
        match error_answer(&root) {
            None => Ok((root, base.clone())),
            Some(ErrorAnswer::Key) => Err(ProviderError::Unauthorized { provider: SOURCE }.into()),
            Some(ErrorAnswer::Limit) => Err(ProviderError::RateLimited {
                provider: SOURCE,
                retry_after: None,
            }
            .into()),
            Some(ErrorAnswer::Other { code, message }) => Err(NewznabError::Api { message, code }),
        }
    }

    fn read_limits(&self, limits: &NewznabApiLimits) {
        let now = self.clock.now_unix();
        let mut allowance = self.indexer.allowance();
        allowance
            .api
            .read(limits.api_current, limits.api_max, limits.api_oldest, now);
        allowance.grab.read(
            limits.grab_current,
            limits.grab_max,
            limits.grab_oldest,
            now,
        );
    }
}

/// `extended=1` always, then the page and the categories.
fn page_params(params: &mut Vec<(String, String)>, categories: &[i32], offset: u32, limit: u32) {
    params.push(("extended".to_owned(), "1".to_owned()));
    params.push(("offset".to_owned(), offset.to_string()));
    params.push(("limit".to_owned(), limit.to_string()));
    if !categories.is_empty() {
        let cats = categories
            .iter()
            .map(i32::to_string)
            .collect::<Vec<_>>()
            .join(",");
        params.push(("cat".to_owned(), cats));
    }
}
