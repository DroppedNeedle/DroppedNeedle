"""OpenSubsonic shim router (03-subsonic.md)."""

from __future__ import annotations

import logging
import time
from dataclasses import dataclass
from typing import Awaitable, Callable

import msgspec
from fastapi import APIRouter, Depends, Request
from fastapi.responses import Response, StreamingResponse

from api.compat.common.deps import CompatServices, get_compat_services
from api.compat.common.enablement import ensure_subsonic_enabled
from api.compat.subsonic.auth import resolve_subsonic_user
from api.compat.subsonic.errors import to_subsonic_code, to_subsonic_message
from api.compat.subsonic.ids import decode, encode
from api.compat.subsonic import models as m
from api.compat.subsonic.serialization import render, render_error
from core.exceptions import DroppedNeedleException, ExternalServiceError, SubsonicError
from infrastructure.msgspec_fastapi import MsgSpecRoute

logger = logging.getLogger(__name__)

router = APIRouter(prefix="/subsonic", route_class=MsgSpecRoute)

_PUBLIC = {"getOpenSubsonicExtensions"}
_BINARY = {"stream", "download", "getCoverArt"}
_AUTH_CODES = {10, 40, 41, 42, 43, 44, 50}

_HANDLERS: dict[str, "Handler"] = {}


@dataclass
class Ctx:
    request: Request
    params: dict[str, list[str]]
    user: object  # UserRecord | None
    fmt: str
    callback: str | None
    services: CompatServices
    transcode_hint: tuple[str, str] | None = None  # (transcodedContentType, suffix)

    def child(self, track) -> "m.SChild":
        if self.transcode_hint:
            ct, suffix = self.transcode_hint
            return m.to_child(track, transcoded_content_type=ct, transcoded_suffix=suffix)
        return m.to_child(track)

    def p(self, key: str, default: str | None = None) -> str | None:
        vals = self.params.get(key)
        return vals[0] if vals else default

    def plist(self, key: str) -> list[str]:
        return self.params.get(key, [])

    def pint(self, key: str, default: int | None = None) -> int | None:
        raw = self.p(key)
        if raw is None or raw == "":
            return default
        try:
            return int(raw)
        except ValueError:
            return default

    def pbool(self, key: str, default: bool) -> bool:
        raw = self.p(key)
        if raw is None:
            return default
        return raw.strip().lower() in ("true", "1", "yes")

    @property
    def server_name(self) -> str:
        return self.services.preferences.get_connect_apps_settings().advertise_server_name

    def render(self, endpoint_key: str | None, payload: object) -> Response:
        return render(
            endpoint_key, payload, fmt=self.fmt, callback=self.callback,
            server_name=self.server_name,
        )


Handler = Callable[[Ctx], Awaitable[Response]]


def endpoint(*names: str) -> Callable[[Handler], Handler]:
    def deco(fn: Handler) -> Handler:
        for n in names:
            _HANDLERS[n] = fn
        return fn
    return deco


async def _parse_params(request: Request) -> dict[str, list[str]]:
    params: dict[str, list[str]] = {}
    for k, v in request.query_params.multi_items():
        params.setdefault(k, []).append(v)
    ctype = request.headers.get("content-type", "")
    if request.method == "POST" and "application/x-www-form-urlencoded" in ctype:
        form = await request.form()
        for k, v in form.multi_items():
            params.setdefault(k, []).append(str(v))
    return params


def _binary_error(code: int, message: str) -> Response:
    status = {70: 404, 50: 403}.get(code, 404)
    return Response(content=message, status_code=status, media_type="text/plain")


def _transcode_hint(settings) -> tuple[str, str] | None:
    """Transcode hint for song Child; None when transcoding off / ffmpeg absent (05 s4.4)."""
    from services.compat.transcode_service import (
        ffmpeg_available, out_media_type, out_suffix,
    )

    if settings.transcoding_enabled and ffmpeg_available():
        fmt = settings.transcode_default_format
        return out_media_type(fmt), out_suffix(fmt)
    return None


