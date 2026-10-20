//! Typed provider failures.
//!
//! Every provider answers through [`ProviderError`], which keeps the
//! distinct meanings of 401, 403, 404, the rest of 4xx and 3xx, 429, 503
//! and other 5xx, so a caller matches on the variant. 429, 503, other 5xx
//! and transport failures may be retried; everything else is the same on
//! every attempt. Payload failures and intended switch-offs never count as
//! an outage: the service answered.
//!
//! A handler answers a provider failure through `From<ProviderError> for
//! ApiError`, the one place it meets a status: an outage is 503
//! `PROVIDER_UNAVAILABLE` naming the service and its wait, a missing or
//! refused key 409 `PROVIDER_NOT_CONFIGURED`, a 404 is absence, and
//! anything else is our fault, the fixed internal error.

use std::time::Duration;

use droppedneedle_kernel::error::{ApiError, Unavailable};
use thiserror::Error;

/// The longest wait most providers may ask for; a provider's spec names
/// its own cap.
pub const MAX_RETRY_AFTER: Duration = Duration::from_secs(60);

/// Why a request got no answer. Fixed text only: a transport error never
/// carries the URL, which can hold a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum TransportKind {
    /// No answer before the deadline.
    #[error("timed out")]
    Timeout,
    /// The connection could not be made (DNS, refused, TLS).
    #[error("connection failed")]
    Connect,
    /// Anything else below HTTP: a reset, a broken body.
    #[error("request failed")]
    Other,
}

/// What was wrong with an answer's body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum PayloadKind {
    /// Larger than the cap it was read under.
    #[error("that is too large")]
    TooLarge,
    /// Not the shape the provider documents.
    #[error("that does not decode")]
    Decode,
    /// Decoded, but breaks the provider's verified contract.
    #[error("that breaks its contract")]
    Contract,
}

