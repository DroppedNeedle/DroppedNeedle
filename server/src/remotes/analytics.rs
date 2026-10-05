//! Listening analytics over a remote history feed (Plex).
//!
//! A port of v2's Plex analytics: count plays per artist, album, and track
//! over the history entries read, keep the top ten of each (ties keep the
//! order they were first seen in, newest first), and total the listening
//! time and the plays of the last 7 and 30 days.

use std::collections::HashMap;

use super::models::{AnalyticsItem, AnalyticsView, HistoryEntry};
use super::service::ANALYTICS_MAX_ENTRIES;

/// How many rows each top list keeps.
pub const TOP_N: usize = 10;

/// Summarize `entries` (newest first). `total_available` is the upstream
/// history size, which decides whether the summary covers everything.
pub fn summarize(entries: &[HistoryEntry], total_available: i64, now_unix: i64) -> AnalyticsView {
    let mut artists = Tally::default();
    let mut albums = Tally::default();
    let mut tracks = Tally::default();
    let mut total_ms: i64 = 0;
    let mut last_7 = 0;
    let mut last_30 = 0;
    let week_ago = now_unix - 7 * 86_400;
    let month_ago = now_unix - 30 * 86_400;
    for entry in entries {
        artists.add(&entry.artist_name, "");
        albums.add(&entry.album_name, &entry.artist_name);
        tracks.add(&entry.track_title, &entry.artist_name);
        total_ms = total_ms.saturating_add(entry.duration_ms.max(0));
        if entry.viewed_at >= week_ago {
            last_7 += 1;
        }
        if entry.viewed_at >= month_ago {
            last_30 += 1;
        }
    }
    let analyzed = entries.len() as i64;
    AnalyticsView {
        top_artists: artists.top(),
        top_albums: albums.top(),
        top_tracks: tracks.top(),
        total_listens: analyzed,
        listens_last_7_days: last_7,
        listens_last_30_days: last_30,
        total_hours: (total_ms as f64 / 3_600_000.0 * 10.0).round() / 10.0,
        is_complete: analyzed >= total_available || total_available <= ANALYTICS_MAX_ENTRIES,
        entries_analyzed: analyzed,
    }
}

/// Play counts keyed by (name, subtitle), remembering first-seen order.
#[derive(Default)]
struct Tally {
    counts: HashMap<(String, String), (i64, usize)>,
}

impl Tally {
    fn add(&mut self, name: &str, subtitle: &str) {
        let next = self.counts.len();
        let slot = self
            .counts
            .entry((name.to_owned(), subtitle.to_owned()))
            .or_insert((0, next));
        slot.0 += 1;
    }

    fn top(self) -> Vec<AnalyticsItem> {
        let mut rows: Vec<_> = self.counts.into_iter().collect();
        rows.sort_by(|left, right| {
            right
                .1
                .0
                .cmp(&left.1.0)
                .then_with(|| left.1.1.cmp(&right.1.1))
        });
        rows.into_iter()
            .take(TOP_N)
            .map(|((name, subtitle), (play_count, _))| AnalyticsItem {
                name,
                subtitle,
                play_count,
            })
            .collect()
    }
}