async def _dispatch(request: Request, endpoint_name: str, services: CompatServices) -> Response:
    name = endpoint_name[:-5] if endpoint_name.endswith(".view") else endpoint_name
    params = await _parse_params(request)
    fmt = (params.get("f") or ["xml"])[0]
    callback = (params.get("callback") or [None])[0]
    settings = services.preferences.get_connect_apps_settings()
    is_binary = name in _BINARY
    try:
        # gate before handler lookup so a disabled API can't be probed to enumerate methods
        ensure_subsonic_enabled(settings)
        handler = _HANDLERS.get(name)
        if handler is None:
            raise SubsonicError(0, f"Unknown method {name}")
        user = None
        if name not in _PUBLIC:
            user = await resolve_subsonic_user(params, services.app_passwords)
        ctx = Ctx(request=request, params=params, user=user, fmt=fmt,
                  callback=callback, services=services,
                  transcode_hint=_transcode_hint(settings))
        return await handler(ctx)
    except Exception as exc:  # noqa: BLE001 - boundary: nothing reaches global handlers
        if not isinstance(exc, DroppedNeedleException):
            logger.exception("Unhandled error in Subsonic endpoint %s", name)
        code = to_subsonic_code(exc)
        message = to_subsonic_message(exc, code)
        if is_binary and code not in _AUTH_CODES:
            return _binary_error(code, message)
        return render_error(
            code, message, fmt=fmt, callback=callback,
            server_name=settings.advertise_server_name,
        )


@router.get("/rest/{endpoint_name}")
async def subsonic_get(
    endpoint_name: str, request: Request,
    services: CompatServices = Depends(get_compat_services),
) -> Response:
    return await _dispatch(request, endpoint_name, services)


@router.post("/rest/{endpoint_name}")
async def subsonic_post(
    endpoint_name: str, request: Request,
    services: CompatServices = Depends(get_compat_services),
) -> Response:
    return await _dispatch(request, endpoint_name, services)


@endpoint("ping")
async def _ping(c: Ctx) -> Response:
    return c.render(None, None)


@endpoint("getLicense")
async def _get_license(c: Ctx) -> Response:
    return c.render("license", m.SLicense(valid=True))


@endpoint("getOpenSubsonicExtensions")
async def _extensions(c: Ctx) -> Response:
    exts = [
        m.SOpenSubsonicExtension(name="apiKeyAuthentication", versions=[1]),
        m.SOpenSubsonicExtension(name="formPost", versions=[1]),
        m.SOpenSubsonicExtension(name="transcodeOffset", versions=[1]),
    ]
    return c.render("openSubsonicExtensions", exts)


_IGNORED_ARTICLES = "The El La Los Las Le Les"
_ARTICLES = tuple(f"{a.lower()} " for a in _IGNORED_ARTICLES.split())
_ALBUMLIST_SORTS = {
    "newest": "recent",
    "alphabeticalByName": "title",
    "alphabeticalByArtist": "artist",
    "random": "random",
}


def _index_letter(name: str) -> str:
    n = (name or "").strip()
    low = n.lower()
    for art in _ARTICLES:
        if low.startswith(art):
            n = n[len(art):].strip()
            break
    if not n:
        return "#"
    ch = n[0].upper()
    return ch if ch.isalpha() else "#"


def _decode_expect(sid: str, kind: str) -> str:
    k, internal = decode(sid)
    if k != kind:
        raise SubsonicError(70, f"Expected a {kind} id")
    return internal


@endpoint("getMusicFolders")
async def _get_music_folders(c: Ctx) -> Response:
    folder = m.SMusicFolder(id=1, name=c.server_name)
    return c.render("musicFolders", {"musicFolder": [folder]})


@endpoint("getArtists")
async def _get_artists(c: Ctx) -> Response:
    artists, _ = await c.services.view.get_artists(limit=100_000, user=c.user)
    buckets: dict[str, list] = {}
    for a in artists:
        buckets.setdefault(_index_letter(a.name), []).append(m.to_artist_id3(a))
    index = [m.SIndexID3(name=k, artist=buckets[k]) for k in sorted(buckets)]
    return c.render(
        "artists", m.SArtistsID3(ignoredArticles=_IGNORED_ARTICLES, index=index)
    )


@endpoint("getIndexes")
async def _get_indexes(c: Ctx) -> Response:
    artists, _ = await c.services.view.get_artists(limit=100_000, user=c.user)
    buckets: dict[str, list] = {}
    for a in artists:
        buckets.setdefault(_index_letter(a.name), []).append(m.to_artist_file(a))
    index = [m.SIndex(name=k, artist=buckets[k]) for k in sorted(buckets)]
    return c.render(
        "indexes",
        m.SIndexes(lastModified=0, ignoredArticles=_IGNORED_ARTICLES, index=index),
    )


