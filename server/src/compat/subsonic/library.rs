//! Library handlers: playlists, favorites, scrobble, queues,
//! bookmarks, info, lyrics, genres, user, scan, discovery.
//! v2: the Subsonic router's endpoint functions.

use super::auth::Principal;
use super::browse::validate_music_folder;
use super::error::{NOT_AUTHORIZED, NOT_FOUND, SubsonicError};
use super::ids::{IdKind, decode, decode_expect, encode};
use super::models::{
    Render, SAlbumInfo, SArtistInfo, SBookmark, SLyrics, SLyricsLine, SPlayQueue,
    SPlayQueueByIndex, SPlaylist, SScanStatus, SStructuredLyrics, SUser,
};
use super::params::SubsonicParameters;
use super::store::{PlaylistDetail, Store};
use super::stream::AudioBackend;
use super::value::{Val, obj};
use super::{Ctx, Outcome};

/// Max play-queue items (v2 `_MAX_QUEUE_ITEMS`).
pub const MAX_QUEUE_ITEMS: usize = 500;
/// Max media position ms (v2 `_MAX_MEDIA_POSITION_MS`).
pub const MAX_MEDIA_POSITION_MS: i64 = 604_800_000;

/// Kinds star/setRating accept (v2 `_FAV_KINDS`).
pub const FAVORITE_KINDS: &[IdKind] = &[IdKind::Artist, IdKind::Album, IdKind::Track];

/// Playlist detail with streamable entries only: legacy/outbound
/// entries and dead file links are skipped, and the count matches what
/// is served (#181).
pub async fn playlist_detail<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
    detail: &PlaylistDetail,
) -> Result<SPlaylist, SubsonicError> {
    let mut songs = Vec::new();
    let mut total = 0i64;
    for entry in &detail.tracks {
        let Some(file_id) = entry.library_file_id.as_deref() else {
            continue;
        };
        let Some(track) = ctx
            .store
            .get_track(file_id)
            .await
            .map_err(Ctx::<P, S, B>::store_err)?
        else {
            continue;
        };
        total += track.duration_seconds.round() as i64;
        songs.push(ctx.child(&track));
    }
    let record = &detail.record;
    let owner = if detail.is_owner {
        ctx.user()?.username().to_owned()
    } else {
        detail.owner_name.clone()
    };
    Ok(SPlaylist {
        id: encode(IdKind::Playlist, &record.id),
        name: record.name.clone(),
        comment: None,
        owner: Some(owner),
        public: Some(record.is_public),
        songCount: songs.len() as i64,
        duration: Some(total),
        created: record.created_at.clone(),
        changed: record.changed_at.clone(),
        coverArt: record
            .has_cover
            .then(|| encode(IdKind::Playlist, &record.id)),
        entry: Some(songs),
    })
}

/// Playlist list with streamable-only counts (#181); redacted private
/// rows are skipped.
pub async fn playlists<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Outcome, SubsonicError> {
    let user_id = ctx.user()?.user_id().to_owned();
    let views = ctx
        .store
        .get_all_playlists(&user_id)
        .await
        .map_err(Ctx::<P, S, B>::store_err)?;
    let streamable = ctx
        .store
        .get_streamable_counts()
        .await
        .map_err(Ctx::<P, S, B>::store_err)?;
    let mut out = Vec::new();
    for view in &views {
        let Some(record) = view.record.as_ref() else {
            continue;
        };
        let owner = if view.is_owner {
            ctx.user()?.username().to_owned()
        } else {
            view.owner_name.clone()
        };
        let (song_count, duration) = streamable.get(&record.id).copied().unwrap_or((0, 0));
        out.push(
            SPlaylist {
                id: encode(IdKind::Playlist, &record.id),
                name: record.name.clone(),
                comment: None,
                owner: Some(owner),
                public: Some(record.is_public),
                songCount: song_count,
                duration: Some(duration),
                created: record.created_at.clone(),
                changed: record.changed_at.clone(),
                coverArt: record
                    .has_cover
                    .then(|| encode(IdKind::Playlist, &record.id)),
                entry: None,
            }
            .render(),
        );
    }
    Ok(Outcome::keyed(
        "playlists",
        obj(vec![("playlist", Val::List(out))]),
    ))
}

/// Playlist detail.
pub async fn playlist<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Outcome, SubsonicError> {
    let pid = decode_expect(&ctx.p("id")?.unwrap_or_default(), IdKind::Playlist)?;
    let user_id = ctx.user()?.user_id().to_owned();
    let detail = ctx
        .store
        .get_playlist_with_tracks(&pid, &user_id)
        .await
        .map_err(Ctx::<P, S, B>::store_err)?
        .ok_or_else(|| SubsonicError::new(NOT_FOUND, "Playlist not found"))?;
    Ok(Outcome::keyed(
        "playlist",
        playlist_detail(ctx, &detail).await?.render(),
    ))
}

