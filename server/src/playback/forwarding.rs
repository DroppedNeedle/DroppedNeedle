//! Scrobble forwarding to each user's linked Last.fm and ListenBrainz
//! accounts.
//!
//! The reporting services are synchronous and must never wait on the
//! network, so [`ScrobbleForwarder`] checks which services the user linked,
//! queues one job per wanted service on that service's bounded channel, and
//! answers at once; [`ForwardWorker`] (spawned at boot) delivers each
//! service's jobs in its own lane, so a slow service never holds up the
//! other. The name
//! dedup and the history write happen before a job is queued, so a retry
//! never records a play twice.
//!
//! Delivery follows v2's clients: ListenBrainz `POST /1/submit-listens`
//! with `Authorization: Token`, Last.fm signed `track.updateNowPlaying` and
//! `track.scrobble` form posts. Scrobbles that fail for a passing reason
//! (no answer, 5xx, 429, Last.fm codes 11/16/29) are retried with backoff,
//! up to [`SCROBBLE_ATTEMPTS`] tries; a rejected credential or a refused
//! play is dropped with a log line. Now-playing reports are not retried: a
//! late one is wrong. Calls are paced per service (ListenBrainz 1/s,
//! Last.fm 5/s).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::time::Instant;

use crate::auth::users::stores::{LastFmStore, LastFmSwitch};
use crate::plugins::scrobble::ListenBrainzLinkStore;
use crate::runtime_config::crypto::Crypto;

use super::ports::{ReportTrack, ScrobbleLinks, ScrobbleSinks, ScrobbleTargets, ServiceOutcome};
use super::scrobble_models::{AdditionalInfo, LastFmError, Listen, SubmitListens, TrackMetadata};

/// Jobs waiting per service. A burst past this answers "queue full".
pub const FORWARD_QUEUE_DEPTH: usize = 512;
/// Tries per scrobble, the first one included.
pub const SCROBBLE_ATTEMPTS: u32 = 4;
/// First retry delay; each later one triples, capped at [`MAX_BACKOFF`].
pub const BASE_BACKOFF: Duration = Duration::from_secs(5);
/// Longest retry delay.
pub const MAX_BACKOFF: Duration = Duration::from_secs(300);
/// Per-request budget.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// ListenBrainz pacing (provider policy: 1 request per second).
pub const LISTENBRAINZ_SPACING: Duration = Duration::from_secs(1);
/// Last.fm pacing (provider policy: 5 requests per second).
pub const LASTFM_SPACING: Duration = Duration::from_millis(200);
/// Production ListenBrainz API root.
pub const LISTENBRAINZ_BASE_URL: &str = "https://api.listenbrainz.org";
/// Production Last.fm API endpoint.
pub const LASTFM_API_URL: &str = "https://ws.audioscrobbler.com/2.0/";

/// One external scrobble service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Service {
    /// Last.fm.
    LastFm,
    /// ListenBrainz.
    ListenBrainz,
}

impl Service {
    /// Wire name, also the outcome map key.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LastFm => "lastfm",
            Self::ListenBrainz => "listenbrainz",
        }
    }

    fn spacing(self) -> Duration {
        match self {
            Self::LastFm => LASTFM_SPACING,
            Self::ListenBrainz => LISTENBRAINZ_SPACING,
        }
    }
}

/// What a job reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForwardKind {
    /// The track just started.
    NowPlaying,
    /// The play counted.
    Scrobble,
}

impl ForwardKind {
    fn attempts(self) -> u32 {
        match self {
            Self::NowPlaying => 1,
            Self::Scrobble => SCROBBLE_ATTEMPTS,
        }
    }
}

/// One queued forward.
#[derive(Debug, Clone)]
pub struct ForwardJob {
    /// Whose account.
    pub user_id: String,
    /// Which service.
    pub service: Service,
    /// Now-playing or scrobble.
    pub kind: ForwardKind,
    /// The play.
    pub track: ReportTrack,
    /// Tries so far.
    pub attempt: u32,
}