/// One typed upstream failure.
///
/// The `source` on every variant is the lowercase provider key (`musicbrainz`,
/// `lastfm`, ...). Variants carry no response bodies, only the status and the
/// signals callers act on (retriability, `Retry-After`).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProviderError {
    /// Bad or missing credentials (HTTP 401). Never retried: the same request
    /// with the same credentials fails the same way.
    #[error("{provider} rejected the credentials (401)")]
    Unauthorized {
        /// Lowercase provider key.
        provider: &'static str,
    },
    /// Authenticated but refused (HTTP 403). Never retried.
    #[error("{provider} refused the request (403)")]
    Forbidden {
        /// Lowercase provider key.
        provider: &'static str,
    },
    /// No such record (HTTP 404). This is *absence*, not failure: the
    /// degradation matrix turns it into `None` without failing the request.
    #[error("{provider} has no such record (404)")]
    NotFound {
        /// Lowercase provider key.
        provider: &'static str,
    },
    /// Any other 4xx rejection: malformed parameters, unknown method, quota
    /// wording that is not a 429, and so on. Deterministic per request shape,
    /// so never retried.
    #[error("{provider} rejected the request ({status})")]
    BadRequest {
        /// Lowercase provider key.
        provider: &'static str,
        /// The exact 4xx status, kept for logs.
        status: u16,
    },
    /// A 3xx redirect that is not followed: not allowed for this request,
    /// to a place its rule refuses, or past the hop limit. Deterministic,
    /// never retried.
    #[error("{provider} answered a redirect ({status}) that is not followed")]
    Redirect {
        /// Lowercase provider key.
        provider: &'static str,
        /// The exact 3xx status, kept for logs.
        status: u16,
    },
    /// Rate limited (HTTP 429). Retriable; the optional delay comes from the
    /// response's `Retry-After` header when it parsed.
    #[error("{provider} is rate limited (429)")]
    RateLimited {
        /// Lowercase provider key.
        provider: &'static str,
        /// The wait the provider asked for, capped at its limit.
        retry_after: Option<Duration>,
    },
    /// Temporarily unavailable (HTTP 503). Retriable, like 429.
    #[error("{provider} is temporarily unavailable (503)")]
    Unavailable {
        /// Lowercase provider key.
        provider: &'static str,
        /// The wait the provider asked for, capped at its limit.
        retry_after: Option<Duration>,
    },
    /// Any other 5xx. Retriable: the next attempt may land on a healthy host.
    #[error("{provider} failed ({status})")]
    Server {
        /// Lowercase provider key.
        provider: &'static str,
        /// The exact 5xx status, kept for logs.
        status: u16,
    },
    /// The request never got an answer: DNS, connect, TLS, timeout, reset.
    /// Always retriable. Carries only the kind, never the URL.
    #[error("{provider} could not be reached ({kind})")]
    Transport {
        /// Lowercase provider key.
        provider: &'static str,
        /// What went wrong below HTTP.
        kind: TransportKind,
    },
    /// An answer whose body is unusable: too large, undecodable, or
    /// breaking the provider's verified contract (a required identity
    /// field arriving null). Deterministic per payload, never retried, and
    /// never counted as an outage: the service answered.
    #[error("{provider} answered a payload {kind}: {message}")]
    Payload {
        /// Lowercase provider key.
        provider: &'static str,
        /// What was wrong.
        kind: PayloadKind,
        /// The decoder's account, one short line; never the body.
        message: String,
    },
    /// The provider switched this sub-API off on purpose, such as a
    /// disabled popularity endpoint. Never counted as an outage: the rest
    /// of the provider is healthy.
    #[error("{provider} has switched this API off: {message}")]
    Disabled {
        /// Lowercase provider key.
        provider: &'static str,
        /// The provider's own wording, safe for logs.
        message: String,
    },
    /// The provider cannot be called because it is not configured (missing
    /// API key, no endpoint). Never retried.
    #[error("{provider} is not configured: {message}")]
    NotConfigured {
        /// Lowercase provider key.
        provider: &'static str,
        /// What is missing, safe for logs.
        message: String,
    },
    /// We refused to send: the request would be malformed (a bad MBID, a
    /// path the provider would not accept). Deterministic, never retried
    /// and never counted against the provider.
    #[error("{provider} request refused before sending: {reason}")]
    Invalid {
        /// Lowercase provider key.
        provider: &'static str,
        /// Why, in a fixed phrase.
        reason: &'static str,
    },
}

impl ProviderError {
    /// The lowercase provider key this failure came from.
    #[must_use]
    pub const fn source(&self) -> &'static str {
        match self {
            Self::Unauthorized { provider: source }
            | Self::Forbidden { provider: source }
            | Self::NotFound { provider: source }
            | Self::BadRequest {
                provider: source, ..
            }
            | Self::Redirect {
                provider: source, ..
            }
            | Self::RateLimited {
                provider: source, ..
            }
            | Self::Unavailable {
                provider: source, ..
            }
            | Self::Server {
                provider: source, ..
            }
            | Self::Transport {
                provider: source, ..
            }
            | Self::Payload {
                provider: source, ..
            }
            | Self::Disabled {
                provider: source, ..
            }
            | Self::NotConfigured {
                provider: source, ..
            }
            | Self::Invalid {
                provider: source, ..
            } => source,
        }
    }

    /// Whether another attempt could plausibly succeed.
    ///
    /// Only 429, 503, other 5xx, and transport failures retry. Auth failures,
    /// rejections, redirects, absence, deterministic payload errors, intended
    /// switch-offs, and missing configuration never do.
    #[must_use]
    pub const fn is_retriable(&self) -> bool {
        matches!(
            self,
            Self::RateLimited { .. }
                | Self::Unavailable { .. }
                | Self::Server { .. }
                | Self::Transport { .. }
        )
    }

    /// The server-asked wait before the next attempt, when the failure
    /// carries one. Only 429 and 503 carry `Retry-After`.
    #[must_use]
    pub const fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::RateLimited { retry_after, .. } | Self::Unavailable { retry_after, .. } => {
                *retry_after
            }
            _ => None,
        }
    }

    /// [`retry_after`](Self::retry_after) in whole seconds for a
    /// `Retry-After` header: rounded up and at least one, so a client never
    /// asks early.
    #[must_use]
    pub fn retry_after_secs(&self) -> Option<u64> {
        self.retry_after()
            .map(|wait| (wait.as_secs() + u64::from(wait.subsec_nanos() > 0)).max(1))
    }

    /// Whether this failure counts against the provider's shared circuit
    /// breaker. Deterministic failures (payload shape, intended switch-off,
    /// bad request shape, auth, absence, missing configuration) never trip
    /// the breaker; only genuine service-health signals do.
    #[must_use]
    pub const fn trips_breaker(&self) -> bool {
        self.is_retriable()
    }

    /// Deterministic failures look the same on every attempt with the same
    /// input, so they are tracked separately in degradation summaries.
    #[must_use]
    pub const fn is_deterministic(&self) -> bool {
        matches!(
            self,
            Self::Payload { .. } | Self::Disabled { .. } | Self::Invalid { .. }
        )
    }
}

