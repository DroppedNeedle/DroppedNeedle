//! The one Deemix session DroppedNeedle keeps.
//!
//! Deemix signs Deezer in per browser session (express-session's
//! `connect.sid` cookie), and the factory's clients keep no cookies. So the
//! cookie lives here, beside the base URL it was set by, built once by the
//! acquire wiring and shared by every [`super::client::DeemixClient`] built
//! after it: a settings save that rebuilds the clients keeps the session,
//! and a save of the Deemix settings drops it only when their address
//! changes. A client for another base URL is never handed this one's
//! cookie.
//!
//! The session is signed in by replaying the Deezer sign-in Deemix saved
//! (the ARL `/connect` offers a signed-out session) to Deemix's own
//! `loginArl`. In single-user mode a sign-in Deemix answers as failed or
//! unavailable erases that saved sign-in, so [`sign_in_if_needed`] tries
//! one only when the session is signed out, Deezer is reachable and an ARL
//! is offered, at most once per 15 minutes plus up to 5 more per base URL.
//! After a failed one it tries none until `/connect` offers an ARL again,
//! which means someone signed in on Deemix's page.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use cookie::Cookie;
use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::jobs::SplitMix64;
use reqwest::Url;
use reqwest::header::{HeaderMap, SET_COOKIE};

use super::client::{DeemixClient, DeemixError};
use super::models::{Connect, DeezerAvailable, LoginStatus};

/// The cookie express-session names its session by.
pub const SESSION_COOKIE: &str = "connect.sid";

/// The longest cookie value kept. express-session's signed ids are about 80
/// characters.
const MAX_COOKIE_LEN: usize = 1024;

/// The least time between two sign-ins on one Deemix.
pub const SIGN_IN_GAP: Duration = Duration::from_secs(15 * 60);

/// The most added to [`SIGN_IN_GAP`] at random after each sign-in.
pub const SIGN_IN_JITTER: Duration = Duration::from_secs(5 * 60);

/// The Deemix session cookie and the base URL it belongs to, and the
/// sign-in limits of every base URL.
#[derive(Default)]
pub struct DeemixSession {
    held: Mutex<Option<Held>>,
    sign_ins: Mutex<SignIns>,
}

struct Held {
    base: Url,
    cookie: String,
}

struct SignIns {
    by_base: HashMap<Url, SignInState>,
    rng: SplitMix64,
}

impl Default for SignIns {
    fn default() -> Self {
        SignIns {
            by_base: HashMap::new(),
            rng: SplitMix64::new(SplitMix64::seed_from_time()),
        }
    }
}

#[derive(Debug, Default)]
struct SignInState {
    /// No sign-in before this time, in unix milliseconds.
    next_at_ms: Option<i64>,
    /// A sign-in failed, and `/connect` has offered no ARL since.
    lost: bool,
}

/// What [`sign_in_if_needed`] found.
#[derive(Debug)]
pub enum SignIn {
    /// The session is signed in, as this `/connect` answer shows: it was
    /// already, or the sign-in just took and `/connect` was read again.
    SignedIn(Box<Connect>),
    /// Deemix lost its Deezer sign-in: a sign-in failed and no ARL has
    /// been offered since. Someone signs in on Deemix's page.
    Lost,
    /// Signed out, and no sign-in can be tried now.
    NotNow(NotNow),
}

/// Why no sign-in was tried, or why one did not take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotNow {
    /// Deemix offers no saved sign-in: none was ever saved, or it runs in
    /// multi-user mode.
    NoArlOffered,
    /// Deemix cannot reach Deezer.
    DeezerUnavailable,
    /// The last sign-in on this Deemix was too recent.
    Waiting,
    /// The sign-in answered a status this version does not read, or the
    /// session still reads signed out after it.
    DidNotTake,
}

