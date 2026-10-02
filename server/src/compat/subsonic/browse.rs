//! Browse handlers: ping through search.
//! v2: `backend/api/compat/subsonic/router.py` (endpoint functions).

use std::collections::BTreeMap;

use super::auth::Principal;
use super::error::{NOT_FOUND, SubsonicError};
use super::ids::{IdKind, decode, decode_expect, encode};
use super::models::{
    Render, SArtistID3, SArtistsID3, SChild, SIndex, SIndexID3, SIndexes, SLicense, SMusicFolder,
    SOpenSubsonicExtension,
};
use super::store::{AlbumSort, Store};
use super::stream::AudioBackend;
use super::value::{IntoVal, Val, obj};
use super::{Ctx, Outcome};

/// Articles stripped for A-Z bucketing (v2 `_IGNORED_ARTICLES`).
pub const IGNORED_ARTICLES: &str = "The El La Los Las Le Les";

/// Album-list types (v2 `_ALBUMLIST_TYPES`).
pub const ALBUMLIST_TYPES: &[&str] = &[
    "newest",
    "alphabeticalByName",
    "alphabeticalByArtist",
    "random",
    "recent",
    "frequent",
    "starred",
    "byYear",
    "byGenre",
    "highest",
];

/// Bucket letter for an artist name: strip one leading article, take
/// the first char uppercased, non-alpha (or empty) -> `#`.
pub fn index_letter(name: &str) -> char {
    let trimmed = name.trim();
    let lower = trimmed.to_lowercase();
    let mut rest = trimmed;
    for article in IGNORED_ARTICLES.split(' ') {
        let prefix = format!("{article} ", article = article.to_lowercase());
        if lower.starts_with(&prefix) {
            rest = trimmed[prefix.len()..].trim();
            break;
        }
    }
    match rest.chars().next() {
        Some(ch) if ch.is_alphabetic() => ch.to_uppercase().next().unwrap_or('#'),
        _ => '#',
    }
}

/// Only folder 1 exists; any other musicFolderId is 70. Repeated-same
/// ids are accepted (Navidrome 0.62.0 probe behavior).
pub fn validate_music_folder(
    params: &super::params::SubsonicParameters,
) -> Result<(), SubsonicError> {
    for folder_id in params.strings("musicFolderId")? {
        if folder_id != "1" {
            return Err(SubsonicError::new(NOT_FOUND, "Music folder not found"));
        }
    }
    Ok(())
}

/// Missing/empty query means match-all (Navidrome/gonic parity for
/// Arpeggi's "all songs" view). Symfonium's full-library sync sends the
/// literal `""` (verified in sentriz/gonic#229 request logs), so
/// surrounding quotes strip before the empty check (issue #129).
pub fn normalize_search_query(raw: Option<&str>) -> Option<String> {
    let raw = raw?;
    let query = raw.trim().trim_matches(['"', '\'']).trim();
    (!query.is_empty()).then(|| query.to_owned())
}

