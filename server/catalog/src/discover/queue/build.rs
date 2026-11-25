//! What one build reads from, and the album pools other surfaces share.
//!
//! The pools of similar artists' albums ([`similar_artist_pools`],
//! [`lastfm_pools`]) are what radio, the Radio shelves, Daily Mixes and
//! playlist suggestions deal from. Every provider read there may fail: the
//! failure is logged and that pool is simply empty.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use futures_util::future::join_all;

use super::QueueSettings;
use super::store::QueueDb;
use crate::discover::listening::{ArtistRow, Ask, LastFmAlbum, ListeningData, ScoredArtist};
use droppedneedle_providers::musicbrainz::is_valid_mbid;

/// The "Various Artists" placeholder; never a real recommendation.
pub const VARIOUS_ARTISTS_MBID: &str = "89ad4ac3-39f7-470e-963a-56509c546377";
/// How long one ListenBrainz similar-artist or album read may take.
const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// What one build reads from.
pub struct BuildContext<'a> {
    /// Provider reads.
    pub listening: &'a ListeningData,
    /// The queue tables.
    pub db: &'a QueueDb,
    /// Settings read at the start of this build.
    pub settings: &'a QueueSettings,
    /// The build's priority (a person waiting, or the warm cycle) and
    /// its cap on provider calls in flight.
    pub ask: Ask,
    /// When the build runs, unix milliseconds, from the kernel clock.
    pub now_ms: i64,
}

/// The cover URL a card shows.
pub fn cover_url(release_group_mbid: &str) -> String {
    format!("/api/v3/covers/release-group/{release_group_mbid}?size=500")
}

/// One album of a pool of similar artists' albums.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlbumCard {
    /// MusicBrainz release-group id, lowercased.
    pub musicbrainz_id: String,
    /// Album title.
    pub title: String,
    /// Artist name.
    pub artist_name: String,
    /// The artist's MusicBrainz id, lowercased, or empty.
    pub artist_id: String,
}

fn album_card(
    release_group_mbid: &str,
    album_name: &str,
    artist_name: &str,
    artist_mbid: &str,
) -> AlbumCard {
    AlbumCard {
        musicbrainz_id: release_group_mbid.to_owned(),
        title: album_name.to_owned(),
        artist_name: artist_name.to_owned(),
        artist_id: artist_mbid.to_owned(),
    }
}

/// Trim and lowercase an id; blank reads as `None`.
pub(in crate::discover) fn normalize(mbid: Option<&str>) -> Option<String> {
    mbid.map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
}

pub(in crate::discover) fn is_various_artists(mbid: Option<&str>) -> bool {
    normalize(mbid).as_deref() == Some(VARIOUS_ARTISTS_MBID)
}

/// Log a failed read and carry on with an empty answer.
pub(in crate::discover) fn or_empty<T: Default, E: std::fmt::Display>(
    what: &str,
    result: Result<T, E>,
) -> T {
    result.unwrap_or_else(|cause| {
        tracing::warn!(read = what, %cause, "discover queue read failed; skipping it");
        T::default()
    })
}

/// Run one read under the per-call timeout.
pub(in crate::discover) async fn timed<T: Default, E: std::fmt::Display>(
    what: &str,
    read: impl std::future::Future<Output = Result<T, E>>,
) -> T {
    match tokio::time::timeout(READ_TIMEOUT, read).await {
        Ok(result) => or_empty(what, result),
        Err(_) => {
            tracing::warn!(read = what, "discover queue read timed out; skipping it");
            T::default()
        }
    }
}

/// Up to `limit` artists similar to `seed`, best first (see
/// [`similar_to_each`]).
pub(in crate::discover) async fn similar_to(
    ctx: &BuildContext<'_>,
    user_id: &str,
    seed: &ArtistRow,
    limit: usize,
) -> Vec<ScoredArtist> {
    similar_to_each(ctx, user_id, std::slice::from_ref(seed), limit)
        .await
        .pop()
        .unwrap_or_default()
}

