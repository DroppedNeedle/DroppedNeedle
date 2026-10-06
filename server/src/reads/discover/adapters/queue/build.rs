//! Building one deck (v2 `DiscoverQueueService.build_queue` and its
//! strategies).
//!
//! A user with a linked listening service gets a personalised deck: seed
//! artists from their listening, then candidate pools (similar artists,
//! their genres, fresh releases, artists they love, deep cuts from their
//! top artists) picked round-robin, with trending wildcards mixed in at
//! fixed slots and used to top the deck up. Without a service the deck is
//! trending albums. Ignored releases never appear, and albums the library
//! already holds are dropped at the end.
//!
//! Every provider read is allowed to fail: the failure is logged and that
//! pool is simply empty.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use futures_util::future::join_all;

use super::QueueSettings;
use super::select::{self, Shuffler};
use super::sources::{
    AlbumRow, ArtistRow, JellyfinList, LastFmAlbum, MusicSource, QueueSources, SourceResult,
    StatsRange, UserMusic,
};
use super::store::QueueDb;
use crate::providers::musicbrainz::is_valid_mbid;
use crate::reads::discover::models::QueueItemLight;

/// The "Various Artists" placeholder; never a real recommendation.
pub const VARIOUS_ARTISTS_MBID: &str = "89ad4ac3-39f7-470e-963a-56509c546377";
/// How long one ListenBrainz similar-artist or album read may take.
const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// Decade tags searched when no trending source answers.
const DECADE_TAGS: [&str; 6] = ["2020s", "2010s", "2000s", "1990s", "1980s", "1970s"];

/// What one build reads from.
pub struct BuildContext<'a> {
    /// Provider reads.
    pub sources: &'a dyn QueueSources,
    /// The queue tables.
    pub db: &'a QueueDb,
    /// Settings read at the start of this build.
    pub settings: &'a QueueSettings,
}

/// The cover URL a card shows.
pub fn cover_url(release_group_mbid: &str) -> String {
    format!("/api/v3/covers/release-group/{release_group_mbid}?size=500")
}

fn card(
    release_group_mbid: &str,
    album_name: &str,
    artist_name: &str,
    artist_mbid: &str,
    reason: &str,
    is_wildcard: bool,
) -> QueueItemLight {
    QueueItemLight {
        release_group_mbid: release_group_mbid.to_owned(),
        album_name: album_name.to_owned(),
        artist_name: artist_name.to_owned(),
        artist_mbid: artist_mbid.to_owned(),
        recommendation_reason: reason.to_owned(),
        cover_url: Some(cover_url(release_group_mbid)),
        is_wildcard,
        in_library: false,
    }
}

/// Trim and lowercase an id; blank reads as `None`.
fn normalize(mbid: Option<&str>) -> Option<String> {
    mbid.map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
}

fn is_various_artists(mbid: Option<&str>) -> bool {
    normalize(mbid).as_deref() == Some(VARIOUS_ARTISTS_MBID)
}

/// Log a failed read and carry on with an empty answer.
fn or_empty<T: Default>(what: &str, result: SourceResult<T>) -> T {
    result.unwrap_or_else(|cause| {
        tracing::warn!(read = what, %cause, "discover queue read failed; skipping it");
        T::default()
    })
}

/// Run one read under the per-call timeout.
async fn timed<T: Default>(
    what: &str,
    read: impl std::future::Future<Output = SourceResult<T>>,
) -> T {
    match tokio::time::timeout(READ_TIMEOUT, read).await {
        Ok(result) => or_empty(what, result),
        Err(_) => {
            tracing::warn!(read = what, "discover queue read timed out; skipping it");
            T::default()
        }
    }
}

