//! Short-lived memos for the two costliest shelves, so a page rebuild
//! within their window reuses them instead of paying for every read again:
//! Daily Mixes for a day, Top Picks for four hours (five minutes when they
//! fell back to the chart only, so the next build tries again).

use std::collections::HashMap;
use std::sync::Mutex;

use crate::reads::discover::models::{ChartSection, TopPicksSection};

/// How long Daily Mixes are reused.
const MIX_SECS: f64 = 86_400.0;
/// How long Top Picks are reused.
const PICKS_SECS: f64 = 14_400.0;
/// How long chart-only Top Picks are reused.
const DEGRADED_PICKS_SECS: f64 = 300.0;

/// Memo entries keyed `<user>:...`, each with its expiry.
#[derive(Default)]
pub struct BuildMemo {
    mixes: Mutex<HashMap<String, (f64, Vec<ChartSection>)>>,
    picks: Mutex<HashMap<String, (f64, Option<TopPicksSection>)>>,
}

fn get<T: Clone>(map: &Mutex<HashMap<String, (f64, T)>>, key: &str, now: f64) -> Option<T> {
    let map = map.lock().ok()?;
    map.get(key)
        .filter(|(expires, _)| *expires > now)
        .map(|(_, value)| value.clone())
}

fn put<T>(map: &Mutex<HashMap<String, (f64, T)>>, key: String, value: T, expires: f64, now: f64) {
    if let Ok(mut map) = map.lock() {
        map.retain(|_, (until, _)| *until > now);
        map.insert(key, (expires, value));
    }
}

impl BuildMemo {
    /// Memoised Daily Mixes, when still fresh. An empty list is a memo too.
    pub fn mixes(&self, key: &str, now: f64) -> Option<Vec<ChartSection>> {
        get(&self.mixes, key, now)
    }

    /// Remember Daily Mixes for a day.
    pub fn store_mixes(&self, key: String, mixes: Vec<ChartSection>, now: f64) {
        put(&self.mixes, key, mixes, now + MIX_SECS, now);
    }

    /// Memoised Top Picks, when still fresh. `Some(None)` is a memoised
    /// "no picks".
    pub fn picks(&self, key: &str, now: f64) -> Option<Option<TopPicksSection>> {
        get(&self.picks, key, now)
    }

    /// Remember Top Picks; chart-only ones for five minutes.
    pub fn store_picks(
        &self,
        key: String,
        picks: Option<TopPicksSection>,
        degraded: bool,
        now: f64,
    ) {
        let ttl = if degraded {
            DEGRADED_PICKS_SECS
        } else {
            PICKS_SECS
        };
        put(&self.picks, key, picks, now + ttl, now);
    }

    /// Forget one user's memos (a manual refresh).
    pub fn clear_user(&self, user_id: &str) {
        let prefix = format!("{user_id}:");
        if let Ok(mut map) = self.mixes.lock() {
            map.retain(|key, _| !key.starts_with(&prefix));
        }
        if let Ok(mut map) = self.picks.lock() {
            map.retain(|key, _| !key.starts_with(&prefix));
        }
    }
}
