//! The MusicBrainz half of unified search: artist and release-group
//! buckets, cached with v2's search lifetimes and backed by a six-hour
//! stale copy that stands in when MusicBrainz is down.

use std::collections::HashSet;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::providers::RequestPriority;
use crate::providers::digest_key;
use crate::providers::musicbrainz::{Criticality, hit_score};

use super::error::CatalogError;
use super::mapping;
use super::{Catalog, mb_error, mb_retry, secs};

/// Stale bucket copies live six hours (v2 `SEARCH_STALE_CACHE_TTL`).
const STALE_TTL: Duration = Duration::from_secs(6 * 3600);
/// Proven-empty album searches are cached ten minutes (v2).
const EMPTY_SEARCH_TTL: Duration = Duration::from_secs(600);

/// Which MusicBrainz bucket a hit came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RemoteKind {
    /// An artist.
    Artist,
    /// A release group.
    Album,
}

/// One MusicBrainz search hit, before library flags.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteHit {
    /// Bucket.
    pub kind: RemoteKind,
    /// Artist or release-group MBID.
    pub mbid: String,
    /// Name or title.
    pub title: String,
    /// Credited artist, for albums.
    pub artist: Option<String>,
    /// First release year, for albums.
    pub year: Option<i32>,
    /// Disambiguation comment.
    pub disambiguation: Option<String>,
    /// Artist type, or `Album + Live` style type label.
    pub type_info: Option<String>,
    /// MusicBrainz relevance score, 0-100.
    pub score: i32,
}

/// One bucket's hits as cached.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteHits {
    /// Hits, MusicBrainz order.
    pub items: Vec<RemoteHit>,
}

/// How a bucket's MusicBrainz leg went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteState {
    /// MusicBrainz answered.
    Ok,
    /// MusicBrainz failed; a stale copy stands in.
    Stale,
    /// MusicBrainz missed the deadline and no stale copy exists.
    Timeout,
    /// MusicBrainz failed and no stale copy exists.
    Error,
}

/// One bucket's answer.
#[derive(Debug, Clone)]
pub struct RemoteBucket {
    /// Hits (possibly stale), empty on failure.
    pub hits: Vec<RemoteHit>,
    /// How the MusicBrainz leg went.
    pub state: RemoteState,
}

impl Catalog {
    /// Search MusicBrainz artists (v2 `search_artists`): placeholder
    /// artists filtered out, at most `limit` hits.
    pub async fn search_artists_remote(
        &self,
        query: &str,
        limit: u32,
        offset: u32,
        deadline: Duration,
    ) -> RemoteBucket {
        let (_, namespace) = self.upstream().musicbrainz(RequestPriority::UserInitiated);
        let parts = [
            namespace.as_str(),
            query.trim(),
            &limit.to_string(),
            &offset.to_string(),
        ];
        let key = digest_key("mb:artist:search:", &parts, "v1");
        let stale = digest_key("mb:artist:search:stale:", &parts, "v1");
        let catalog = self.clone();
        let text = query.trim().to_owned();
        let ttl = secs(self.upstream().settings().advanced().cache_ttl_search);
        let fetch = move || async move {
            let (client, _) = catalog
                .upstream()
                .musicbrainz(RequestPriority::UserInitiated);
            let wide = (limit * 2).clamp(25, 100);
            let page = mb_retry(|| {
                client.search_artists_text(&text, wide, offset, Criticality::IdentityCritical)
            })
            .await
            .map_err(mb_error)?;
            let mut seen = HashSet::new();
            let items: Vec<RemoteHit> = page
                .items
                .into_iter()
                .filter(|hit| seen.insert(hit.id.to_ascii_lowercase()))
                .filter_map(|hit| {
                    let name = hit
                        .name
                        .clone()
                        .unwrap_or_else(|| "Unknown Artist".to_owned());
                    if mapping::is_filtered_artist(&hit.id, &name) {
                        return None;
                    }
                    Some(RemoteHit {
                        kind: RemoteKind::Artist,
                        title: name,
                        artist: None,
                        year: None,
                        disambiguation: hit.disambiguation.clone().filter(|text| !text.is_empty()),
                        type_info: hit.artist_type.clone(),
                        score: i32::try_from(hit_score(hit.score, hit.ext_score)).unwrap_or(0),
                        mbid: hit.id,
                    })
                })
                .take(limit as usize)
                .collect();
            Ok((RemoteHits { items }, Some(ttl)))
        };
        self.remote_bucket(key, stale, deadline, fetch).await
    }