/// Build one deck of up to `count` cards for the user. Fails only when
/// the library check fails, because a deck full of albums the user
/// already owns would be wrong.
pub async fn build_deck(
    ctx: &BuildContext<'_>,
    user_id: &str,
    count: usize,
) -> Result<Vec<QueueItemLight>, String> {
    let user = ctx.sources.user_music(user_id).await;
    let source = user.resolved_source();
    let ignored = ctx.db.ignored_mbids(user_id).await.unwrap_or_else(|cause| {
        tracing::warn!(%cause, "ignored releases unreadable; the deck may repeat them");
        HashSet::new()
    });
    let mut shuffler = Shuffler::random();
    let has_services = user.listenbrainz.is_some()
        || user.jellyfin
        || (user.lastfm && user.lastfm_username.is_some());
    let mut items = if has_services {
        personalized(ctx, user_id, &user, source, count, &ignored, &mut shuffler).await
    } else {
        anonymous(ctx, user_id, &user, source, count, &ignored, &mut shuffler).await
    };
    let mut owned = owned_of(ctx, &items).await?;
    items.retain(|item| !owned.contains(&item.release_group_mbid.to_ascii_lowercase()));
    // v2 stopped here, so owning a few picks shrank the deck. One more
    // trending pass fills the gap, skipping everything seen so far.
    if items.len() < count {
        let mut skip = ignored.clone();
        skip.extend(owned.iter().cloned());
        let seen = lower_ids(&items);
        let more = trending_filler(
            ctx,
            user_id,
            &user,
            source,
            count - items.len(),
            &skip,
            &seen,
            &mut shuffler,
        )
        .await;
        owned = owned_of(ctx, &more).await?;
        items.extend(
            more.into_iter()
                .filter(|item| !owned.contains(&item.release_group_mbid.to_ascii_lowercase())),
        );
    }
    items.truncate(count);
    Ok(items)
}

async fn owned_of(
    ctx: &BuildContext<'_>,
    items: &[QueueItemLight],
) -> Result<HashSet<String>, String> {
    let ids: Vec<String> = items
        .iter()
        .map(|item| item.release_group_mbid.clone())
        .collect();
    if ids.is_empty() {
        return Ok(HashSet::new());
    }
    ctx.db.owned(&ids).await
}

fn lower_ids(items: &[QueueItemLight]) -> HashSet<String> {
    items
        .iter()
        .map(|item| item.release_group_mbid.to_ascii_lowercase())
        .collect()
}

/// Seed artists from the user's listening: Last.fm top artists for a
/// Last.fm user, else ListenBrainz top artists over widening windows,
/// else Jellyfin's most played and favorite artists.
async fn seed_artists(
    ctx: &BuildContext<'_>,
    user_id: &str,
    user: &UserMusic,
    source: MusicSource,
) -> Vec<ArtistRow> {
    let wanted = ctx.settings.seed_artists.max(1);
    let mut seeds: Vec<ArtistRow> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut take = |rows: Vec<ArtistRow>, seeds: &mut Vec<ArtistRow>| {
        for row in rows {
            if seeds.len() >= wanted {
                break;
            }
            if let Some(mbid) = normalize(row.mbid.as_deref())
                && seen.insert(mbid.clone())
            {
                seeds.push(ArtistRow {
                    mbid: Some(mbid),
                    ..row
                });
            }
        }
    };
    if source == MusicSource::LastFm
        && user.lastfm
        && let Some(username) = &user.lastfm_username
    {
        let rows = or_empty(
            "lastfm top artists",
            ctx.sources
                .lastfm_top_artists(user_id, username, 10.max(wanted as u32))
                .await,
        );
        take(rows, &mut seeds);
    }
    if source == MusicSource::LastFm {
        return seeds;
    }
    if let Some(username) = &user.listenbrainz {
        // Recent listening first, wider windows for quiet but real history.
        for range in [
            StatsRange::ThisWeek,
            StatsRange::ThisMonth,
            StatsRange::ThisYear,
            StatsRange::AllTime,
        ] {
            if seeds.len() >= wanted {
                break;
            }
            let rows = or_empty(
                "listenbrainz top artists",
                ctx.sources
                    .listenbrainz_top_artists(username, range, 10.max(wanted as u32))
                    .await,
            );
            take(rows, &mut seeds);
        }
    }
    if user.jellyfin {
        for list in [JellyfinList::MostPlayed, JellyfinList::Favorites] {
            if seeds.len() >= wanted {
                break;
            }
            let rows = or_empty(
                "jellyfin artists",
                ctx.sources
                    .jellyfin_artists(user_id, list, 10.max(wanted as u32))
                    .await,
            );
            take(rows, &mut seeds);
        }
    }
    seeds
}