/// Create a playlist, or replace contents when `playlistId` is given.
pub async fn create_playlist<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Outcome, SubsonicError> {
    let user_id = ctx.user()?.user_id().to_owned();
    let song_file_ids = ctx
        .plist("songId")?
        .iter()
        .map(|sid| decode_expect(sid, IdKind::Track))
        .collect::<Result<Vec<_>, _>>()?;
    let pid = match ctx.p("playlistId")? {
        Some(playlist_id) => {
            let pid = decode_expect(&playlist_id, IdKind::Playlist)?;
            let existing = ctx
                .store
                .get_playlist_tracks(&pid)
                .await
                .map_err(Ctx::<P, S, B>::store_err)?;
            if !existing.is_empty() {
                let entry_ids = existing
                    .iter()
                    .map(|entry| entry.id.clone())
                    .collect::<Vec<_>>();
                ctx.store
                    .remove_playlist_tracks(&pid, &user_id, &entry_ids)
                    .await
                    .map_err(Ctx::<P, S, B>::store_err)?;
            }
            if let Some(name) = ctx.p("name")? {
                ctx.store
                    .update_playlist(&pid, &user_id, &name)
                    .await
                    .map_err(Ctx::<P, S, B>::store_err)?;
            }
            pid
        }
        None => {
            let name = ctx.p("name")?.unwrap_or_default();
            if name.is_empty() {
                return Err(SubsonicError::missing("name"));
            }
            ctx.store
                .create_playlist(&name, &user_id)
                .await
                .map_err(Ctx::<P, S, B>::store_err)?
                .id
        }
    };
    for fid in &song_file_ids {
        ctx.store
            .add_playlist_file(&pid, fid, &user_id)
            .await
            .map_err(Ctx::<P, S, B>::store_err)?;
    }
    let detail = ctx
        .store
        .get_playlist_with_tracks(&pid, &user_id)
        .await
        .map_err(Ctx::<P, S, B>::store_err)?
        .ok_or_else(|| SubsonicError::new(NOT_FOUND, "Playlist not found"))?;
    Ok(Outcome::keyed(
        "playlist",
        playlist_detail(ctx, &detail).await?.render(),
    ))
}

/// Update a playlist: rename, visibility, remove-by-index, append.
pub async fn update_playlist<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Outcome, SubsonicError> {
    let pid = decode_expect(&ctx.p("playlistId")?.unwrap_or_default(), IdKind::Playlist)?;
    let user_id = ctx.user()?.user_id().to_owned();
    let add_file_ids = ctx
        .plist("songIdToAdd")?
        .iter()
        .map(|sid| decode_expect(sid, IdKind::Track))
        .collect::<Result<Vec<_>, _>>()?;
    let mut remove_indices = Vec::new();
    for value in ctx.plist("songIndexToRemove")? {
        let single = SubsonicParameters::new(vec![("songIndexToRemove".to_owned(), value)]);
        remove_indices.push(single.integer(
            "songIndexToRemove",
            None,
            Some(0),
            Some(2_147_483_647),
        )?);
    }
    if let Some(name) = ctx.p("name")? {
        ctx.store
            .update_playlist(&pid, &user_id, &name)
            .await
            .map_err(Ctx::<P, S, B>::store_err)?;
    }
    if let Some(public) = ctx.p("public")? {
        let single = SubsonicParameters::new(vec![("public".to_owned(), public)]);
        let is_public = single.boolean("public", false)?;
        ctx.store
            .set_playlist_public(&pid, &user_id, is_public)
            .await
            .map_err(Ctx::<P, S, B>::store_err)?;
    }
    if !remove_indices.is_empty() {
        let entries = ctx
            .store
            .get_playlist_tracks(&pid)
            .await
            .map_err(Ctx::<P, S, B>::store_err)?;
        let ids = remove_indices
            .into_iter()
            .flatten()
            .filter_map(|index| entries.get(index as usize).map(|entry| entry.id.clone()))
            .collect::<Vec<_>>();
        if !ids.is_empty() {
            ctx.store
                .remove_playlist_tracks(&pid, &user_id, &ids)
                .await
                .map_err(Ctx::<P, S, B>::store_err)?;
        }
    }
    for fid in &add_file_ids {
        ctx.store
            .add_playlist_file(&pid, fid, &user_id)
            .await
            .map_err(Ctx::<P, S, B>::store_err)?;
    }
    Ok(Outcome::ok())
}

/// Delete a playlist.
pub async fn delete_playlist<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Outcome, SubsonicError> {
    let pid = decode_expect(&ctx.p("id")?.unwrap_or_default(), IdKind::Playlist)?;
    let user_id = ctx.user()?.user_id().to_owned();
    ctx.store
        .delete_playlist(&pid, &user_id)
        .await
        .map_err(Ctx::<P, S, B>::store_err)?;
    Ok(Outcome::ok())
}

/// (kind, internal id) pairs from `id` (prefix-routed) + `albumId` +
/// `artistId`, deduped. Non-favorite `id` prefixes are ignored.
pub fn collect_star_targets(
    ctx: &Ctx<impl Principal, impl Store, impl AudioBackend>,
) -> Result<Vec<(IdKind, String)>, SubsonicError> {
    let mut targets = Vec::new();
    for sid in ctx.plist("id")? {
        let (kind, internal) = decode(&sid)?;
        if FAVORITE_KINDS.contains(&kind) {
            targets.push((kind, internal));
        }
    }
    for sid in ctx.plist("albumId")? {
        targets.push((IdKind::Album, decode_expect(&sid, IdKind::Album)?));
    }
    for sid in ctx.plist("artistId")? {
        targets.push((IdKind::Artist, decode_expect(&sid, IdKind::Artist)?));
    }
    targets.dedup();
    Ok(targets)
}