@endpoint("getArtist")
async def _get_artist(c: Ctx) -> Response:
    artist_mbid = _decode_expect(c.p("id") or "", "artist")
    result = await c.services.view.get_artist_with_albums(artist_mbid, user=c.user)
    if result is None:
        raise SubsonicError(70, "Artist not found")
    artist, albums = result
    s = m.to_artist_id3(artist)
    s.album = [m.to_album_id3(a) for a in albums]
    return c.render("artist", s)


@endpoint("getAlbum")
async def _get_album(c: Ctx) -> Response:
    rg = _decode_expect(c.p("id") or "", "album")
    album = await c.services.view.get_album(rg, user=c.user)
    if album is None:
        raise SubsonicError(70, "Album not found")
    tracks = await c.services.view.get_album_tracks(rg, user=c.user)
    s = m.to_album_id3(album)
    s.song = [c.child(t) for t in tracks]
    return c.render("album", s)


@endpoint("getSong")
async def _get_song(c: Ctx) -> Response:
    fid = _decode_expect(c.p("id") or "", "track")
    track = await c.services.view.get_track(fid, user=c.user)
    if track is None:
        raise SubsonicError(70, "Song not found")
    return c.render("song", c.child(track))


def _album_page(c: Ctx) -> tuple[str, int, int]:
    typ = c.p("type")
    if not typ:
        raise SubsonicError(10, "Required parameter 'type' is missing")
    sort = _ALBUMLIST_SORTS.get(typ, "recent")
    size = min(max(c.pint("size", 10) or 10, 1), 500)
    offset = max(c.pint("offset", 0) or 0, 0)
    page = offset // size + 1
    return sort, page, size


@endpoint("getAlbumList2")
async def _get_album_list2(c: Ctx) -> Response:
    sort, page, size = _album_page(c)
    albums, _ = await c.services.view.get_albums(
        page=page, page_size=size, sort=sort, user=c.user
    )
    return c.render("albumList2", {"album": [m.to_album_id3(a) for a in albums]})


@endpoint("getAlbumList")
async def _get_album_list(c: Ctx) -> Response:
    sort, page, size = _album_page(c)
    albums, _ = await c.services.view.get_albums(
        page=page, page_size=size, sort=sort, user=c.user
    )
    return c.render("albumList", {"album": [m.to_album_child(a) for a in albums]})


@endpoint("getRandomSongs")
async def _get_random_songs(c: Ctx) -> Response:
    size = min(max(c.pint("size", 10) or 10, 1), 500)
    tracks = await c.services.discover.get_random_songs(
        count=size, genre=c.p("genre"),
        from_year=c.pint("fromYear"), to_year=c.pint("toYear"), user=c.user,
    )
    return c.render("randomSongs", {"song": [c.child(t) for t in tracks]})


@endpoint("getMusicDirectory")
async def _get_music_directory(c: Ctx) -> Response:
    sid = c.p("id") or ""
    kind, internal = decode(sid)
    if kind == "artist":
        result = await c.services.view.get_artist_with_albums(internal, user=c.user)
        if result is None:
            raise SubsonicError(70, "Artist not found")
        artist, albums = result
        children = [m.to_album_child(a) for a in albums]
        return c.render("directory", {"id": sid, "name": artist.name, "child": children})
    if kind == "album":
        album = await c.services.view.get_album(internal, user=c.user)
        if album is None:
            raise SubsonicError(70, "Album not found")
        tracks = await c.services.view.get_album_tracks(internal, user=c.user)
        parent = encode("artist", album.artist_mbid) if album.artist_mbid else None
        return c.render(
            "directory",
            {"id": sid, "parent": parent, "name": album.title,
             "child": [c.child(t) for t in tracks]},
        )
    raise SubsonicError(70, "Not a directory")


def _normalize_search_query(raw: str | None) -> str | None:
    """Missing/empty query means "match everything", matching Navidrome/gonic (clients
    like Arpeggi rely on this for their "all songs" view). Symfonium's full-library
    sync sends the literal two-character string `""` (query=%22%22, verified in
    sentriz/gonic#229 request logs), so surrounding quotes are stripped before the
    empty check - otherwise native-mode sync matches nothing (issue #129)."""
    if raw is None:
        return None
    q = raw.strip().strip("\"'").strip()
    return q or None