/// Release groups the user has played, so deep cuts skip them.
async fn listened_albums(
    ctx: &BuildContext<'_>,
    user: &UserMusic,
    source: MusicSource,
) -> HashSet<String> {
    let Some(username) = user.listenbrainz.as_deref() else {
        return HashSet::new();
    };
    if source != MusicSource::ListenBrainz {
        return HashSet::new();
    }
    let rows = or_empty(
        "listenbrainz listened albums",
        ctx.sources
            .listenbrainz_top_albums(username, StatsRange::AllTime, 100)
            .await,
    );
    rows.into_iter()
        .map(|row| row.release_group_mbid.to_ascii_lowercase())
        .collect()
}

#[allow(clippy::too_many_arguments)]
async fn personalized(
    ctx: &BuildContext<'_>,
    user_id: &str,
    user: &UserMusic,
    source: MusicSource,
    count: usize,
    ignored: &HashSet<String>,
    shuffler: &mut Shuffler,
) -> Vec<QueueItemLight> {
    let seeds = seed_artists(ctx, user_id, user, source).await;
    if seeds.is_empty() {
        return anonymous(ctx, user_id, user, source, count, ignored, shuffler).await;
    }
    let settings = ctx.settings;
    // During a ListenBrainz popularity outage, Last.fm stands in for the
    // album reads even for a ListenBrainz-first user.
    let use_lastfm = (source == MusicSource::LastFm || ctx.sources.listenbrainz_popularity_down())
        && user.lastfm;
    let wildcard_slots = settings.wildcard_slots;
    let target = count.saturating_sub(wildcard_slots);
    let per_seed = (target / seeds.len().max(1) + 3).max(4);
    let pools = if use_lastfm {
        lastfm_pools(ctx, user_id, &seeds, ignored, per_seed).await
    } else {
        let listened = listened_albums(ctx, user, source).await;
        listenbrainz_pools(ctx, user_id, user, &seeds, ignored, &listened).await
    };
    let picked = select::round_robin(pools, target, select::MAX_PER_ARTIST, shuffler);
    let seen = lower_ids(&picked);
    let wildcard_count = wildcard_slots.max(count.saturating_sub(picked.len()));
    let wildcards = trending_filler(
        ctx,
        user_id,
        user,
        source,
        wildcard_count,
        ignored,
        &seen,
        shuffler,
    )
    .await;
    let mut deck = select::interleave(picked, wildcards, &select::WILDCARD_POSITIONS);
    if deck.len() < count {
        let seen = lower_ids(&deck);
        let more = trending_filler(
            ctx,
            user_id,
            user,
            source,
            count - deck.len(),
            ignored,
            &seen,
            shuffler,
        )
        .await;
        deck.extend(more);
    }
    deck.truncate(count);
    deck
}

/// The ListenBrainz candidate pools: one per seed of similar artists'
/// albums, then genres, fresh releases, loved artists and deep cuts.
async fn listenbrainz_pools(
    ctx: &BuildContext<'_>,
    user_id: &str,
    user: &UserMusic,
    seeds: &[ArtistRow],
    excluded: &HashSet<String>,
    listened: &HashSet<String>,
) -> Vec<Vec<QueueItemLight>> {
    let username = user.listenbrainz.as_deref().unwrap_or("");
    let per_artist = ctx.settings.albums_per_similar;
    let mut deep_excluded = excluded.clone();
    deep_excluded.extend(listened.iter().cloned());
    let (similar, genres, fresh, loved, deep) = tokio::join!(
        similar_artist_pools(ctx, user_id, seeds, excluded),
        genre_albums(ctx, username, excluded),
        fresh_releases(ctx, username, excluded),
        loved_artist_albums(ctx, user_id, username, excluded, per_artist),
        deep_cuts(ctx, user_id, username, &deep_excluded, listened, per_artist),
    );
    let mut pools = similar;
    pools.extend(
        [genres, fresh, loved, deep]
            .into_iter()
            .filter(|pool| !pool.is_empty()),
    );
    pools
}

