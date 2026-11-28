//! The ListenBrainz rows other areas read without a user's listening data:
//! search counts, album pages and the Weekly Mix. They go through the same
//! rows as [`ListeningData`](super::ListeningData), so one artist is read
//! once whichever feature asks first. The answers are public, so the keys
//! carry no token. Similar artists and an artist's album list are kept
//! facts and lend no one's link; the other reads lend the person's own
//! link when they go out.

use std::collections::HashMap;
use std::sync::Arc;

use droppedneedle_media::playback::listening::ListeningStore;
use droppedneedle_providers::listenbrainz::{
    ListenBrainzClient, ListenBrainzCredentials, TopReleaseGroup,
};
use droppedneedle_providers::{
    CacheKey, Fetched, Policy, ProviderCacheStore, ProviderError, RequestPriority, Source,
};

use super::rows::{AlbumFacts, AlbumRow, RankedAlbum, ScoredArtist};
use super::{albums, similar};
use crate::discover::facts::KeptFacts;
use crate::metadata::Catalog;

/// Whose ListenBrainz link a read lends: a person's own, looked up only
/// when no fresh answer is held, so a fresh one costs no store read; or
/// no one's. A read never lends the server's account.
#[derive(Clone)]
pub enum Lender {
    /// No one's: the read goes out anonymous.
    Anonymous,
    /// This person's own link when they have one, else anonymous.
    Person(ListeningStore, String),
}

impl Lender {
    /// The identity to send now.
    pub async fn credentials(&self) -> ListenBrainzCredentials {
        match self {
            Self::Person(store, user_id) if !user_id.is_empty() => {
                store.listenbrainz_credentials(user_id).await
            }
            _ => ListenBrainzCredentials::default(),
        }
    }
}

/// Whether `creds` carry a token, which ListenBrainz needs for an
/// artist's top albums and songs.
pub fn has_token(creds: &ListenBrainzCredentials) -> bool {
    creds
        .user_token
        .as_deref()
        .is_some_and(|token| !token.is_empty())
}

fn lower(text: &str) -> String {
    text.trim().to_lowercase()
}

/// The cache key of an artist's top albums: ListenBrainz returns the full list
/// regardless of requested count, so all counts share one key.
fn artist_albums_key(artist_mbid: &str) -> CacheKey {
    CacheKey::new(
        Source::ListenBrainz,
        &["artist_albums", &lower(artist_mbid)],
    )
}

/// The cache key of one release group's listen count.
pub fn listen_count_key(release_group_mbid: &str) -> CacheKey {
    CacheKey::new(
        Source::ListenBrainz,
        &["rg_popularity", &lower(release_group_mbid)],
    )
}

/// A clean empty list is kept for the policy's "nothing there" time.
fn found_or_missing<T>(rows: Vec<T>) -> Fetched<Vec<T>> {
    if rows.is_empty() {
        Fetched::Missing
    } else {
        Fetched::Found(rows)
    }
}

/// ListenBrainz's top release groups as kept rows, every one as sent: the
/// gate runs when they are read. A row without a title stays: it still
/// counts toward an artist's listens, and shelves that show titles leave it
/// out when they read it.
fn ranked_albums(rows: Vec<TopReleaseGroup>, artist_mbid: &str) -> Vec<RankedAlbum> {
    rows.into_iter()
        .map(|row| RankedAlbum {
            album: AlbumRow {
                release_group_mbid: row.release_group_mbid,
                title: row.name,
                artist_name: row.artist_name,
                artist_mbid: Some(artist_mbid.to_lowercase()),
                facts: AlbumFacts {
                    primary_type: row.primary_type,
                    // ListenBrainz sends none.
                    secondary_types: None,
                    first_release_date: row.first_release_date,
                    listeners: row.listeners,
                    credited_artist_mbids: row.credited_artist_mbids,
                },
            },
            listen_count: row.listen_count,
        })
        .collect()
}

/// The albums `artist_mbid` may be offered by, `count` at most, read
/// through the same kept list as [`ListeningData`](super::ListeningData)'s
/// ([`albums`](super::albums)): the discography through `catalog`, counted
/// by `client` with no token. `None` is "not yet": no list is kept, or the
/// artist's discography is still being read, so an answer built without it
/// is not one to keep.
///
/// # Errors
///
/// The discography read's failure, for a person, when no list is kept.
pub async fn artist_albums(
    catalog: &Catalog,
    client: &ListenBrainzClient,
    artist_mbid: &str,
    count: usize,
    priority: RequestPriority,
) -> Result<Option<Vec<RankedAlbum>>, ProviderError> {
    let (_, namespace) = catalog.upstream().musicbrainz(priority);
    let reads = Arc::new(albums::CatalogReads {
        catalog: catalog.clone(),
        client: client.clone(),
    });
    let rows = albums::artist_albums(
        catalog.facts(),
        catalog.upstream().cache(),
        reads,
        (&namespace, artist_mbid),
        true,
        priority,
    )
    .await?;
    Ok(rows.map(|mut rows| {
        rows.truncate(count);
        rows
    }))
}