/// The production sink: queue a job per wanted, linked service.
pub struct ScrobbleForwarder {
    lastfm_tx: tokio::sync::mpsc::Sender<ForwardJob>,
    listenbrainz_tx: tokio::sync::mpsc::Sender<ForwardJob>,
    links: Arc<dyn ScrobbleLinks>,
    lastfm_switch: Arc<dyn LastFmSwitch>,
}

/// The per-service queues the worker drains.
pub struct ForwardQueues {
    lastfm: tokio::sync::mpsc::Receiver<ForwardJob>,
    listenbrainz: tokio::sync::mpsc::Receiver<ForwardJob>,
}

impl ScrobbleForwarder {
    /// Build the sink and the queues its worker drains. Last.fm forwards
    /// also need the admin's Last.fm master switch on.
    pub fn channel(
        links: Arc<dyn ScrobbleLinks>,
        lastfm_switch: Arc<dyn LastFmSwitch>,
    ) -> (Self, ForwardQueues) {
        let (lastfm_tx, lastfm) = tokio::sync::mpsc::channel(FORWARD_QUEUE_DEPTH);
        let (listenbrainz_tx, listenbrainz) = tokio::sync::mpsc::channel(FORWARD_QUEUE_DEPTH);
        (
            Self {
                lastfm_tx,
                listenbrainz_tx,
                links,
                lastfm_switch,
            },
            ForwardQueues {
                lastfm,
                listenbrainz,
            },
        )
    }

    fn enqueue(
        &self,
        user_id: &str,
        track: &ReportTrack,
        targets: ScrobbleTargets,
        kind: ForwardKind,
    ) -> HashMap<String, ServiceOutcome> {
        let mut linked = self.links.linked(user_id);
        linked.lastfm &= self.lastfm_switch.enabled();
        let mut outcomes = HashMap::new();
        for (service, wanted, has_link) in [
            (Service::LastFm, targets.lastfm, linked.lastfm),
            (
                Service::ListenBrainz,
                targets.listenbrainz,
                linked.listenbrainz,
            ),
        ] {
            if !wanted || !has_link {
                continue;
            }
            let job = ForwardJob {
                user_id: user_id.to_owned(),
                service,
                kind,
                track: track.clone(),
                attempt: 0,
            };
            let tx = match service {
                Service::LastFm => &self.lastfm_tx,
                Service::ListenBrainz => &self.listenbrainz_tx,
            };
            let outcome = match tx.try_send(job) {
                Ok(()) => ServiceOutcome {
                    success: true,
                    error: None,
                },
                Err(_) => {
                    tracing::warn!(
                        service = service.as_str(),
                        "scrobble forward queue full; dropping the forward"
                    );
                    ServiceOutcome {
                        success: false,
                        error: Some("Forwarding is busy; try again shortly".to_owned()),
                    }
                }
            };
            outcomes.insert(service.as_str().to_owned(), outcome);
        }
        outcomes
    }
}

impl ScrobbleSinks for ScrobbleForwarder {
    fn report_now_playing(
        &self,
        user_id: &str,
        track: &ReportTrack,
        targets: ScrobbleTargets,
    ) -> HashMap<String, ServiceOutcome> {
        self.enqueue(user_id, track, targets, ForwardKind::NowPlaying)
    }

    fn submit_scrobble(
        &self,
        user_id: &str,
        track: &ReportTrack,
        targets: ScrobbleTargets,
    ) -> HashMap<String, ServiceOutcome> {
        self.enqueue(user_id, track, targets, ForwardKind::Scrobble)
    }
}

/// A user's Last.fm signing material. Never logged.
pub struct LastFmSession {
    /// Per-user API key.
    pub api_key: String,
    /// Per-user shared secret.
    pub shared_secret: String,
    /// Session key from the link flow.
    pub session_key: String,
}

/// Credentials the worker needs, looked up per job.
pub trait ScrobbleCredentials: Send + Sync {
    /// The user's ListenBrainz token, when linked.
    fn listenbrainz_token<'a>(
        &'a self,
        user_id: &'a str,
    ) -> crate::remotes::adapter::BoxFuture<'a, Option<String>>;
    /// The user's Last.fm session, when linked and Last.fm is switched on.
    fn lastfm_session<'a>(
        &'a self,
        user_id: &'a str,
    ) -> crate::remotes::adapter::BoxFuture<'a, Option<LastFmSession>>;
}