/// One pool per seed: the top albums of each artist ListenBrainz calls
/// similar to it.
async fn similar_artist_pools(
    ctx: &BuildContext<'_>,
    user_id: &str,
    seeds: &[ArtistRow],
    excluded: &HashSet<String>,
) -> Vec<Vec<QueueItemLight>> {
    let settings = ctx.settings;
    join_all(seeds.iter().map(|seed| async move {
        let Some(seed_mbid) = seed.mbid.as_deref() else {
            return Vec::new();
        };
        let similar = timed(
            "listenbrainz similar artists",
            ctx.sources.listenbrainz_similar_artists(
                user_id,
                seed_mbid,
                settings.similar_artists_limit,
            ),
        )
        .await;
        let reason = format!("Similar to {}", seed.name);
        let mut pool = Vec::new();
        let mut seen = HashSet::new();
        for artist in similar {
            let Some(artist_mbid) = normalize(artist.mbid.as_deref()) else {
                continue;
            };
            if artist_mbid == VARIOUS_ARTISTS_MBID {
                continue;
            }
            let albums = timed(
                "listenbrainz artist albums",
                ctx.sources.listenbrainz_artist_albums(
                    user_id,
                    &artist_mbid,
                    settings.albums_per_similar,
                ),
            )
            .await;
            for album in albums {
                let Some(group) = normalize(Some(&album.release_group_mbid)) else {
                    continue;
                };
                if excluded.contains(&group) || !seen.insert(group.clone()) {
                    continue;
                }
                pool.push(card(
                    &group,
                    &album.title,
                    &album.artist_name,
                    &artist_mbid,
                    &reason,
                    false,
                ));
            }
        }
        pool
    }))
    .await
}

/// Albums tagged with the user's top genres (v2 `discover_by_genres`).
async fn genre_albums(
    ctx: &BuildContext<'_>,
    username: &str,
    excluded: &HashSet<String>,
) -> Vec<QueueItemLight> {
    if username.is_empty() {
        return Vec::new();
    }
    let genres = or_empty(
        "listenbrainz genres",
        ctx.sources.listenbrainz_genres(username).await,
    );
    let top: Vec<String> = genres.into_iter().take(4).collect();
    let found = join_all(
        top.iter()
            .map(|genre| ctx.sources.musicbrainz_tag_albums(genre, 8)),
    )
    .await;
    let mut items = Vec::new();
    let mut seen = HashSet::new();
    for (genre, result) in top.iter().zip(found) {
        let reason = format!("Because you listen to {genre}");
        for album in or_empty("musicbrainz tag search", result) {
            let Some(group) = normalize(Some(&album.release_group_mbid)) else {
                continue;
            };
            if excluded.contains(&group) || !seen.insert(group.clone()) {
                continue;
            }
            // v2 put the release-group id in the artist slot here; the
            // credited artist is the right one.
            let artist_mbid = album.artist_mbid.clone().unwrap_or_default();
            items.push(card(
                &group,
                &album.title,
                &album.artist_name,
                &artist_mbid,
                &reason,
                false,
            ));
        }
    }
    items
}

/// The user's ListenBrainz fresh releases.
async fn fresh_releases(
    ctx: &BuildContext<'_>,
    username: &str,
    excluded: &HashSet<String>,
) -> Vec<QueueItemLight> {
    if username.is_empty() {
        return Vec::new();
    }
    let rows = or_empty(
        "listenbrainz fresh releases",
        ctx.sources.listenbrainz_fresh_releases(username).await,
    );
    let mut items = Vec::new();
    let mut seen = HashSet::new();
    for album in rows {
        let Some(group) = normalize(Some(&album.release_group_mbid)) else {
            continue;
        };
        if excluded.contains(&group) || !seen.insert(group.clone()) {
            continue;
        }
        let artist_mbid = normalize(album.artist_mbid.as_deref()).unwrap_or_default();
        items.push(card(
            &group,
            &album.title,
            &album.artist_name,
            &artist_mbid,
            "New release for you",
            false,
        ));
    }
    items
}

