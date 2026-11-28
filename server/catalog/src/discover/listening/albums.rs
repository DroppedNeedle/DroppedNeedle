//! Which albums of an artist to offer, for everyone, linked or not: the
//! artist's discography from the install's MusicBrainz source, through the
//! row the artist page reads, with ListenBrainz's listener and listen counts
//! for the groups that may be offered, asked with no token. Discover's
//! shelves, the queue, radio and the album page's similar albums all read
//! it, so no person's link is lent for it.
//!
//! The list is a kept fact ([`KeptFacts`]), the gate ([`gate`](super::gate))
//! already run: an artist's own albums and EPs, no live sets, demos or
//! compilations, with each group's counts kept beside it. It is read again
//! after 14 days, and an artist with none is kept as such for 14 days. The
//! list answers in listener order, most first, a group with no count after
//! them, newest first; while ListenBrainz reads are off it is in date order.
//!
//! A reader at background priority reads kept rows only: a list not kept
//! is wanted and answers "not yet", and counts not read are wanted. A
//! reader a person waits on reads a list nobody has asked about, and its
//! counts when they were not read, and keeps them. A discography still
//! being walked answers "not yet", and a failed read keeps the kept list.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::Arc;

use droppedneedle_providers::listenbrainz::{
    ListenBrainzClient, ListenBrainzCredentials, ReleaseGroupCounts,
};
use droppedneedle_providers::musicbrainz::is_valid_mbid;
use droppedneedle_providers::{
    CacheKey, ProviderCacheStore, ProviderError, RequestPriority, Source,
};
use tokio::sync::Semaphore;

use super::network::ListeningNetwork;
use super::rows::{AlbumFacts, AlbumRow, RankedAlbum};
use super::{Ask, Health, ListeningError, gate};
use crate::discover::facts::{
    Counts, FactKind, FactSource, Kept, KeptFacts, Looked, Notes, WantOrder,
};
use crate::discover::ports::BoxFuture;
use crate::metadata::Catalog;
use crate::metadata::artist::ReleaseGroupItem;
use crate::metadata::error::CatalogError;

/// An artist's album list and its groups' counts.
const LISTS: (FactKind, FactSource) = (FactKind::Albums, FactSource::MusicBrainz);
const COUNTS: (FactKind, FactSource) = (FactKind::AlbumCounts, FactSource::ListenBrainz);

/// The two outside reads behind the list.
pub(crate) trait AlbumReads: Send + Sync + 'static {
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