/// Credentials from the stored links: ListenBrainz rows through the link
/// store, Last.fm rows through the auth store (each secret sealed on its
/// own).
pub struct StoredScrobbleCredentials {
    listenbrainz: Arc<dyn ListenBrainzLinkStore>,
    lastfm: Arc<dyn LastFmStore>,
    lastfm_switch: Arc<dyn LastFmSwitch>,
    crypto: Arc<Crypto>,
}

impl StoredScrobbleCredentials {
    /// Bind the link stores, the Last.fm master switch, and the key.
    pub fn new(
        listenbrainz: Arc<dyn ListenBrainzLinkStore>,
        lastfm: Arc<dyn LastFmStore>,
        lastfm_switch: Arc<dyn LastFmSwitch>,
        crypto: Arc<Crypto>,
    ) -> Self {
        Self {
            listenbrainz,
            lastfm,
            lastfm_switch,
            crypto,
        }
    }
}

impl ScrobbleCredentials for StoredScrobbleCredentials {
    fn listenbrainz_token<'a>(
        &'a self,
        user_id: &'a str,
    ) -> crate::remotes::adapter::BoxFuture<'a, Option<String>> {
        Box::pin(async move { self.listenbrainz.token_for(user_id).await })
    }

    fn lastfm_session<'a>(
        &'a self,
        user_id: &'a str,
    ) -> crate::remotes::adapter::BoxFuture<'a, Option<LastFmSession>> {
        Box::pin(async move {
            if !self.lastfm_switch.enabled() {
                return None;
            }
            let link = match self.lastfm.get(user_id).await {
                Ok(link) => link?,
                Err(error) => {
                    tracing::warn!(?error, "last.fm link read failed; skipping the forward");
                    return None;
                }
            };
            let open = |sealed: Option<String>| -> Option<String> {
                let plaintext = self.crypto.decrypt(sealed.as_deref()?).ok()?;
                (!plaintext.is_empty()).then_some(plaintext)
            };
            let session = LastFmSession {
                api_key: open(link.api_key_encrypted)?,
                shared_secret: open(link.shared_secret_encrypted)?,
                session_key: open(link.session_key_encrypted)?,
            };
            Some(session)
        })
    }
}

/// Where the worker sends requests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoints {
    /// ListenBrainz API root.
    pub listenbrainz: String,
    /// Last.fm API endpoint.
    pub lastfm: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            listenbrainz: LISTENBRAINZ_BASE_URL.to_owned(),
            lastfm: LASTFM_API_URL.to_owned(),
        }
    }
}

/// How one delivery ended.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Delivery {
    /// The service took it.
    Done,
    /// A passing failure; try again after the hinted delay, if any.
    Retry(Option<Duration>, String),
    /// A failure retrying cannot fix.
    Drop(String),
}

/// Drains the forward queues until every sink handle drops.
pub struct ForwardWorker {
    queues: ForwardQueues,
    courier: Courier,
}

/// Makes the upstream calls; shared by both service lanes.
struct Courier {
    http: reqwest::Client,
    credentials: Arc<dyn ScrobbleCredentials>,
    endpoints: Endpoints,
}

impl ForwardWorker {
    /// Build the worker over the forwarder's queues.
    pub fn new(
        queues: ForwardQueues,
        http: reqwest::Client,
        credentials: Arc<dyn ScrobbleCredentials>,
        endpoints: Endpoints,
    ) -> Self {
        Self {
            queues,
            courier: Courier {
                http,
                credentials,
                endpoints,
            },
        }
    }

    /// Deliver both services' jobs side by side, each paced on its own.
    pub async fn run(self) {
        let Self { queues, courier } = self;
        tokio::join!(
            lane(&courier, Service::LastFm, queues.lastfm),
            lane(&courier, Service::ListenBrainz, queues.listenbrainz),
        );
    }
}