/// Top albums of up to six artists behind the user's loved recordings.
async fn loved_artist_albums(
    ctx: &BuildContext<'_>,
    user_id: &str,
    username: &str,
    excluded: &HashSet<String>,
    per_artist: usize,
) -> Vec<QueueItemLight> {
    if username.is_empty() {
        return Vec::new();
    }
    let loved = or_empty(
        "listenbrainz loved recordings",
        ctx.sources.listenbrainz_loved_artists(username, 50).await,
    );
    let mut artists: Vec<String> = Vec::new();
    for mbid in loved {
        if let Some(mbid) = normalize(Some(&mbid))
            && !artists.contains(&mbid)
        {
            artists.push(mbid);
        }
        if artists.len() >= 6 {
            break;
        }
    }
    let found = join_all(artists.iter().map(|artist| {
        ctx.sources
            .listenbrainz_artist_albums(user_id, artist, per_artist)
    }))
    .await;
    let mut items = Vec::new();
    let mut seen = HashSet::new();
    for (artist, result) in artists.iter().zip(found) {
        for album in or_empty("listenbrainz artist albums", result) {
            let Some(group) = normalize(Some(&album.release_group_mbid)) else {
                continue;
            };
            if excluded.contains(&group) || !seen.insert(group.clone()) {
                continue;
            }
            items.push(card(
                &group,
                &album.title,
                &album.artist_name,
                artist,
                "From an artist you love",
                false,
            ));
        }
    }
    items
}

/// Lesser-played albums by the user's top artists this month (v2
/// `get_artist_deep_cuts`).
async fn deep_cuts(
    ctx: &BuildContext<'_>,
    user_id: &str,
    username: &str,
    excluded: &HashSet<String>,
    listened: &HashSet<String>,
    per_artist: usize,
) -> Vec<QueueItemLight> {
    if username.is_empty() {
        return Vec::new();
    }
    let top = or_empty(
        "listenbrainz top albums",
        ctx.sources
            .listenbrainz_top_albums(username, StatsRange::ThisMonth, 25)
            .await,
    );
    let current: HashSet<String> = top
        .iter()
        .map(|album| album.release_group_mbid.to_ascii_lowercase())
        .collect();
    let mut artists: Vec<(String, String)> = Vec::new();
    for album in &top {
        if let Some(mbid) = normalize(album.artist_mbid.as_deref())
            && !artists.iter().any(|(known, _)| *known == mbid)
        {
            artists.push((mbid, album.artist_name.clone()));
        }
        if artists.len() >= 6 {
            break;
        }
    }
    let wanted = (per_artist + 2).max(4);
    let found = join_all(artists.iter().map(|(artist, _)| {
        ctx.sources
            .listenbrainz_artist_albums(user_id, artist, wanted)
    }))
    .await;
    let mut items = Vec::new();
    let mut seen = HashSet::new();
    for ((artist, name), result) in artists.iter().zip(found) {
        for album in or_empty("listenbrainz artist albums", result) {
            let Some(group) = normalize(Some(&album.release_group_mbid)) else {
                continue;
            };
            if current.contains(&group)
                || listened.contains(&group)
                || excluded.contains(&group)
                || !seen.insert(group.clone())
            {
                continue;
            }
            let source_name = if name.is_empty() {
                &album.artist_name
            } else {
                name
            };
            items.push(card(
                &group,
                &album.title,
                &album.artist_name,
                artist,
                &format!("More from {source_name}"),
                false,
            ));
        }
    }
    items
}

