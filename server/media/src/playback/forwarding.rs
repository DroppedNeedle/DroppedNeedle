//! Sending plays to each user's linked Last.fm and ListenBrainz accounts.
//!
//! **Scrobbles** wait in the jobs table. A counted play is written with one
//! job per service it goes to (kinds `scrobble.lastfm` and
//! `scrobble.listenbrainz`, keyed by the play's id, the id as payload), so
//! a scrobble outlives a restart and an outage. [`ScrobbleJobs`] sends one:
//! the request is built from the stored play, so every send of a play is
//! the same request. A job fails for good when its play is gone, when the
//! play is older than [`WINDOW_SECS`], when the user has turned that
//! service off or unlinked it, when the service refuses the user's
//! credential, and when it refuses the listen itself. Anything passing (no
//! answer, 5xx, a rate limit, Last.fm's offline codes and daily limit,
//! ListenBrainz's policy block) is tried again, backing off to once an
//! hour, after a longer wait the service named, while the play is inside
//! the window. One user's failure goes back to wait its turn; other users'
//! jobs go first.
//!
//! The window is Last.fm's: a scrobble older than about two weeks comes
//! back "ignored, timestamp too old" (ignored code 3,
//! https://www.last.fm/api/show/track.scrobble). The docs name the code but
//! no day count; two weeks is what clients report
//! (https://redmine.audacious-media-player.org/issues/260, read
//! 2026-10-21) and the age the scrobble route accepts. ListenBrainz takes
//! listens back to 2002 (`LISTEN_MINIMUM_TS`,
//! https://listenbrainz.readthedocs.io/en/latest/users/api/core.html), and
//! keeps the same window so a play is either sent to both or to neither.
//!
//! Order: first tries go in play order. A retried play may land after
//! later ones; each listen carries its own play time. ListenBrainz keys and
//! sorts listens by that time. Last.fm's scrobbling page asks for cached
//! scrobbles before new ones (https://www.last.fm/api/scrobbling), but its
//! only time rule is the too-old and too-new ignore, so one failing play is
//! never allowed to hold the rest back.
//!
//! Repeats: a crash after the service answered and before the job is
//! marked done sends that play once more. ListenBrainz drops it: it stores
//! listens with `ON CONFLICT (listened_at, user_id, recording_msid) DO
//! NOTHING` (listenbrainz-server `listenstore/timescale_listenstore.py`,
//! read 2026-10-21) and the body is the same. Last.fm documents nothing, so
//! that one play may show twice. Each kind runs one job at a time, so a
//! crash costs at most one play per service.
//!
//! **Now-playing** reports are not history: they stay in memory, one slot
//! per user and service where the latest report wins, sent once by
//! [`NowPlayingWorker`] and dropped at shutdown. Both paths go through the
//! service's one provider client at background priority, sharing its
//! bucket with every other call to it. No token reaches a log line.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

use droppedneedle_auth::users::stores::LastFmSwitch;
use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::DbError;
use droppedneedle_kernel::jobs::BoxFuture;
use droppedneedle_kernel::jobs::CancellationToken;
use droppedneedle_kernel::jobs::queue::{
    JobCtx, JobHandler, JobKind, KindSpec, Outcome, PAYLOAD_INVALID,
};
use droppedneedle_providers::lastfm::{
    IgnoredReason, LastFmClient, LastFmCredentials, ScrobbleAnswer,
};
use droppedneedle_providers::listenbrainz::ListenBrainzClient;
use droppedneedle_providers::{PayloadKind, ProviderError, RequestPriority};

use super::listening::LastFmSession;
use super::ports::{ReportTrack, ScrobbleSinks, ScrobbleTargets};
use super::report::SCROBBLE_TIMESTAMP_MAX_AGE_SECS;
use super::scrobble_models::{AdditionalInfo, Listen, SubmitListens, TrackMetadata};
use super::sqlite::{PlaybackStore, StoredPlay};