/// One service's delivery loop: jobs in order, paced by the service's
/// policy, retries held until they come due.
async fn lane(
    courier: &Courier,
    service: Service,
    mut rx: tokio::sync::mpsc::Receiver<ForwardJob>,
) {
    let mut retries: Vec<(Instant, ForwardJob)> = Vec::new();
    let mut next_slot = Instant::now();
    loop {
        let next_due = retries.iter().map(|(due, _)| *due).min();
        let job = tokio::select! {
            received = rx.recv() => match received {
                Some(job) => job,
                None => break,
            },
            () = sleep_until(next_due), if next_due.is_some() => {
                let now = Instant::now();
                let Some(index) = retries.iter().position(|(due, _)| *due <= now) else {
                    continue;
                };
                retries.swap_remove(index).1
            }
        };
        tokio::time::sleep_until(next_slot).await;
        next_slot = Instant::now() + service.spacing();
        let mut job = job;
        job.attempt += 1;
        match courier.deliver(&job).await {
            Delivery::Done => {}
            Delivery::Retry(hint, cause) if job.attempt < job.kind.attempts() => {
                let delay = hint.unwrap_or_else(|| backoff(job.attempt));
                tracing::debug!(
                    service = job.service.as_str(),
                    attempt = job.attempt,
                    %cause,
                    "scrobble forward failed; retrying"
                );
                retries.push((Instant::now() + delay, job));
            }
            Delivery::Retry(_, cause) => tracing::warn!(
                service = job.service.as_str(),
                attempts = job.attempt,
                kind = ?job.kind,
                %cause,
                "scrobble forward failed; giving up"
            ),
            Delivery::Drop(cause) => tracing::warn!(
                service = job.service.as_str(),
                kind = ?job.kind,
                %cause,
                "scrobble forward refused; dropping it"
            ),
        }
    }
    if !retries.is_empty() {
        tracing::warn!(
            service = service.as_str(),
            pending = retries.len(),
            "scrobble forwarding stopped with retries pending; they are dropped"
        );
    }
}

impl Courier {
    async fn deliver(&self, job: &ForwardJob) -> Delivery {
        match job.service {
            Service::ListenBrainz => {
                let Some(token) = self.credentials.listenbrainz_token(&job.user_id).await else {
                    return Delivery::Drop("ListenBrainz is not linked".to_owned());
                };
                self.listenbrainz(job, &token).await
            }
            Service::LastFm => {
                let Some(session) = self.credentials.lastfm_session(&job.user_id).await else {
                    return Delivery::Drop("Last.fm is not linked".to_owned());
                };
                self.lastfm(job, &session).await
            }
        }
    }