/// Auth probe: bare ok.
pub async fn ping<P: Principal, S: Store, B: AudioBackend>(
    _ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Outcome, SubsonicError> {
    Ok(Outcome::ok())
}

/// Static license.
pub async fn license<P: Principal, S: Store, B: AudioBackend>(
    _ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Outcome, SubsonicError> {
    Ok(Outcome::keyed("license", SLicense { valid: true }.render()))
}

/// Advertised OpenSubsonic extensions, all v1. PUBLIC (no auth).
/// The set is owned by `compat::shared::extensions` (matrix wins: exactly
/// 3; `transcoding` is served but deliberately NOT advertised).
pub async fn extensions<P: Principal, S: Store, B: AudioBackend>(
    _ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Outcome, SubsonicError> {
    Ok(Outcome::keyed(
        "openSubsonicExtensions",
        Val::List(
            crate::compat::shared::extensions::ADVERTISED
                .iter()
                .map(|ext| {
                    SOpenSubsonicExtension {
                        name: ext.name.to_string(),
                        versions: ext
                            .versions
                            .iter()
                            .map(|version| i64::from(*version))
                            .collect(),
                    }
                    .render()
                })
                .collect(),
        ),
    ))
}

/// Single music folder (id 1, named for the server).
pub async fn music_folders<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Outcome, SubsonicError> {
    let folder = SMusicFolder {
        id: 1,
        name: ctx.settings.server_name.clone(),
    };
    Ok(Outcome::keyed(
        "musicFolders",
        obj(vec![("musicFolder", Val::List(vec![folder.render()]))]),
    ))
}

/// ID3 artists in A-Z buckets.
pub async fn artists<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Outcome, SubsonicError> {
    validate_music_folder(&ctx.params)?;
    let (artists, _) = ctx
        .store
        .get_artists(100_000, 0, None)
        .await
        .map_err(Ctx::<P, S, B>::store_err)?;
    let mut buckets: BTreeMap<char, Vec<SArtistID3>> = BTreeMap::new();
    for artist in &artists {
        buckets
            .entry(index_letter(&artist.name))
            .or_default()
            .push(super::views::to_artist_id3(artist));
    }
    let index = buckets
        .into_iter()
        .map(|(name, artist)| SIndexID3 {
            name: name.to_string(),
            artist,
        })
        .collect();
    Ok(Outcome::keyed(
        "artists",
        SArtistsID3 {
            ignoredArticles: IGNORED_ARTICLES.to_owned(),
            index,
        }
        .render(),
    ))
}

/// File-structure artist indexes, with freshness short-circuit.
pub async fn indexes<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Outcome, SubsonicError> {
    validate_music_folder(&ctx.params)?;
    let revision = ctx
        .store
        .get_library_revision()
        .await
        .map_err(Ctx::<P, S, B>::store_err)?;
    let since = ctx.pint("ifModifiedSince", None, Some(0), None)?;
    if since.is_some_and(|since| since >= revision) {
        return Ok(Outcome::keyed(
            "indexes",
            SIndexes {
                lastModified: revision,
                ignoredArticles: IGNORED_ARTICLES.to_owned(),
                index: vec![],
            }
            .render(),
        ));
    }
    let (artists, _) = ctx
        .store
        .get_artists(100_000, 0, None)
        .await
        .map_err(Ctx::<P, S, B>::store_err)?;
    let mut buckets: BTreeMap<char, Vec<super::models::SArtist>> = BTreeMap::new();
    for artist in &artists {
        buckets
            .entry(index_letter(&artist.name))
            .or_default()
            .push(super::views::to_artist_file(artist));
    }
    let index = buckets
        .into_iter()
        .map(|(name, artist)| SIndex {
            name: name.to_string(),
            artist,
        })
        .collect();
    Ok(Outcome::keyed(
        "indexes",
        SIndexes {
            lastModified: revision,
            ignoredArticles: IGNORED_ARTICLES.to_owned(),
            index,
        }
        .render(),
    ))
}

/// Artist detail (embeds albums).
pub async fn artist<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Outcome, SubsonicError> {
    let mbid = decode_expect(&ctx.p("id")?.unwrap_or_default(), IdKind::Artist)?;
    let (artist, albums) = ctx
        .store
        .get_artist_with_albums(&mbid)
        .await
        .map_err(Ctx::<P, S, B>::store_err)?
        .ok_or_else(|| SubsonicError::new(NOT_FOUND, "Artist not found"))?;
    let mut rendered = super::views::to_artist_id3(&artist);
    rendered.album = Some(albums.iter().map(super::views::to_album_id3).collect());
    Ok(Outcome::keyed("artist", rendered.render()))
}

/// Album detail (embeds songs).
pub async fn album<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Outcome, SubsonicError> {
    let rg = decode_expect(&ctx.p("id")?.unwrap_or_default(), IdKind::Album)?;
    let album = ctx
        .store
        .get_album(&rg)
        .await
        .map_err(Ctx::<P, S, B>::store_err)?
        .ok_or_else(|| SubsonicError::new(NOT_FOUND, "Album not found"))?;
    let tracks = ctx
        .store
        .get_album_tracks(&rg)
        .await
        .map_err(Ctx::<P, S, B>::store_err)?;
    let mut rendered = super::views::to_album_id3(&album);
    rendered.song = Some(tracks.iter().map(|track| ctx.child(track)).collect());
    Ok(Outcome::keyed("album", rendered.render()))
}

/// Song detail.
pub async fn song<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Outcome, SubsonicError> {
    let fid = decode_expect(&ctx.p("id")?.unwrap_or_default(), IdKind::Track)?;
    let track = ctx
        .store
        .get_track(&fid)
        .await
        .map_err(Ctx::<P, S, B>::store_err)?
        .ok_or_else(|| SubsonicError::new(NOT_FOUND, "Song not found"))?;
    Ok(Outcome::keyed("song", ctx.child(&track).render()))
}

/// Shared album-list query (v2 `_album_list`).
pub async fn album_list_query<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Vec<super::views::ViewAlbum>, SubsonicError> {
    validate_music_folder(&ctx.params)?;
    let list_type = ctx.params.one_of("type", ALBUMLIST_TYPES, None)?;
    let list_type = list_type.ok_or_else(|| SubsonicError::missing("type"))?;
    if list_type == "highest" {
        return Err(SubsonicError::new(
            0,
            "Album list type 'highest' is not supported",
        ));
    }
    let size = ctx
        .pint("size", Some(10), Some(1), Some(500))?
        .unwrap_or(10)
        .max(1) as usize;
    let offset = ctx
        .pint("offset", Some(0), Some(0), Some(2_147_483_647))?
        .unwrap_or(0)
        .max(0) as usize;
    let user_id = ctx.user()?.user_id().to_owned();
    if list_type == "recent" || list_type == "frequent" {
        return ctx
            .store
            .get_history_albums(&user_id, list_type == "frequent", size, offset)
            .await
            .map_err(Ctx::<P, S, B>::store_err);
    }
    if list_type == "starred" {
        return ctx
            .store
            .get_starred_albums(&user_id, size, offset)
            .await
            .map_err(Ctx::<P, S, B>::store_err);
    }
    let mut sort = match list_type.as_str() {
        "newest" => AlbumSort::Recent,
        "alphabeticalByName" => AlbumSort::Title,
        "alphabeticalByArtist" => AlbumSort::Artist,
        "random" => AlbumSort::Random,
        _ => AlbumSort::Recent,
    };
    let (mut from_year, mut to_year) = (None, None);
    let mut genre: Option<String> = None;
    if list_type == "byYear" {
        // Clients use year 0 as an unbounded endpoint; param ORDER sets
        // the direction (v2 `_album_list`).
        let first = ctx.pint("fromYear", None, Some(0), Some(9999))?;
        let last = ctx.pint("toYear", None, Some(0), Some(9999))?;
        match (first, last) {
            (Some(first), Some(last)) => {
                sort = if first <= last {
                    AlbumSort::YearAsc
                } else {
                    AlbumSort::YearDesc
                };
                let bounds: Vec<i64> = [first, last]
                    .into_iter()
                    .filter(|year| *year != 0)
                    .collect();
                if bounds.len() == 2 {
                    from_year = Some(bounds[0].min(bounds[1]));
                    to_year = Some(bounds[0].max(bounds[1]));
                } else if bounds.len() == 1 {
                    to_year = Some(bounds[0]);
                }
            }
            _ => {
                return Err(SubsonicError::new(
                    10,
                    "fromYear and toYear are required for byYear",
                ));
            }
        }
    } else if ctx.p("fromYear")?.is_some() || ctx.p("toYear")?.is_some() {
        return Err(SubsonicError::new(
            10,
            "fromYear and toYear require type=byYear",
        ));
    }
    if list_type == "byGenre" {
        let value = ctx.p("genre")?.unwrap_or_default();
        if value.trim().is_empty() {
            return Err(SubsonicError::new(10, "genre is required for byGenre"));
        }
        genre = Some(value);
    } else if ctx.p("genre")?.is_some() {
        return Err(SubsonicError::new(10, "genre requires type=byGenre"));
    }
    let (albums, _) = ctx
        .store
        .get_albums_offset(
            size,
            offset,
            sort,
            from_year,
            to_year,
            genre.as_deref(),
            None,
        )
        .await
        .map_err(Ctx::<P, S, B>::store_err)?;
    Ok(albums)
}

/// ID3 album list.
pub async fn album_list2<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Outcome, SubsonicError> {
    let albums = album_list_query(ctx).await?;
    Ok(Outcome::keyed(
        "albumList2",
        obj(vec![(
            "album",
            Val::List(
                albums
                    .iter()
                    .map(|album| super::views::to_album_id3(album).render())
                    .collect(),
            ),
        )]),
    ))
}

/// File-structure album list.
pub async fn album_list<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Outcome, SubsonicError> {
    let albums = album_list_query(ctx).await?;
    Ok(Outcome::keyed(
        "albumList",
        obj(vec![(
            "album",
            Val::List(
                albums
                    .iter()
                    .map(|album| super::views::to_album_child(album).render())
                    .collect(),
            ),
        )]),
    ))
}

/// Random songs with filters.
pub async fn random_songs<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Outcome, SubsonicError> {
    validate_music_folder(&ctx.params)?;
    let size = ctx
        .pint("size", Some(10), Some(1), Some(500))?
        .unwrap_or(10)
        .max(1) as usize;
    let tracks = ctx
        .store
        .get_random_songs(
            size,
            ctx.p("genre")?.as_deref(),
            ctx.pint("fromYear", None, None, None)?,
            ctx.pint("toYear", None, None, None)?,
        )
        .await
        .map_err(Ctx::<P, S, B>::store_err)?;
    Ok(Outcome::keyed(
        "randomSongs",
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

/// File-structure directory: id 1 -> artists, artist -> albums, album
/// -> songs; anything else is 70 ("Not a directory").
pub async fn music_directory<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Outcome, SubsonicError> {
    let sid = ctx.p("id")?.unwrap_or_default();
    if sid == "1" {
        let (artists, _) = ctx
            .store
            .get_artists(100_000, 0, None)
            .await
            .map_err(Ctx::<P, S, B>::store_err)?;
        let children = artists
            .iter()
            .map(|artist| {
                let aid = encode(IdKind::Artist, &artist.artist_mbid);
                SChild {
                    id: aid.clone(),
                    isDir: true,
                    title: artist.name.clone(),
                    artist: Some(artist.name.clone()),
                    coverArt: Some(aid),
                    ..SChild::default()
                }
                .render()
            })
            .collect();
        return Ok(Outcome::keyed(
            "directory",
            obj(vec![
                ("id", "1".into_val()),
                ("name", ctx.settings.server_name.as_str().into_val()),
                ("child", Val::List(children)),
            ]),
        ));
    }
    let (kind, internal) = decode(&sid)?;
    match kind {
        IdKind::Artist => {
            let (artist, albums) = ctx
                .store
                .get_artist_with_albums(&internal)
                .await
                .map_err(Ctx::<P, S, B>::store_err)?
                .ok_or_else(|| SubsonicError::new(NOT_FOUND, "Artist not found"))?;
            let children = albums
                .iter()
                .map(|album| super::views::to_album_child(album).render())
                .collect();
            Ok(Outcome::keyed(
                "directory",
                obj(vec![
                    ("id", sid.into_val()),
                    ("name", artist.name.into_val()),
                    ("child", Val::List(children)),
                ]),
            ))
        }
        IdKind::Album => {
            let album = ctx
                .store
                .get_album(&internal)
                .await
                .map_err(Ctx::<P, S, B>::store_err)?
                .ok_or_else(|| SubsonicError::new(NOT_FOUND, "Album not found"))?;
            let tracks = ctx
                .store
                .get_album_tracks(&internal)
                .await
                .map_err(Ctx::<P, S, B>::store_err)?;
            let children = tracks
                .iter()
                .map(|track| ctx.child(track).render())
                .collect();
            let parent = album
                .artist_mbid
                .as_ref()
                .map(|mbid| encode(IdKind::Artist, mbid))
                .into_val();
            Ok(Outcome::keyed(
                "directory",
                obj(vec![
                    ("id", sid.into_val()),
                    ("parent", parent),
                    ("name", album.title.into_val()),
                    ("child", Val::List(children)),
                ]),
            ))
        }
        _ => Err(SubsonicError::new(NOT_FOUND, "Not a directory")),
    }
}

/// Shared search query with per-type count+offset (v2 `_search`).
pub async fn search_query<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<
    (
        Vec<super::views::ViewArtist>,
        Vec<super::views::ViewAlbum>,
        Vec<super::views::ViewTrack>,
    ),
    SubsonicError,
> {
    validate_music_folder(&ctx.params)?;
    let query = normalize_search_query(ctx.p("query")?.as_deref());
    let artist_count = ctx
        .pint("artistCount", Some(20), Some(0), Some(500))?
        .unwrap_or(0)
        .max(0) as usize;
    let artist_offset = ctx
        .pint("artistOffset", Some(0), Some(0), Some(2_147_483_647))?
        .unwrap_or(0)
        .max(0) as usize;
    let album_count = ctx
        .pint("albumCount", Some(20), Some(0), Some(500))?
        .unwrap_or(0)
        .max(0) as usize;
    let album_offset = ctx
        .pint("albumOffset", Some(0), Some(0), Some(2_147_483_647))?
        .unwrap_or(0)
        .max(0) as usize;
    let song_count = ctx
        .pint("songCount", Some(20), Some(0), Some(500))?
        .unwrap_or(0)
        .max(0) as usize;
    let song_offset = ctx
        .pint("songOffset", Some(0), Some(0), Some(2_147_483_647))?
        .unwrap_or(0)
        .max(0) as usize;
    let mut artists = Vec::new();
    if artist_count > 0 {
        (artists, _) = ctx
            .store
            .get_artists(artist_count, artist_offset, query.as_deref())
            .await
            .map_err(Ctx::<P, S, B>::store_err)?;
    }
    let mut albums = Vec::new();
    if album_count > 0 {
        (albums, _) = ctx
            .store
            .get_albums_offset(
                album_count,
                album_offset,
                AlbumSort::Recent,
                None,
                None,
                None,
                query.as_deref(),
            )
            .await
            .map_err(Ctx::<P, S, B>::store_err)?;
    }
    let mut songs = Vec::new();
    if song_count > 0 {
        (songs, _) = ctx
            .store
            .get_tracks_page(song_count, song_offset, query.as_deref())
            .await
            .map_err(Ctx::<P, S, B>::store_err)?;
    }
    Ok((artists, albums, songs))
}

/// ID3 search.
pub async fn search3<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Outcome, SubsonicError> {
    let (artists, albums, songs) = search_query(ctx).await?;
    Ok(Outcome::keyed(
        "searchResult3",
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

/// File-structure search.
pub async fn search2<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Outcome, SubsonicError> {
    let (artists, albums, songs) = search_query(ctx).await?;
    Ok(Outcome::keyed(
        "searchResult2",
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