/// Validated star targets: empty -> 10, unknown target -> 70.
pub async fn validated_star_targets<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Vec<(IdKind, String)>, SubsonicError> {
    let targets = collect_star_targets(ctx)?;
    if targets.is_empty() {
        return Err(SubsonicError::new(
            10,
            "At least one favorite target is required",
        ));
    }
    let missing = ctx
        .store
        .missing_targets(&targets)
        .await
        .map_err(Ctx::<P, S, B>::store_err)?;
    if !missing.is_empty() {
        return Err(SubsonicError::new(NOT_FOUND, "Favorite target not found"));
    }
    Ok(targets)
}

/// Star (`add=true`) or unstar.
pub async fn star<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
    add: bool,
) -> Result<Outcome, SubsonicError> {
    let targets = validated_star_targets(ctx).await?;
    let user_id = ctx.user()?.user_id().to_owned();
    ctx.store
        .apply_favorites(&user_id, &targets, add)
        .await
        .map_err(Ctx::<P, S, B>::store_err)?;
    Ok(Outcome::ok())
}

/// Starred artists/albums/songs (dead links skipped).
pub async fn starred_lists<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<
    (
        Vec<super::views::ViewArtist>,
        Vec<super::views::ViewAlbum>,
        Vec<super::views::ViewTrack>,
    ),
    SubsonicError,
> {
    let user_id = ctx.user()?.user_id().to_owned();
    let mut artists = Vec::new();
    for (mbid, _) in ctx
        .store
        .list_favorites(&user_id, IdKind::Artist)
        .await
        .map_err(Ctx::<P, S, B>::store_err)?
    {
        if let Some((artist, _)) = ctx
            .store
            .get_artist_with_albums(&mbid)
            .await
            .map_err(Ctx::<P, S, B>::store_err)?
        {
            artists.push(artist);
        }
    }
    let mut albums = Vec::new();
    for (rg, _) in ctx
        .store
        .list_favorites(&user_id, IdKind::Album)
        .await
        .map_err(Ctx::<P, S, B>::store_err)?
    {
        if let Some(album) = ctx
            .store
            .get_album(&rg)
            .await
            .map_err(Ctx::<P, S, B>::store_err)?
        {
            albums.push(album);
        }
    }
    let mut songs = Vec::new();
    for (fid, _) in ctx
        .store
        .list_favorites(&user_id, IdKind::Track)
        .await
        .map_err(Ctx::<P, S, B>::store_err)?
    {
        if let Some(track) = ctx
            .store
            .get_track(&fid)
            .await
            .map_err(Ctx::<P, S, B>::store_err)?
        {
            songs.push(track);
        }
    }
    Ok((artists, albums, songs))
}

/// ID3 starred.
pub async fn starred2<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Outcome, SubsonicError> {
    validate_music_folder(&ctx.params)?;
    let (artists, albums, songs) = starred_lists(ctx).await?;
    Ok(Outcome::keyed(
        "starred2",
        obj(vec![
            (
                "artist",
                Val::List(
                    artists
                        .iter()
                        .map(|artist| super::views::to_artist_id3(artist).render())
                        .collect(),
                ),
            ),
            (
                "album",
                Val::List(
                    albums
                        .iter()
                        .map(|album| super::views::to_album_id3(album).render())
                        .collect(),
                ),
            ),
            (
                "song",
                Val::List(
                    songs
                        .iter()
                        .map(|track| ctx.child(track).render())
                        .collect(),
                ),
            ),
        ]),
    ))
}

/// File-structure starred.
pub async fn starred<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Outcome, SubsonicError> {
    validate_music_folder(&ctx.params)?;
    let (artists, albums, songs) = starred_lists(ctx).await?;
    Ok(Outcome::keyed(
        "starred",
        obj(vec![
            (
                "artist",
                Val::List(
                    artists
                        .iter()
                        .map(|artist| super::views::to_artist_file(artist).render())
                        .collect(),
                ),
            ),
            (
                "album",
                Val::List(
                    albums
                        .iter()
                        .map(|album| super::views::to_album_child(album).render())
                        .collect(),
                ),
            ),
            (
                "song",
                Val::List(
                    songs
                        .iter()
                        .map(|track| ctx.child(track).render())
                        .collect(),
                ),
            ),
        ]),
    ))
}

/// Validated no-op: the target and 0-5 range are checked, then
/// nothing is persisted.
pub async fn set_rating<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Outcome, SubsonicError> {
    let rating = ctx.pint("rating", None, Some(0), Some(5))?;
    if rating.is_none() {
        return Err(SubsonicError::missing("rating"));
    }
    let (kind, internal) = decode(&ctx.p("id")?.unwrap_or_default())?;
    let missing = ctx
        .store
        .missing_targets(&[(kind, internal)])
        .await
        .map_err(Ctx::<P, S, B>::store_err)?;
    if !FAVORITE_KINDS.contains(&kind) || !missing.is_empty() {
        return Err(SubsonicError::new(NOT_FOUND, "Rating target not found"));
    }
    Ok(Outcome::ok())
}