/// One pool per seed from Last.fm: similar artists' top albums, matched
/// to release groups.
async fn lastfm_pools(
    ctx: &BuildContext<'_>,
    user_id: &str,
    seeds: &[ArtistRow],
    excluded: &HashSet<String>,
    per_seed: usize,
) -> Vec<Vec<QueueItemLight>> {
    let settings = ctx.settings;
    join_all(seeds.iter().map(|seed| async move {
        if seed.mbid.is_none() {
            return Vec::new();
        }
        let similar = or_empty(
            "lastfm similar artists",
            ctx.sources
                .lastfm_similar_artists(user_id, seed, settings.similar_artists_limit as u32)
                .await,
        );
        let similar: Vec<ArtistRow> = similar
            .into_iter()
            .filter(|artist| {
                normalize(artist.mbid.as_deref()).is_some()
                    && !is_various_artists(artist.mbid.as_deref())
            })
            .collect();
        let albums = join_all(similar.iter().map(|artist| {
            ctx.sources
                .lastfm_artist_albums(user_id, artist, settings.albums_per_similar as u32)
        }))
        .await;
        let pairs: Vec<(ArtistRow, Vec<LastFmAlbum>)> = similar
            .into_iter()
            .zip(albums)
            .map(|(artist, result)| (artist, or_empty("lastfm artist albums", result)))
            .collect();
        lastfm_cards(
            ctx,
            pairs,
            excluded,
            per_seed,
            &format!("Similar to {}", seed.name),
            false,
            false,
        )
        .await
    }))
    .await
}

/// Turn Last.fm albums into cards, matching each to its release group
/// (v2 `lastfm_albums_to_queue_items`). Albums that do not match yet are
/// left out; v2 passed their ids through as if they were release groups,
/// which broke covers and ignores for those cards.
async fn lastfm_cards(
    ctx: &BuildContext<'_>,
    pairs: Vec<(ArtistRow, Vec<LastFmAlbum>)>,
    excluded: &HashSet<String>,
    target: usize,
    reason: &str,
    is_wildcard: bool,
    album_artist_names: bool,
) -> Vec<QueueItemLight> {
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
            let artist_name = if album_artist_names && !album.artist_name.is_empty() {
                album.artist_name.as_str()
            } else {
                artist.name.as_str()
            };
            items.push(card(
                group,
                &album.name,
                artist_name,
                &artist_mbid,
                reason,
                is_wildcard,
            ));
        }
    }
    items
}