/// Up to `limit` artists similar to each of `seeds`, best first, in their
/// order: ListenBrainz's one answer while its reads are on, read as one
/// pass so every neighbour is counted in one read; else, or when it has
/// nothing for a seed, Last.fm's where the user can read it.
pub(in crate::discover) async fn similar_to_each(
    ctx: &BuildContext<'_>,
    user_id: &str,
    seeds: &[ArtistRow],
    limit: usize,
) -> Vec<Vec<ScoredArtist>> {
    let mbids: Vec<String> = seeds.iter().filter_map(|seed| seed.mbid.clone()).collect();
    let mut answers = timed("listenbrainz similar artists", async {
        Ok::<_, std::convert::Infallible>(
            ctx.listening
                .similar_artists_each(user_id, &mbids, limit, &ctx.ask)
                .await,
        )
    })
    .await
    .into_iter();
    let lists: Vec<Vec<ScoredArtist>> = seeds
        .iter()
        .map(|seed| match seed.mbid {
            Some(_) => answers
                .next()
                .map(|found| or_empty("listenbrainz similar artists", found))
                .unwrap_or_default(),
            None => Vec::new(),
        })
        .collect();
    join_all(seeds.iter().zip(lists).map(|(seed, rows)| async move {
        if !rows.is_empty() {
            return rows;
        }
        timed(
            "lastfm similar artists",
            ctx.listening
                .lastfm_similar(user_id, seed, limit as u32, &ctx.ask),
        )
        .await
    }))
    .await
}

/// One pool per seed: the top `albums_per` albums of each of up to
/// `similar_limit` artists similar to it (see [`similar_to_each`]).
pub(in crate::discover) async fn similar_artist_pools(
    ctx: &BuildContext<'_>,
    user_id: &str,
    seeds: &[ArtistRow],
    excluded: &HashSet<String>,
    similar_limit: usize,
    albums_per: usize,
) -> Vec<Vec<AlbumCard>> {
    let lists = similar_to_each(ctx, user_id, seeds, similar_limit).await;
    join_all(seeds.iter().zip(lists).map(|(seed, similar)| async move {
        if seed.mbid.is_none() {
            return Vec::new();
        }
        let mut pool = Vec::new();
        let mut seen = HashSet::new();
        for artist in similar.into_iter().map(|scored| scored.artist) {
            let Some(artist_mbid) = normalize(artist.mbid.as_deref()) else {
                continue;
            };
            if artist_mbid == VARIOUS_ARTISTS_MBID {
                continue;
            }
            let albums = timed(
                "artist albums",
                ctx.listening
                    .artist_albums(user_id, &artist_mbid, albums_per, &ctx.ask),
            )
            .await;
            for album in albums.into_iter().map(|ranked| ranked.album) {
                let Some(group) = normalize(Some(&album.release_group_mbid)) else {
                    continue;
                };
                if excluded.contains(&group) || !seen.insert(group.clone()) {
                    continue;
                }
                pool.push(album_card(
                    &group,
                    &album.title,
                    &album.artist_name,
                    &artist_mbid,
                ));
            }
        }
        pool
    }))
    .await
}

/// One pool per seed from Last.fm: the top `albums_per` albums of each of
/// up to `similar_limit` similar artists, matched to release groups, at
/// most `per_seed` a seed. The similar artists are read as
/// [`similar_to_each`] reads them.
pub(in crate::discover) async fn lastfm_pools(
    ctx: &BuildContext<'_>,
    user_id: &str,
    seeds: &[ArtistRow],
    excluded: &HashSet<String>,
    similar_limit: usize,
    albums_per: usize,
    per_seed: usize,
) -> Vec<Vec<AlbumCard>> {
    let lists = similar_to_each(ctx, user_id, seeds, similar_limit).await;
    join_all(seeds.iter().zip(lists).map(|(seed, similar)| async move {
        if seed.mbid.is_none() {
            return Vec::new();
        }
        let similar: Vec<ArtistRow> = similar
            .into_iter()
            .map(|scored| scored.artist)
            .filter(|artist| {
                normalize(artist.mbid.as_deref()).is_some()
                    && !is_various_artists(artist.mbid.as_deref())
            })
            .collect();
        let albums = join_all(similar.iter().map(|artist| {
            ctx.listening
                .lastfm_artist_albums(user_id, artist, albums_per as u32, &ctx.ask)
        }))
        .await;
        let pairs: Vec<(ArtistRow, Vec<LastFmAlbum>)> = similar
            .into_iter()
            .zip(albums)
            .map(|(artist, result)| (artist, or_empty("lastfm artist albums", result)))
            .collect();
        lastfm_cards(ctx, pairs, excluded, per_seed).await
    }))
    .await
}