/// Scrobble: `time[]` is positionally parallel to `id[]` (a count
/// mismatch is code 0); `submission=false` records now-playing for the
/// first id only.
pub async fn scrobble<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Outcome, SubsonicError> {
    let ids = ctx.plist("id")?;
    if ids.is_empty() {
        return Err(SubsonicError::missing("id"));
    }
    let times = ctx.plist("time")?;
    if !times.is_empty() && times.len() != ids.len() {
        return Err(SubsonicError::new(
            0,
            format!(
                "Wrong number of timestamps: {}, should be {}",
                times.len(),
                ids.len()
            ),
        ));
    }
    let mut timestamps = Vec::new();
    for value in &times {
        let single = SubsonicParameters::new(vec![("time".to_owned(), value.clone())]);
        timestamps.push(single.integer("time", None, Some(0), Some(9_007_199_254_740_991))?);
    }
    let submission = ctx.pbool("submission", true)?;
    let client = ctx.p("c")?;
    let user_id = ctx.user()?.user_id().to_owned();
    let user_name = ctx.user()?.display_name().to_owned();
    if !submission {
        let fid = decode_expect(&ids[0], IdKind::Track)?;
        ctx.store
            .now_playing(&fid, &user_id, client.as_deref(), &user_name)
            .await
            .map_err(Ctx::<P, S, B>::store_err)?;
        return Ok(Outcome::ok());
    }
    for (index, sid) in ids.iter().enumerate() {
        let fid = decode_expect(sid, IdKind::Track)?;
        let played_at = timestamps
            .get(index)
            .copied()
            .flatten()
            .map(|ms| ms as f64 / 1000.0);
        ctx.store
            .scrobble(&fid, &user_id, client.as_deref(), played_at, &user_name)
            .await
            .map_err(Ctx::<P, S, B>::store_err)?;
    }
    Ok(Outcome::ok())
}

/// Presence: song children plus username/minutesAgo/playerId/playerName.
pub async fn now_playing<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Outcome, SubsonicError> {
    let rows = ctx
        .store
        .compat_now_playing()
        .await
        .map_err(Ctx::<P, S, B>::store_err)?;
    let now = ctx.now_unix;
    let mut entries = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let Some(track) = ctx
            .store
            .get_track(&row.file_id)
            .await
            .map_err(Ctx::<P, S, B>::store_err)?
        else {
            continue;
        };
        entries.push(
            super::models::SNowPlayingEntry {
                child: ctx.child(&track),
                username: row.user_name.clone(),
                minutesAgo: ((now - row.updated_at) / 60.0).max(0.0) as i64,
                playerId: index as i64,
                playerName: row.source.clone().or_else(|| row.device_name.clone()),
            }
            .render(),
        );
    }
    Ok(Outcome::keyed(
        "nowPlaying",
        obj(vec![("entry", Val::List(entries))]),
    ))
}

/// Playback report (extension playbackReport:1): GET, form, or JSON
/// (the HTTP layer flattens allowed JSON fields into params).
pub async fn report_playback<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Outcome, SubsonicError> {
    if ctx
        .params
        .one_of("mediaType", &["song", "podcast"], None)?
        .as_deref()
        != Some("song")
    {
        return Err(SubsonicError::new(
            10,
            "Only song playback reports are supported",
        ));
    }
    let file_id = decode_expect(&ctx.p("mediaId")?.unwrap_or_default(), IdKind::Track)?;
    let position_ms = ctx.pint("positionMs", None, Some(0), Some(MAX_MEDIA_POSITION_MS))?;
    if position_ms.is_none() {
        return Err(SubsonicError::missing("positionMs"));
    }
    let state = ctx
        .params
        .one_of("state", &["starting", "playing", "paused", "stopped"], None)?;
    if state.is_none() {
        return Err(SubsonicError::missing("state"));
    }
    ctx.pfloat("playbackRate", Some(1.0), Some(0.01), Some(16.0))?;
    let user_id = ctx.user()?.user_id().to_owned();
    let user_name = ctx.user()?.display_name().to_owned();
    ctx.store
        .report_playback(
            &file_id,
            &user_id,
            &user_name,
            &ctx.p("c")?.unwrap_or_default(),
            position_ms.unwrap_or(0),
            &state.unwrap_or_default(),
            ctx.pbool("ignoreScrobble", false)?,
        )
        .await
        .map_err(Ctx::<P, S, B>::store_err)?;
    Ok(Outcome::ok())
}

/// Validated queue ids: at most 500, all tracks, all present.
pub async fn validated_queue_ids<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Vec<String>, SubsonicError> {
    let ids = ctx.plist("id")?;
    if ids.len() > MAX_QUEUE_ITEMS {
        return Err(SubsonicError::new(
            10,
            "Play queue exceeds the 500 item limit",
        ));
    }
    let file_ids = ids
        .iter()
        .map(|sid| decode_expect(sid, IdKind::Track))
        .collect::<Result<Vec<_>, _>>()?;
    let unique: Vec<(IdKind, String)> = file_ids
        .iter()
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .map(|fid| (IdKind::Track, (*fid).clone()))
        .collect();
    let missing = ctx
        .store
        .missing_targets(&unique)
        .await
        .map_err(Ctx::<P, S, B>::store_err)?;
    if !missing.is_empty() {
        return Err(SubsonicError::new(NOT_FOUND, "Play queue song not found"));
    }
    Ok(file_ids)
}