/// How long after a play its scrobble may still be sent, in seconds.
pub const WINDOW_SECS: i64 = SCROBBLE_TIMESTAMP_MAX_AGE_SECS;
/// First retry delay; each later one doubles, up to [`BACKOFF_CAP`].
pub const BACKOFF_FIRST: Duration = Duration::from_secs(60);
/// Longest retry delay.
pub const BACKOFF_CAP: Duration = Duration::from_secs(3600);
/// Tries per scrobble. Without jitter the ladder needs 342 tries to cover
/// the window (six retries reach an hour, then one an hour); the rest
/// cover restarts, which requeue a running job at once. The window, not
/// this count, is what ends a waiting job.
pub const MAX_ATTEMPTS: u32 = 400;

/// The play's row is gone (the user was deleted).
pub const PLAY_GONE: &str = "PLAY_GONE";
/// The play is older than the window, or the service said it is too old.
pub const WINDOW_PASSED: &str = "WINDOW_PASSED";
/// The user turned forwarding to this service off.
pub const FORWARDING_OFF: &str = "FORWARDING_OFF";
/// The user has no usable link to this service.
pub const NOT_LINKED: &str = "NOT_LINKED";
/// The admin has Last.fm switched off.
pub const LASTFM_OFF: &str = "LASTFM_OFF";
/// The service refused the user's credential.
pub const CREDENTIAL_REFUSED: &str = "CREDENTIAL_REFUSED";
/// The service refused the listen itself.
pub const LISTEN_REFUSED: &str = "LISTEN_REFUSED";
/// ListenBrainz switched the endpoint off upstream for now.
pub const POLICY_BLOCKED: &str = "POLICY_BLOCKED";
/// No answer, or a server error.
pub const SERVICE_UNAVAILABLE: &str = "SERVICE_UNAVAILABLE";
/// The service asked us to slow down.
pub const RATE_LIMITED: &str = "RATE_LIMITED";
/// The user's Last.fm account reached its daily scrobble limit.
pub const DAILY_LIMIT: &str = "DAILY_LIMIT";
/// Last.fm refused the admin's API key, which the user's session runs on:
/// waits for the admin to fix the key.
pub const INSTANCE_KEY_REFUSED: &str = "INSTANCE_KEY_REFUSED";
/// The database read failed.
pub const STORE_FAILED: &str = "STORE_FAILED";
/// The admin's Last.fm settings cannot be read: waits until they can.
pub const SETTINGS_UNREADABLE: &str = "SETTINGS_UNREADABLE";

/// One external scrobble service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Service {
    /// Last.fm.
    LastFm,
    /// ListenBrainz.
    ListenBrainz,
}

impl Service {
    /// Both services.
    pub const ALL: [Self; 2] = [Self::LastFm, Self::ListenBrainz];

    /// Wire name, also the response map key.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LastFm => "lastfm",
            Self::ListenBrainz => "listenbrainz",
        }
    }

    /// The job kind its scrobbles wait as.
    pub const fn kind(self) -> JobKind {
        match self {
            Self::LastFm => JobKind::new("scrobble.lastfm"),
            Self::ListenBrainz => JobKind::new("scrobble.listenbrainz"),
        }
    }

    /// How its scrobble jobs run.
    pub const fn spec(self) -> KindSpec {
        KindSpec::new(self.kind())
            .max_attempts(MAX_ATTEMPTS)
            .backoff(BACKOFF_FIRST, BACKOFF_CAP)
            .keep_done(Duration::from_secs(24 * 3600))
    }
}

/// A scrobble job's payload: the play to send.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScrobblePayload {
    /// The play's id in `library_play_history`.
    pub play_id: String,
}

/// Makes the calls to both services. Credentials are read per send, so a
/// link made or removed since the play counts.
pub struct Courier {
    listenbrainz: ListenBrainzClient,
    lastfm: LastFmClient,
    store: Arc<PlaybackStore>,
    lastfm_switch: LastFmSwitch,
    clock: Arc<dyn Clock>,
}

