"""OpenSubsonic shim router (03-subsonic.md)."""

from __future__ import annotations

import asyncio
import logging
import re
import time
from dataclasses import dataclass
from typing import Awaitable, Callable

import msgspec
from fastapi import APIRouter, Depends, Request
from fastapi.responses import FileResponse, Response, StreamingResponse
from starlette.background import BackgroundTask

from api.compat.common.deps import CompatServices, get_compat_services
from api.compat.common.enablement import ensure_subsonic_enabled
from api.compat.common.ratelimit import (
    compat_rate_limits,
    is_media_request,
    is_mutation_request,
    reject_subsonic,
    trusted_client_ip,
)
from api.compat.subsonic.auth import resolve_subsonic_user
from api.compat.subsonic.errors import to_subsonic_code, to_subsonic_message
from api.compat.subsonic.ids import decode, encode
from api.compat.subsonic import models as m
from api.compat.subsonic.parameters import (
    SubsonicParameters,
    parse_request_parameters,
)
from api.compat.subsonic.serialization import render, render_error
from core.exceptions import (
    DroppedNeedleException,
    RangeNotSatisfiableError,
    SubsonicError,
)
from infrastructure.msgspec_fastapi import MsgSpecRoute

logger = logging.getLogger(__name__)

router = APIRouter(prefix="/subsonic", route_class=MsgSpecRoute)

_PUBLIC = {"getopensubsonicextensions"}
_BINARY = {"stream", "download", "getcoverart", "getavatar", "gettranscodestream"}
_AUTH_CODES = {10, 40, 41, 42, 43, 44, 50}
_PLAYBACK_REPORT_JSON_FIELDS = frozenset(
    {"mediaId", "mediaType", "positionMs", "state", "playbackRate", "ignoreScrobble"}
)

_HANDLERS: dict[str, "Handler"] = {}


@dataclass
class Ctx:
    request: Request
    endpoint_name: str
    params: dict[str, list[str]]
    decoded: SubsonicParameters
    user: object  # UserRecord | None
    fmt: str
    callback: str | None
    services: CompatServices
    transcode_hint: tuple[str, str] | None = None  # (transcodedContentType, suffix)

    def child(self, track) -> "m.SChild":
        if self.transcode_hint:
            ct, suffix = self.transcode_hint
            return m.to_child(
                track, transcoded_content_type=ct, transcoded_suffix=suffix
            )
        return m.to_child(track)

    def p(self, key: str, default: str | None = None) -> str | None:
        return self.decoded.string(key, default)

    def plist(self, key: str) -> list[str]:
        return self.decoded.strings(key)

    def pint(
        self,
        key: str,
        default: int | None = None,
        *,
        minimum: int | None = None,
        maximum: int | None = None,
    ) -> int | None:
        return self.decoded.integer(key, default, minimum=minimum, maximum=maximum)

    def pfloat(
        self,
        key: str,
        default: float | None = None,
        *,
        minimum: float | None = None,
        maximum: float | None = None,
    ) -> float | None:
        return self.decoded.number(key, default, minimum=minimum, maximum=maximum)

    def pbool(self, key: str, default: bool) -> bool:
        return self.decoded.boolean(key, default)

    @property
    def server_name(self) -> str:
        return (
            self.services.preferences.get_connect_apps_settings().advertise_server_name
        )

    @property
    def server_version(self) -> str:
        return self.services.version.get_current_version().version

    def render(self, endpoint_key: str | None, payload: object) -> Response:
        return render(
            endpoint_key,
            payload,
            fmt=self.fmt,
            callback=self.callback,
            server_name=self.server_name,
            server_version=self.server_version,
        )


Handler = Callable[[Ctx], Awaitable[Response]]


def endpoint(*names: str) -> Callable[[Handler], Handler]:
    def deco(fn: Handler) -> Handler:
        for n in names:
            _HANDLERS[n.casefold()] = fn
        return fn

    return deco


def _binary_error(code: int, message: str) -> Response:
    status = {70: 404, 50: 403}.get(code, 404)
    return Response(content=message, status_code=status, media_type="text/plain")


def _transcode_hint(settings) -> tuple[str, str] | None:
    """Transcode hint for song Child; None when transcoding off / ffmpeg absent (05 s4.4)."""
    from services.compat.transcode_service import (
        ffmpeg_available,
        out_media_type,
        out_suffix,
    )

    if settings.transcoding_enabled and ffmpeg_available():
        fmt = settings.transcode_default_format
        return out_media_type(fmt), out_suffix(fmt)
    return None