/// One artist's most played release groups of every type, `count` at
/// most, from ListenBrainz and not gated: what the artist's listens add up
/// from, singles included. ListenBrainz answers them only with a token:
/// without one, what is held answers and no fetch is started or joined.
///
/// # Errors
///
/// ListenBrainz's failure when no row is kept to stand in.
pub async fn artist_release_groups(
    cache: &ProviderCacheStore,
    client: &ListenBrainzClient,
    artist_mbid: &str,
    count: usize,
    lender: &Lender,
    priority: RequestPriority,
) -> Result<Vec<RankedAlbum>, ProviderError> {
    let mut rows = release_group_rows(cache, client, artist_mbid, lender, priority).await?;
    rows.truncate(count);
    Ok(rows)
}

/// Every kept row of one artist's top release groups.
async fn release_group_rows(
    cache: &ProviderCacheStore,
    client: &ListenBrainzClient,
    artist_mbid: &str,
    lender: &Lender,
    priority: RequestPriority,
) -> Result<Vec<RankedAlbum>, ProviderError> {
    let key = artist_albums_key(artist_mbid);
    if !cache.fresh(&key).await && !has_token(&lender.credentials().await) {
        let held: Option<Vec<RankedAlbum>> = cache.peek(&key).await.flatten();
        return Ok(held.unwrap_or_default());
    }
    let (client, mbid, lender) = (client.clone(), artist_mbid.to_owned(), lender.clone());
    let rows = cache
        .get_or_fetch(
            &key,
            Policy::LISTENBRAINZ_ARTIST,
            priority,
            move |priority| async move {
                let creds = lender.credentials().await;
                match client
                    .artist_top_release_groups(&mbid, usize::MAX, &creds, priority)
                    .await
                {
                    Ok(rows) => Ok(found_or_missing(ranked_albums(rows, &mbid))),
                    // Not sent: shown as nothing, kept for no one.
                    Err(ProviderError::NotConfigured { .. }) => Ok(Fetched::Partial(Vec::new())),
                    Err(error) => Err(error),
                }
            },
        )
        .await?;
    Ok(rows.unwrap_or_default())
}

/// Artists like one artist, best first, `limit` at most: the one answer
/// ([`similar`](super::similar)), from the kept facts. No token is sent.
///
/// # Errors
///
/// "Not yet" while no list is kept and no person waits on the read, or
/// ListenBrainz's failure.
pub async fn similar_artists(
    facts: &KeptFacts,
    cache: &ProviderCacheStore,
    client: &ListenBrainzClient,
    artist_mbid: &str,
    limit: usize,
    priority: RequestPriority,
) -> Result<Vec<ScoredArtist>, ProviderError> {
    let reads = similar::ClientReads::new(client);
    let mut rows = similar::neighbours(facts, cache, reads, artist_mbid, priority).await?;
    rows.truncate(limit);
    Ok(rows)
}

/// [`similar_artists`] for each of `artist_mbids`, in their order, as one
/// pass.
pub async fn similar_artists_each(
    facts: &KeptFacts,
    cache: &ProviderCacheStore,
    client: &ListenBrainzClient,
    artist_mbids: &[String],
    limit: usize,
    priority: RequestPriority,
) -> Vec<Result<Vec<ScoredArtist>, ProviderError>> {
    let reads = similar::ClientReads::new(client);
    similar::neighbours_each(facts, cache, reads, artist_mbids, priority)
        .await
        .0
        .into_iter()
        .map(|rows| {
            rows.map(|mut rows| {
                rows.truncate(limit);
                rows
            })
        })
        .collect()
}

/// Listen counts per release group, each kept on its own.
///
/// # Errors
///
/// ListenBrainz's failure, for the ids not held.
pub async fn listen_counts(
    cache: &ProviderCacheStore,
    client: &ListenBrainzClient,
    release_group_mbids: &[String],
    lender: &Lender,
    priority: RequestPriority,
) -> Result<HashMap<String, i64>, ProviderError> {
    cache
        .fetch_each(
            release_group_mbids,
            listen_count_key,
            Policy::LISTENBRAINZ_ARTIST,
            |missing| async move {
                let creds = lender.credentials().await;
                client
                    .release_group_popularity(&missing, &creds, priority)
                    .await
            },
        )
        .await
}