impl Courier {
    /// Send through the one ListenBrainz client and the one Last.fm client,
    /// reading plays and links from `store`. `clock` is the jobs table's:
    /// the window is measured on it.
    pub fn new(
        listenbrainz: ListenBrainzClient,
        lastfm: LastFmClient,
        store: Arc<PlaybackStore>,
        lastfm_switch: LastFmSwitch,
        clock: Arc<dyn Clock>,
    ) -> Arc<Self> {
        Arc::new(Self {
            listenbrainz,
            lastfm,
            store,
            lastfm_switch,
            clock,
        })
    }
}

/// The scrobble job handler of one service.
pub struct ScrobbleJobs {
    service: Service,
    courier: Arc<Courier>,
}

impl ScrobbleJobs {
    /// Both services' kinds and handlers, for the builder to register.
    pub fn handlers(courier: &Arc<Courier>) -> Vec<(KindSpec, Arc<dyn JobHandler>)> {
        Service::ALL
            .into_iter()
            .map(|service| {
                let handler: Arc<dyn JobHandler> = Arc::new(Self {
                    service,
                    courier: courier.clone(),
                });
                (service.spec(), handler)
            })
            .collect()
    }
}

impl JobHandler for ScrobbleJobs {
    fn run(&self, job: JobCtx) -> BoxFuture<'static, Outcome> {
        let (service, courier) = (self.service, self.courier.clone());
        Box::pin(async move {
            let outcome = courier.scrobble(service, &job).await;
            if let Outcome::Fail { code } = outcome {
                tracing::info!(service = service.as_str(), code, "scrobble not sent");
            }
            outcome
        })
    }
}

impl Courier {
    /// Send one waiting scrobble, or say why not.
    async fn scrobble(&self, service: Service, job: &JobCtx) -> Outcome {
        let Ok(payload) = job.payload::<ScrobblePayload>() else {
            return fail(PAYLOAD_INVALID);
        };
        let play = match self.store.play(&payload.play_id).await {
            Ok(Some(play)) => play,
            Ok(None) => return fail(PLAY_GONE),
            Err(error) => return store_failed(&error),
        };
        if play
            .played_at
            .saturating_add(WINDOW_SECS)
            .saturating_mul(1000)
            < self.clock.now_ms()
        {
            return fail(WINDOW_PASSED);
        }
        let (prefs, links) = match self.store.forwarding(&play.user_id) {
            Ok(read) => read,
            Err(error) => return store_failed(&error),
        };
        let on = match service {
            Service::LastFm => prefs.scrobble_to_lastfm,
            Service::ListenBrainz => prefs.scrobble_to_listenbrainz,
        };
        if !on {
            return fail(FORWARDING_OFF);
        }
        // Only a section that reads as off drops Last.fm's scrobbles; one
        // that cannot be read holds them until it can.
        let lastfm = match service {
            Service::LastFm => match self.lastfm_switch.read() {
                Ok(state) if state.enabled => Some(state),
                Ok(_) => return fail(LASTFM_OFF),
                Err(_) => return retry(SETTINGS_UNREADABLE, None),
            },
            Service::ListenBrainz => None,
        };
        if !links.wants(service) {
            return fail(NOT_LINKED);
        }
        // A read that fails waits; only a link that reads but does not open
        // ends the job.
        match lastfm {
            None => match self.store.listenbrainz_token_checked(&play.user_id) {
                Ok(Some(token)) => self.send_listen(&play, &token, job.attempt()).await,
                Ok(None) => fail(NOT_LINKED),
                Err(error) => store_failed(&error),
            },
            Some(state) => {
                let instance = state.instance_keys.as_ref();
                match self.store.lastfm_session_checked(&play.user_id, instance) {
                    Ok(Some(session)) => {
                        let on_instance =
                            instance.is_some_and(|keys| keys.api_key == session.api_key);
                        self.send_scrobble(&play, &session, on_instance, job.attempt())
                            .await
                    }
                    Ok(None) => fail(NOT_LINKED),
                    Err(error) => store_failed(&error),
                }
            }
        }
    }