/// Turn Last.fm albums into cards, matching each to its release group.
/// Albums that do not match yet are left out: their release ids are not
/// release group ids, and would break covers and ignores for those cards.
async fn lastfm_cards(
    ctx: &BuildContext<'_>,
    pairs: Vec<(ArtistRow, Vec<LastFmAlbum>)>,
    excluded: &HashSet<String>,
    target: usize,
) -> Vec<AlbumCard> {
    let releases: Vec<String> = pairs
        .iter()
        .flat_map(|(_, albums)| albums.iter())
        .filter_map(|album| album.mbid.clone())
        .collect();
    let groups = resolve_release_groups(ctx, &releases).await;
    let mut seen: HashSet<String> = excluded.clone();
    let mut items = Vec::new();
    'artists: for (artist, albums) in pairs {
        let artist_mbid = normalize(artist.mbid.as_deref()).unwrap_or_default();
        for album in albums {
            if items.len() >= target {
                break 'artists;
            }
            let Some(release) = normalize(album.mbid.as_deref()) else {
                continue;
            };
            let Some(group) = groups.get(&release) else {
                continue;
            };
            if !seen.insert(group.to_ascii_lowercase()) {
                continue;
            }
            items.push(album_card(group, &album.name, &artist.name, &artist_mbid));
        }
    }
    items
}

/// Match Last.fm album ids to release groups: remembered answers first,
/// then at most `lastfm_mbid_max_lookups` MusicBrainz lookups, within what
/// the build has left of its budget, whose answers (misses included) are
/// remembered one by one, so a caller's bound cutting the lookups off keeps
/// the ones made. An id that is not a release is tried as a release group
/// itself, which costs a second call. Matching runs on an ask of its own: a
/// lookup that failed or was cut short is simply tried by a later build, and
/// does not mark the page incomplete.
pub async fn resolve_release_groups(
    ctx: &BuildContext<'_>,
    release_mbids: &[String],
) -> HashMap<String, String> {
    let mut wanted: Vec<String> = release_mbids
        .iter()
        .filter_map(|mbid| normalize(Some(mbid)))
        .collect();
    wanted.sort();
    wanted.dedup();
    let mut resolved = HashMap::new();
    if wanted.is_empty() {
        return resolved;
    }
    let ask = ctx.ask.watched();
    let known = ctx.db.resolutions(&wanted).await.unwrap_or_else(|cause| {
        tracing::warn!(%cause, "release group answers unreadable; looking them up again");
        HashMap::new()
    });
    let mut pending = Vec::new();
    for release in wanted {
        match known.get(&release) {
            Some(Some(group)) => {
                resolved.insert(release, group.clone());
            }
            Some(None) => {}
            None => pending.push(release),
        }
    }
    for release in pending
        .into_iter()
        .take(ctx.settings.lastfm_mbid_max_lookups)
    {
        if !ask.take_lookup() {
            break;
        }
        let group = match ctx.listening.release_group_of(&release, &ask).await {
            Ok(Some(group)) => Some(group.to_ascii_lowercase()),
            Ok(None) if is_valid_mbid(&release) => {
                if !ask.take_lookup() {
                    break;
                }
                match ctx.listening.release_group(&release, &ask).await {
                    Ok(Some(_)) => Some(release.clone()),
                    Ok(None) => None,
                    Err(cause) => {
                        tracing::debug!(%cause, "release group check failed; trying again later");
                        continue;
                    }
                }
            }
            Ok(None) => None,
            Err(cause) => {
                tracing::debug!(%cause, "release lookup failed; trying again later");
                continue;
            }
        };
        if let Some(group) = &group {
            resolved.insert(release.clone(), group.clone());
        }
        ctx.db.save_resolution(release, group).await;
    }
    resolved
}