/// The name a person knows a provider by, for messages.
#[must_use]
pub fn display_name(provider: &'static str) -> &'static str {
    match provider {
        "musicbrainz" => "MusicBrainz",
        "brainzmash" => "BrainzMash",
        "listenbrainz" => "ListenBrainz",
        "lastfm" => "Last.fm",
        "coverartarchive" => "Cover Art Archive",
        "audiodb" => "TheAudioDB",
        "acoustid" => "AcoustID",
        "discogs" => "Discogs",
        "itunes" => "iTunes",
        "deezer" => "Deezer",
        "lrclib" => "LRCLIB",
        "wikidata" => "Wikidata",
        "youtube" => "YouTube",
        "ticketmaster" => "Ticketmaster",
        "skiddle" => "Skiddle",
        "geocoding" => "Open-Meteo",
        "github" => "GitHub",
        other => other,
    }
}

impl From<ProviderError> for ApiError {
    fn from(error: ProviderError) -> Self {
        let provider = error.source();
        match error {
            ProviderError::RateLimited { .. }
            | ProviderError::Unavailable { .. }
            | ProviderError::Server { .. }
            | ProviderError::Transport { .. }
            | ProviderError::Disabled { .. } => {
                let retry_after = error.retry_after_secs();
                ApiError::unavailable(
                    Unavailable::Provider {
                        provider,
                        retry_after,
                    },
                    error,
                )
            }
            ProviderError::NotConfigured { .. } => ApiError::provider_not_configured(
                provider,
                format!("{} is not set up.", display_name(provider)),
            ),
            ProviderError::Unauthorized { .. } | ProviderError::Forbidden { .. } => {
                ApiError::provider_not_configured(
                    provider,
                    format!("{} refused the saved key.", display_name(provider)),
                )
            }
            ProviderError::NotFound { .. } => ApiError::not_found(),
            ProviderError::BadRequest { .. }
            | ProviderError::Redirect { .. }
            | ProviderError::Payload { .. }
            | ProviderError::Invalid { .. } => ApiError::internal(error),
        }
    }
}

/// Read the wait-before-retry from a response's headers, uncapped.
///
/// `headers` yields `(name, value)` pairs so this stays free of any HTTP
/// client type. Lookup order: `Retry-After` first (delta-seconds or HTTP
/// date), then the `RateLimit-Reset` delta-seconds draft header, then
/// `X-RateLimit-Reset` as unix-epoch seconds. Dates and epochs are read
/// against `now_unix`, the kernel clock's time. Anything unparseable, in
/// the past, or missing yields `None`.
#[must_use]
pub fn retry_delay_from_headers<'a>(
    headers: impl IntoIterator<Item = (&'a str, &'a str)>,
    now_unix: i64,
) -> Option<Duration> {
    let mut reset_delta: Option<&str> = None;
    let mut reset_epoch: Option<&str> = None;
    for (name, value) in headers {
        if name.eq_ignore_ascii_case("retry-after") {
            if let Some(delay) = parse_retry_after_at(value, now_unix) {
                return Some(delay);
            }
        } else if name.eq_ignore_ascii_case("ratelimit-reset") {
            reset_delta.get_or_insert(value);
        } else if name.eq_ignore_ascii_case("x-ratelimit-reset") {
            reset_epoch.get_or_insert(value);
        }
    }
    if let Some(value) = reset_delta
        && let Some(delay) = parse_delta_seconds(value)
    {
        return Some(delay);
    }
    if let Some(value) = reset_epoch
        && let Some(delay) = parse_epoch_seconds(value, now_unix)
    {
        return Some(delay);
    }
    None
}