/// Queue entries with stale file ids filtered and the current index
/// remapped (out-of-range current falls back to 0).
pub async fn queue_entries<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<
    (
        super::store::QueueState,
        Vec<super::models::SChild>,
        Option<usize>,
    ),
    SubsonicError,
> {
    let user_id = ctx.user()?.user_id().to_owned();
    let queue = ctx
        .store
        .get_play_queue(&user_id)
        .await
        .map_err(Ctx::<P, S, B>::store_err)?;
    let mapped = ctx
        .store
        .get_tracks_by_file_ids(&queue.file_ids)
        .await
        .map_err(Ctx::<P, S, B>::store_err)?;
    let mut tracks = Vec::new();
    let mut retained = Vec::new();
    for (index, file_id) in queue.file_ids.iter().enumerate() {
        if let Some(track) = mapped.get(file_id) {
            tracks.push(ctx.child(track));
            retained.push(index);
        }
    }
    let mut current_index = None;
    if !tracks.is_empty() {
        current_index = queue
            .current_index
            .and_then(|current| retained.iter().position(|index| *index == current))
            .or(Some(0));
    }
    Ok((queue, tracks, current_index))
}

/// Saved queue (current is a song id; position only when non-empty).
pub async fn get_play_queue<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Outcome, SubsonicError> {
    let (queue, tracks, current_index) = queue_entries(ctx).await?;
    let current = current_index
        .and_then(|index| tracks.get(index))
        .map(|child| child.id.clone());
    Ok(Outcome::keyed(
        "playQueue",
        SPlayQueue {
            username: effective_username(ctx.user()?),
            changed: super::views::iso(Some(queue.updated_at as i64))
                .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_owned()),
            changedBy: queue.changed_by_client,
            current,
            position: current_index.map(|_| queue.position_ms),
            entry: tracks,
        }
        .render(),
    ))
}

/// Save the queue: `current` must reference a queued song for
/// non-empty queues and must be absent for empty ones.
pub async fn save_play_queue<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Outcome, SubsonicError> {
    let file_ids = validated_queue_ids(ctx).await?;
    let current = ctx.p("current")?;
    let position = ctx
        .pint("position", Some(0), Some(0), Some(MAX_MEDIA_POSITION_MS))?
        .unwrap_or(0)
        .max(0);
    let mut current_index = None;
    if !file_ids.is_empty() {
        let Some(current) = current else {
            return Err(SubsonicError::new(
                10,
                "current is required for a non-empty play queue",
            ));
        };
        let current_file_id = decode_expect(&current, IdKind::Track)?;
        current_index = Some(
            file_ids
                .iter()
                .position(|fid| *fid == current_file_id)
                .ok_or_else(|| SubsonicError::new(10, "current must reference a queued song"))?,
        );
    } else if current.is_some() {
        return Err(SubsonicError::new(
            10,
            "current is invalid for an empty play queue",
        ));
    }
    let user_id = ctx.user()?.user_id().to_owned();
    ctx.store
        .replace_play_queue(
            &user_id,
            &file_ids,
            current_index,
            position,
            &ctx.p("c")?.unwrap_or_default(),
        )
        .await
        .map_err(Ctx::<P, S, B>::store_err)?;
    Ok(Outcome::ok())
}

/// Saved queue by index (extension indexBasedQueue:1).
pub async fn get_play_queue_by_index<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Outcome, SubsonicError> {
    let (queue, tracks, current_index) = queue_entries(ctx).await?;
    Ok(Outcome::keyed(
        "playQueueByIndex",
        SPlayQueueByIndex {
            username: effective_username(ctx.user()?),
            changed: super::views::iso(Some(queue.updated_at as i64))
                .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_owned()),
            changedBy: queue.changed_by_client,
            currentIndex: current_index.map(|index| index as i64),
            position: current_index.map(|_| queue.position_ms),
            entry: tracks,
        }
        .render(),
    ))
}

/// Save the queue by index.
pub async fn save_play_queue_by_index<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Outcome, SubsonicError> {
    let file_ids = validated_queue_ids(ctx).await?;
    let current_index = ctx.pint(
        "currentIndex",
        None,
        Some(0),
        Some(MAX_QUEUE_ITEMS as i64 - 1),
    )?;
    let position = ctx
        .pint("position", Some(0), Some(0), Some(MAX_MEDIA_POSITION_MS))?
        .unwrap_or(0)
        .max(0);
    if !file_ids.is_empty() && current_index.is_none() {
        return Err(SubsonicError::new(
            10,
            "currentIndex is required for a non-empty play queue",
        ));
    }
    if current_index.is_some_and(|index| index as usize >= file_ids.len()) {
        return Err(SubsonicError::new(
            10,
            "currentIndex is outside the play queue",
        ));
    }
    let user_id = ctx.user()?.user_id().to_owned();
    ctx.store
        .replace_play_queue(
            &user_id,
            &file_ids,
            current_index.map(|index| index as usize),
            position,
            &ctx.p("c")?.unwrap_or_default(),
        )
        .await
        .map_err(Ctx::<P, S, B>::store_err)?;
    Ok(Outcome::ok())
}