async def _search(c: Ctx):
    q = _normalize_search_query(c.p("query"))
    a_count = max(c.pint("artistCount", 20) or 0, 0)
    a_offset = max(c.pint("artistOffset", 0) or 0, 0)
    al_count = max(c.pint("albumCount", 20) or 0, 0)
    al_offset = max(c.pint("albumOffset", 0) or 0, 0)
    s_count = max(c.pint("songCount", 20) or 0, 0)
    s_offset = max(c.pint("songOffset", 0) or 0, 0)

    artists = []
    if a_count:
        artists, _ = await c.services.view.get_artists(
            limit=a_count, offset=a_offset, q=q, user=c.user
        )
    albums = []
    if al_count:
        albums, _ = await c.services.view.get_albums(
            page=al_offset // al_count + 1, page_size=al_count, q=q, user=c.user
        )
    songs = []
    if s_count:
        songs, _ = await c.services.view.get_tracks_page(
            limit=s_count, offset=s_offset, q=q, user=c.user
        )
    return artists, albums, songs


@endpoint("search3")
async def _search3(c: Ctx) -> Response:
    artists, albums, songs = await _search(c)
    return c.render("searchResult3", {
        "artist": [m.to_artist_id3(a) for a in artists],
        "album": [m.to_album_id3(a) for a in albums],
        "song": [c.child(t) for t in songs],
    })


@endpoint("search2")
async def _search2(c: Ctx) -> Response:
    artists, albums, songs = await _search(c)
    return c.render("searchResult2", {
        "artist": [m.to_artist_file(a) for a in artists],
        "album": [m.to_album_child(a) for a in albums],
        "song": [c.child(t) for t in songs],
    })


_PLACEHOLDER_SVG = (
    '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 200 200">'
    '<rect fill="#374151" width="200" height="200"/>'
    '<circle cx="100" cy="100" r="70" fill="#1f2937" stroke="#4B5563" stroke-width="2"/>'
    '<circle cx="100" cy="100" r="12" fill="#4B5563"/></svg>'
).encode()


def _cover_size(size: str | None) -> str:
    if not size:
        return "500"
    try:
        px = int(size)
    except ValueError:
        return "500"
    if px <= 300:
        return "250"
    if px <= 750:
        return "500"
    return "1200"


def _placeholder() -> Response:
    return Response(
        content=_PLACEHOLDER_SVG, media_type="image/svg+xml",
        headers={"Cache-Control": "public, max-age=86400"},
    )


@endpoint("getCoverArt")
async def _get_cover_art(c: Ctx) -> Response:
    kind, internal = decode(c.p("id") or "")  # unknown prefix -> 70 -> 404 (binary)
    size = _cover_size(c.p("size"))
    disc = c.request.is_disconnected
    result = None
    if kind == "album":
        result = await c.services.coverart.get_release_group_cover(
            internal, size, is_disconnected=disc
        )
    elif kind == "track":
        track = await c.services.view.get_track(internal)
        if track and track.rg_mbid:
            result = await c.services.coverart.get_release_group_cover(
                track.rg_mbid, size, is_disconnected=disc
            )
    elif kind == "artist":
        px = int(size) if size.isdigit() else None
        result = await c.services.coverart.get_artist_image(
            internal, px, is_disconnected=disc
        )
    # playlist (pl-) and any miss fall through to the placeholder
    if result:
        data, content_type, _ = result
        return Response(
            content=data, media_type=content_type,
            headers={"Cache-Control": "public, max-age=31536000, immutable"},
        )
    return _placeholder()


async def _serve_file(c: Ctx, file_id: str) -> Response:
    range_header = c.request.headers.get("Range")
    try:
        chunks, headers, status = await c.services.local_files.stream_track(
            file_id, range_header=range_header
        )
    except ExternalServiceError as exc:
        if "Range not satisfiable" in str(exc):
            return Response(status_code=416)
        raise
    # 05 s10: keep gzip off audio; it drops Content-Length and breaks Range/seeking
    out_headers = {**headers, "Content-Encoding": "identity"}
    return StreamingResponse(
        chunks, status_code=status, headers=out_headers,
        media_type=headers.get("Content-Type", "application/octet-stream"),
    )