/// The key the provider cache kept an artist's list under before lists
/// were kept facts: ranked by listeners, or in date order.
fn carried_key(namespace: &str, artist: &str, ranked: bool) -> CacheKey {
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
fn by_date(rows: Vec<RankedAlbum>) -> Vec<RankedAlbum> {
    let mut rows: Vec<RankedAlbum> = rows
        .into_iter()
        .map(|mut row| {
            row.album.facts.listeners = None;
            row.listen_count = 0;
            row
        })
        .collect();
    rows.sort_by(|a, b| newest_first(&a.album, &b.album));
    rows
}

/// `rows` most listeners first, then the uncounted newest first.
fn by_listeners(mut rows: Vec<RankedAlbum>) -> Vec<RankedAlbum> {
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

/// Read `artist`'s discography and keep the albums that may be offered.
/// False when it is still being walked: asked again soon.
///
/// # Errors
///
/// The read's failure; the list kept before stays.
pub(crate) async fn fetch_list<R: AlbumReads + ?Sized>(
    facts: &KeptFacts,
    reads: &R,
    artist: &str,
    priority: RequestPriority,
) -> Result<bool, R::Error> {
    let kept = match reads.discography(artist, priority).await {
        Ok(None) => facts.keep_not_yet(LISTS, artist, WantOrder::Pool).await,
        Ok(Some(groups)) => {
            let rows: Vec<AlbumRow> = groups
                .into_iter()
                .map(|group| album_row(group, artist))
                .filter(|row| gate::may_offer(row, artist))
                .collect();
            let kept = facts.keep_albums(artist, rows).await;
            if let Err(cause) = &kept {
                tracing::warn!(%cause, "an album list was read but not kept");
            }
            return Ok(true);
        }
        Err(error) => {
            let artists = [artist.to_owned()];
            if let Err(cause) = facts.keep_failed(LISTS, &artists, WantOrder::Pool).await {
                tracing::warn!(%cause, "a failed discography read went unrecorded");
            }
            return Err(error);
        }
    };
    if let Err(cause) = kept {
        tracing::warn!(%cause, "a discography not read yet went unrecorded");
    }
    Ok(false)
}

/// Count every kept album of `artists` in one read, which the client cuts
/// into its batches, and keep the counts.
///
/// # Errors
///
/// The read's failure; the counts kept before stay.
pub(crate) async fn fetch_counts<R: AlbumReads + ?Sized>(
    facts: &KeptFacts,
    reads: &R,
    artists: &[String],
    priority: RequestPriority,
) -> Result<(), R::Error> {
    let groups = match facts.album_groups(artists).await {
        Ok(groups) => groups,
        Err(cause) => {
            tracing::warn!(%cause, "kept album groups unreadable; nothing counted");
            return Ok(());
        }
    };
    let found = if groups.is_empty() {
        HashMap::new()
    } else {
        match reads.counts(&groups, priority).await {
            Ok(found) => found,
            Err(error) => {
                if let Err(cause) = facts.keep_failed(COUNTS, artists, WantOrder::Pool).await {
                    tracing::warn!(%cause, "a failed album count read went unrecorded");
                }
                return Err(error);
            }
        }
    };
    let counts = found
        .into_iter()
        .map(|(group, counts)| {
            let counts = Counts {
                listeners: counts.listeners,
                listens: counts.listen_count,
            };
            (lower(&group), counts)
        })
        .collect();
    if let Err(cause) = facts.keep_album_counts(artists, counts).await {
        tracing::warn!(%cause, "album counts were read but not kept");
    }
    Ok(())
}

/// The list the provider cache kept for `artist` from before, kept as a
/// fact with its counts, so an upgrade reads nothing again. False when it
/// held none.
async fn carry(
    cache: &ProviderCacheStore,
    facts: &KeptFacts,
    namespace: &str,
    artist: &str,
) -> bool {
    let (held, ranked) = match cache
        .peek::<Vec<RankedAlbum>>(&carried_key(namespace, artist, true))
        .await
    {
        Some(held) => (held, true),
        None => match cache
            .peek::<Vec<RankedAlbum>>(&carried_key(namespace, artist, false))
            .await
        {
            Some(held) => (held, false),
            None => return false,
        },
    };
    let rows = held.unwrap_or_default();
    let counts: HashMap<String, Counts> = rows
        .iter()
        .filter(|row| ranked && row.album.facts.listeners.is_some())
        .map(|row| {
            let counts = Counts {
                listeners: row.album.facts.listeners,
                listens: Some(row.listen_count),
            };
            (row.album.release_group_mbid.clone(), counts)
        })
        .collect();
    let albums: Vec<AlbumRow> = rows.into_iter().map(|row| row.album).collect();
    let artists = [artist.to_owned()];
    let carried = match facts.keep_albums(artist, albums).await {
        Ok(()) if ranked => facts.keep_album_counts(&artists, counts).await,
        kept => kept,
    };
    if let Err(cause) = &carried {
        tracing::warn!(%cause, "a cached album list was not carried");
    }
    carried.is_ok()
}

/// Every album `artist_mbid` may be offered by, in the list's order, from
/// the MusicBrainz source `namespace`; `ranked` is whether ListenBrainz
/// reads are on. `None` is "not yet": no list is kept, or its discography
/// is still being read. A list whose counts are not read yet answers in
/// date order, and its counts are wanted: the list itself is whole.
///
/// # Errors
///
/// The discography read's failure, for a person, when no list is kept.
pub(super) async fn artist_albums<R: AlbumReads>(
    facts: &KeptFacts,
    cache: &ProviderCacheStore,
    reads: Arc<R>,
    (namespace, artist_mbid): (&str, &str),
    ranked: bool,
    priority: RequestPriority,
) -> Result<Option<Vec<RankedAlbum>>, R::Error> {
    let artist = lower(artist_mbid);
    if !is_valid_mbid(&artist) {
        return Ok(Some(Vec::new()));
    }
    let read = || async {
        facts.albums(&artist).await.unwrap_or_else(|cause| {
            tracing::warn!(%cause, "a kept album list unreadable");
            (Looked::missing(), Looked::missing())
        })
    };
    let person = priority.is_user_facing();
    let (mut list, mut counted) = read().await;
    if matches!(list.kept, Kept::Missing) && carry(cache, facts, namespace, &artist).await {
        (list, counted) = read().await;
    }
    if matches!(list.kept, Kept::Missing) && person {
        let _flight = facts.lock_fetch("albums_list", &artist).await;
        (list, counted) = read().await;
        if matches!(list.kept, Kept::Missing) {
            if !fetch_list(facts, &*reads, &artist, priority).await? {
                return Ok(None);
            }
            (list, counted) = read().await;
        }
    }
    let is_counted = |counted: &Looked<()>| matches!(counted.kept, Kept::Found(()));
    if ranked
        && person
        && matches!(list.kept, Kept::Found(_))
        && !is_counted(&counted)
        && !reads.counts_down()
    {
        let _flight = facts.lock_fetch("albums_counts", &artist).await;
        (list, counted) = read().await;
        if matches!(list.kept, Kept::Found(_)) && !is_counted(&counted) {
            let artists = [artist.clone()];
            match fetch_counts(facts, &*reads, &artists, priority).await {
                Ok(()) => (list, counted) = read().await,
                Err(error) => {
                    tracing::debug!(%error, "album counts unread; the list is in date order")
                }
            }
        }
    }
    let now = facts.now();
    let mut notes = Notes::default();
    notes.saw(now, (LISTS.0, LISTS.1, WantOrder::Pool), &artist, &list);
    if ranked && matches!(list.kept, Kept::Found(_)) {
        notes.saw(
            now,
            (COUNTS.0, COUNTS.1, WantOrder::Pool),
            &artist,
            &counted,
        );
    }
    if let Err(cause) = facts.note(notes).await {
        tracing::warn!(%cause, "what an album list read saw went unnoted");
    }
    Ok(match list.kept {
        Kept::Missing => None,
        Kept::Nothing => Some(Vec::new()),
        Kept::Found(rows) if ranked => Some(by_listeners(rows)),
        Kept::Found(rows) => Some(by_date(rows)),
    })
}