    async fn send_listen(&self, play: &StoredPlay, token: &str, attempt: u32) -> Outcome {
        let body = SubmitListens {
            listen_type: "single",
            payload: vec![Listen {
                listened_at: Some(play.played_at),
                track_metadata: metadata(
                    &play.artist_name,
                    &play.track_name,
                    play.album_name.as_deref(),
                    play.duration_ms,
                    play.recording_mbid.as_deref(),
                ),
            }],
        };
        let Ok(body) = serde_json::to_vec(&body) else {
            return fail(LISTEN_REFUSED);
        };
        let sent = self
            .listenbrainz
            .submit_listens(token, body, RequestPriority::BackgroundSync)
            .await;
        match sent {
            Ok(()) => Outcome::Done,
            // A stored token that cannot ride in a header.
            Err(ProviderError::Invalid { .. }) => fail(CREDENTIAL_REFUSED),
            Err(ProviderError::Disabled { .. }) => retry(POLICY_BLOCKED, None),
            Err(error) => self.answer(Service::ListenBrainz, &error, attempt),
        }
    }

    async fn send_scrobble(
        &self,
        play: &StoredPlay,
        session: &LastFmSession,
        on_instance_key: bool,
        attempt: u32,
    ) -> Outcome {
        let timestamp = play.played_at.to_string();
        let duration = (play.duration_ms / 1_000).to_string();
        let mut params: Vec<(&str, &str)> = vec![
            ("artist", &play.artist_name),
            ("track", &play.track_name),
            ("timestamp", &timestamp),
        ];
        if let Some(album) = play.album_name.as_deref().filter(|name| !name.is_empty()) {
            params.push(("album", album));
        }
        if play.duration_ms >= 1_000 {
            params.push(("duration", &duration));
        }
        let mbid = recording(play.recording_mbid.as_deref());
        if let Some(mbid) = mbid.as_deref() {
            params.push(("mbid", mbid));
        }
        let sent = self
            .lastfm
            .scrobble(
                &credentials(session),
                &params,
                RequestPriority::BackgroundSync,
            )
            .await;
        match sent {
            Ok(ScrobbleAnswer::Accepted) => Outcome::Done,
            Ok(ScrobbleAnswer::Ignored(IgnoredReason::DailyLimit)) => retry(DAILY_LIMIT, None),
            Ok(ScrobbleAnswer::Ignored(IgnoredReason::TooOld)) => fail(WINDOW_PASSED),
            Ok(ScrobbleAnswer::Ignored(reason)) => {
                tracing::debug!(?reason, "Last.fm ignored the scrobble");
                fail(LISTEN_REFUSED)
            }
            // A success whose answer does not read: the play most likely
            // landed, and a resend could show it twice.
            Err(ProviderError::Payload {
                kind: PayloadKind::Decode | PayloadKind::Contract,
                ..
            }) => {
                tracing::warn!("Last.fm took a scrobble with an answer that did not read");
                Outcome::Done
            }
            // An answer too large to read says nothing; try again.
            Err(ProviderError::Payload { .. }) => retry(SERVICE_UNAVAILABLE, None),
            // Users without their own key pair scrobble on the admin's. A
            // key or signature refusal is then the admin's to fix: the play
            // waits for it rather than failing.
            Err(ProviderError::Unauthorized { .. }) if on_instance_key => {
                retry(INSTANCE_KEY_REFUSED, None)
            }
            Err(error) => self.answer(Service::LastFm, &error, attempt),
        }
    }

