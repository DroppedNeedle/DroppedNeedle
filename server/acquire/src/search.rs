//! Release polling for the flows loops: [`MusicBrainzReleasePoll`] reads a
//! followed artist's release groups for the follow poll, reading Discover's
//! shared `fresh_feed` first for already-baselined artists and caching
//! MusicBrainz browse responses in `ProviderCacheStore` for 7 days.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use futures_util::future::BoxFuture;

use super::flows::seams::{ObservedRelease, PolledArtist, ReleasePoll};
use droppedneedle_catalog::discover::listening::fresh::{self, FreshFeed, STALE_AFTER_MS};
use droppedneedle_kernel::clock::Clock;
use droppedneedle_providers::musicbrainz::{Criticality, LiveSource, MusicBrainzView};
use droppedneedle_providers::{
    CacheKey, Fetched, Policy, ProviderCacheStore, RequestPriority, Source,
};

/// Release groups read per page (the MusicBrainz browse maximum).
const RELEASE_PAGE: u32 = 100;
/// Pages read per artist at most: 1,000 release groups covers all but a
/// handful of artists, and bounds one poll's provider work.
const MAX_RELEASE_PAGES: u32 = 10;
/// Weekly MusicBrainz fallback cache policy for followed artists.
const FOLLOW_POLL_POLICY: Policy = Policy {
    fresh: Duration::from_secs(7 * 24 * 3_600),
    miss: Duration::from_secs(6 * 3_600),
};

/// The artist's release groups and name from Discover's `fresh_feed` (when
/// fresh and the artist is already baselined inside the 7-day MB window),
/// falling back to MusicBrainz browse cached for 7 days in `ProviderCacheStore`.
pub struct MusicBrainzReleasePoll {
    client: MusicBrainzView,
    cache: ProviderCacheStore,
    clock: Arc<dyn Clock>,
}

impl MusicBrainzReleasePoll {
    /// Poll over one client with shared provider cache and clock.
    pub fn new(client: MusicBrainzView, cache: ProviderCacheStore, clock: Arc<dyn Clock>) -> Self {
        Self {
            client,
            cache,
            clock,
        }
    }

    async fn fresh_feed_releases(&self, followed: &str) -> Option<PolledArtist> {
        let feed: FreshFeed = self.cache.peek(&fresh::key()).await.flatten()?;
        let now_ms = self.clock.now_ms();
        if now_ms.saturating_sub(feed.read_at) > STALE_AFTER_MS {
            return None;
        }
        let mut out = PolledArtist::default();
        for row in feed.rows {
            if !row
                .artist_mbids
                .iter()
                .any(|id| id.eq_ignore_ascii_case(followed))
            {
                continue;
            }
            if out.artist_name.is_none() && !row.artist_name.trim().is_empty() {
                out.artist_name = Some(row.artist_name.trim().to_owned());
            }
            out.releases.push(ObservedRelease {
                rg_mbid: row.release_group_mbid,
                title: row.title,
                first_release_date: Some(row.release_date).filter(|d| !d.is_empty()),
                primary_type: row.primary_type,
                secondary_types: row.secondary_type.into_iter().collect(),
            });
        }
        Some(out)
    }

    async fn browse_musicbrainz(&self, artist_mbid: &str) -> Result<PolledArtist, String> {
        let followed = artist_mbid.to_lowercase();
        let key = CacheKey::new(Source::MusicBrainz, &["follow_poll", &followed]);
        let client = self.client.clone();
        let mbid = artist_mbid.to_owned();
        let cached = self
            .cache
            .get_or_fetch(
                &key,
                FOLLOW_POLL_POLICY,
                RequestPriority::BackgroundSync,
                move |_| async move {
                    let mut out = PolledArtist::default();
                    for page in 0..MAX_RELEASE_PAGES {
                        let offset = page * RELEASE_PAGE;
                        let found = client
                            .browse_artist_release_groups(
                                &mbid,
                                RELEASE_PAGE,
                                offset,
                                Criticality::IdentityCritical,
                            )
                            .await
                            .map_err(|error| error.to_string())?;
                        let read = found.items.len();
                        for group in found.items {
                            if out.artist_name.is_none() {
                                out.artist_name = group
                                    .artist_credit
                                    .iter()
                                    .find(|credit| {
                                        credit.artist.id.to_lowercase() == followed
                                            && !credit.artist.name.trim().is_empty()
                                    })
                                    .map(|credit| credit.artist.name.trim().to_owned());
                            }
                            out.releases.push(ObservedRelease {
                                rg_mbid: group.id,
                                title: group.title.unwrap_or_default(),
                                first_release_date: group.first_release_date,
                                primary_type: group.primary_type,
                                secondary_types: group.secondary_types,
                            });
                        }
                        if read == 0 || u64::from(offset) + read as u64 >= found.count {
                            return Ok::<_, String>(Fetched::Found(out));
                        }
                    }
                    tracing::debug!(artist_mbid = %mbid, "follow poll read the page cap for this artist");
                    Ok(Fetched::Found(out))
                },
            )
            .await?;
        Ok(cached.unwrap_or_default())
    }
}

impl ReleasePoll for MusicBrainzReleasePoll {
    fn poll_releases<'a>(
        &'a self,
        artist_mbid: &'a str,
    ) -> BoxFuture<'a, Result<PolledArtist, String>> {
        self.poll_releases_for(artist_mbid, false)
    }

    fn poll_releases_for<'a>(
        &'a self,
        artist_mbid: &'a str,
        baselined: bool,
    ) -> BoxFuture<'a, Result<PolledArtist, String>> {
        Box::pin(async move {
            let followed = artist_mbid.to_lowercase();
            let key = CacheKey::new(Source::MusicBrainz, &["follow_poll", &followed]);
            if baselined
                && self.cache.fresh(&key).await
                && let Some(fresh_polled) = self.fresh_feed_releases(&followed).await
            {
                return Ok(fresh_polled);
            }
            let mut polled = self.browse_musicbrainz(artist_mbid).await?;
            // Merge any fresh_feed rows on top so a cached MB browse never
            // hides a brand-new release ListenBrainz already reported today.
            if let Some(fresh_polled) = self.fresh_feed_releases(&followed).await {
                if polled.artist_name.is_none() {
                    polled.artist_name = fresh_polled.artist_name;
                }
                let mut seen: HashSet<String> = polled
                    .releases
                    .iter()
                    .map(|row| row.rg_mbid.to_lowercase())
                    .collect();
                for row in fresh_polled.releases {
                    if seen.insert(row.rg_mbid.to_lowercase()) {
                        polled.releases.push(row);
                    }
                }
            }
            Ok(polled)
        })
    }
}

/// The production poll: background priority on the shared limiter, the
/// configured MusicBrainz source read per request, failures counted
/// toward system health.
pub fn live_release_poll(
    providers: &droppedneedle_providers::Providers,
    source: LiveSource,
) -> MusicBrainzReleasePoll {
    MusicBrainzReleasePoll::new(
        providers
            .musicbrainz
            .live(source, RequestPriority::BackgroundSync),
        providers.cache.clone(),
        providers.cache.clock(),
    )
}