/// Bookmarks (dead links skipped; pinned fields always present).
pub async fn get_bookmarks<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Outcome, SubsonicError> {
    let user_id = ctx.user()?.user_id().to_owned();
    let records = ctx
        .store
        .list_bookmarks(&user_id)
        .await
        .map_err(Ctx::<P, S, B>::store_err)?;
    let file_ids = records
        .iter()
        .map(|record| record.file_id.clone())
        .collect::<Vec<_>>();
    let mapped = ctx
        .store
        .get_tracks_by_file_ids(&file_ids)
        .await
        .map_err(Ctx::<P, S, B>::store_err)?;
    let mut bookmarks = Vec::new();
    for bookmark in &records {
        let Some(track) = mapped.get(&bookmark.file_id) else {
            continue;
        };
        bookmarks.push(
            SBookmark {
                position: bookmark.position_ms,
                username: effective_username(ctx.user()?),
                created: super::views::iso(Some(bookmark.created_at))
                    .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_owned()),
                changed: super::views::iso(Some(bookmark.changed_at))
                    .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_owned()),
                entry: ctx.child(track),
                comment: (!bookmark.comment.is_empty()).then(|| bookmark.comment.clone()),
            }
            .render(),
        );
    }
    Ok(Outcome::keyed(
        "bookmarks",
        obj(vec![("bookmark", Val::List(bookmarks))]),
    ))
}

/// Create or update a bookmark.
pub async fn create_bookmark<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Outcome, SubsonicError> {
    let file_id = decode_expect(&ctx.p("id")?.unwrap_or_default(), IdKind::Track)?;
    let position = ctx.pint("position", None, Some(0), Some(MAX_MEDIA_POSITION_MS))?;
    if position.is_none() {
        return Err(SubsonicError::missing("position"));
    }
    let comment = ctx
        .params
        .string_max("comment", Some(""), 4096)?
        .unwrap_or_default();
    let user_id = ctx.user()?.user_id().to_owned();
    let missing = ctx
        .store
        .missing_targets(&[(IdKind::Track, file_id.clone())])
        .await
        .map_err(Ctx::<P, S, B>::store_err)?;
    if !missing.is_empty() {
        return Err(SubsonicError::new(NOT_FOUND, "Bookmark song not found"));
    }
    ctx.store
        .upsert_bookmark(&user_id, &file_id, position.unwrap_or(0), &comment)
        .await
        .map_err(Ctx::<P, S, B>::store_err)?;
    Ok(Outcome::ok())
}

/// Delete a bookmark (the song must exist).
pub async fn delete_bookmark<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Outcome, SubsonicError> {
    let file_id = decode_expect(&ctx.p("id")?.unwrap_or_default(), IdKind::Track)?;
    let user_id = ctx.user()?.user_id().to_owned();
    let missing = ctx
        .store
        .missing_targets(&[(IdKind::Track, file_id.clone())])
        .await
        .map_err(Ctx::<P, S, B>::store_err)?;
    if !missing.is_empty() {
        return Err(SubsonicError::new(NOT_FOUND, "Bookmark song not found"));
    }
    ctx.store
        .delete_bookmark(&user_id, &file_id)
        .await
        .map_err(Ctx::<P, S, B>::store_err)?;
    Ok(Outcome::ok())
}

/// Login name, falling back to the display name (v2 `username or
/// display_name` in queue/bookmark/user responses).
pub fn effective_username(user: &impl Principal) -> String {
    if user.username().is_empty() {
        user.display_name().to_owned()
    } else {
        user.username().to_owned()
    }
}

/// Artist info: projected MBID (else the raw mbid when it looks like
/// one) plus small/medium/large credential-free cover URLs. `count`
/// and `includeNotPresent` are parsed and ignored.
pub async fn artist_info<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Outcome, SubsonicError> {
    let mbid = decode_expect(&ctx.p("id")?.unwrap_or_default(), IdKind::Artist)?;
    ctx.pint("count", Some(20), Some(0), Some(500))?;
    ctx.pbool("includeNotPresent", false)?;
    let (artist, _) = ctx
        .store
        .get_artist_with_albums(&mbid)
        .await
        .map_err(Ctx::<P, S, B>::store_err)?
        .ok_or_else(|| SubsonicError::new(NOT_FOUND, "Artist not found"))?;
    let cover_id = encode(IdKind::Artist, &mbid);
    let music_brainz_id = if artist.provider_identity_projected {
        artist.musicbrainz_artist_id.clone()
    } else if artist.artist_mbid.contains('-') {
        Some(artist.artist_mbid.clone())
    } else {
        None
    };
    let key = if ctx.endpoint_name == "getartistinfo2" {
        "artistInfo2"
    } else {
        "artistInfo"
    };
    Ok(Outcome::keyed(
        key,
        SArtistInfo {
            biography: None,
            musicBrainzId: music_brainz_id,
            lastFmUrl: None,
            smallImageUrl: Some(ctx.cover_art_url(&cover_id, 250)),
            mediumImageUrl: Some(ctx.cover_art_url(&cover_id, 500)),
            largeImageUrl: Some(ctx.cover_art_url(&cover_id, 1200)),
            similarArtist: Vec::new(),
        }
        .render(),
    ))
}

