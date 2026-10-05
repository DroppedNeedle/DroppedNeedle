//! Typed provider failures and HTTP status semantics.
//!
//! Every upstream answers through [`ProviderError`], which keeps the distinct
//! meanings of 401, 403, 404, the rest of 4xx/3xx, 429, 503, and other 5xx.
//! There is no blanket non-2xx mapping, on purpose: callers match on the
//! variant, and [`classify_status`] is the single place that turns a status
//! code into one.
//!
//! Retry behavior ports the v2 rules: 429/503/5xx and transport failures are
//! retriable, everything else is not. Payload-shape failures and intended
//! upstream switch-offs are deterministic: never retried and never tripping a
//! shared breaker, because the service is healthy (or switched off) and
//! retrying cannot help. `Retry-After` parsing ports v2 too: delta-seconds or
//! an HTTP date, negative or past values ignored, honored values capped.

use std::time::Duration;

use thiserror::Error;

/// Upper bound on any honored `Retry-After` / rate-limit delay.
///
/// Ports v2's `_MB_MAX_RETRY_AFTER_SECONDS`: a provider asking for a longer
/// wait still only holds one minute of backoff here; the retry budget stops
/// earlier anyway when the caller configured one.
pub const MAX_RETRY_AFTER: Duration = Duration::from_secs(60);

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
    /// A 3xx redirect. Provider clients never follow redirects on their own
    /// (an unexpected redirect is a configuration or endpoint change, not a
    /// lookup answer), so this is a deterministic failure, never retried.
    #[error("{provider} answered a redirect ({status}); redirects are never followed here")]
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
        /// Honored `Retry-After`, already capped at [`MAX_RETRY_AFTER`].
        retry_after: Option<Duration>,
    },
    /// Temporarily unavailable (HTTP 503). Retriable, like 429.
    #[error("{provider} is temporarily unavailable (503)")]
    Unavailable {
        /// Lowercase provider key.
        provider: &'static str,
        /// Honored `Retry-After`, already capped at [`MAX_RETRY_AFTER`].
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
    /// Always retriable. The message stays transport-shaped (no bodies).
    #[error("{provider} could not be reached: {message}")]
    Transport {
        /// Lowercase provider key.
        provider: &'static str,
        /// Short transport description, safe for logs.
        message: String,
    },
    /// A 2xx answer whose payload violates the provider's verified contract
    /// (a required identity field arriving null, a mistyped object). Ports
    /// v2's `InvalidExternalPayloadError`: deterministic per payload, never
    /// retried, and must not trip the provider's shared breaker.
    #[error("{provider} answered a payload that violates its contract: {message}")]
    Payload {
        /// Lowercase provider key.
        provider: &'static str,
        /// What failed to decode, safe for logs.
        message: String,
    },
    /// The provider switched this sub-API off on purpose (ports v2's
    /// `ServiceDisabledUpstreamError`, e.g. a disabled popularity endpoint).
    /// Deterministic for the outage and breaker-exempt: the rest of the
    /// provider is healthy.
    #[error("{provider} has switched this API off: {message}")]
    Disabled {
        /// Lowercase provider key.
        provider: &'static str,
        /// The provider's own wording, safe for logs.
        message: String,
    },
    /// The provider cannot be called because it is not configured (missing
    /// API key, no endpoint). Never retried. Ports v2's `ConfigurationError`
    /// at the provider boundary.
    #[error("{provider} is not configured: {message}")]
    NotConfigured {
        /// Lowercase provider key.
        provider: &'static str,
        /// What is missing, safe for logs.
        message: String,
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
        matches!(self, Self::Payload { .. } | Self::Disabled { .. })
    }
}

/// Turn one HTTP status into a typed failure, or `None` for success.
///
/// `retry_after_value` is the raw `Retry-After` header value when the
/// response carried one; it is parsed (and capped) only for 429 and 503.
/// Every other detail of the response is intentionally ignored here: 2xx is
/// success, and each non-2xx family maps to exactly one variant, so a 401
/// can never surface as a 503 and a 404 can never look like an outage.
#[must_use]
pub fn classify_status(
    source: &'static str,
    status: u16,
    retry_after_value: Option<&str>,
) -> Option<ProviderError> {
    if (200..300).contains(&status) {
        return None;
    }
    let retry_after = || parse_retry_after(retry_after_value);
    Some(match status {
        401 => ProviderError::Unauthorized { provider: source },
        403 => ProviderError::Forbidden { provider: source },
        404 => ProviderError::NotFound { provider: source },
        429 => ProviderError::RateLimited {
            provider: source,
            retry_after: retry_after(),
        },
        503 => ProviderError::Unavailable {
            provider: source,
            retry_after: retry_after(),
        },
        300..=399 => ProviderError::Redirect {
            provider: source,
            status,
        },
        400..=499 => ProviderError::BadRequest {
            provider: source,
            status,
        },
        _ => ProviderError::Server {
            provider: source,
            status,
        },
    })
}