impl fmt::Debug for DeemixSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let held = self.lock();
        f.debug_struct("DeemixSession")
            .field("base", &held.as_ref().map(|held| held.base.as_str()))
            .field("cookie", &held.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl DeemixSession {
    /// No session yet: the first call opens one.
    #[must_use]
    pub fn new() -> Self {
        DeemixSession::default()
    }

    fn lock(&self) -> MutexGuard<'_, Option<Held>> {
        self.held
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The cookie to send to `base`: none when the one held was set by
    /// another address.
    #[must_use]
    pub fn cookie_for(&self, base: &Url) -> Option<String> {
        self.lock()
            .as_ref()
            .filter(|held| held.base == *base)
            .map(|held| held.cookie.clone())
    }

    /// Hold `cookie` for `base`, in place of any cookie held before. A
    /// value that cannot go back in a `Cookie` header is not held.
    pub fn replace(&self, base: &Url, cookie: &str) {
        if !sendable(cookie) {
            return;
        }
        *self.lock() = Some(Held {
            base: base.clone(),
            cookie: cookie.to_owned(),
        });
    }

    /// Forget the session, so the next call opens a new one.
    pub fn drop_cookie(&self) {
        *self.lock() = None;
    }

    /// Forget the session unless it was set by `base`, the address the
    /// Deemix settings name now, so a new address opens a new session.
    pub fn keep_only_for(&self, base: Option<&Url>) {
        let mut held = self.lock();
        if held.as_ref().is_some_and(|held| Some(&held.base) != base) {
            *held = None;
        }
    }

    /// Hold the session cookie `base` set in `headers`, when it set one.
    pub fn keep_from(&self, base: &Url, headers: &HeaderMap) {
        let set = headers
            .get_all(SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .filter_map(|value| Cookie::parse(value).ok())
            .find(|cookie| cookie.name() == SESSION_COOKIE);
        if let Some(cookie) = set {
            self.replace(base, cookie.value());
        }
    }

    fn sign_ins(&self) -> MutexGuard<'_, SignIns> {
        self.sign_ins
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Whether the Deemix at `base` lost its Deezer sign-in: a sign-in
    /// failed there and no ARL has been offered since.
    #[must_use]
    pub fn sign_in_lost(&self, base: &Url) -> bool {
        self.sign_ins()
            .by_base
            .get(base)
            .is_some_and(|state| state.lost)
    }

    fn arl_offered(&self, base: &Url) {
        if let Some(state) = self.sign_ins().by_base.get_mut(base) {
            state.lost = false;
        }
    }

    fn mark_lost(&self, base: &Url) {
        self.sign_ins()
            .by_base
            .entry(base.clone())
            .or_default()
            .lost = true;
    }

    /// Take the sign-in turn for `base` at `now_ms`, when it is free: the
    /// next one is then due after the gap and a fresh jitter draw.
    fn take_turn(&self, base: &Url, now_ms: i64) -> bool {
        let mut sign_ins = self.sign_ins();
        let wait = SIGN_IN_GAP.saturating_add(sign_ins.rng.below_duration(SIGN_IN_JITTER));
        let state = sign_ins.by_base.entry(base.clone()).or_default();
        if state.next_at_ms.is_some_and(|next| now_ms < next) {
            return false;
        }
        let wait_ms = i64::try_from(wait.as_millis()).unwrap_or(i64::MAX);
        state.next_at_ms = Some(now_ms.saturating_add(wait_ms));
        true
    }
}

/// Sign the client's session in to Deezer when it is signed out and a
/// sign-in may be tried, by replaying the ARL Deemix offers to its own
/// `loginArl`. The ARL goes nowhere else and is never returned.
pub async fn sign_in_if_needed(
    client: &DeemixClient,
    clock: &dyn Clock,
) -> Result<SignIn, DeemixError> {
    let session = client.session();
    let base = client.base_url();
    let mut connect = client.connect().await?;
    let arl = connect.take_arl();
    if connect.signed_in() {
        return Ok(SignIn::SignedIn(Box::new(connect)));
    }
    let Some(arl) = arl else {
        return Ok(if session.sign_in_lost(base) {
            SignIn::Lost
        } else {
            SignIn::NotNow(NotNow::NoArlOffered)
        });
    };
    session.arl_offered(base);
    if connect.deezer_available != DeezerAvailable::Yes {
        return Ok(SignIn::NotNow(NotNow::DeezerUnavailable));
    }
    if !session.take_turn(base, clock.now_ms()) {
        return Ok(SignIn::NotNow(NotNow::Waiting));
    }
    match client.login_arl(arl).await? {
        LoginStatus::Success | LoginStatus::AlreadyLogged => {}
        LoginStatus::Failed | LoginStatus::NotAvailable => {
            session.mark_lost(base);
            return Ok(SignIn::Lost);
        }
        LoginStatus::Unknown => return Ok(SignIn::NotNow(NotNow::DidNotTake)),
    }
    let mut connect = client.connect().await?;
    drop(connect.take_arl());
    Ok(if connect.signed_in() {
        SignIn::SignedIn(Box::new(connect))
    } else {
        SignIn::NotNow(NotNow::DidNotTake)
    })
}

/// Cookie-octets only (RFC 6265), so the value cannot end the header or
/// add another cookie.
fn sendable(cookie: &str) -> bool {
    !cookie.is_empty()
        && cookie.len() <= MAX_COOKIE_LEN
        && cookie
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && !matches!(byte, b'"' | b',' | b';' | b'\\'))
}
