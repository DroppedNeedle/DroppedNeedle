//! Memos for the two costliest shelves, so a page rebuild within their
//! window reuses them instead of paying for every read again: Daily Mixes
//! for the rest of the day, Top Picks for four hours (five minutes when
//! they fell back to the chart only, so the next build tries again). Each
//! is a row of the page cache, so they survive a restart.

use serde::{Deserialize, Serialize};

use crate::reads::discover::cache::{UserPageCache, Variant};
use crate::reads::discover::models::{ChartSection, TopPicksSection};

/// How long Daily Mixes are reused at most, ms.
const MIX_MS: i64 = 86_400_000;
/// How long Top Picks are reused, ms.
const PICKS_MS: i64 = 14_400_000;
/// How long chart-only Top Picks are reused, ms.
const DEGRADED_PICKS_MS: i64 = 300_000;

/// One memo: the shelf, and what it was built from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memo<T> {
    /// The service the shelf was built from (`listenbrainz`, `lastfm`).
    listening: String,
    /// The UTC day it was built on (`YYYY-MM-DD`).
    day: String,
    /// Whether it fell back to the chart only.
    degraded: bool,
    /// The shelf.
    value: T,
}

/// The two memos.
pub struct BuildMemo {
    /// Daily Mixes.
    pub mixes: UserPageCache<Memo<Vec<ChartSection>>>,
    /// Top Picks.
    pub picks: UserPageCache<Memo<Option<TopPicksSection>>>,
}

/// What a memo lookup is for.
pub struct MemoKey<'a> {
    /// The user.
    pub user_id: &'a str,
    /// What the page is built for.
    pub variant: &'a Variant,
    /// The clear generation the build was claimed in.
    pub generation: i64,
    /// The service the shelf is built from.
    pub listening: &'a str,
    /// Today (`YYYY-MM-DD`, UTC).
    pub today: &'a str,
}

impl BuildMemo {
    /// Memoised Daily Mixes, when built today from the same service. An
    /// empty list is a memo too.
    pub async fn mixes(&self, key: &MemoKey<'_>) -> Option<Vec<ChartSection>> {
        let saved = self.mixes.load(key.user_id, key.variant).await?;
        let fresh = self.mixes.now_ms() - saved.built_at < MIX_MS
            && saved.value.day == key.today
            && saved.value.listening == key.listening;
        fresh.then_some(saved.value.value)
    }

    /// Remember Daily Mixes.
    pub async fn store_mixes(&self, key: &MemoKey<'_>, mixes: Vec<ChartSection>) {
        let memo = Memo {
            listening: key.listening.to_owned(),
            day: key.today.to_owned(),
            degraded: false,
            value: mixes,
        };
        self.mixes
            .save(key.user_id, key.variant, &memo, key.generation)
            .await;
    }

    /// Memoised Top Picks, when still fresh and from the same service.
    /// `Some(None)` is a memoised "no picks".
    pub async fn picks(&self, key: &MemoKey<'_>) -> Option<Option<TopPicksSection>> {
        let saved = self.picks.load(key.user_id, key.variant).await?;
        let lifetime = if saved.value.degraded {
            DEGRADED_PICKS_MS
        } else {
            PICKS_MS
        };
        let fresh = self.picks.now_ms() - saved.built_at < lifetime
            && saved.value.listening == key.listening;
        fresh.then_some(saved.value.value)
    }

    /// Remember Top Picks; chart-only ones for five minutes.
    pub async fn store_picks(
        &self,
        key: &MemoKey<'_>,
        picks: Option<TopPicksSection>,
        degraded: bool,
    ) {
        let memo = Memo {
            listening: key.listening.to_owned(),
            day: key.today.to_owned(),
            degraded,
            value: picks,
        };
        self.picks
            .save(key.user_id, key.variant, &memo, key.generation)
            .await;
    }

    /// Forget one user's memos (a manual refresh).
    pub async fn clear_user(&self, user_id: &str) {
        self.mixes.forget(user_id).await;
        self.picks.forget(user_id).await;
    }
}