/// Read the wait-before-retry from a response's headers.
///
/// `headers` yields lowercase-friendly `(name, value)` pairs so this stays
/// free of any HTTP client type; callers adapt from their header map. Lookup
/// order: `Retry-After` first (delta-seconds or HTTP date), then the
/// `RateLimit-Reset` delta-seconds draft header, then `X-RateLimit-Reset` as
/// unix-epoch seconds. Anything unparseable, in the past, or missing yields
/// `None`.
#[must_use]
pub fn retry_delay_from_headers<'a>(
    headers: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Option<Duration> {
    let mut reset_delta: Option<&str> = None;
    let mut reset_epoch: Option<&str> = None;
    for (name, value) in headers {
        if name.eq_ignore_ascii_case("retry-after") {
            if let Some(delay) = parse_retry_after(Some(value)) {
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
        && let Some(delay) = parse_epoch_seconds(value)
    {
        return Some(delay);
    }
    None
}

/// Parse one `Retry-After` value: delta-seconds or an HTTP date.
///
/// Ports v2's `_parse_retry_after_seconds`: plain numbers first, then the
/// three HTTP date shapes, past or negative values ignored (`None`), honored
/// values capped at [`MAX_RETRY_AFTER`].
#[must_use]
pub fn parse_retry_after(value: Option<&str>) -> Option<Duration> {
    let text = value?.trim();
    if text.is_empty() {
        return None;
    }
    if let Some(delay) = parse_delta_seconds(text) {
        return Some(delay);
    }
    let when = parse_http_date(text)?;
    let now = unix_now();
    let seconds = when.saturating_sub(now);
    // A past date means "no usable signal", exactly like v2's negative check.
    if when < now {
        return None;
    }
    Some(cap_retry_after(Duration::from_secs(seconds)))
}

/// Parse delta-seconds (`Retry-After: 120`, `RateLimit-Reset: 5`).
/// Fractional values are accepted and truncated, matching v2's float parse.
fn parse_delta_seconds(text: &str) -> Option<Duration> {
    let seconds: f64 = text.trim().parse().ok()?;
    if !seconds.is_finite() || seconds < 0.0 {
        return None;
    }
    Some(cap_retry_after(Duration::from_secs_f64(seconds)))
}

/// Parse unix-epoch seconds (`X-RateLimit-Reset: 1730000000`) into the wait
/// from now. Past epochs yield `None`.
fn parse_epoch_seconds(text: &str) -> Option<Duration> {
    let when: u64 = text.trim().parse().ok()?;
    let now = unix_now();
    if when < now {
        return None;
    }
    Some(cap_retry_after(Duration::from_secs(when - now)))
}

fn cap_retry_after(delay: Duration) -> Duration {
    delay.min(MAX_RETRY_AFTER)
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
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
    fn success_statuses_have_no_error() {
        for status in [200, 201, 204, 299] {
            assert_eq!(classify_status("musicbrainz", status, None), None);
        }
    }

    #[test]
    fn auth_and_absence_keep_distinct_meanings() {
        assert!(matches!(
            classify_status("lastfm", 401, None),
            Some(ProviderError::Unauthorized { .. })
        ));
        assert!(matches!(
            classify_status("lastfm", 403, None),
            Some(ProviderError::Forbidden { .. })
        ));
        assert!(matches!(
            classify_status("musicbrainz", 404, None),
            Some(ProviderError::NotFound { .. })
        ));
        assert!(matches!(
            classify_status("musicbrainz", 400, None),
            Some(ProviderError::BadRequest { status: 400, .. })
        ));
        assert!(matches!(
            classify_status("musicbrainz", 302, None),
            Some(ProviderError::Redirect { status: 302, .. })
        ));
    }

    #[test]
    fn rate_limit_and_outage_carry_retry_after() {
        let limited = classify_status("musicbrainz", 429, Some("7")).expect("429 maps");
        assert_eq!(limited.retry_after(), Some(Duration::from_secs(7)));
        assert!(limited.is_retriable());

        let down = classify_status("lastfm", 503, Some("3")).expect("503 maps");
        assert_eq!(down.retry_after(), Some(Duration::from_secs(3)));
        assert!(down.is_retriable());

        let broken = classify_status("lastfm", 500, Some("3")).expect("500 maps");
        assert!(matches!(broken, ProviderError::Server { status: 500, .. }));
        assert!(broken.is_retriable());
        // Only 429/503 honor Retry-After; a 500 header is not a wait signal.
        assert_eq!(broken.retry_after(), None);
    }

    #[test]
    fn only_health_signals_retry_and_trip_the_breaker() {
        for (status, retriable) in [
            (401, false),
            (403, false),
            (404, false),
            (400, false),
            (422, false),
            (301, false),
            (429, true),
            (503, true),
            (500, true),
            (502, true),
        ] {
            let error = classify_status("audiodb", status, None).expect("non-2xx maps");
            assert_eq!(error.is_retriable(), retriable, "status {status}");
            assert_eq!(error.trips_breaker(), retriable, "status {status}");
            assert!(!error.is_deterministic(), "status {status}");
        }
        let payload = ProviderError::Payload {
            provider: "musicbrainz",
            message: "null where an MBID belongs".to_owned(),
        };
        assert!(!payload.is_retriable());
        assert!(!payload.trips_breaker());
        assert!(payload.is_deterministic());
        let disabled = ProviderError::Disabled {
            provider: "listenbrainz",
            message: "currently disabled due to high load".to_owned(),
        };
        assert!(!disabled.is_retriable());
        assert!(!disabled.trips_breaker());
        assert!(disabled.is_deterministic());
    }

    #[test]
    fn retry_after_accepts_seconds_and_dates_and_caps() {
        assert_eq!(parse_retry_after(Some("5")), Some(Duration::from_secs(5)));
        assert_eq!(
            parse_retry_after(Some("  12  ")),
            Some(Duration::from_secs(12))
        );
        // Huge values cap at one minute, like v2.
        assert_eq!(parse_retry_after(Some("3600")), Some(MAX_RETRY_AFTER));
        // A far-future HTTP date is a valid wait (capped); a past one is not.
        assert_eq!(
            parse_retry_after(Some("Wed, 01 Jan 2031 00:00:00 GMT")),
            Some(MAX_RETRY_AFTER)
        );
        assert_eq!(
            parse_retry_after(Some("Sunday, 06-Nov-94 08:49:37 GMT")),
            None
        );
        assert_eq!(parse_retry_after(Some("Sun Nov  6 08:49:37 1994")), None);
        assert_eq!(parse_retry_after(Some("-3")), None);
        assert_eq!(parse_retry_after(Some("not-a-date")), None);
        assert_eq!(parse_retry_after(Some("")), None);
        assert_eq!(parse_retry_after(None), None);
    }

    #[test]
    fn header_lookup_prefers_retry_after_then_rate_limit_resets() {
        let headers = [("X-RateLimit-Reset", "9999999999"), ("Retry-After", "4")];
        assert_eq!(
            retry_delay_from_headers(headers),
            Some(Duration::from_secs(4))
        );
        let headers = [("ratelimit-reset", "9")];
        assert_eq!(
            retry_delay_from_headers(headers),
            Some(Duration::from_secs(9))
        );
        let headers = [("X-RateLimit-Reset", "9999999999")];
        assert_eq!(retry_delay_from_headers(headers), Some(MAX_RETRY_AFTER));
        // A reset epoch in the past carries no wait.
        let headers = [("X-RateLimit-Reset", "1000000000")];
        assert_eq!(retry_delay_from_headers(headers), None);
        let headers = [("Content-Type", "application/json")];
        assert_eq!(retry_delay_from_headers(headers), None);
    }

    #[test]
    fn imf_fixdate_parses_the_epoch() {
        assert_eq!(parse_imf_fixdate("Thu, 01 Jan 1970 00:00:00 GMT"), Some(0));
        assert_eq!(parse_imf_fixdate("not a date"), None);
        assert_eq!(parse_imf_fixdate("Sun, 06 Nov 1994 08:49:37 EST"), None);
    }
}
