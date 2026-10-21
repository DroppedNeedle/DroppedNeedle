//! Pure reporting rules (v2 ports, directly covered by tests) and the
//! service error.

use super::{error::PlaybackError, ports::TrackInfo};

/// Subsonic scrobble ceiling in milliseconds (v2 `240_000`, 4 minutes).
pub const SUBSONIC_THRESHOLD_CAP_MS: i64 = 240_000;

/// Normalize a reporting client name (v2 `CompatScrobbleAdapter._norm_client`:
/// strip, casefold, empty to `None`).
pub fn normalize_client(client: Option<&str>) -> Option<String> {
    let folded = caseless::default_case_fold_str(client.unwrap_or("").trim());
    if folded.is_empty() {
        None
    } else {
        Some(folded)
    }
}

/// Native scrobble threshold: the v2 Jellyfin stop rule, exact
/// (v2's Jellyfin router `_should_scrobble`).
///
/// - Omitted position counts (v2 "reference s6" quirk: a client that sends
///   no position is trusted to have finished).
/// - Past 90% of runtime counts.
/// - Within the last second counts (catches rounding at the tail).
/// - Unknown or non-positive runtime never counts on its own.
pub fn should_scrobble_native(position_ms: Option<i64>, duration_ms: Option<i64>) -> bool {
    let position = match position_ms {
        None => return true,
        Some(position) => position,
    };
    let runtime = duration_ms.unwrap_or(0);
    if runtime <= 0 {
        return false;
    }
    if position * 100 > 90 * runtime {
        return true;
    }
    position >= runtime - 1_000
}

/// Subsonic stop threshold in milliseconds (v2
/// `PlaybackReportService.report`: half the track, capped at 4 minutes;
/// unknown duration wants a full 4 minutes of play).
pub fn subsonic_scrobble_threshold_ms(duration_ms: i64) -> i64 {
    if duration_ms > 0 {
        (duration_ms / 2).min(SUBSONIC_THRESHOLD_CAP_MS)
    } else {
        SUBSONIC_THRESHOLD_CAP_MS
    }
}

/// True when `position_ms` reaches the Subsonic stop threshold.
pub fn meets_subsonic_threshold(position_ms: i64, duration_ms: i64) -> bool {
    position_ms >= subsonic_scrobble_threshold_ms(duration_ms)
}

/// True for sources with a remote server to attribute to. Local and YouTube
/// plays stay home (v2 only ever reports remote plays to their own server).
pub fn has_remote(source: &str) -> bool {
    matches!(source, "jellyfin" | "navidrome" | "plex")
}

/// Presence cover URL for a resolved track: the v3 release-group cover
/// route when an MBID is known, else empty (v2
/// `CompatScrobbleAdapter._write_presence` built the old route; same rule,
/// new path).
pub fn presence_cover_url(track: &TrackInfo) -> String {
    track
        .rg_mbid
        .as_deref()
        .filter(|mbid| !mbid.is_empty())
        .map(|mbid| format!("/api/v3/covers/release-group/{mbid}"))
        .unwrap_or_default()
}

/// Domain failures. Forwarding and attribution failures never surface here:
/// they are logged and swallowed per the never-fail-the-player rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceError {
    /// Unknown track id.
    TrackNotFound,
    /// Malformed input. The message is user-facing.
    InvalidInput(String),
}

impl ServiceError {
    /// Render into the playback envelope.
    pub fn into_playback_error(self) -> PlaybackError {
        match self {
            Self::TrackNotFound => PlaybackError::NotFound,
            Self::InvalidInput(message) => PlaybackError::InvalidInput { message },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn omitted_position_counts() {
        assert!(should_scrobble_native(None, Some(200_000)));
        assert!(should_scrobble_native(None, None));
    }

    #[test]
    fn past_ninety_percent_counts() {
        assert!(should_scrobble_native(Some(181_000), Some(200_000)));
        assert!(!should_scrobble_native(Some(180_000), Some(200_000)));
    }

    #[test]
    fn last_second_counts() {
        assert!(should_scrobble_native(Some(199_500), Some(200_000)));
        assert!(!should_scrobble_native(Some(100_000), Some(200_000)));
    }

    #[test]
    fn unknown_runtime_never_counts() {
        assert!(!should_scrobble_native(Some(100_000), None));
        assert!(!should_scrobble_native(Some(100_000), Some(0)));
    }

    #[test]
    fn subsonic_threshold_halves_and_caps() {
        assert_eq!(subsonic_scrobble_threshold_ms(600_000), 240_000);
        assert_eq!(subsonic_scrobble_threshold_ms(200_000), 100_000);
        assert_eq!(subsonic_scrobble_threshold_ms(0), 240_000);
        assert!(meets_subsonic_threshold(100_000, 200_000));
        assert!(!meets_subsonic_threshold(99_999, 200_000));
    }

    #[test]
    fn client_normalization_trims_and_folds() {
        assert_eq!(normalize_client(None), None);
        assert_eq!(normalize_client(Some("  ")), None);
        assert_eq!(
            normalize_client(Some(" Symfonium ")),
            Some("symfonium".to_owned())
        );
    }
}