async def _dispatch(
    request: Request, endpoint_name: str, services: CompatServices
) -> Response:
    folded_name = endpoint_name.casefold()
    name = folded_name[:-5] if folded_name.endswith(".view") else folded_name
    params: dict[str, list[str]] = {}
    decoded = SubsonicParameters(params)
    fmt = "xml"
    callback = None
    settings = services.preferences.get_connect_apps_settings()
    server_version = services.version.get_current_version().version
    is_binary = name in _BINARY
    client_ip = trusted_client_ip(request)
    # Preserve the requested error envelope even when strict body decoding fails.
    # Only a unique, already-valid query value is trusted at this early stage.
    early_formats = request.query_params.getlist("f")
    if len(early_formats) == 1 and early_formats[0] in {"xml", "json", "jsonp"}:
        fmt = early_formats[0]
    early_callbacks = request.query_params.getlist("callback")
    if len(early_callbacks) == 1 and len(early_callbacks[0]) <= 128:
        callback = early_callbacks[0]
    try:
        params = await parse_request_parameters(
            request,
            json_fields=_PLAYBACK_REPORT_JSON_FIELDS
            if name == "reportplayback"
            else frozenset(),
        )
        decoded = SubsonicParameters(params)
        requested_format = decoded.string("f", "xml", max_length=16)
        if requested_format not in {"xml", "json", "jsonp"}:
            raise SubsonicError(10, "Invalid parameter 'f'")
        fmt = requested_format
        callback = decoded.string("callback", max_length=128)
        # gate before handler lookup so a disabled API can't be probed to enumerate methods
        ensure_subsonic_enabled(settings)
        handler = _HANDLERS.get(name)
        if handler is None:
            raise SubsonicError(0, f"Unknown method {name}")
        user = None
        if name in _PUBLIC:
            retry_after = await compat_rate_limits.public_retry_after(client_ip)
            if retry_after is not None:
                return reject_subsonic(
                    fmt,
                    callback,
                    retry_after,
                    server_name=settings.advertise_server_name,
                    server_version=server_version,
                )
        else:
            retry_after = compat_rate_limits.auth_failure_retry_after(client_ip)
            if retry_after is not None:
                return reject_subsonic(
                    fmt,
                    callback,
                    retry_after,
                    server_name=settings.advertise_server_name,
                    server_version=server_version,
                )
            try:
                user = await resolve_subsonic_user(params, services.app_passwords)
            except SubsonicError as exc:
                if exc.code in {10, 40, 41, 42, 43, 44}:
                    retry_after = compat_rate_limits.record_auth_failure(client_ip)
                    if retry_after is not None:
                        return reject_subsonic(
                            fmt,
                            callback,
                            retry_after,
                            server_name=settings.advertise_server_name,
                            server_version=server_version,
                        )
                raise
            if not is_media_request(request.url.path):
                retry_after = await compat_rate_limits.principal_retry_after(
                    user.id,
                    mutation=is_mutation_request(request.method, request.url.path),
                )
                if retry_after is not None:
                    return reject_subsonic(
                        fmt,
                        callback,
                        retry_after,
                        server_name=settings.advertise_server_name,
                        server_version=server_version,
                    )
        ctx = Ctx(
            request=request,
            endpoint_name=name,
            params=params,
            decoded=decoded,
            user=user,
            fmt=fmt,
            callback=callback,
            services=services,
            transcode_hint=_transcode_hint(settings),
        )
        return await handler(ctx)
    except Exception as exc:  # noqa: BLE001 - boundary: nothing reaches global handlers
        if not isinstance(exc, DroppedNeedleException):
            logger.exception("Unhandled error in Subsonic endpoint %s", name)
        code = to_subsonic_code(exc)
        message = to_subsonic_message(exc, code)
        if is_binary and code not in _AUTH_CODES:
            return _binary_error(code, message)
        return render_error(
            code,
            message,
            fmt=fmt,
            callback=callback,
            server_name=settings.advertise_server_name,
            server_version=services.version.get_current_version().version,
        )


@router.post("/rest/reportPlayback")
async def subsonic_playback_report_json(
    request: Request,
    services: CompatServices = Depends(get_compat_services),
) -> Response:
    """Dedicated JSON/form route for the playbackReport extension contract."""
    return await _dispatch(request, "reportPlayback", services)


@router.get("/rest/{endpoint_name}")
async def subsonic_get(
    endpoint_name: str,
    request: Request,
    services: CompatServices = Depends(get_compat_services),
) -> Response:
    return await _dispatch(request, endpoint_name, services)


@router.post("/rest/{endpoint_name}")
async def subsonic_post(
    endpoint_name: str,
    request: Request,
    services: CompatServices = Depends(get_compat_services),
) -> Response:
    return await _dispatch(request, endpoint_name, services)