    /// Search MusicBrainz release groups (v2 `search_albums`), filtered by
    /// the release-type preferences, at most `limit` hits.
    pub async fn search_albums_remote(
        &self,
        query: &str,
        limit: u32,
        offset: u32,
        deadline: Duration,
    ) -> RemoteBucket {
        let preferences = self.upstream().settings().preferences();
        let primary = mapping::type_set(&preferences.primary_types);
        let secondary = mapping::type_set(&preferences.secondary_types);
        let mut primary_key: Vec<&String> = primary.iter().collect();
        primary_key.sort();
        let mut secondary_key: Vec<&String> = secondary.iter().collect();
        secondary_key.sort();
        let filter_key = format!(
            "{}|{}",
            primary_key
                .iter()
                .map(|value| value.as_str())
                .collect::<Vec<_>>()
                .join(","),
            secondary_key
                .iter()
                .map(|value| value.as_str())
                .collect::<Vec<_>>()
                .join(",")
        );
        let (_, namespace) = self.upstream().musicbrainz(RequestPriority::UserInitiated);
        let parts = [
            namespace.as_str(),
            query.trim(),
            &limit.to_string(),
            &offset.to_string(),
            filter_key.as_str(),
        ];
        let key = digest_key("mb:album:search:", &parts, "v1");
        let stale = digest_key("mb:album:search:stale:", &parts, "v1");
        let catalog = self.clone();
        let text = query.trim().to_owned();
        let ttl = secs(self.upstream().settings().advanced().cache_ttl_search * 2);
        let fetch = move || async move {
            let (client, _) = catalog
                .upstream()
                .musicbrainz(RequestPriority::UserInitiated);
            let wide = (limit + limit / 2).clamp(25, 100);
            let page = mb_retry(|| {
                client.search_release_groups_text(
                    &text,
                    wide,
                    offset,
                    Criticality::IdentityCritical,
                )
            })
            .await
            .map_err(mb_error)?;
            let mut seen = HashSet::new();
            let items: Vec<RemoteHit> = page
                .items
                .into_iter()
                .filter(|hit| seen.insert(hit.id.to_ascii_lowercase()))
                .filter(|hit| {
                    mapping::should_include_release(
                        hit.primary_type.as_deref(),
                        &hit.secondary_types,
                        Some(&primary),
                        Some(&secondary),
                        true,
                    )
                })
                .map(|hit| RemoteHit {
                    kind: RemoteKind::Album,
                    title: hit
                        .title
                        .clone()
                        .unwrap_or_else(|| "Unknown Album".to_owned()),
                    artist: crate::providers::musicbrainz::credit_display_name(&hit.artist_credit)
                        .map(str::to_owned),
                    year: mapping::year_of(hit.first_release_date.as_deref()),
                    disambiguation: hit.disambiguation.clone().filter(|text| !text.is_empty()),
                    type_info: mapping::type_info(
                        hit.primary_type.as_deref(),
                        &hit.secondary_types,
                    ),
                    score: i32::try_from(hit_score(hit.score, hit.ext_score)).unwrap_or(0),
                    mbid: hit.id,
                })
                .take(limit as usize)
                .collect();
            let ttl = if items.is_empty() {
                EMPTY_SEARCH_TTL
            } else {
                ttl
            };
            Ok((RemoteHits { items }, Some(ttl)))
        };
        self.remote_bucket(key, stale, deadline, fetch).await
    }

    /// Run one bucket under its deadline. Fresh answers refresh the stale
    /// copy; failures fall back to it.
    async fn remote_bucket<F, Fut>(
        &self,
        key: String,
        stale_key: String,
        deadline: Duration,
        fetch: F,
    ) -> RemoteBucket
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<(RemoteHits, Option<Duration>), CatalogError>>
            + Send
            + 'static,
    {
        let outcome = tokio::time::timeout(
            deadline,
            self.cached(&self.inner.flights.search, key, fetch),
        )
        .await;
        let failed = match outcome {
            Ok(Ok(hits)) => {
                if !hits.items.is_empty()
                    && let Ok(bytes) = serde_json::to_vec(&hits)
                {
                    self.upstream()
                        .cache()
                        .set_bytes(&stale_key, bytes, STALE_TTL)
                        .await;
                }
                return RemoteBucket {
                    hits: hits.items,
                    state: RemoteState::Ok,
                };
            }
            Ok(Err(error)) => {
                tracing::warn!(%error, "musicbrainz search failed");
                RemoteState::Error
            }
            Err(_) => {
                tracing::warn!("musicbrainz search missed its deadline");
                RemoteState::Timeout
            }
        };
        crate::providers::record_current(
            "musicbrainz",
            crate::providers::IntegrationStatus::Error,
            false,
        );
        if let Some(bytes) = self.upstream().cache().get_bytes(&stale_key).await
            && let Ok(hits) = serde_json::from_slice::<RemoteHits>(&bytes)
        {
            return RemoteBucket {
                hits: hits.items,
                state: RemoteState::Stale,
            };
        }
        RemoteBucket {
            hits: Vec::new(),
            state: failed,
        }
    }

    /// Library flags for search rows: owned artists, owned albums, and
    /// albums with open requests (all lowercase MBIDs).
    pub async fn search_flags(
        &self,
        artists: &[String],
        albums: &[String],
    ) -> (HashSet<String>, HashSet<String>, HashSet<String>) {
        let (owned_artists, (owned_albums, requested)) =
            tokio::join!(self.artist_flags(artists), self.album_flags(albums));
        (owned_artists, owned_albums, requested)
    }
}