    async fn listenbrainz(&self, job: &ForwardJob, token: &str) -> Delivery {
        let Ok(authorization) = reqwest::header::HeaderValue::from_str(&format!("Token {token}"))
        else {
            return Delivery::Drop("stored ListenBrainz token is not header-safe".to_owned());
        };
        let track = &job.track;
        let body = SubmitListens {
            listen_type: match job.kind {
                ForwardKind::NowPlaying => "playing_now",
                ForwardKind::Scrobble => "single",
            },
            payload: vec![Listen {
                listened_at: match job.kind {
                    ForwardKind::NowPlaying => None,
                    ForwardKind::Scrobble => track.played_at,
                },
                track_metadata: TrackMetadata {
                    artist_name: track.artist_name.clone(),
                    track_name: track.track_name.clone(),
                    release_name: track.album_name.clone().filter(|name| !name.is_empty()),
                    additional_info: (track.duration_ms > 0).then_some(AdditionalInfo {
                        duration_ms: track.duration_ms,
                    }),
                },
            }],
        };
        let Ok(payload) = serde_json::to_vec(&body) else {
            return Delivery::Drop("listen body failed to render".to_owned());
        };
        let response = self
            .http
            .post(format!(
                "{}/1/submit-listens",
                self.endpoints.listenbrainz.trim_end_matches('/')
            ))
            .header(reqwest::header::AUTHORIZATION, authorization)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::ACCEPT, "application/json")
            .body(payload)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await;
        let response = match response {
            Ok(response) => response,
            Err(error) => return Delivery::Retry(None, transport_cause(&error)),
        };
        let status = response.status();
        if status.is_success() {
            return Delivery::Done;
        }
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Delivery::Retry(retry_after(response.headers()), "rate limited".to_owned());
        }
        if status.is_server_error() {
            return Delivery::Retry(None, format!("ListenBrainz answered {status}"));
        }
        Delivery::Drop(format!("ListenBrainz refused the listen ({status})"))
    }

    async fn lastfm(&self, job: &ForwardJob, session: &LastFmSession) -> Delivery {
        let track = &job.track;
        let method = match job.kind {
            ForwardKind::NowPlaying => "track.updateNowPlaying",
            ForwardKind::Scrobble => "track.scrobble",
        };
        let mut params: Vec<(String, String)> = vec![
            ("method".to_owned(), method.to_owned()),
            ("api_key".to_owned(), session.api_key.clone()),
            ("sk".to_owned(), session.session_key.clone()),
            ("artist".to_owned(), track.artist_name.clone()),
            ("track".to_owned(), track.track_name.clone()),
        ];
        if job.kind == ForwardKind::Scrobble {
            let Some(played_at) = track.played_at else {
                return Delivery::Drop("scrobble has no play time".to_owned());
            };
            params.push(("timestamp".to_owned(), played_at.to_string()));
        }
        if let Some(album) = track.album_name.as_deref().filter(|name| !name.is_empty()) {
            params.push(("album".to_owned(), album.to_owned()));
        }
        if track.duration_ms >= 1_000 {
            params.push((
                "duration".to_owned(),
                (track.duration_ms / 1_000).to_string(),
            ));
        }
        if let Some(mbid) = track.mbid.as_deref().filter(|mbid| !mbid.is_empty()) {
            params.push(("mbid".to_owned(), mbid.to_owned()));
        }
        let signature = crate::providers::lastfm::api_sig(&params, &session.shared_secret);
        params.push(("api_sig".to_owned(), signature));
        params.push(("format".to_owned(), "json".to_owned()));
        let response = self
            .http
            .post(&self.endpoints.lastfm)
            .form(&params)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await;
        let response = match response {
            Ok(response) => response,
            Err(error) => return Delivery::Retry(None, transport_cause(&error)),
        };
        let status = response.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Delivery::Retry(retry_after(response.headers()), "rate limited".to_owned());
        }
        if status.is_server_error() {
            return Delivery::Retry(None, format!("Last.fm answered {status}"));
        }
        let body = match response.bytes().await {
            Ok(body) => body,
            Err(error) => return Delivery::Retry(None, transport_cause(&error)),
        };
        if let Ok(error) = serde_json::from_slice::<LastFmError>(&body) {
            return match error.error {
                11 | 16 | 29 => Delivery::Retry(
                    None,
                    format!("Last.fm error {}: {}", error.error, error.message),
                ),
                code => Delivery::Drop(format!("Last.fm error {code}: {}", error.message)),
            };
        }
        if status.is_success() {
            Delivery::Done
        } else {
            Delivery::Drop(format!("Last.fm refused the scrobble ({status})"))
        }
    }
}

/// Retry delay for the nth failed try: 5s, 15s, 45s, ... capped at five
/// minutes, plus up to a second of jitter.
fn backoff(attempt: u32) -> Duration {
    let factor = 3u32.saturating_pow(attempt.saturating_sub(1));
    let base = BASE_BACKOFF.saturating_mul(factor).min(MAX_BACKOFF);
    let mut jitter = [0u8; 2];
    let jitter_ms = if getrandom::fill(&mut jitter).is_ok() {
        u64::from(u16::from_le_bytes(jitter)) % 1_000
    } else {
        0
    };
    base + Duration::from_millis(jitter_ms)
}

/// A `Retry-After` in seconds, capped at the longest backoff.
fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|text| text.trim().parse::<u64>().ok())
        .map(|secs| Duration::from_secs(secs).min(MAX_BACKOFF))
}

/// Transport failures without the URL (it may carry nothing secret, but
/// the log does not need it).
fn transport_cause(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        "timed out".to_owned()
    } else if error.is_connect() {
        "could not connect".to_owned()
    } else {
        "request failed".to_owned()
    }
}

async fn sleep_until(due: Option<Instant>) {
    match due {
        Some(due) => tokio::time::sleep_until(due).await,
        None => std::future::pending::<()>().await,
    }
}
