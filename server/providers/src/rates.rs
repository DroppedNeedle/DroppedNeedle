//! The one rate table: every outside provider's pace, with where it was
//! read.
//!
//! This is the only place a provider rate is written. [`LimiterSet`]
//! builds one limiter per row, and each provider's executor paces through
//! the limiter of its row. Raise a row only after reading the provider's
//! documented limit again, and update its date and basis in the same
//! change. LAN services (slskd, SABnzbd) have no row and are never paced.
//!
//! [`LimiterSet`]: crate::limiter::LimiterSet

/// How fast one row's bucket refills.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Rate {
    /// A fixed number of requests per second.
    Fixed(f64),
    /// Set by the operator in Settings and handed over with each request;
    /// zero or less means unpaced.
    Setting,
}

/// One provider's documented limit and the bucket built from it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RateRow {
    /// The row key, which the provider's executor names.
    pub source: &'static str,
    /// Sustained requests per second.
    pub per_second: Rate,
    /// How many requests may go at once after a quiet spell.
    pub burst: u32,
    /// Requests of this row allowed in flight at once, where the provider
    /// documents a concurrency limit.
    pub max_in_flight: Option<u32>,
    /// The day the basis was last read (`YYYY-MM-DD`).
    pub verified_on: &'static str,
    /// What the provider says, in one line.
    pub basis: &'static str,
    /// Where it was read; empty only for an operator-set rate or a provider
    /// that publishes no limit.
    pub url: &'static str,
}