/// Parse one `Retry-After` value: delta-seconds or an HTTP date read
/// against `now_unix`. Plain numbers first, then the three HTTP date
/// shapes; past or negative values mean no usable signal.
fn parse_retry_after_at(value: &str, now_unix: i64) -> Option<Duration> {
    let text = value.trim();
    if text.is_empty() {
        return None;
    }
    if let Some(delay) = parse_delta_seconds(text) {
        return Some(delay);
    }
    wait_until(parse_http_date(text)?, now_unix)
}

/// Parse delta-seconds (`Retry-After: 120`, `RateLimit-Reset: 5`).
/// Fractional values are accepted.
fn parse_delta_seconds(text: &str) -> Option<Duration> {
    let seconds: f64 = text.trim().parse().ok()?;
    if !seconds.is_finite() || seconds < 0.0 {
        return None;
    }
    Duration::try_from_secs_f64(seconds).ok()
}

/// Parse unix-epoch seconds (`X-RateLimit-Reset: 1730000000`) into the wait
/// from `now_unix`. Past epochs yield `None`.
fn parse_epoch_seconds(text: &str, now_unix: i64) -> Option<Duration> {
    wait_until(text.trim().parse().ok()?, now_unix)
}

/// The wait from `now_unix` until `when`, or `None` once it has passed.
fn wait_until(when: u64, now_unix: i64) -> Option<Duration> {
    let now = u64::try_from(now_unix).unwrap_or(0);
    (when >= now).then(|| Duration::from_secs(when - now))
}

/// Parse the three HTTP date shapes (IMF-fixdate, RFC 850, ANSI C asctime)
/// into unix seconds. All HTTP dates are GMT by spec, so no zone handling.
fn parse_http_date(text: &str) -> Option<u64> {
    parse_imf_fixdate(text)
        .or_else(|| parse_rfc850_date(text))
        .or_else(|| parse_asctime_date(text))
}

/// `Sun, 06 Nov 1994 08:49:37 GMT`.
fn parse_imf_fixdate(text: &str) -> Option<u64> {
    let (weekday, rest) = text.split_once(',')?;
    if weekday.trim().len() != 3 {
        return None;
    }
    let mut parts = rest.split_whitespace();
    let day: u32 = parts.next()?.parse().ok()?;
    let month = month_number(parts.next()?)?;
    let year: i32 = parts.next()?.parse().ok()?;
    let (hour, minute, second) = parse_hms(parts.next()?)?;
    if parts.next()? != "GMT" || parts.next().is_some() {
        return None;
    }
    civil_to_unix(year, month, day, hour, minute, second)
}

/// `Sunday, 06-Nov-94 08:49:37 GMT`.
fn parse_rfc850_date(text: &str) -> Option<u64> {
    let (_, rest) = text.split_once(',')?;
    let mut parts = rest.split_whitespace();
    let mut date = parts.next()?.split('-');
    let day: u32 = date.next()?.parse().ok()?;
    let month = month_number(date.next()?)?;
    let short_year: i32 = date.next()?.parse().ok()?;
    if date.next().is_some() {
        return None;
    }
    // Two-digit years: HTTP senders mean the late twentieth century for
    // high values; low values are this century. Either way the result is a
    // retry hint, and past dates are discarded by the caller.
    let year = if short_year >= 70 {
        1900 + short_year
    } else {
        2000 + short_year
    };
    let (hour, minute, second) = parse_hms(parts.next()?)?;
    if parts.next()? != "GMT" || parts.next().is_some() {
        return None;
    }
    civil_to_unix(year, month, day, hour, minute, second)
}

