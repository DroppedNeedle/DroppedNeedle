//! Which albums of an artist to offer, for everyone, linked or not: the
//! artist's discography from the install's MusicBrainz source, through the
//! row the artist page reads, with ListenBrainz's listener and listen counts
//! for the groups that may be offered, asked with no token. Discover's
//! shelves, the queue, radio and the album page's similar albums all read
//! it, so no person's link is lent for it.
//!
//! The list is kept under one key per artist for 14 days, the gate
//! ([`gate`](super::gate)) already run: an artist's own albums and EPs, no
//! live sets, demos or compilations. An artist with none is kept as such
//! for 14 days too, once its discography read cleanly. The list is ordered
//! by listeners, most first; a group with no count goes after them, newest
//! first. While ListenBrainz reads are off the list is in date order, kept
//! under a key of its own. While popularity is down, or its read failed, it
//! answers in date order and is not kept. A discography still being walked answers "not
//! yet" and is not kept, and a failed discography read keeps the kept list.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use droppedneedle_providers::listenbrainz::{
    ListenBrainzClient, ListenBrainzCredentials, ReleaseGroupCounts,
};
use droppedneedle_providers::musicbrainz::is_valid_mbid;
use droppedneedle_providers::{
    CacheKey, Fetched, Policy, ProviderCacheStore, ProviderError, RequestPriority, Source,
};
use tokio::sync::Semaphore;

use super::network::ListeningNetwork;
use super::rows::{AlbumFacts, AlbumRow, RankedAlbum};
use super::{Ask, Health, ListeningError, gate};
use crate::discover::ports::BoxFuture;
use crate::metadata::Catalog;
use crate::metadata::artist::ReleaseGroupItem;
use crate::metadata::error::CatalogError;

/// How long a list is kept, and an artist with nothing to offer: it moves
/// over weeks. Only an answer read without a failure is kept.
const LISTS: Policy = Policy {
    fresh: Duration::from_secs(14 * 86_400),
    miss: Duration::from_secs(14 * 86_400),
};

/// The two outside reads behind the list.
pub(super) trait AlbumReads: Send + Sync + 'static {
    /// How a read fails.
    type Error: std::fmt::Display + Clone + Send + Sync + 'static;
    /// The artist's whole discography, `None` while it is still being
    /// fetched.
    fn discography<'a>(
        &'a self,
        artist_mbid: &'a str,
        priority: RequestPriority,
    ) -> BoxFuture<'a, Result<Option<Vec<ReleaseGroupItem>>, Self::Error>>;
    /// Listen and listener counts for release groups, sent with no token.
    fn counts<'a>(
        &'a self,
        release_group_mbids: &'a [String],
        priority: RequestPriority,
    ) -> BoxFuture<'a, Result<HashMap<String, ReleaseGroupCounts>, Self::Error>>;
    /// Whether ListenBrainz refuses popularity reads right now, so no
    /// count read is sent.
    fn counts_down(&self) -> bool {
        false
    }
}

/// The reads straight from the catalog and the client, for the album
/// page's similar albums.
pub(super) struct CatalogReads {
    pub(super) catalog: Catalog,
    pub(super) client: ListenBrainzClient,
}

/// A catalog failure as the provider failure behind it.
pub(super) fn provider_error(error: CatalogError) -> ProviderError {
    match error {
        CatalogError::Unavailable(error) => error,
        other => {
            tracing::warn!(error = %other, "artist discography unread");
            ProviderError::Unavailable {
                provider: "musicbrainz",
                retry_after: None,
            }
        }
    }
}

impl AlbumReads for CatalogReads {
    type Error = ProviderError;

    fn discography<'a>(
        &'a self,
        artist_mbid: &'a str,
        priority: RequestPriority,
    ) -> BoxFuture<'a, Result<Option<Vec<ReleaseGroupItem>>, ProviderError>> {
        Box::pin(async move {
            self.catalog
                .complete_release_groups(artist_mbid, priority)
                .await
                .map_err(provider_error)
        })
    }

    fn counts<'a>(
        &'a self,
        release_group_mbids: &'a [String],
        priority: RequestPriority,
    ) -> BoxFuture<'a, Result<HashMap<String, ReleaseGroupCounts>, ProviderError>> {
        Box::pin(async move {
            self.client
                .release_group_counts(
                    release_group_mbids,
                    &ListenBrainzCredentials::default(),
                    priority,
                )
                .await
        })
    }
}

/// The reads through the listening network, under an ask's cap. The count
/// read is noted in and held back by the popularity health.
pub(super) struct NetworkReads {
    pub(super) network: Arc<dyn ListeningNetwork>,
    pub(super) permits: Option<Arc<Semaphore>>,
    pub(super) health: Arc<Health>,
}

impl AlbumReads for NetworkReads {
    type Error = ListeningError;

    fn discography<'a>(
        &'a self,
        artist_mbid: &'a str,
        priority: RequestPriority,
    ) -> BoxFuture<'a, Result<Option<Vec<ReleaseGroupItem>>, ListeningError>> {
        Box::pin(async move {
            let _permit = Ask::permit(self.permits.clone()).await;
            self.network.discography(artist_mbid, priority).await
        })
    }

    fn counts<'a>(
        &'a self,
        release_group_mbids: &'a [String],
        priority: RequestPriority,
    ) -> BoxFuture<'a, Result<HashMap<String, ReleaseGroupCounts>, ListeningError>> {
        Box::pin(async move {
            let _permit = Ask::permit(self.permits.clone()).await;
            let counts = self
                .network
                .release_group_counts(release_group_mbids, priority)
                .await;
            self.health.note(&counts, priority);
            counts
        })
    }

    fn counts_down(&self) -> bool {
        self.health.popularity_down()
    }
}