@endpoint("stream")
async def _stream(c: Ctx) -> Response:
    from services.compat.transcode_service import decide, ffmpeg_available

    fid = _decode_expect(c.p("id") or "", "track")
    fmt = c.p("format")
    if fmt == "raw":  # force original, skip the track lookup
        return await _serve_file(c, fid)
    track = await c.services.view.get_track(fid)
    if track is None:
        raise SubsonicError(70, "Song not found")
    settings = c.services.preferences.get_connect_apps_settings()
    plan = decide(
        track, requested_format=fmt, max_bitrate_kbps=c.pint("maxBitRate"),
        force_original=False, start_seconds=float(c.pint("timeOffset", 0) or 0),
        settings=settings, ffmpeg_available=ffmpeg_available(),
    )
    if not plan.transcode:
        return await _serve_file(c, fid)
    path = await c.services.local_files.resolve_validated_path(fid)
    return c.services.transcode.stream(
        str(path), plan, is_disconnected=c.request.is_disconnected,
        estimate=c.pbool("estimateContentLength", False),
    )


@endpoint("download")
async def _download(c: Ctx) -> Response:
    fid = _decode_expect(c.p("id") or "", "track")
    return await _serve_file(c, fid)


def _playlist_cover(record) -> str | None:
    return encode("playlist", record.id) if record.cover_image_path else None


async def _build_playlist_detail(c: Ctx, pid: str):
    from services.playlist_service import PlaylistDetailView

    detail = await c.services.playlists.get_playlist_with_tracks(pid, c.user)
    if not isinstance(detail, PlaylistDetailView):
        raise SubsonicError(70, "Playlist not found")
    r = detail.record
    songs, total = [], 0
    for entry in detail.tracks:
        if not entry.library_file_id:  # legacy/outbound entry, not streamable
            continue
        track = await c.services.view.get_track(entry.library_file_id, user=c.user)
        if track is None:
            continue
        songs.append(c.child(track))
        total += round(track.duration_seconds)
    owner = c.user.username if detail.is_owner else detail.owner_name
    return m.SPlaylist(
        id=encode("playlist", r.id), name=r.name, owner=owner, public=r.is_public,
        songCount=len(songs), duration=total,
        created=r.created_at, changed=r.updated_at, coverArt=_playlist_cover(r),
        entry=songs,
    )


@endpoint("getPlaylists")
async def _get_playlists(c: Ctx) -> Response:
    views = await c.services.playlists.get_all_playlists(c.user)
    # counts must match what getPlaylist serves (library-linked entries only);
    # a higher songCount reads as a broken playlist in clients (#181)
    streamable = await c.services.playlists.get_streamable_counts()
    playlists = []
    for v in views:
        record = getattr(v, "record", None)
        if record is None:  # RedactedSummaryView (private, non-owned)
            continue
        owner = c.user.username if v.is_owner else v.owner_name
        song_count, duration = streamable.get(record.id, (0, 0))
        playlists.append(m.SPlaylist(
            id=encode("playlist", record.id), name=record.name, owner=owner,
            public=record.is_public, songCount=song_count,
            duration=duration, created=record.created_at,
            changed=record.updated_at, coverArt=_playlist_cover(record),
        ))
    return c.render("playlists", {"playlist": playlists})


@endpoint("getPlaylist")
async def _get_playlist(c: Ctx) -> Response:
    pid = _decode_expect(c.p("id") or "", "playlist")
    return c.render("playlist", await _build_playlist_detail(c, pid))


@endpoint("createPlaylist")
async def _create_playlist(c: Ctx) -> Response:
    playlist_id = c.p("playlistId")
    song_file_ids = [_decode_expect(s, "track") for s in c.plist("songId")]
    if playlist_id:  # createPlaylist with playlistId replaces existing contents
        pid = _decode_expect(playlist_id, "playlist")
        existing = await c.services.playlists.get_tracks(pid)
        if existing:
            await c.services.playlists.remove_tracks(
                pid, c.user, [e.id for e in existing]
            )
        name = c.p("name")
        if name:
            await c.services.playlists.update_playlist(pid, c.user, name=name)
    else:
        name = c.p("name")
        if not name:
            raise SubsonicError(10, "Required parameter 'name' is missing")
        record = await c.services.playlists.create_playlist(name, user_id=c.user.id)
        pid = record.id
    for fid in song_file_ids:
        await c.services.playlists.add_file_id_entry(pid, fid, requesting=c.user)
    return c.render("playlist", await _build_playlist_detail(c, pid))