/// `Sun Nov  6 08:49:37 1994`.
fn parse_asctime_date(text: &str) -> Option<u64> {
    let mut parts = text.split_whitespace();
    if parts.next()?.len() != 3 {
        return None;
    }
    let month = month_number(parts.next()?)?;
    let day: u32 = parts.next()?.parse().ok()?;
    let (hour, minute, second) = parse_hms(parts.next()?)?;
    let year: i32 = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    civil_to_unix(year, month, day, hour, minute, second)
}

fn month_number(name: &str) -> Option<u32> {
    Some(match name {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    })
}

fn parse_hms(text: &str) -> Option<(u32, u32, u32)> {
    let mut parts = text.split(':');
    let hour: u32 = parts.next()?.parse().ok()?;
    let minute: u32 = parts.next()?.parse().ok()?;
    let second: u32 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    Some((hour, minute, second))
}

/// Gregorian civil date to unix seconds (Howard Hinnant's days-from-civil).
fn civil_to_unix(
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
) -> Option<u64> {
    if month == 0 || month > 12 || day == 0 || day > 31 {
        return None;
    }
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400) as u64;
    let month_prime = (month + 9) % 12;
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1;
    let day_of_era =
        year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + u64::from(day_of_year);
    let days = era as i64 * 146_097 + day_of_era as i64 - 719_468;
    let seconds = u64::try_from(days).ok()?;
    seconds
        .checked_mul(86_400)?
        .checked_add(u64::from(hour) * 3_600 + u64::from(minute) * 60 + u64::from(second))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_after_accepts_seconds_and_the_three_date_shapes() {
        let now = 1_700_000_000;
        let at = |value| parse_retry_after_at(value, now);
        assert_eq!(at("5"), Some(Duration::from_secs(5)));
        assert_eq!(at("  12  "), Some(Duration::from_secs(12)));
        assert_eq!(
            at("Wed, 01 Jan 2031 00:00:00 GMT"),
            Some(Duration::from_secs(1_924_992_000 - 1_700_000_000))
        );
        // Past dates in each shape, negatives and junk carry no wait.
        assert_eq!(at("Sunday, 06-Nov-94 08:49:37 GMT"), None);
        assert_eq!(at("Sun Nov  6 08:49:37 1994"), None);
        assert_eq!(at("-3"), None);
        assert_eq!(at("not-a-date"), None);
        assert_eq!(at(""), None);
    }

    #[test]
    fn header_lookup_prefers_retry_after_then_rate_limit_resets() {
        let now = 1_700_000_000;
        let headers = [("X-RateLimit-Reset", "1700000090"), ("Retry-After", "4")];
        assert_eq!(
            retry_delay_from_headers(headers, now),
            Some(Duration::from_secs(4))
        );
        let headers = [("ratelimit-reset", "9")];
        assert_eq!(
            retry_delay_from_headers(headers, now),
            Some(Duration::from_secs(9))
        );
        let headers = [("X-RateLimit-Reset", "1700000090")];
        assert_eq!(
            retry_delay_from_headers(headers, now),
            Some(Duration::from_secs(90))
        );
        // A reset epoch or a date in the past carries no wait.
        let headers = [("X-RateLimit-Reset", "1000000000")];
        assert_eq!(retry_delay_from_headers(headers, now), None);
        let headers = [("Retry-After", "Sun, 06 Nov 1994 08:49:37 GMT")];
        assert_eq!(retry_delay_from_headers(headers, now), None);
        let headers = [("Content-Type", "application/json")];
        assert_eq!(retry_delay_from_headers(headers, now), None);
    }

    #[test]
    fn imf_fixdate_parses_the_epoch() {
        assert_eq!(parse_imf_fixdate("Thu, 01 Jan 1970 00:00:00 GMT"), Some(0));
        assert_eq!(parse_imf_fixdate("not a date"), None);
        assert_eq!(parse_imf_fixdate("Sun, 06 Nov 1994 08:49:37 EST"), None);
    }
}