/// Album info: projected MBID (else the raw rg mbid) plus sized
/// credential-free cover URLs.
pub async fn album_info<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Outcome, SubsonicError> {
    let rg = decode_expect(&ctx.p("id")?.unwrap_or_default(), IdKind::Album)?;
    let album = ctx
        .store
        .get_album(&rg)
        .await
        .map_err(Ctx::<P, S, B>::store_err)?
        .ok_or_else(|| SubsonicError::new(NOT_FOUND, "Album not found"))?;
    let cover_id = encode(IdKind::Album, &rg);
    let music_brainz_id = if album.provider_identity_projected {
        album.musicbrainz_release_group_id.clone()
    } else {
        Some(album.rg_mbid.clone())
    };
    let key = if ctx.endpoint_name == "getalbuminfo" {
        "albumInfo"
    } else {
        "albumInfo2"
    };
    Ok(Outcome::keyed(
        key,
        SAlbumInfo {
            notes: None,
            musicBrainzId: music_brainz_id,
            lastFmUrl: None,
            smallImageUrl: Some(ctx.cover_art_url(&cover_id, 250)),
            mediumImageUrl: Some(ctx.cover_art_url(&cover_id, 500)),
            largeImageUrl: Some(ctx.cover_art_url(&cover_id, 1200)),
        }
        .render(),
    ))
}

/// Structured lyrics (extension songLyrics:1); no lyrics -> empty list.
pub async fn lyrics_by_song_id<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Outcome, SubsonicError> {
    let file_id = decode_expect(&ctx.p("id")?.unwrap_or_default(), IdKind::Track)?;
    let track = ctx
        .store
        .get_track(&file_id)
        .await
        .map_err(Ctx::<P, S, B>::store_err)?
        .ok_or_else(|| SubsonicError::new(NOT_FOUND, "Song not found"))?;
    let lyrics = ctx
        .store
        .get_lyrics(&file_id)
        .await
        .map_err(Ctx::<P, S, B>::store_err)?;
    let mut values = Vec::new();
    if let Some(lyrics) = lyrics {
        values.push(
            SStructuredLyrics {
                lang: lyrics.language,
                synced: lyrics.synced,
                line: lyrics
                    .lines
                    .into_iter()
                    .map(|line| SLyricsLine {
                        value: line.value,
                        start: line.start_ms,
                    })
                    .collect(),
                displayArtist: (!track.artist_name.is_empty()).then(|| track.artist_name.clone()),
                displayTitle: (!track.title.is_empty()).then(|| track.title.clone()),
                offset: None,
            }
            .render(),
        );
    }
    Ok(Outcome::keyed(
        "lyricsList",
        obj(vec![("structuredLyrics", Val::List(values))]),
    ))
}