/// Match Last.fm album ids to release groups: remembered answers first,
/// then at most `lastfm_mbid_max_lookups` MusicBrainz lookups, whose
/// answers (misses included) are remembered. An id that is not a release
/// is tried as a release group itself.
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
    let mut answers = Vec::new();
    for release in pending
        .into_iter()
        .take(ctx.settings.lastfm_mbid_max_lookups)
    {
        let group = match ctx.sources.musicbrainz_release_group_of(&release).await {
            Ok(Some(group)) => Some(group.to_ascii_lowercase()),
            Ok(None) if is_valid_mbid(&release) => {
                match ctx.sources.musicbrainz_release_group(&release).await {
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
        answers.push((release, group));
    }
    ctx.db.save_resolutions(answers).await;
    resolved
}

/// Trending albums to fill wildcard slots and short decks: Last.fm's
/// chart for Last.fm users (or during a ListenBrainz popularity outage),
/// else ListenBrainz's weekly chart, else MusicBrainz decade tags.
#[allow(clippy::too_many_arguments)]
async fn trending_filler(
    ctx: &BuildContext<'_>,
    user_id: &str,
    user: &UserMusic,
    source: MusicSource,
    count: usize,
    ignored: &HashSet<String>,
    seen: &HashSet<String>,
    shuffler: &mut Shuffler,
) -> Vec<QueueItemLight> {
    if count == 0 {
        return Vec::new();
    }
    let mut exclude: HashSet<String> = ignored.union(seen).cloned().collect();
    let use_lastfm = (source == MusicSource::LastFm || ctx.sources.listenbrainz_popularity_down())
        && user.lastfm;
    let target = (count * 2).max(6);
    let mut wildcards = if use_lastfm {
        let mut artists = or_empty(
            "lastfm chart",
            ctx.sources.lastfm_chart_artists(user_id, 15).await,
        );
        shuffler.shuffle(&mut artists);
        let artists: Vec<ArtistRow> = artists
            .into_iter()
            .take(10)
            .filter(|artist| !is_various_artists(artist.mbid.as_deref()))
            .collect();
        lastfm_trending(ctx, user_id, artists, &exclude, target).await
    } else {
        let mut albums = or_empty(
            "listenbrainz trending",
            ctx.sources.listenbrainz_trending(25).await,
        );
        shuffler.shuffle(&mut albums);
        trending_cards(albums, &mut exclude, target, "Trending This Week")
    };
    if wildcards.is_empty() {
        // Needs no popularity endpoint, so it still fills the slots during
        // a ListenBrainz outage for users without Last.fm.
        for tag in DECADE_TAGS {
            if wildcards.len() >= target {
                break;
            }
            let albums = or_empty(
                "musicbrainz decade tags",
                ctx.sources.musicbrainz_tag_albums(tag, 25).await,
            );
            let left = target - wildcards.len();
            wildcards.extend(trending_cards(albums, &mut exclude, left, "Trending"));
        }
    }
    if wildcards.is_empty() {
        tracing::warn!("no trending albums for the discover queue; the deck stays short");
    }
    wildcards.truncate(count);
    wildcards
}

async fn lastfm_trending(
    ctx: &BuildContext<'_>,
    user_id: &str,
    artists: Vec<ArtistRow>,
    exclude: &HashSet<String>,
    target: usize,
) -> Vec<QueueItemLight> {
    let albums = join_all(
        artists
            .iter()
            .map(|artist| ctx.sources.lastfm_artist_albums(user_id, artist, 3)),
    )
    .await;
    let pairs: Vec<(ArtistRow, Vec<LastFmAlbum>)> = artists
        .into_iter()
        .zip(albums)
        .map(|(artist, result)| (artist, or_empty("lastfm artist albums", result)))
        .collect();
    lastfm_cards(
        ctx,
        pairs,
        exclude,
        target,
        "Trending on Last.fm",
        true,
        true,
    )
    .await
}

/// Wildcard cards from chart or tag rows, skipping excluded ids and
/// Various Artists, recording what they take in `exclude`.
fn trending_cards(
    albums: Vec<AlbumRow>,
    exclude: &mut HashSet<String>,
    target: usize,
    reason: &str,
) -> Vec<QueueItemLight> {
    let mut items = Vec::new();
    for album in albums {
        if items.len() >= target {
            break;
        }
        let Some(group) = normalize(Some(&album.release_group_mbid)) else {
            continue;
        };
        if exclude.contains(&group) || is_various_artists(album.artist_mbid.as_deref()) {
            continue;
        }
        let artist_mbid = normalize(album.artist_mbid.as_deref()).unwrap_or_default();
        items.push(card(
            &group,
            &album.title,
            &album.artist_name,
            &artist_mbid,
            reason,
            true,
        ));
        exclude.insert(group);
    }
    items
}

/// A deck without personal listening: trending albums, topped up from the
/// wildcard sources.
async fn anonymous(
    ctx: &BuildContext<'_>,
    user_id: &str,
    user: &UserMusic,
    source: MusicSource,
    count: usize,
    ignored: &HashSet<String>,
    shuffler: &mut Shuffler,
) -> Vec<QueueItemLight> {
    let mut exclude = ignored.clone();
    let mut items = if source == MusicSource::LastFm && user.lastfm {
        let mut artists = or_empty(
            "lastfm chart",
            ctx.sources.lastfm_chart_artists(user_id, 15).await,
        );
        shuffler.shuffle(&mut artists);
        let artists: Vec<ArtistRow> = artists
            .into_iter()
            .filter(|artist| !is_various_artists(artist.mbid.as_deref()))
            .collect();
        lastfm_trending(ctx, user_id, artists, &exclude, count).await
    } else {
        let mut albums = or_empty(
            "listenbrainz trending",
            ctx.sources.listenbrainz_trending(50).await,
        );
        shuffler.shuffle(&mut albums);
        trending_cards(albums, &mut exclude, count, "Trending This Week")
    };
    if items.len() < count {
        let seen = lower_ids(&items);
        let more = trending_filler(
            ctx,
            user_id,
            user,
            source,
            count - items.len(),
            ignored,
            &seen,
            shuffler,
        )
        .await;
        items.extend(more);
    }
    items.truncate(count);
    items
}
