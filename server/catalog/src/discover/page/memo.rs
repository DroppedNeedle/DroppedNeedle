//! The memo of the costliest shelf, so a page rebuild the same day reuses
//! it instead of paying for every read again: Daily Mixes, for the rest of
//! the day. It is a row of the page cache, so it survives a restart.

use serde::{Deserialize, Serialize};

use crate::discover::cache::{UserPageCache, Variant};
use crate::discover::models::ChartSection;

/// How long Daily Mixes are reused at most, ms.
const MIX_MS: i64 = 86_400_000;

/// One memo: the shelf, and what it was built from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memo<T> {
    /// The service the shelf was built from (`listenbrainz`, `lastfm`).
    listening: String,
    /// The UTC day it was built on (`YYYY-MM-DD`).
    day: String,
    /// The shelf.
    value: T,
}

/// The memo.
pub struct BuildMemo {
    /// Daily Mixes.
    pub mixes: UserPageCache<Memo<Vec<ChartSection>>>,
}

/// What a memo lookup is for.
pub struct MemoKey<'a> {
    /// The user.
    pub user_id: &'a str,
    /// What the page is built for.
    pub variant: &'a Variant,
    /// What the build was claimed under.
    pub generation: crate::discover::cache::Generation,
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
            value: mixes,
        };
        self.mixes
            .save(key.user_id, key.variant, &memo, key.generation)
            .await;
    }

    /// Forget one user's memo (a manual refresh).
    pub async fn clear_user(&self, user_id: &str) {
        self.mixes.forget(user_id).await;
    }
}