@endpoint("updatePlaylist")
async def _update_playlist(c: Ctx) -> Response:
    pid = _decode_expect(c.p("playlistId") or "", "playlist")
    name = c.p("name")
    public = c.p("public")
    add_file_ids = [_decode_expect(s, "track") for s in c.plist("songIdToAdd")]
    remove_indices = [int(i) for i in c.plist("songIndexToRemove") if i.isdigit()]
    if name is not None:
        await c.services.playlists.update_playlist(pid, c.user, name=name)
    if public is not None:
        is_pub = public.strip().lower() in ("true", "1", "yes")
        await c.services.playlists.set_public(pid, c.user, is_pub)
    if remove_indices:
        entries = await c.services.playlists.get_tracks(pid)
        ids = [entries[i].id for i in remove_indices if 0 <= i < len(entries)]
        if ids:
            await c.services.playlists.remove_tracks(pid, c.user, ids)
    for fid in add_file_ids:
        await c.services.playlists.add_file_id_entry(pid, fid, requesting=c.user)
    return c.render(None, None)


@endpoint("deletePlaylist")
async def _delete_playlist(c: Ctx) -> Response:
    pid = _decode_expect(c.p("id") or "", "playlist")
    await c.services.playlists.delete_playlist(pid, c.user)
    return c.render(None, None)


_FAV_KINDS = {"artist", "album", "track"}


def _collect_star_targets(c: Ctx) -> list[tuple[str, str]]:
    """(kind, internal_id) pairs from id (routed by prefix) + albumId + artistId."""
    targets: list[tuple[str, str]] = []
    for s in c.plist("id"):
        kind, internal = decode(s)
        if kind in _FAV_KINDS:
            targets.append((kind, internal))
    for s in c.plist("albumId"):
        targets.append(("album", _decode_expect(s, "album")))
    for s in c.plist("artistId"):
        targets.append(("artist", _decode_expect(s, "artist")))
    return targets


@endpoint("star")
async def _star(c: Ctx) -> Response:
    for kind, internal in _collect_star_targets(c):
        await c.services.favorites.add(c.user.id, kind, internal)
    return c.render(None, None)


@endpoint("unstar")
async def _unstar(c: Ctx) -> Response:
    for kind, internal in _collect_star_targets(c):
        await c.services.favorites.remove(c.user.id, kind, internal)
    return c.render(None, None)


async def _starred_lists(c: Ctx):
    artists = []
    for mbid, _ in await c.services.favorites.list(c.user.id, "artist"):
        res = await c.services.view.get_artist_with_albums(mbid, user=c.user)
        if res:
            artists.append(res[0])
    albums = []
    for rg, _ in await c.services.favorites.list(c.user.id, "album"):
        album = await c.services.view.get_album(rg, user=c.user)
        if album:
            albums.append(album)
    songs = []
    for fid, _ in await c.services.favorites.list(c.user.id, "track"):
        track = await c.services.view.get_track(fid, user=c.user)
        if track:
            songs.append(track)
    return artists, albums, songs


@endpoint("getStarred2")
async def _get_starred2(c: Ctx) -> Response:
    artists, albums, songs = await _starred_lists(c)
    return c.render("starred2", {
        "artist": [m.to_artist_id3(a) for a in artists],
        "album": [m.to_album_id3(a) for a in albums],
        "song": [c.child(t) for t in songs],
    })


@endpoint("getStarred")
async def _get_starred(c: Ctx) -> Response:
    artists, albums, songs = await _starred_lists(c)
    return c.render("starred", {
        "artist": [m.to_artist_file(a) for a in artists],
        "album": [m.to_album_child(a) for a in albums],
        "song": [c.child(t) for t in songs],
    })


@endpoint("setRating")
async def _set_rating(c: Ctx) -> Response:
    # ratings not implemented (decision Q4): accept and no-op
    return c.render(None, None)