    /// The outcome of a failed send that each service reads alike.
    fn answer(&self, service: Service, error: &ProviderError, attempt: u32) -> Outcome {
        tracing::debug!(service = service.as_str(), %error, "scrobble send failed");
        match error {
            ProviderError::RateLimited { .. } => retry(RATE_LIMITED, self.retry_at(error, attempt)),
            error if error.is_retriable() => {
                retry(SERVICE_UNAVAILABLE, self.retry_at(error, attempt))
            }
            ProviderError::Unauthorized { .. }
            | ProviderError::Forbidden { .. }
            | ProviderError::NotConfigured { .. } => fail(CREDENTIAL_REFUSED),
            _ => fail(LISTEN_REFUSED),
        }
    }

    /// When to try again after `error`: the wait the service named, when
    /// it is longer than the ladder's own step; else the ladder.
    fn retry_at(&self, error: &ProviderError, attempt: u32) -> Option<i64> {
        let wait = error.retry_after()?;
        (wait > step(attempt)).then(|| {
            self.clock
                .now_ms()
                .saturating_add(i64::try_from(wait.as_millis()).unwrap_or(i64::MAX))
        })
    }
}

/// The ladder's delay after the `attempt`th try, before jitter.
fn step(attempt: u32) -> Duration {
    BACKOFF_FIRST
        .saturating_mul(1_u32 << attempt.saturating_sub(1).min(30))
        .min(BACKOFF_CAP)
}

fn fail(code: &'static str) -> Outcome {
    Outcome::Fail { code }
}

fn retry(code: &'static str, at: Option<i64>) -> Outcome {
    Outcome::Retry { code, at }
}

/// A failed store read: busy waits without spending the try.
fn store_failed(error: &DbError) -> Outcome {
    if matches!(error, DbError::Busy { .. }) {
        return Outcome::Busy;
    }
    tracing::warn!(%error, "scrobble job could not read its play");
    retry(STORE_FAILED, None)
}

/// The recording id as both services take it: a UUID, else nothing.
fn recording(mbid: Option<&str>) -> Option<String> {
    AdditionalInfo::of(0, mbid).and_then(|info| info.recording_mbid)
}

fn metadata(
    artist: &str,
    track: &str,
    album: Option<&str>,
    duration_ms: i64,
    recording_mbid: Option<&str>,
) -> TrackMetadata {
    TrackMetadata {
        artist_name: artist.to_owned(),
        track_name: track.to_owned(),
        release_name: album.filter(|name| !name.is_empty()).map(str::to_owned),
        additional_info: AdditionalInfo::of(duration_ms, recording_mbid),
    }
}

fn credentials(session: &LastFmSession) -> LastFmCredentials {
    LastFmCredentials {
        api_key: session.api_key.clone(),
        shared_secret: session.shared_secret.clone(),
        username: None,
        session_key: Some(session.session_key.clone()),
    }
}

/// One service's latest now-playing report per user.
#[derive(Default)]
struct Slot {
    latest: Mutex<HashMap<String, ReportTrack>>,
    ready: Notify,
}

impl Slot {
    fn put(&self, user_id: &str, track: &ReportTrack) {
        if let Ok(mut latest) = self.latest.lock() {
            latest.insert(user_id.to_owned(), track.clone());
        }
        self.ready.notify_one();
    }

    fn take(&self) -> Vec<(String, ReportTrack)> {
        self.latest
            .lock()
            .map(|mut latest| latest.drain().collect())
            .unwrap_or_default()
    }
}

/// The production now-playing sink: keeps each linked, wanted service's
/// latest report per user for [`NowPlayingWorker`] to send.
pub struct NowPlayingSender {
    slots: [Arc<Slot>; 2],
    store: Arc<PlaybackStore>,
    lastfm_switch: LastFmSwitch,
}

/// The slots the worker drains.
pub struct NowPlayingSlots {
    slots: [Arc<Slot>; 2],
}

