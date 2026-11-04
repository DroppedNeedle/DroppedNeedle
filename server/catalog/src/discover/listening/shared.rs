//! The ListenBrainz rows other areas read without a user's listening data:
//! search counts and the Weekly Mix. They go through the same cache rows
//! as [`ListeningData`](super::ListeningData), so one artist is read once
//! whichever feature asks first.

use std::collections::HashMap;

use droppedneedle_providers::listenbrainz::{
    ListenBrainzClient, ListenBrainzCredentials, SimilarArtist, TopReleaseGroup,
};
use droppedneedle_providers::{
    CacheKey, Fetched, Policy, ProviderCacheStore, ProviderError, RequestPriority, Source,
};

use super::rows::{AlbumRow, ArtistRow, RankedAlbum, ScoredArtist};

/// Similar artists read per artist.
pub const SIMILAR_ROWS: usize = 25;
/// An artist's top albums read per artist.
pub const ARTIST_ALBUM_ROWS: usize = 25;

fn lower(text: &str) -> String {
    text.trim().to_lowercase()
}

/// The cache key of an artist's similar artists, `read` rows long.
pub(super) fn similar_key(artist_mbid: &str, read: usize) -> CacheKey {
    CacheKey::new(
        Source::ListenBrainz,
        &["similar_artists", &lower(artist_mbid), &read.to_string()],
    )
}

/// The cache key of an artist's top albums, `read` rows long.
pub(super) fn artist_albums_key(artist_mbid: &str, read: usize) -> CacheKey {
    CacheKey::new(
        Source::ListenBrainz,
        &["artist_albums", &lower(artist_mbid), &read.to_string()],
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

/// Up to `limit` artists similar to one artist, straight from ListenBrainz.
/// LB Radio counts the seed among the artists it is asked for and the
/// client drops it, so one more is asked for (the recorded answer,
/// `tests/fixtures/listenbrainz/lb_radio_artist.json`, was asked for three
/// and holds the seed and two others).
///
/// # Errors
///
/// The typed failure of the call.
pub(super) async fn similar_from(
    client: &ListenBrainzClient,
    artist_mbid: &str,
    limit: usize,
    creds: &ListenBrainzCredentials,
    priority: RequestPriority,
) -> Result<Vec<ScoredArtist>, ProviderError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut rows = client
        .similar_artists(artist_mbid, limit + 1, creds, priority)
        .await?;
    rows.truncate(limit);
    Ok(scored_artists(rows))
}

/// ListenBrainz's similar artists as kept rows.
fn scored_artists(rows: Vec<SimilarArtist>) -> Vec<ScoredArtist> {
    rows.into_iter()
        .map(|row| ScoredArtist {
            score: row.listen_count as f64,
            artist: ArtistRow {
                name: row.artist_name,
                mbid: Some(row.artist_mbid),
                listen_count: Some(row.listen_count),
            },
        })
        .collect()
}

/// ListenBrainz's top release groups as kept rows. A row without a title
/// stays: it still counts toward an artist's listens, and shelves that show
/// titles leave it out when they read it.
pub(super) fn ranked_albums(rows: Vec<TopReleaseGroup>, artist_mbid: &str) -> Vec<RankedAlbum> {
    rows.into_iter()
        .map(|row| RankedAlbum {
            album: AlbumRow {
                release_group_mbid: row.release_group_mbid,
                title: row.name,
                artist_name: row.artist_name,
                artist_mbid: Some(artist_mbid.to_lowercase()),
            },
            listen_count: row.listen_count,
        })
        .collect()
}

/// One artist's most played release groups, `count` at most, read through
/// the shared rows.
///
/// # Errors
///
/// ListenBrainz's failure when no row is kept to stand in.
pub async fn artist_albums(
    cache: &ProviderCacheStore,
    client: &ListenBrainzClient,
    artist_mbid: &str,
    count: usize,
    priority: RequestPriority,
) -> Result<Vec<RankedAlbum>, ProviderError> {
    let read = count.max(ARTIST_ALBUM_ROWS);
    let (client, mbid) = (client.clone(), artist_mbid.to_owned());
    let rows = cache
        .get_or_fetch(
            &artist_albums_key(artist_mbid, read),
            Policy::LISTENBRAINZ_ARTIST,
            priority,
            move |priority| async move {
                let rows = client
                    .artist_top_release_groups(
                        &mbid,
                        read,
                        &ListenBrainzCredentials::default(),
                        priority,
                    )
                    .await?;
                Ok(found_or_missing(ranked_albums(rows, &mbid)))
            },
        )
        .await?;
    let mut rows: Vec<RankedAlbum> = rows.unwrap_or_default();
    rows.truncate(count);
    Ok(rows)
}

/// Artists similar to one artist, `limit` at most, read through the shared
/// rows.
///
/// # Errors
///
/// ListenBrainz's failure when no row is kept to stand in.
pub async fn similar_artists(
    cache: &ProviderCacheStore,
    client: &ListenBrainzClient,
    artist_mbid: &str,
    limit: usize,
    priority: RequestPriority,
) -> Result<Vec<ScoredArtist>, ProviderError> {
    let read = limit.max(SIMILAR_ROWS);
    let (client, mbid) = (client.clone(), artist_mbid.to_owned());
    let rows = cache
        .get_or_fetch(
            &similar_key(artist_mbid, read),
            Policy::LISTENBRAINZ_ARTIST,
            priority,
            move |priority| async move {
                let rows = similar_from(
                    &client,
                    &mbid,
                    read,
                    &ListenBrainzCredentials::default(),
                    priority,
                )
                .await?;
                Ok(found_or_missing(rows))
            },
        )
        .await?;
    let mut rows: Vec<ScoredArtist> = rows.unwrap_or_default();
    rows.truncate(limit);
    Ok(rows)
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
    priority: RequestPriority,
) -> Result<HashMap<String, i64>, ProviderError> {
    cache
        .fetch_each(
            release_group_mbids,
            listen_count_key,
            Policy::LISTENBRAINZ_ARTIST,
            |missing| async move {
                client
                    .release_group_popularity(
                        &missing,
                        &ListenBrainzCredentials::default(),
                        priority,
                    )
                    .await
            },
        )
        .await
}