@endpoint("scrobble")
async def _scrobble(c: Ctx) -> Response:
    ids = c.plist("id")
    if not ids:
        raise SubsonicError(10, "Required parameter 'id' is missing")
    times = c.plist("time")  # ms epoch, positionally parallel to ids
    submission = c.pbool("submission", True)
    client = c.p("c")
    for i, sid in enumerate(ids):
        fid = _decode_expect(sid, "track")
        if submission:
            played_at = None
            if i < len(times) and times[i].isdigit():
                played_at = int(times[i]) / 1000.0
            await c.services.scrobble.scrobble(
                fid, user_id=c.user.id, client=client, played_at=played_at,
                user_name=getattr(c.user, "display_name", ""),
            )
        else:
            await c.services.scrobble.now_playing(
                fid, user_id=c.user.id, client=client,
                user_name=getattr(c.user, "display_name", ""),
            )
    return c.render(None, None)


@endpoint("getNowPlaying")
async def _get_now_playing(c: Ctx) -> Response:
    entries = []
    now = time.time()
    for idx, (proj, fid, updated_at) in enumerate(c.services.now_playing.compat_now_playing()):
        track = await c.services.view.get_track(fid, user=c.user)
        if track is None:
            continue
        child = c.child(track)
        entries.append(m.SNowPlayingEntry(
            **msgspec.structs.asdict(child),
            username=proj.user_name,
            minutesAgo=max(int((now - updated_at) // 60), 0),
            playerId=idx,
            playerName=proj.source or proj.device_name or None,
        ))
    return c.render("nowPlaying", {"entry": entries})


@endpoint("getArtistInfo2", "getArtistInfo")
async def _get_artist_info(c: Ctx) -> Response:
    # no biography/similar-artist data to serve; an empty element is valid and
    # spares clients that call this unconditionally an "Unknown method" error
    # (setRating no-op pattern). Artist images flow through coverArt ids.
    leaf = c.request.url.path.rsplit("/", 1)[-1].removesuffix(".view")
    return c.render("artistInfo2" if leaf == "getArtistInfo2" else "artistInfo", {})


@endpoint("getGenres")
async def _get_genres(c: Ctx) -> Response:
    genres = await c.services.view.get_genres()
    return c.render("genres", {"genre": [m.to_genre(g) for g in genres]})


@endpoint("getSongsByGenre")
async def _get_songs_by_genre(c: Ctx) -> Response:
    genre = c.p("genre")
    if not genre:
        raise SubsonicError(10, "Required parameter 'genre' is missing")
    count = min(max(c.pint("count", 10) or 10, 1), 500)
    offset = max(c.pint("offset", 0) or 0, 0)
    tracks = await c.services.view.get_songs_by_genre(
        genre, limit=count, offset=offset, user=c.user
    )
    return c.render("songsByGenre", {"song": [c.child(t) for t in tracks]})


@endpoint("getUser")
async def _get_user(c: Ctx) -> Response:
    username = c.p("username")
    if not username:
        raise SubsonicError(10, "Required parameter 'username' is missing")
    is_admin = c.user.role == "admin"
    if username.strip().lower() != (c.user.username or "") and not is_admin:
        raise SubsonicError(50, "Not authorized to view this user")
    settings = c.services.preferences.get_connect_apps_settings()
    return c.render("user", m.SUser(
        username=c.user.username or username,
        adminRole=is_admin,
        maxBitRate=settings.transcode_max_bitrate_kbps,
    ))


@endpoint("getTopSongs")
async def _get_top_songs(c: Ctx) -> Response:
    artist = c.p("artist")
    if not artist:
        raise SubsonicError(10, "Required parameter 'artist' is missing")
    count = c.pint("count", 50) or 50
    tracks = await c.services.discover.get_top_songs(
        artist, user_id=c.user.id, count=count, user=c.user
    )
    return c.render("topSongs", {"song": [c.child(t) for t in tracks]})


async def _similar_songs(c: Ctx) -> list:
    artist_mbid = _decode_expect(c.p("id") or "", "artist")
    count = c.pint("count", 50) or 50
    return await c.services.discover.get_similar_songs(
        artist_mbid, user_id=c.user.id, count=count, user=c.user
    )


@endpoint("getSimilarSongs2")
async def _get_similar_songs2(c: Ctx) -> Response:
    tracks = await _similar_songs(c)
    return c.render("similarSongs2", {"song": [c.child(t) for t in tracks]})


@endpoint("getSimilarSongs")
async def _get_similar_songs(c: Ctx) -> Response:
    tracks = await _similar_songs(c)
    return c.render("similarSongs", {"song": [c.child(t) for t in tracks]})