impl NowPlayingSender {
    /// The sink and the slots its worker drains. Links are read from
    /// `store`; Last.fm also needs the admin's switch on.
    pub fn channel(
        store: Arc<PlaybackStore>,
        lastfm_switch: LastFmSwitch,
    ) -> (Self, NowPlayingSlots) {
        let slots = [Arc::new(Slot::default()), Arc::new(Slot::default())];
        (
            Self {
                slots: slots.clone(),
                store,
                lastfm_switch,
            },
            NowPlayingSlots { slots },
        )
    }
}

impl ScrobbleSinks for NowPlayingSender {
    fn report_now_playing(
        &self,
        user_id: &str,
        track: &ReportTrack,
        targets: ScrobbleTargets,
    ) -> ScrobbleTargets {
        let mut linked = self.store.linked(user_id);
        linked.lastfm &= self.lastfm_switch.enabled();
        let handed = targets.and(linked);
        for (service, slot) in Service::ALL.into_iter().zip(&self.slots) {
            if handed.wants(service) {
                slot.put(user_id, track);
            }
        }
        handed
    }
}

/// Sends the latest now-playing reports, once each, until the stop.
pub struct NowPlayingWorker {
    slots: NowPlayingSlots,
    courier: Arc<Courier>,
}

impl NowPlayingWorker {
    /// The worker over the sender's slots, sending through `courier`.
    pub fn new(slots: NowPlayingSlots, courier: Arc<Courier>) -> Self {
        Self { slots, courier }
    }

    /// Send each service's reports side by side until `stop`; what is
    /// still waiting then is dropped.
    pub async fn run(self, stop: CancellationToken) {
        let Self { slots, courier } = self;
        let [lastfm, listenbrainz] = slots.slots;
        tokio::join!(
            now_playing_lane(&courier, Service::LastFm, &lastfm, &stop),
            now_playing_lane(&courier, Service::ListenBrainz, &listenbrainz, &stop),
        );
    }
}

async fn now_playing_lane(
    courier: &Courier,
    service: Service,
    slot: &Slot,
    stop: &CancellationToken,
) {
    loop {
        tokio::select! {
            biased;
            () = stop.cancelled() => return,
            () = slot.ready.notified() => {}
        }
        for (user_id, track) in slot.take() {
            if stop.is_cancelled() {
                return;
            }
            courier.now_playing(service, &user_id, &track).await;
        }
    }
}

impl Courier {
    /// Send one now-playing report, once. A failure is only logged: a late
    /// one would be wrong.
    async fn now_playing(&self, service: Service, user_id: &str, track: &ReportTrack) {
        let sent = match service {
            Service::ListenBrainz => {
                let Some(token) = self.store.listenbrainz_token(user_id) else {
                    return;
                };
                let body = SubmitListens {
                    listen_type: "playing_now",
                    payload: vec![Listen {
                        listened_at: None,
                        track_metadata: metadata(
                            &track.artist_name,
                            &track.track_name,
                            track.album_name.as_deref(),
                            track.duration_ms,
                            track.mbid.as_deref(),
                        ),
                    }],
                };
                let Ok(body) = serde_json::to_vec(&body) else {
                    return;
                };
                self.listenbrainz
                    .submit_listens(&token, body, RequestPriority::BackgroundSync)
                    .await
            }
            Service::LastFm => {
                let Some(session) = self.store.lastfm_session(user_id, &self.lastfm_switch) else {
                    return;
                };
                let duration = (track.duration_ms / 1_000).to_string();
                let mut params: Vec<(&str, &str)> =
                    vec![("artist", &track.artist_name), ("track", &track.track_name)];
                if let Some(album) = track.album_name.as_deref().filter(|name| !name.is_empty()) {
                    params.push(("album", album));
                }
                if track.duration_ms >= 1_000 {
                    params.push(("duration", &duration));
                }
                self.lastfm
                    .submit(
                        "track.updateNowPlaying",
                        &credentials(&session),
                        &params,
                        RequestPriority::BackgroundSync,
                    )
                    .await
            }
        };
        if let Err(error) = sent {
            tracing::debug!(service = service.as_str(), %error, "now-playing report not sent");
        }
    }
}