/// Every row. Order is the admin view's order before it sorts.
pub const RATES: &[RateRow] = &[
    RateRow {
        source: "musicbrainz",
        per_second: Rate::Fixed(1.0),
        burst: 1,
        max_in_flight: None,
        verified_on: "2026-10-18",
        basis: "About 1 request per second per IP; over it every request is declined \
                with HTTP 503. No burst, so two callers never share an instant.",
        url: "https://musicbrainz.org/doc/MusicBrainz_API/Rate_Limiting",
    },
    RateRow {
        source: "brainzmash",
        per_second: Rate::Fixed(10.0),
        burst: 1,
        max_in_flight: Some(1),
        verified_on: "2026-10-18",
        basis: "No documented limit: the page asks for clean, responsible use and \
                approves apps case by case. Kept at the forced 10 per second, one in \
                flight; its 429 cooldown is the real governor.",
        url: "https://brainzmash.cc",
    },
    RateRow {
        source: "musicbrainz-mirror",
        per_second: Rate::Setting,
        burst: 1,
        max_in_flight: None,
        verified_on: "2026-10-18",
        basis: "Set by the operator in Settings; 0 means unpaced. A mirror on a public \
                MusicBrainz host is paced at the musicbrainz row instead.",
        url: "",
    },
    RateRow {
        source: "listenbrainz",
        per_second: Rate::Fixed(1.0),
        burst: 1,
        max_in_flight: None,
        verified_on: "2026-10-18",
        basis: "Never more than one call per second; 429 when exceeded, with \
                X-RateLimit headers on every answer.",
        url: "https://listenbrainz.readthedocs.io/en/latest/users/api/index.html",
    },
    RateRow {
        source: "lastfm",
        per_second: Rate::Fixed(5.0),
        burst: 10,
        max_in_flight: None,
        verified_on: "2026-10-18",
        basis: "No documented limit: the terms say only that Last.fm sets and enforces \
                limits. Kept at 5 per second, which ran without bans; error 29 and 429 \
                cool the bucket down.",
        url: "https://www.last.fm/api/tos",
    },
    RateRow {
        source: "audiodb",
        per_second: Rate::Fixed(0.48),
        burst: 1,
        max_in_flight: None,
        verified_on: "2026-10-18",
        basis: "Free users 30 requests per minute; after a 429 wait a minute. The \
                shared key is banned when over-used, so never raise it.",
        url: "https://www.theaudiodb.com/free_music_api",
    },
    RateRow {
        source: "audiodb-premium",
        per_second: Rate::Fixed(1.6),
        burst: 2,
        max_in_flight: None,
        verified_on: "2026-10-18",
        basis: "Premium 100 requests per minute, held a little under it.",
        url: "https://www.theaudiodb.com/free_music_api",
    },
    RateRow {
        source: "acoustid",
        per_second: Rate::Fixed(3.0),
        burst: 1,
        max_in_flight: None,
        verified_on: "2026-10-18",
        basis: "Do not make more than 3 requests per second.",
        url: "https://acoustid.org/webservice",
    },
    RateRow {
        source: "coverartarchive",
        per_second: Rate::Fixed(10.0),
        burst: 1,
        max_in_flight: None,
        verified_on: "2026-10-18",
        basis: "No rate limiting rules in place; some endpoints answer 503 when a user \
                exceeds a limit. Held at 10 per second, one at a time: a burst of 20 \
                cold-grid fetches was followed by a five-minute refusal from every \
                MetaBrainz host. Redirect hops to archive.org are not paced.",
        url: "https://musicbrainz.org/doc/Cover_Art_Archive/API",
    },
    RateRow {
        source: "wikimedia-api",
        per_second: Rate::Fixed(4.0),
        burst: 1,
        max_in_flight: Some(1),
        verified_on: "2026-10-18",
        basis: "Unauthenticated action API: concurrency of 1 and below 5 requests per \
                second overall. One bucket for Wikidata, Wikipedia and Commons.",
        url: "https://wikitech.wikimedia.org/wiki/Robot_policy",
    },
    RateRow {
        source: "wikimedia-media",
        per_second: Rate::Fixed(5.0),
        burst: 2,
        max_in_flight: Some(2),
        verified_on: "2026-10-18",
        basis: "upload.wikimedia.org: total concurrency of at most 2. The policy does \
                not name thumb.wikimedia.org or commons.wikimedia.org image pages; they \
                share this row, unverified.",
        url: "https://wikitech.wikimedia.org/wiki/Robot_policy",
    },
    RateRow {
        source: "audiodb-media",
        per_second: Rate::Fixed(5.0),
        burst: 2,
        max_in_flight: None,
        verified_on: "2026-10-18",
        basis: "No documented limit for the image CDN, which is not the 30 per minute \
                lookup API. Kept at 5 per second.",
        url: "",
    },
    RateRow {
        source: "discogs",
        per_second: Rate::Fixed(1.0 / 2.4),
        burst: 1,
        max_in_flight: None,
        verified_on: "2026-10-18",
        basis: "Official page refused the fetch (403); copies of the published spec give \
                25 per minute unauthenticated. Kept at one request every 2.4 seconds \
                with no burst.",
        url: "https://www.discogs.com/developers",
    },
    RateRow {
        source: "itunes",
        per_second: Rate::Fixed(0.3),
        burst: 2,
        max_in_flight: None,
        verified_on: "2026-10-18",
        basis: "Limited to approximately 20 calls per minute (subject to change).",
        url: "https://performance-partners.apple.com/search-api",
    },
    RateRow {
        source: "deezer",
        per_second: Rate::Fixed(5.0),
        burst: 5,
        max_in_flight: None,
        verified_on: "2026-10-18",
        basis: "No number on the official page; a third-party copy cites 50 requests \
                per 5 seconds. Held at half that.",
        url: "https://developers.deezer.com/api",
    },
    RateRow {
        source: "lrclib",
        per_second: Rate::Fixed(1.0),
        burst: 1,
        max_in_flight: None,
        verified_on: "2026-10-18",
        basis: "No documented limit (the docs page renders empty); a volunteer service, \
                kept at one request per second.",
        url: "https://lrclib.net/docs",
    },
    RateRow {
        source: "youtube",
        per_second: Rate::Fixed(1.0),
        burst: 2,
        max_in_flight: None,
        verified_on: "2026-10-18",
        basis: "Quota only, 100 searches a day by default; no per-second limit. 1 per \
                second keeps a burst of searches from spending the day at once.",
        url: "https://developers.google.com/youtube/v3/getting-started",
    },
    RateRow {
        source: "github",
        per_second: Rate::Fixed(50.0 / 3600.0),
        burst: 10,
        max_in_flight: None,
        verified_on: "2026-10-18",
        basis: "Unauthenticated 60 requests per hour; on 403 or 429 wait for \
                retry-after or x-ratelimit-reset. Held at 50 per hour.",
        url: "https://docs.github.com/en/rest/using-the-rest-api/rate-limits-for-the-rest-api",
    },
    RateRow {
        source: "ticketmaster",
        per_second: Rate::Fixed(2.0),
        burst: 1,
        max_in_flight: None,
        verified_on: "2026-10-18",
        basis: "5000 calls per day and 5 requests per second; kept at 2 per second, \
                since the daily allowance binds a sweep first.",
        url: "https://developer.ticketmaster.com/products-and-docs/apis/getting-started/",
    },
    RateRow {
        source: "skiddle",
        per_second: Rate::Fixed(1.0),
        burst: 1,
        max_in_flight: None,
        verified_on: "2026-10-18",
        basis: "No documented limit: excessive requests may be limited or blocked. Kept \
                at 1 per second.",
        url: "https://www.skiddle.com/api/",
    },
    RateRow {
        source: "open-meteo",
        per_second: Rate::Fixed(2.0),
        burst: 1,
        max_in_flight: None,
        verified_on: "2026-10-18",
        basis: "Free API: under 600 calls per minute; kept at 2 per second for a city \
                picker.",
        url: "https://open-meteo.com/en/terms",
    },
];

/// One row by key.
#[must_use]
pub fn row(source: &str) -> Option<&'static RateRow> {
    RATES.iter().find(|row| row.source == source)
}
