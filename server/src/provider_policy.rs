//! Verified per-provider rate policy table.
//!
//! Limits encode provider-documented allocations. Raise a row only by
//! re-verifying against the provider's documented limit; re-verify any row
//! touched. LAN services (slskd, SABnzbd) get no limiter and no row.

/// One provider's verified rate policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderPolicy {
    /// Lowercase provider key.
    pub name: &'static str,
    /// The verified limit in plain words.
    pub limit: &'static str,
    /// Date the row was last verified, YYYY-MM-DD.
    pub verified_on: &'static str,
    /// Where the limit comes from.
    pub source: &'static str,
}

/// The verified table. Six rows, verified 2026-09-28.
pub const PROVIDER_POLICIES: &[ProviderPolicy] = &[
    ProviderPolicy {
        name: "musicbrainz",
        limit: "1 req/s",
        verified_on: "2026-09-28",
        source: "MusicBrainz API terms, hard upstream rule",
    },
    ProviderPolicy {
        name: "listenbrainz",
        limit: "1 req/s",
        verified_on: "2026-09-28",
        source: "Official API docs; honor rate-limit headers",
    },
    ProviderPolicy {
        name: "audiodb",
        limit: "30 req/min on the free tier",
        verified_on: "2026-09-28",
        source: "Official free-API page",
    },
    ProviderPolicy {
        name: "acoustid",
        limit: "3 req/s",
        verified_on: "2026-09-28",
        source: "Official webservice docs",
    },
    ProviderPolicy {
        name: "coverartarchive",
        limit: "no documented allocation; stay conservative (~1/s), back off on 429/503",
        verified_on: "2026-09-28",
        source: "CAA docs state no rate limiting rules are in place",
    },
    ProviderPolicy {
        name: "lastfm",
        limit: "5/s is community practice, not a documented contract; back off on errors",
        verified_on: "2026-09-28",
        source: "Community practice only",
    },
];

/// Look up one provider row by lowercase name.
pub fn lookup(name: &str) -> Option<&'static ProviderPolicy> {
    PROVIDER_POLICIES.iter().find(|row| row.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_holds_the_six_verified_rows() {
        let names: Vec<&str> = PROVIDER_POLICIES.iter().map(|row| row.name).collect();
        assert_eq!(
            names,
            [
                "musicbrainz",
                "listenbrainz",
                "audiodb",
                "acoustid",
                "coverartarchive",
                "lastfm"
            ]
        );
        for row in PROVIDER_POLICIES {
            assert_eq!(row.verified_on, "2026-09-28");
        }
        assert_eq!(lookup("musicbrainz").unwrap().limit, "1 req/s");
        assert_eq!(lookup("listenbrainz").unwrap().limit, "1 req/s");
        assert_eq!(
            lookup("audiodb").unwrap().limit,
            "30 req/min on the free tier"
        );
        assert_eq!(lookup("acoustid").unwrap().limit, "3 req/s");
        assert!(lookup("slskd").is_none());
    }
}