@router.head("/rest/{endpoint_name}")
async def subsonic_head(
    endpoint_name: str,
    request: Request,
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
_ALBUMLIST_TYPES = frozenset(
    {
        *_ALBUMLIST_SORTS,
        "recent",
        "frequent",
        "starred",
        "byYear",
        "byGenre",
        "highest",
    }
)


def _index_letter(name: str) -> str:
    n = (name or "").strip()
    low = n.lower()
    for art in _ARTICLES:
        if low.startswith(art):
            n = n[len(art) :].strip()
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


def _validate_music_folder(c: Ctx) -> None:
    folder_ids = c.plist("musicFolderId")
    if any(folder_id != "1" for folder_id in folder_ids):
        raise SubsonicError(70, "Music folder not found")


@endpoint("getMusicFolders")
async def _get_music_folders(c: Ctx) -> Response:
    folder = m.SMusicFolder(id=1, name=c.server_name)
    return c.render("musicFolders", {"musicFolder": [folder]})


@endpoint("getArtists")
async def _get_artists(c: Ctx) -> Response:
    _validate_music_folder(c)
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
    _validate_music_folder(c)
    revision = await c.services.view.get_library_revision()
    if_modified_since = c.pint("ifModifiedSince", minimum=0)
    if if_modified_since is not None and if_modified_since >= revision:
        return c.render(
            "indexes",
            m.SIndexes(
                lastModified=revision,
                ignoredArticles=_IGNORED_ARTICLES,
                index=[],
            ),
        )
    artists, _ = await c.services.view.get_artists(limit=100_000, user=c.user)
    buckets: dict[str, list] = {}
    for a in artists:
        buckets.setdefault(_index_letter(a.name), []).append(m.to_artist_file(a))
    index = [m.SIndex(name=k, artist=buckets[k]) for k in sorted(buckets)]
    return c.render(
        "indexes",
        m.SIndexes(
            lastModified=revision, ignoredArticles=_IGNORED_ARTICLES, index=index
        ),
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


async def _album_list(c: Ctx):
    _validate_music_folder(c)
    typ = c.decoded.enum("type", _ALBUMLIST_TYPES)
    if typ is None:
        raise SubsonicError(10, "Required parameter 'type' is missing")
    if typ == "highest":
        raise SubsonicError(0, "Album list type 'highest' is not supported")
    size = c.pint("size", 10, minimum=1, maximum=500) or 10
    offset = c.pint("offset", 0, minimum=0, maximum=2_147_483_647) or 0
    if typ in {"recent", "frequent"}:
        return await c.services.discover.get_history_albums(
            user_id=c.user.id,
            frequent=typ == "frequent",
            limit=size,
            offset=offset,
            user=c.user,
        )
    if typ == "starred":
        return await c.services.view.get_starred_albums(
            c.user, limit=size, offset=offset
        )
    sort = _ALBUMLIST_SORTS.get(typ, "recent")
    from_year = None
    to_year = None
    genre = None
    if typ == "byYear":
        first = c.pint("fromYear", minimum=0, maximum=9999)
        last = c.pint("toYear", minimum=0, maximum=9999)
        if first is None or last is None:
            raise SubsonicError(10, "fromYear and toYear are required for byYear")
        # Subsonic client convention: 0 means "no bound" on that side (real
        # years are never 0), not a literal year value.
        first = first or None
        last = last or None
        if first is not None and last is not None:
            from_year, to_year = min(first, last), max(first, last)
            sort = "year_asc" if first <= last else "year_desc"
        else:
            from_year, to_year = first, last
            sort = "year_desc" if last is not None else "year_asc"
    elif c.p("fromYear") is not None or c.p("toYear") is not None:
        raise SubsonicError(10, "fromYear and toYear require type=byYear")
    if typ == "byGenre":
        genre = c.p("genre")
        if not genre or not genre.strip():
            raise SubsonicError(10, "genre is required for byGenre")
    elif c.p("genre") is not None:
        raise SubsonicError(10, "genre requires type=byGenre")
    albums, _ = await c.services.view.get_albums_offset(
        limit=size,
        offset=offset,
        sort=sort,
        from_year=from_year,
        to_year=to_year,
        genre=genre,
        user=c.user,
    )
    return albums


@endpoint("getAlbumList2")
async def _get_album_list2(c: Ctx) -> Response:
    albums = await _album_list(c)
    return c.render("albumList2", {"album": [m.to_album_id3(a) for a in albums]})


@endpoint("getAlbumList")
async def _get_album_list(c: Ctx) -> Response:
    albums = await _album_list(c)
    return c.render("albumList", {"album": [m.to_album_child(a) for a in albums]})


@endpoint("getRandomSongs")
async def _get_random_songs(c: Ctx) -> Response:
    _validate_music_folder(c)
    size = c.pint("size", 10, minimum=1, maximum=500) or 10
    tracks = await c.services.discover.get_random_songs(
        count=size,
        genre=c.p("genre"),
        from_year=c.pint("fromYear"),
        to_year=c.pint("toYear"),
        user=c.user,
    )
    return c.render("randomSongs", {"song": [c.child(t) for t in tracks]})


@endpoint("getMusicDirectory")
async def _get_music_directory(c: Ctx) -> Response:
    sid = c.p("id") or ""
    if sid == "1":
        artists, _ = await c.services.view.get_artists(limit=100_000, user=c.user)
        return c.render(
            "directory",
            {
                "id": "1",
                "name": c.server_name,
                "child": [
                    m.SChild(
                        id=encode("artist", artist.artist_mbid),
                        isDir=True,
                        title=artist.name,
                        artist=artist.name,
                        coverArt=encode("artist", artist.artist_mbid),
                    )
                    for artist in artists
                ],
            },
        )
    kind, internal = decode(sid)
    if kind == "artist":
        result = await c.services.view.get_artist_with_albums(internal, user=c.user)
        if result is None:
            raise SubsonicError(70, "Artist not found")
        artist, albums = result
        children = [m.to_album_child(a) for a in albums]
        return c.render(
            "directory", {"id": sid, "name": artist.name, "child": children}
        )
    if kind == "album":
        album = await c.services.view.get_album(internal, user=c.user)
        if album is None:
            raise SubsonicError(70, "Album not found")
        tracks = await c.services.view.get_album_tracks(internal, user=c.user)
        parent = encode("artist", album.artist_mbid) if album.artist_mbid else None
        return c.render(
            "directory",
            {
                "id": sid,
                "parent": parent,
                "name": album.title,
                "child": [c.child(t) for t in tracks],
            },
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
    _validate_music_folder(c)
    q = _normalize_search_query(c.p("query"))
    a_count = c.pint("artistCount", 20, minimum=0, maximum=500) or 0
    a_offset = c.pint("artistOffset", 0, minimum=0, maximum=2_147_483_647) or 0
    al_count = c.pint("albumCount", 20, minimum=0, maximum=500) or 0
    al_offset = c.pint("albumOffset", 0, minimum=0, maximum=2_147_483_647) or 0
    s_count = c.pint("songCount", 20, minimum=0, maximum=500) or 0
    s_offset = c.pint("songOffset", 0, minimum=0, maximum=2_147_483_647) or 0

    artists = []
    if a_count:
        artists, _ = await c.services.view.get_artists(
            limit=a_count, offset=a_offset, q=q, user=c.user
        )
    albums = []
    if al_count:
        albums, _ = await c.services.view.get_albums_offset(
            limit=al_count, offset=al_offset, q=q, user=c.user
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
    return c.render(
        "searchResult3",
        {
            "artist": [m.to_artist_id3(a) for a in artists],
            "album": [m.to_album_id3(a) for a in albums],
            "song": [c.child(t) for t in songs],
        },
    )


@endpoint("search2")
async def _search2(c: Ctx) -> Response:
    artists, albums, songs = await _search(c)
    return c.render(
        "searchResult2",
        {
            "artist": [m.to_artist_file(a) for a in artists],
            "album": [m.to_album_child(a) for a in albums],
            "song": [c.child(t) for t in songs],
        },
    )


_PLACEHOLDER_SVG = (
    '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 200 200">'
    '<rect fill="#374151" width="200" height="200"/>'
    '<circle cx="100" cy="100" r="70" fill="#1f2937" stroke="#4B5563" stroke-width="2"/>'
    '<circle cx="100" cy="100" r="12" fill="#4B5563"/></svg>'
).encode()


def _cover_size(px: int | None) -> str:
    if px is None:
        return "500"
    if px <= 300:
        return "250"
    if px <= 750:
        return "500"
    return "1200"


def _placeholder() -> Response:
    return Response(
        content=_PLACEHOLDER_SVG,
        media_type="image/svg+xml",
        headers={"Cache-Control": "public, max-age=86400"},
    )


@endpoint("getCoverArt")
async def _get_cover_art(c: Ctx) -> Response:
    kind, internal = decode(c.p("id") or "")  # unknown prefix -> 70 -> 404 (binary)
    size = _cover_size(c.pint("size", minimum=1, maximum=2_000))
    disc = c.request.is_disconnected
    result = None
    if kind == "album":
        if await c.services.view.get_album(internal, user=c.user) is None:
            raise SubsonicError(70, "Album not found")
        result = await c.services.coverart.get_release_group_cover(
            internal, size, is_disconnected=disc
        )
    elif kind == "track":
        track = await c.services.view.get_track(internal)
        if track is None:
            raise SubsonicError(70, "Song not found")
        if track.rg_mbid:
            result = await c.services.coverart.get_release_group_cover(
                track.rg_mbid, size, is_disconnected=disc
            )
    elif kind == "artist":
        if await c.services.view.get_artist_with_albums(internal, user=c.user) is None:
            raise SubsonicError(70, "Artist not found")
        px = int(size) if size.isdigit() else None
        result = await c.services.coverart.get_artist_image(
            internal, px, is_disconnected=disc
        )
    elif kind == "playlist":
        path = await c.services.playlists.get_cover_path(internal, c.user)
        if path is None:
            return _placeholder()
        media_type = {
            ".jpg": "image/jpeg",
            ".jpeg": "image/jpeg",
            ".png": "image/png",
            ".webp": "image/webp",
        }.get(path.suffix.lower(), "application/octet-stream")
        if c.request.method == "HEAD":
            stat = await asyncio.to_thread(path.stat)
            return Response(
                media_type=media_type,
                headers={
                    "Content-Length": str(stat.st_size),
                    "Cache-Control": "private, max-age=3600",
                },
            )
        return FileResponse(
            path,
            media_type=media_type,
            headers={"Cache-Control": "private, max-age=3600"},
        )
    else:
        raise SubsonicError(70, "Cover art target not found")
    if result:
        data, content_type, _ = result
        return Response(
            content=data,
            media_type=content_type,
            headers={"Cache-Control": "public, max-age=31536000, immutable"},
        )
    return _placeholder()


async def _serve_file(
    c: Ctx, file_id: str, *, content_disposition: str | None = None
) -> Response:
    from services.compat.stream_concurrency import (
        StreamCapacityError,
        leased_chunks,
    )

    if c.request.method == "HEAD":
        headers = await c.services.local_files.head_track(file_id)
        headers["Content-Encoding"] = "identity"
        if content_disposition:
            headers["Content-Disposition"] = content_disposition
        return Response(headers=headers, media_type=headers.get("Content-Type"))
    range_header = c.request.headers.get("Range")
    try:
        chunks, headers, status = await c.services.local_files.stream_track(
            file_id, range_header=range_header
        )
    except RangeNotSatisfiableError as exc:
        return Response(
            status_code=416,
            headers={"Content-Range": f"bytes */{exc.file_size}"},
        )
    try:
        lease = await c.services.stream_concurrency.acquire_direct(c.user.id)
    except StreamCapacityError:
        return Response(status_code=429, headers={"Retry-After": "1"})
    # 05 s10: keep gzip off audio; it drops Content-Length and breaks Range/seeking
    out_headers = {**headers, "Content-Encoding": "identity"}
    if content_disposition:
        out_headers["Content-Disposition"] = content_disposition
    try:
        return StreamingResponse(
            leased_chunks(chunks, lease),
            status_code=status,
            headers=out_headers,
            media_type=headers.get("Content-Type", "application/octet-stream"),
            background=BackgroundTask(lease.release),
        )
    except BaseException:
        await lease.release()
        raise


@endpoint("stream")
async def _stream(c: Ctx) -> Response:
    from services.compat.stream_concurrency import StreamCapacityError
    from services.compat.transcode_service import decide, ffmpeg_available

    fid = _decode_expect(c.p("id") or "", "track")
    fmt = c.decoded.enum("format", {"raw", "mp3", "opus"})
    max_bitrate = c.pint("maxBitRate", minimum=0, maximum=1_000_000)
    time_offset = c.pfloat("timeOffset", 0.0, minimum=0, maximum=604_800) or 0.0
    estimate = c.pbool("estimateContentLength", False)
    if fmt == "raw":  # force original, skip the track lookup
        return await _serve_file(c, fid)
    track = await c.services.view.get_track(fid)
    if track is None:
        raise SubsonicError(70, "Song not found")
    settings = c.services.preferences.get_connect_apps_settings()
    plan = decide(
        track,
        requested_format=fmt,
        max_bitrate_kbps=max_bitrate,
        force_original=False,
        start_seconds=time_offset,
        settings=settings,
        ffmpeg_available=ffmpeg_available(),
    )
    if not plan.transcode:
        return await _serve_file(c, fid)
    path = await c.services.local_files.resolve_validated_path(fid)
    try:
        return await c.services.transcode.stream(
            str(path),
            plan,
            principal=c.user.id,
            is_disconnected=c.request.is_disconnected,
            estimate=estimate,
        )
    except StreamCapacityError:
        return Response(status_code=429, headers={"Retry-After": "1"})


@endpoint("download")
async def _download(c: Ctx) -> Response:
    fid = _decode_expect(c.p("id") or "", "track")
    track = await c.services.view.get_track(fid, user=c.user)
    if track is None:
        raise SubsonicError(70, "Song not found")
    suffix = re.sub(r"[^a-z0-9]", "", (track.file_format or "").lower()) or "bin"
    stem = re.sub(r"[^A-Za-z0-9._ -]", "_", track.title).strip(" .") or "track"
    filename = f"{stem}.{suffix}"
    disposition = f'attachment; filename="{filename.replace(chr(34), "_")}"'
    return await _serve_file(c, fid, content_disposition=disposition)


def _stream_details(value) -> m.SStreamDetails | None:
    if value is None:
        return None
    return m.SStreamDetails(
        protocol=value.protocol,
        container=value.container,
        codec=value.codec,
        audioChannels=value.audio_channels,
        audioBitrate=value.audio_bitrate,
        audioSamplerate=value.audio_samplerate,
        audioBitdepth=value.audio_bitdepth,
    )


@endpoint("getTranscodeDecision")
async def _get_transcode_decision(c: Ctx) -> Response:
    from services.compat.advanced_transcode_service import (
        AdvancedClientInfo,
        AdvancedCodecLimitation,
        AdvancedCodecProfile,
        AdvancedDirectPlayProfile,
        AdvancedTranscodingProfile,
    )

    if c.request.method != "POST":
        raise SubsonicError(10, "getTranscodeDecision requires POST")
    if (
        c.request.headers.get("content-type", "").split(";", 1)[0].strip().lower()
        != "application/json"
    ):
        raise SubsonicError(10, "getTranscodeDecision requires JSON")
    body = await c.request.body()
    if not body or len(body) > 64 * 1024:
        raise SubsonicError(10, "Invalid transcode client information")
    try:
        client = msgspec.json.decode(body, type=m.SClientInfo)
    except msgspec.DecodeError as exc:
        raise SubsonicError(10, "Invalid transcode client information") from exc
    if c.decoded.enum("mediaType", {"song", "podcast"}) != "song":
        raise SubsonicError(10, "Only song transcoding is supported")
    file_id = _decode_expect(c.p("mediaId") or "", "track")
    track = await c.services.view.get_track(file_id, user=c.user)
    if track is None:
        raise SubsonicError(70, "Song not found")
    internal = AdvancedClientInfo(
        name=client.name,
        platform=client.platform,
        max_audio_bitrate=client.maxAudioBitrate,
        max_transcoding_audio_bitrate=client.maxTranscodingAudioBitrate,
        direct_play_profiles=tuple(
            AdvancedDirectPlayProfile(
                tuple(profile.containers),
                tuple(profile.audioCodecs),
                tuple(profile.protocols),
                profile.maxAudioChannels,
            )
            for profile in client.directPlayProfiles
        ),
        transcoding_profiles=tuple(
            AdvancedTranscodingProfile(
                profile.container,
                profile.audioCodec,
                profile.protocol,
                profile.maxAudioChannels,
            )
            for profile in client.transcodingProfiles
        ),
        codec_profiles=tuple(
            AdvancedCodecProfile(
                profile.type,
                profile.name,
                tuple(
                    AdvancedCodecLimitation(
                        limitation.name,
                        limitation.comparison,
                        tuple(limitation.values),
                        limitation.required,
                    )
                    for limitation in profile.limitations
                ),
            )
            for profile in client.codecProfiles
        ),
    )
    decision = c.services.advanced_transcode.decide(
        track,
        internal,
        user_id=c.user.id,
        settings=c.services.preferences.get_connect_apps_settings(),
    )
    return c.render(
        "transcodeDecision",
        m.STranscodeDecision(
            canDirectPlay=decision.can_direct_play,
            canTranscode=decision.can_transcode,
            transcodeReason=list(decision.transcode_reason),
            errorReason=decision.error_reason,
            transcodeParams=decision.transcode_params,
            sourceStream=_stream_details(decision.source_stream),
            transcodeStream=_stream_details(decision.transcode_stream),
        ),
    )


@endpoint("getTranscodeStream")
async def _get_transcode_stream(c: Ctx) -> Response:
    from services.compat.stream_concurrency import StreamCapacityError
    from services.compat.transcode_service import StreamPlan

    if c.decoded.enum("mediaType", {"song", "podcast"}) != "song":
        raise SubsonicError(10, "Only song transcoding is supported")
    file_id = _decode_expect(c.p("mediaId") or "", "track")
    params = c.decoded.string("transcodeParams", max_length=8192)
    if not params:
        raise SubsonicError(10, "Required parameter 'transcodeParams' is missing")
    offset = c.pint("offset", 0, minimum=0, maximum=604_800) or 0
    track = await c.services.view.get_track(file_id, user=c.user)
    if track is None:
        raise SubsonicError(70, "Song not found")
    direct, output_format, bitrate = c.services.advanced_transcode.decode_params(
        params, user_id=c.user.id, file_id=file_id
    )
    if direct:
        return await _serve_file(c, file_id)
    settings = c.services.preferences.get_connect_apps_settings()
    if not settings.transcoding_enabled:
        raise SubsonicError(0, "Transcoding is disabled")
    if output_format is None or bitrate is None:
        raise SubsonicError(10, "Invalid transcode parameters")
    path = await c.services.local_files.resolve_validated_path(file_id)
    try:
        return await c.services.transcode.stream(
            str(path),
            StreamPlan(
                transcode=True,
                out_format=output_format,
                out_bitrate_kbps=bitrate,
                start_seconds=float(offset),
                source_duration_seconds=track.duration_seconds,
            ),
            principal=c.user.id,
            is_disconnected=c.request.is_disconnected,
            estimate=False,
        )
    except StreamCapacityError:
        return Response(status_code=429, headers={"Retry-After": "1"})


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
        id=encode("playlist", r.id),
        name=r.name,
        owner=owner,
        public=r.is_public,
        songCount=len(songs),
        duration=total,
        created=r.created_at,
        changed=r.updated_at,
        coverArt=_playlist_cover(r),
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
        playlists.append(
            m.SPlaylist(
                id=encode("playlist", record.id),
                name=record.name,
                owner=owner,
                public=record.is_public,
                songCount=song_count,
                duration=duration,
                created=record.created_at,
                changed=record.updated_at,
                coverArt=_playlist_cover(record),
            )
        )
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
    remove_indices = [
        SubsonicParameters({"songIndexToRemove": [value]}).integer(
            "songIndexToRemove", minimum=0, maximum=2_147_483_647
        )
        for value in c.plist("songIndexToRemove")
    ]
    if name is not None:
        await c.services.playlists.update_playlist(pid, c.user, name=name)
    if public is not None:
        is_pub = SubsonicParameters({"public": [public]}).boolean("public", False)
        await c.services.playlists.set_public(pid, c.user, is_pub)
    if remove_indices:
        entries = await c.services.playlists.get_tracks(pid)
        ids = [entries[i].id for i in remove_indices if i < len(entries)]
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
    return list(dict.fromkeys(targets))


async def _validated_star_targets(c: Ctx) -> list[tuple[str, str]]:
    targets = _collect_star_targets(c)
    if not targets:
        raise SubsonicError(10, "At least one favorite target is required")
    if await c.services.view.missing_targets(targets):
        raise SubsonicError(70, "Favorite target not found")
    return targets


@endpoint("star")
async def _star(c: Ctx) -> Response:
    targets = await _validated_star_targets(c)
    await c.services.favorites.apply_many(c.user.id, targets, add=True)
    return c.render(None, None)


@endpoint("unstar")
async def _unstar(c: Ctx) -> Response:
    targets = await _validated_star_targets(c)
    await c.services.favorites.apply_many(c.user.id, targets, add=False)
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
    _validate_music_folder(c)
    artists, albums, songs = await _starred_lists(c)
    return c.render(
        "starred2",
        {
            "artist": [m.to_artist_id3(a) for a in artists],
            "album": [m.to_album_id3(a) for a in albums],
            "song": [c.child(t) for t in songs],
        },
    )


@endpoint("getStarred")
async def _get_starred(c: Ctx) -> Response:
    _validate_music_folder(c)
    artists, albums, songs = await _starred_lists(c)
    return c.render(
        "starred",
        {
            "artist": [m.to_artist_file(a) for a in artists],
            "album": [m.to_album_child(a) for a in albums],
            "song": [c.child(t) for t in songs],
        },
    )


@endpoint("setRating")
async def _set_rating(c: Ctx) -> Response:
    rating = c.pint("rating", minimum=0, maximum=5)
    if rating is None:
        raise SubsonicError(10, "Required parameter 'rating' is missing")
    kind, internal = decode(c.p("id") or "")
    if kind not in _FAV_KINDS or await c.services.view.missing_targets(
        [(kind, internal)]
    ):
        raise SubsonicError(70, "Rating target not found")
    # D11: validate target/range, then deliberately do not persist a rating.
    return c.render(None, None)


@endpoint("scrobble")
async def _scrobble(c: Ctx) -> Response:
    ids = c.plist("id")
    if not ids:
        raise SubsonicError(10, "Required parameter 'id' is missing")
    times = c.plist("time")  # ms epoch, positionally parallel to ids
    if times and len(times) != len(ids):
        raise SubsonicError(
            0, f"Wrong number of timestamps: {len(times)}, should be {len(ids)}"
        )
    timestamps = [
        SubsonicParameters({"time": [value]}).integer(
            "time", minimum=0, maximum=9_007_199_254_740_991
        )
        for value in times
    ]
    submission = c.pbool("submission", True)
    client = c.p("c")
    if not submission:
        fid = _decode_expect(ids[0], "track")
        await c.services.scrobble.now_playing(
            fid,
            user_id=c.user.id,
            client=client,
            user_name=getattr(c.user, "display_name", ""),
        )
        return c.render(None, None)
    for i, sid in enumerate(ids):
        fid = _decode_expect(sid, "track")
        played_at = timestamps[i] / 1000.0 if timestamps else None
        await c.services.scrobble.scrobble(
            fid,
            user_id=c.user.id,
            client=client,
            played_at=played_at,
            user_name=getattr(c.user, "display_name", ""),
        )
    return c.render(None, None)


@endpoint("getNowPlaying")
async def _get_now_playing(c: Ctx) -> Response:
    entries = []
    now = time.time()
    for idx, (proj, fid, updated_at) in enumerate(
        c.services.now_playing.compat_now_playing()
    ):
        track = await c.services.view.get_track(fid, user=c.user)
        if track is None:
            continue
        child = c.child(track)
        entries.append(
            m.SNowPlayingEntry(
                **msgspec.structs.asdict(child),
                username=proj.user_name,
                minutesAgo=max(int((now - updated_at) // 60), 0),
                playerId=idx,
                playerName=proj.source or proj.device_name or None,
            )
        )
    return c.render("nowPlaying", {"entry": entries})


@endpoint("reportPlayback")
async def _report_playback(c: Ctx) -> Response:
    if c.decoded.enum("mediaType", {"song", "podcast"}) != "song":
        raise SubsonicError(10, "Only song playback reports are supported")
    file_id = _decode_expect(c.p("mediaId") or "", "track")
    position_ms = c.pint("positionMs", minimum=0, maximum=_MAX_MEDIA_POSITION_MS)
    if position_ms is None:
        raise SubsonicError(10, "Required parameter 'positionMs' is missing")
    state = c.decoded.enum("state", {"starting", "playing", "paused", "stopped"})
    if state is None:
        raise SubsonicError(10, "Required parameter 'state' is missing")
    c.pfloat("playbackRate", 1.0, minimum=0.01, maximum=16.0)
    await c.services.playback_report.report(
        file_id,
        user_id=c.user.id,
        user_name=c.user.display_name,
        client=c.p("c", "") or "",
        position_ms=position_ms,
        state=state,
        ignore_scrobble=c.pbool("ignoreScrobble", False),
    )
    return c.render(None, None)


_MAX_QUEUE_ITEMS = 500
_MAX_MEDIA_POSITION_MS = 604_800_000


async def _validated_queue_ids(c: Ctx) -> tuple[str, ...]:
    ids = c.plist("id")
    if len(ids) > _MAX_QUEUE_ITEMS:
        raise SubsonicError(10, "Play queue exceeds the 500 item limit")
    file_ids = tuple(_decode_expect(item, "track") for item in ids)
    missing = await c.services.view.missing_targets(
        [("track", file_id) for file_id in dict.fromkeys(file_ids)]
    )
    if missing:
        raise SubsonicError(70, "Play queue song not found")
    return file_ids


async def _queue_entries(c: Ctx):
    queue = await c.services.play_queue.get(c.user.id)
    mapped = await c.services.view.get_tracks_by_file_ids(
        list(queue.file_ids), user=c.user
    )
    tracks = []
    retained_indices = []
    for index, file_id in enumerate(queue.file_ids):
        track = mapped.get(file_id)
        if track is not None:
            tracks.append(c.child(track))
            retained_indices.append(index)
    current_index = None
    if tracks:
        if queue.current_index in retained_indices:
            current_index = retained_indices.index(queue.current_index)
        else:
            current_index = 0
    return queue, tracks, current_index


@endpoint("getPlayQueue")
async def _get_play_queue(c: Ctx) -> Response:
    queue, tracks, current_index = await _queue_entries(c)
    current = tracks[current_index].id if current_index is not None else None
    return c.render(
        "playQueue",
        m.SPlayQueue(
            username=c.user.username or c.user.display_name,
            changed=m.iso(queue.updated_at) or "1970-01-01T00:00:00Z",
            changedBy=queue.changed_by_client,
            current=current,
            position=queue.position_ms if current is not None else None,
            entry=tracks,
        ),
    )


@endpoint("savePlayQueue")
async def _save_play_queue(c: Ctx) -> Response:
    file_ids = await _validated_queue_ids(c)
    current = c.p("current")
    position = c.pint("position", 0, minimum=0, maximum=_MAX_MEDIA_POSITION_MS) or 0
    current_index = None
    if file_ids:
        if current is None:
            raise SubsonicError(10, "current is required for a non-empty play queue")
        current_file_id = _decode_expect(current, "track")
        try:
            current_index = file_ids.index(current_file_id)
        except ValueError as exc:
            raise SubsonicError(10, "current must reference a queued song") from exc
    elif current is not None:
        raise SubsonicError(10, "current is invalid for an empty play queue")
    await c.services.play_queue.replace(
        c.user.id,
        file_ids,
        current_index=current_index,
        position_ms=position,
        changed_by_client=c.p("c", "") or "",
    )
    return c.render(None, None)


@endpoint("getPlayQueueByIndex")
async def _get_play_queue_by_index(c: Ctx) -> Response:
    queue, tracks, current_index = await _queue_entries(c)
    return c.render(
        "playQueueByIndex",
        m.SPlayQueueByIndex(
            username=c.user.username or c.user.display_name,
            changed=m.iso(queue.updated_at) or "1970-01-01T00:00:00Z",
            changedBy=queue.changed_by_client,
            currentIndex=current_index,
            position=queue.position_ms if current_index is not None else None,
            entry=tracks,
        ),
    )


@endpoint("savePlayQueueByIndex")
async def _save_play_queue_by_index(c: Ctx) -> Response:
    file_ids = await _validated_queue_ids(c)
    current_index = c.pint("currentIndex", minimum=0, maximum=_MAX_QUEUE_ITEMS - 1)
    position = c.pint("position", 0, minimum=0, maximum=_MAX_MEDIA_POSITION_MS) or 0
    if file_ids and current_index is None:
        raise SubsonicError(10, "currentIndex is required for a non-empty play queue")
    if current_index is not None and current_index >= len(file_ids):
        raise SubsonicError(10, "currentIndex is outside the play queue")
    await c.services.play_queue.replace(
        c.user.id,
        file_ids,
        current_index=current_index,
        position_ms=position,
        changed_by_client=c.p("c", "") or "",
    )
    return c.render(None, None)


@endpoint("getBookmarks")
async def _get_bookmarks(c: Ctx) -> Response:
    bookmarks = []
    records = await c.services.bookmarks.list(c.user.id)
    mapped = await c.services.view.get_tracks_by_file_ids(
        [record.file_id for record in records], user=c.user
    )
    for bookmark in records:
        track = mapped.get(bookmark.file_id)
        if track is None:
            continue
        bookmarks.append(
            m.SBookmark(
                position=bookmark.position_ms,
                username=c.user.username or c.user.display_name,
                created=m.iso(bookmark.created_at) or "1970-01-01T00:00:00Z",
                changed=m.iso(bookmark.changed_at) or "1970-01-01T00:00:00Z",
                comment=bookmark.comment or None,
                entry=c.child(track),
            )
        )
    return c.render("bookmarks", {"bookmark": bookmarks})


@endpoint("createBookmark")
async def _create_bookmark(c: Ctx) -> Response:
    file_id = _decode_expect(c.p("id") or "", "track")
    position = c.pint("position", minimum=0, maximum=_MAX_MEDIA_POSITION_MS)
    if position is None:
        raise SubsonicError(10, "Required parameter 'position' is missing")
    comment = c.decoded.string("comment", "", max_length=4096) or ""
    if await c.services.view.missing_targets([("track", file_id)]):
        raise SubsonicError(70, "Bookmark song not found")
    await c.services.bookmarks.upsert(c.user.id, file_id, position, comment)
    return c.render(None, None)


@endpoint("deleteBookmark")
async def _delete_bookmark(c: Ctx) -> Response:
    file_id = _decode_expect(c.p("id") or "", "track")
    if await c.services.view.missing_targets([("track", file_id)]):
        raise SubsonicError(70, "Bookmark song not found")
    await c.services.bookmarks.delete(c.user.id, file_id)
    return c.render(None, None)


@endpoint("getArtistInfo2", "getArtistInfo")
async def _get_artist_info(c: Ctx) -> Response:
    artist_mbid = _decode_expect(c.p("id") or "", "artist")
    c.pint("count", 20, minimum=0, maximum=500)
    c.pbool("includeNotPresent", False)
    result = await c.services.view.get_artist_with_albums(artist_mbid, user=c.user)
    if result is None:
        raise SubsonicError(70, "Artist not found")
    artist, _albums = result
    cover_id = encode("artist", artist_mbid)
    cover_base = (
        f"{str(c.request.base_url).rstrip('/')}/subsonic/rest/getCoverArt?id={cover_id}"
    )
    return c.render(
        "artistInfo2" if c.endpoint_name == "getartistinfo2" else "artistInfo",
        m.SArtistInfo(
            musicBrainzId=(
                artist.musicbrainz_artist_id
                if artist.provider_identity_projected
                else (artist.artist_mbid if "-" in artist.artist_mbid else None)
            ),
            smallImageUrl=f"{cover_base}&size=250",
            mediumImageUrl=f"{cover_base}&size=500",
            largeImageUrl=f"{cover_base}&size=1200",
        ),
    )


@endpoint("getAlbumInfo2", "getAlbumInfo")
async def _get_album_info(c: Ctx) -> Response:
    release_group_mbid = _decode_expect(c.p("id") or "", "album")
    album = await c.services.view.get_album(release_group_mbid, user=c.user)
    if album is None:
        raise SubsonicError(70, "Album not found")
    cover_id = encode("album", release_group_mbid)
    cover_base = (
        f"{str(c.request.base_url).rstrip('/')}/subsonic/rest/getCoverArt?id={cover_id}"
    )
    return c.render(
        "albumInfo" if c.endpoint_name == "getalbuminfo" else "albumInfo2",
        m.SAlbumInfo(
            musicBrainzId=(
                album.musicbrainz_release_group_id
                if album.provider_identity_projected
                else album.rg_mbid
            ),
            smallImageUrl=f"{cover_base}&size=250",
            mediumImageUrl=f"{cover_base}&size=500",
            largeImageUrl=f"{cover_base}&size=1200",
        ),
    )


@endpoint("getAvatar")
async def _get_avatar(c: Ctx) -> Response:
    username = c.p("username")
    own_names = {
        (c.user.username or "").casefold(),
        (c.user.username_display or "").casefold(),
        c.user.display_name.casefold(),
    }
    if not username or username.casefold() not in own_names:
        return _binary_error(50, "Avatar access is limited to the authenticated user")
    resolved = await asyncio.to_thread(c.services.avatars.resolve, c.user.id)
    if resolved is None:
        raise SubsonicError(70, "Avatar not found")
    path, media_type = resolved
    stat = await asyncio.to_thread(path.stat)
    headers = {
        "Content-Length": str(stat.st_size),
        "Cache-Control": "private, max-age=3600",
    }
    if c.request.method == "HEAD":
        return Response(headers=headers, media_type=media_type)
    return FileResponse(path, media_type=media_type, headers=headers)


def _structured_lyrics(lyrics, track) -> m.SStructuredLyrics:
    return m.SStructuredLyrics(
        lang=lyrics.language,
        synced=lyrics.synced,
        line=[
            m.SLyricsLine(value=line.value, start=line.start_ms)
            for line in lyrics.lines
        ],
        displayArtist=track.artist_name or None,
        displayTitle=track.title or None,
    )


@endpoint("getLyricsBySongId")
async def _get_lyrics_by_song_id(c: Ctx) -> Response:
    file_id = _decode_expect(c.p("id") or "", "track")
    track = await c.services.view.get_track(file_id, user=c.user)
    if track is None:
        raise SubsonicError(70, "Song not found")
    lyrics = await c.services.lyrics.get(file_id)
    values = [_structured_lyrics(lyrics, track)] if lyrics is not None else []
    return c.render("lyricsList", {"structuredLyrics": values})


@endpoint("getLyrics")
async def _get_lyrics(c: Ctx) -> Response:
    artist = c.p("artist")
    title = c.p("title")
    if not title:
        raise SubsonicError(10, "Required parameter 'title' is missing")
    tracks, _ = await c.services.view.get_tracks_page(
        limit=100, offset=0, q=title, user=c.user
    )
    candidates = [
        track for track in tracks if track.title.casefold() == title.casefold()
    ]
    if artist:
        candidates = [
            track
            for track in candidates
            if track.artist_name.casefold() == artist.casefold()
        ]
    if not candidates:
        return c.render("lyrics", m.SLyrics(artist=artist or "", title=title, value=""))
    track = sorted(candidates, key=lambda item: item.file_id)[0]
    lyrics = await c.services.lyrics.get(track.file_id)
    value = "\n".join(line.value for line in lyrics.lines) if lyrics else ""
    return c.render(
        "lyrics",
        m.SLyrics(artist=track.artist_name, title=track.title, value=value),
    )


@endpoint("getGenres")
async def _get_genres(c: Ctx) -> Response:
    genres = await c.services.view.get_genres()
    return c.render("genres", {"genre": [m.to_genre(g) for g in genres]})


@endpoint("getSongsByGenre")
async def _get_songs_by_genre(c: Ctx) -> Response:
    _validate_music_folder(c)
    genre = c.p("genre")
    if not genre:
        raise SubsonicError(10, "Required parameter 'genre' is missing")
    count = c.pint("count", 10, minimum=1, maximum=500) or 10
    offset = c.pint("offset", 0, minimum=0, maximum=2_147_483_647) or 0
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
    settings = c.services.preferences.get_connect_apps_settings()
    return c.render(
        "user",
        m.SUser(
            username=c.user.username or c.user.display_name,
            adminRole=is_admin,
            settingsRole=is_admin,
            maxBitRate=settings.transcode_max_bitrate_kbps,
        ),
    )


@endpoint("getScanStatus")
async def _get_scan_status(c: Ctx) -> Response:
    scanning, count = await c.services.scan.status()
    return c.render("scanStatus", m.SScanStatus(scanning=scanning, count=count))


@endpoint("startScan")
async def _start_scan(c: Ctx) -> Response:
    if c.user.role != "admin":
        raise SubsonicError(50, "Administrator role is required to start a scan")
    await c.services.scan.start()
    return c.render("scanStatus", m.SScanStatus(scanning=True, count=0))


@endpoint("getTopSongs")
async def _get_top_songs(c: Ctx) -> Response:
    artist = c.p("artist")
    if not artist:
        raise SubsonicError(10, "Required parameter 'artist' is missing")
    count = c.pint("count", 50, minimum=1, maximum=500) or 50
    artists, _ = await c.services.view.get_artists(limit=10, q=artist, user=c.user)
    if not any(item.name.casefold() == artist.casefold() for item in artists):
        raise SubsonicError(70, "Artist not found")
    tracks = await c.services.discover.get_top_songs(
        artist, user_id=c.user.id, count=count, user=c.user
    )
    return c.render("topSongs", {"song": [c.child(t) for t in tracks]})


async def _similar_songs(c: Ctx) -> list:
    artist_mbid = _decode_expect(c.p("id") or "", "artist")
    count = c.pint("count", 50, minimum=1, maximum=500) or 50
    if await c.services.view.get_artist_with_albums(artist_mbid, user=c.user) is None:
        raise SubsonicError(70, "Artist not found")
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