fn lower(text: &str) -> String {
    text.trim().to_ascii_lowercase()
}

/// The key of one artist's list from the MusicBrainz source `namespace`:
/// ranked by listeners, or in date order while ListenBrainz reads are off.
fn list_key(namespace: &str, artist: &str, ranked: bool) -> CacheKey {
    let order = if ranked { "by_listeners" } else { "by_date" };
    CacheKey::new(
        Source::MusicBrainz,
        &[namespace, "artist_album_list", artist, order],
    )
}

/// One discography row as an album row filed under `artist`.
fn album_row(group: ReleaseGroupItem, artist: &str) -> AlbumRow {
    AlbumRow {
        release_group_mbid: lower(&group.id),
        title: group.title.unwrap_or_default(),
        artist_name: group.artist_name.unwrap_or_default(),
        artist_mbid: Some(artist.to_owned()),
        facts: AlbumFacts {
            primary_type: group.primary_type,
            secondary_types: Some(group.secondary_types),
            first_release_date: group.first_release_date.filter(|date| !date.is_empty()),
            listeners: None,
            credited_artist_mbids: Some(group.credited_artist_mbids).filter(|ids| !ids.is_empty()),
        },
    }
}

/// Newest first; a group with no date last.
fn newest_first(a: &AlbumRow, b: &AlbumRow) -> Ordering {
    match (&a.facts.first_release_date, &b.facts.first_release_date) {
        (Some(a), Some(b)) => b.cmp(a),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// `rows` uncounted, newest first.
fn by_date(rows: Vec<AlbumRow>) -> Vec<RankedAlbum> {
    let mut rows: Vec<RankedAlbum> = rows
        .into_iter()
        .map(|album| RankedAlbum {
            album,
            listen_count: 0,
        })
        .collect();
    rows.sort_by(|a, b| newest_first(&a.album, &b.album));
    rows
}

/// `rows` with their counts, most listeners first, then the uncounted
/// newest first.
fn by_listeners(
    rows: Vec<AlbumRow>,
    counts: &HashMap<String, ReleaseGroupCounts>,
) -> Vec<RankedAlbum> {
    let counts: HashMap<String, ReleaseGroupCounts> = counts
        .iter()
        .map(|(id, counts)| (lower(id), *counts))
        .collect();
    let mut rows: Vec<RankedAlbum> = rows
        .into_iter()
        .map(|mut album| {
            let found = counts.get(&album.release_group_mbid).copied();
            album.facts.listeners = found.and_then(|found| found.listeners);
            RankedAlbum {
                listen_count: found.and_then(|found| found.listen_count).unwrap_or(0),
                album,
            }
        })
        .collect();
    rows.sort_by(
        |a, b| match (a.album.facts.listeners, b.album.facts.listeners) {
            (Some(left), Some(right)) => right.cmp(&left).then(b.listen_count.cmp(&a.listen_count)),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => newest_first(&a.album, &b.album),
        },
    );
    rows
}

/// Every album `artist_mbid` may be offered by, in the list's order, from
/// the MusicBrainz source `namespace`; `ranked` is whether ListenBrainz
/// reads are on. `None` is "not yet": its discography is still being read.
///
/// # Errors
///
/// The discography read's failure when no list is kept to stand in.
pub(super) async fn artist_albums<R: AlbumReads>(
    cache: &ProviderCacheStore,
    reads: Arc<R>,
    namespace: &str,
    artist_mbid: &str,
    ranked: bool,
    priority: RequestPriority,
) -> Result<Option<Vec<RankedAlbum>>, R::Error> {
    let artist = lower(artist_mbid);
    if !is_valid_mbid(&artist) {
        return Ok(Some(Vec::new()));
    }
    let key = list_key(namespace, &artist, ranked);
    let found = cache
        .get_or_fetch(&key, LISTS, priority, move |priority| async move {
            let Some(groups) = reads.discography(&artist, priority).await? else {
                // A large discography is still being walked: not yet the
                // artist's albums. The next build asks again.
                return Ok(Fetched::Partial(Vec::new()));
            };
            let rows: Vec<AlbumRow> = groups
                .into_iter()
                .map(|group| album_row(group, &artist))
                .filter(|row| gate::may_offer(row, &artist))
                .collect();
            if rows.is_empty() {
                return Ok(Fetched::Missing);
            }
            if !ranked {
                return Ok(Fetched::Found(by_date(rows)));
            }
            if reads.counts_down() {
                return Ok(Fetched::Partial(by_date(rows)));
            }
            let ids: Vec<String> = rows
                .iter()
                .map(|row| row.release_group_mbid.clone())
                .collect();
            Ok(match reads.counts(&ids, priority).await {
                Ok(counts) => Fetched::Found(by_listeners(rows, &counts)),
                Err(error) => {
                    tracing::debug!(%error, "album counts unread; the list is in date order");
                    Fetched::Partial(by_date(rows))
                }
            })
        })
        .await?;
    // Only "not yet" answers an empty list: a kept or dated one has rows,
    // and an artist with none reads as nothing there.
    Ok(match found {
        None => Some(Vec::new()),
        Some(rows) if rows.is_empty() => None,
        Some(rows) => Some(rows),
    })
}