/// Legacy flat lyrics: exact title (+artist) match, deterministic by
/// file id; a miss renders an empty value (not an error).
pub async fn lyrics<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Outcome, SubsonicError> {
    let artist = ctx.p("artist")?;
    let title = ctx.p("title")?.unwrap_or_default();
    if title.is_empty() {
        return Err(SubsonicError::missing("title"));
    }
    let tracks = ctx
        .store
        .get_tracks_by_title(&title)
        .await
        .map_err(Ctx::<P, S, B>::store_err)?;
    let mut candidates: Vec<_> = tracks
        .into_iter()
        .filter(|track| track.title.to_lowercase() == title.to_lowercase())
        .collect();
    if let Some(artist) = artist.as_deref() {
        candidates.retain(|track| track.artist_name.to_lowercase() == artist.to_lowercase());
    }
    if candidates.is_empty() {
        return Ok(Outcome::keyed(
            "lyrics",
            SLyrics {
                artist: artist.unwrap_or_default(),
                title,
                value: String::new(),
            }
            .render(),
        ));
    }
    candidates.sort_by(|a, b| a.file_id.cmp(&b.file_id));
    let track = &candidates[0];
    let lyrics = ctx
        .store
        .get_lyrics(&track.file_id)
        .await
        .map_err(Ctx::<P, S, B>::store_err)?;
    let value = lyrics
        .map(|lyrics| {
            lyrics
                .lines
                .into_iter()
                .map(|line| line.value)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    Ok(Outcome::keyed(
        "lyrics",
        SLyrics {
            artist: track.artist_name.clone(),
            title: track.title.clone(),
            value,
        }
        .render(),
    ))
}

/// Genre list.
pub async fn genres<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Outcome, SubsonicError> {
    let genres = ctx
        .store
        .get_genres()
        .await
        .map_err(Ctx::<P, S, B>::store_err)?;
    Ok(Outcome::keyed(
        "genres",
        obj(vec![(
            "genre",
            Val::List(
                genres
                    .iter()
                    .map(|genre| super::views::to_genre(genre).render())
                    .collect(),
            ),
        )]),
    ))
}

/// Songs of a genre page.
pub async fn songs_by_genre<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Outcome, SubsonicError> {
    validate_music_folder(&ctx.params)?;
    let genre = ctx.p("genre")?.unwrap_or_default();
    if genre.is_empty() {
        return Err(SubsonicError::missing("genre"));
    }
    let count = ctx
        .pint("count", Some(10), Some(1), Some(500))?
        .unwrap_or(10)
        .max(1) as usize;
    let offset = ctx
        .pint("offset", Some(0), Some(0), Some(2_147_483_647))?
        .unwrap_or(0)
        .max(0) as usize;
    let tracks = ctx
        .store
        .get_songs_by_genre(&genre, count, offset)
        .await
        .map_err(Ctx::<P, S, B>::store_err)?;
    Ok(Outcome::keyed(
        "songsByGenre",
        obj(vec![(
            "song",
            Val::List(
                tracks
                    .iter()
                    .map(|track| ctx.child(track).render())
                    .collect(),
            ),
        )]),
    ))
}

/// Caller user object; the `username` param is required but ignored.
pub async fn user<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Outcome, SubsonicError> {
    if ctx.p("username")?.unwrap_or_default().is_empty() {
        return Err(SubsonicError::missing("username"));
    }
    let caller = ctx.user()?;
    Ok(Outcome::keyed(
        "user",
        SUser::caller(
            effective_username(caller),
            caller.is_admin(),
            ctx.settings.transcode_max_bitrate_kbps,
        )
        .render(),
    ))
}

/// Scan status.
pub async fn scan_status<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Outcome, SubsonicError> {
    let (scanning, count) = ctx
        .store
        .scan_status()
        .await
        .map_err(Ctx::<P, S, B>::store_err)?;
    Ok(Outcome::keyed(
        "scanStatus",
        SScanStatus { scanning, count }.render(),
    ))
}

/// Start a scan (admin-only -> 50).
pub async fn start_scan<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Outcome, SubsonicError> {
    if !ctx.user()?.is_admin() {
        return Err(SubsonicError::new(
            NOT_AUTHORIZED,
            "Administrator role is required to start a scan",
        ));
    }
    ctx.store
        .start_scan()
        .await
        .map_err(Ctx::<P, S, B>::store_err)?;
    Ok(Outcome::keyed(
        "scanStatus",
        SScanStatus {
            scanning: true,
            count: Some(0),
        }
        .render(),
    ))
}

/// Top songs: the artist name must match exactly (case-insensitive),
/// else 70.
pub async fn top_songs<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Outcome, SubsonicError> {
    let artist = ctx.p("artist")?.unwrap_or_default();
    if artist.is_empty() {
        return Err(SubsonicError::missing("artist"));
    }
    let count = ctx
        .pint("count", Some(50), Some(1), Some(500))?
        .unwrap_or(50)
        .max(1) as usize;
    let artists = ctx
        .store
        .get_artists(10, 0, Some(&artist))
        .await
        .map_err(Ctx::<P, S, B>::store_err)?;
    if !artists
        .iter()
        .any(|item| item.name.to_lowercase() == artist.to_lowercase())
    {
        return Err(SubsonicError::new(NOT_FOUND, "Artist not found"));
    }
    let user_id = ctx.user()?.user_id().to_owned();
    let tracks = ctx
        .store
        .get_top_songs(&artist, &user_id, count)
        .await
        .map_err(Ctx::<P, S, B>::store_err)?;
    Ok(Outcome::keyed(
        "topSongs",
        obj(vec![(
            "song",
            Val::List(
                tracks
                    .iter()
                    .map(|track| ctx.child(track).render())
                    .collect(),
            ),
        )]),
    ))
}

/// Similar songs for an artist id (shared by both spellings).
pub async fn similar_songs_query<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Vec<super::views::ViewTrack>, SubsonicError> {
    let mbid = decode_expect(&ctx.p("id")?.unwrap_or_default(), IdKind::Artist)?;
    let count = ctx
        .pint("count", Some(50), Some(1), Some(500))?
        .unwrap_or(50)
        .max(1) as usize;
    if ctx
        .store
        .get_artist_with_albums(&mbid)
        .await
        .map_err(Ctx::<P, S, B>::store_err)?
        .is_none()
    {
        return Err(SubsonicError::new(NOT_FOUND, "Artist not found"));
    }
    let user_id = ctx.user()?.user_id().to_owned();
    ctx.store
        .get_similar_songs(&mbid, &user_id, count)
        .await
        .map_err(Ctx::<P, S, B>::store_err)
}

/// Similar songs (ID3 key).
pub async fn similar_songs2<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Outcome, SubsonicError> {
    let tracks = similar_songs_query(ctx).await?;
    Ok(Outcome::keyed(
        "similarSongs2",
        obj(vec![(
            "song",
            Val::List(
                tracks
                    .iter()
                    .map(|track| ctx.child(track).render())
                    .collect(),
            ),
        )]),
    ))
}

/// Similar songs (file key).
pub async fn similar_songs<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Outcome, SubsonicError> {
    let tracks = similar_songs_query(ctx).await?;
    Ok(Outcome::keyed(
        "similarSongs",
        obj(vec![(
            "song",
            Val::List(
                tracks
                    .iter()
                    .map(|track| ctx.child(track).render())
                    .collect(),
            ),
        )]),
    ))
}
