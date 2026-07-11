"""Jellyfin shim router (04-jellyfin.md)."""

from __future__ import annotations

import base64
import inspect
import logging
from datetime import datetime, timezone
from typing import Awaitable, Callable

from uuid import uuid4

from fastapi import APIRouter, Depends, Request
from fastapi.responses import Response, StreamingResponse

from api.compat.common.deps import CompatServices, get_compat_services
from api.compat.jellyfin import models as jm
from api.compat.jellyfin.auth import (
    extract_client,
    extract_device,
    extract_token,
    resolve_user,
)
from api.compat.jellyfin.errors import to_jellyfin_status
from api.compat.jellyfin.serialization import (
    decode_body,
    error_response,
    jellyfin_response,
    no_content,
)
from core.exceptions import DroppedNeedleException, ExternalServiceError, JellyfinError
from infrastructure.constants import JELLYFIN_TICKS_PER_SECOND
from infrastructure.msgspec_fastapi import MsgSpecRoute

logger = logging.getLogger(__name__)

router = APIRouter(prefix="/jellyfin", route_class=MsgSpecRoute)

Handler = Callable[[Request, CompatServices, object], Awaitable[object]]


def _to_response(result: object) -> Response:
    if isinstance(result, Response):
        return result
    if result is None:
        return no_content()
    return jellyfin_response(result)


async def _handle(
    request: Request,
    services: CompatServices,
    fn: Handler,
    *,
    auth: bool = True,
    **extra,
) -> Response:
    try:
        settings = services.preferences.get_connect_apps_settings()
        if not settings.jellyfin_enabled:
            raise JellyfinError(404, "Jellyfin API is disabled")
        user = await resolve_user(request, services.app_passwords) if auth else None
        result = fn(request, services, user, **extra)
        if inspect.isawaitable(result):
            result = await result
        return _to_response(result)
    except Exception as exc:  # noqa: BLE001 - boundary: never reach global handlers
        if not isinstance(exc, DroppedNeedleException):
            logger.exception("Unhandled error in Jellyfin handler %s", getattr(fn, "__name__", fn))
        status, body = to_jellyfin_status(exc)
        return error_response(status, body)


def _local_address(request: Request) -> str:
    proto = request.headers.get("x-forwarded-proto") or request.url.scheme
    host = request.headers.get("x-forwarded-host") or request.url.netloc
    return f"{proto}://{host}/jellyfin"


# ===== System / identity =====

@router.get("/System/Info/Public")
async def system_info_public(
    request: Request, services: CompatServices = Depends(get_compat_services)
) -> Response:
    return await _handle(request, services, _public_info, auth=False)


async def _public_info(request, services, _user) -> jm.PublicSystemInfo:
    settings = services.preferences.get_connect_apps_settings()
    return jm.PublicSystemInfo(
        LocalAddress=_local_address(request),
        ServerName=settings.advertise_server_name,
        Version=settings.advertise_server_version,
    )


@router.get("/System/Info")
async def system_info(
    request: Request, services: CompatServices = Depends(get_compat_services)
) -> Response:
    return await _handle(request, services, _system_info)


async def _system_info(request, services, _user) -> jm.SystemInfo:
    settings = services.preferences.get_connect_apps_settings()
    return jm.SystemInfo(
        LocalAddress=_local_address(request),
        ServerName=settings.advertise_server_name,
        Version=settings.advertise_server_version,
    )


@router.get("/QuickConnect/Enabled")
async def quick_connect_enabled(
    request: Request, services: CompatServices = Depends(get_compat_services)
) -> Response:
    return await _handle(
        request, services, lambda r, s, u: Response(b"false", media_type="application/json"),
        auth=False,
    )


@router.post("/Sessions/Logout")
async def sessions_logout(
    request: Request, services: CompatServices = Depends(get_compat_services)
) -> Response:
    return await _handle(request, services, lambda r, s, u: None, auth=False)


# ===== Auth / user =====

def _user_dto(user) -> jm.UserDto:
    return jm.UserDto(
        Id=user.id, Name=user.username_display or user.username or user.display_name,
        HasPassword=True,
        Policy={
            # Without EnableAllFolders strict clients (Manet) conclude "no libraries"
            # and never call /UserViews; rest are permissive defaults.
            "IsAdministrator": user.role == "admin",
            "IsHidden": False,
            "IsDisabled": False,
            "EnableAllFolders": True,
            "EnabledFolders": [],
            "EnableAllChannels": True,
            "EnabledChannels": [],
            "EnableAllDevices": True,
            "EnabledDevices": [],
            "EnableMediaPlayback": True,
            "EnableAudioPlaybackTranscoding": True,
            "EnableVideoPlaybackTranscoding": True,
            "EnablePlaybackRemuxing": True,
            "EnableContentDownloading": True,
            "EnableRemoteAccess": True,
            "EnableSyncTranscoding": True,
            "EnableUserPreferenceAccess": True,
            "EnableLiveTvAccess": False,
            "EnableRemoteControlOfOtherUsers": False,
            "EnableSharedDeviceControl": False,
            "BlockedTags": [],
            "AllowedTags": [],
            "AccessSchedules": [],
            "BlockUnratedItems": [],
        },
    )


@router.post("/Users/AuthenticateByName")
async def authenticate_by_name(
    request: Request, services: CompatServices = Depends(get_compat_services)
) -> Response:
    return await _handle(request, services, _authenticate, auth=False)


async def _authenticate(request, services, _user) -> jm.AuthenticationResult:
    from core.exceptions import PermissionDeniedError

    body = await decode_body(request, jm.AuthenticateRequest)
    if body is None:
        raise JellyfinError(400, "Missing body")
    try:
        user = await services.app_passwords.authenticate_username_password(
            body.Username, body.Pw, extract_client(request)
        )
    except PermissionDeniedError:
        raise JellyfinError(401, "Invalid username or password")  # login -> 401, not 403
    device_name, device_id = extract_device(request)
    session = jm.SessionInfo(
        Id=uuid4().hex,
        UserId=user.id,
        UserName=user.username_display or user.username or user.display_name,
        LastActivityDate=datetime.now(timezone.utc).isoformat(),
        Client=extract_client(request),
        DeviceName=device_name or "",
        DeviceId=device_id,
    )
    return jm.AuthenticationResult(
        User=_user_dto(user), AccessToken=body.Pw, SessionInfo=session
    )


@router.get("/Users/Me")
async def users_me(
    request: Request, services: CompatServices = Depends(get_compat_services)
) -> Response:
    return await _handle(request, services, lambda r, s, u: _user_dto(u))


@router.get("/Users/{user_id}")
async def users_by_id(
    user_id: str, request: Request,
    services: CompatServices = Depends(get_compat_services),
) -> Response:
    return await _handle(request, services, lambda r, s, u: _user_dto(u))


# ===== Library browsing =====

def _builder(services: CompatServices):
    from api.compat.jellyfin.builders import JellyfinBuilder

    return JellyfinBuilder(services.id_map, services.coverart, jm.SERVER_ID)


class _CIParams:
    """Case-insensitive query params: real Jellyfin (ASP.NET Core) binds query
    strings case-insensitively, so clients send mixed casing (parentId vs ParentId)."""

    __slots__ = ("_multi",)

    def __init__(self, qp) -> None:
        self._multi: dict[str, list[str]] = {}
        for key, value in qp.multi_items():
            self._multi.setdefault(key.lower(), []).append(value)

    def get(self, key: str, default: str | None = None) -> str | None:
        values = self._multi.get(key.lower())
        return values[0] if values else default

    def getlist(self, key: str) -> list[str]:
        return self._multi.get(key.lower(), [])

    def __contains__(self, key: str) -> bool:
        return key.lower() in self._multi


def _params(request: Request) -> _CIParams:
    return _CIParams(request.query_params)


def _csv_param(request: Request, key: str) -> list[str]:
    out: list[str] = []
    for value in _params(request).getlist(key):
        out.extend(p for p in value.split(",") if p)
    return out


def _wants_favorites(request: Request) -> bool:
    if (_params(request).get("isFavorite") or "").lower() == "true":
        return True
    return any(f.lower() == "isfavorite" for f in _csv_param(request, "Filters"))


def _wants_played(request: Request) -> bool:
    if (_params(request).get("isPlayed") or "").lower() == "true":
        return True
    return any(f.lower() == "isplayed" for f in _csv_param(request, "Filters"))


def _qint(request: Request, key: str, default: int) -> int:
    raw = _params(request).get(key)
    try:
        return int(raw) if raw not in (None, "") else default
    except ValueError:
        return default


async def _decode_artist(services: CompatServices, jf_id: str) -> str | None:
    try:
        kind, internal = await services.id_map.from_jf(jf_id)
    except JellyfinError:
        return None
    return internal if kind == "artist" else None


async def _build_qr(build_fn, items, total, start):
    built = [await build_fn(i) for i in items]
    return jm.BaseItemDtoQueryResult(Items=built, TotalRecordCount=total, StartIndex=start)


async def _build_page(build_fn, items, start, limit):
    total = len(items)
    page = items[start: start + limit] if limit else items[start:]
    return await _build_qr(build_fn, page, total, start)


def _music_view(library_id: str) -> jm.BaseItemDto:
    # Strict clients (Manet) report "No music libraries found" unless the view carries
    # UserData / non-empty ImageTags.Primary / LocationType (06-data-mapping).
    return jm.BaseItemDto(
        Id=library_id, Name="Music", Type="CollectionFolder", SortName="Music",
        IsFolder=True, MediaType="Unknown", CollectionType="music",
        ImageTags={"Primary": library_id},
        UserData=jm.UserItemDataDto(ItemId=library_id, Key=library_id),
    )


async def _views(request, services, user, **_) -> jm.BaseItemDtoQueryResult:
    library_id = await services.id_map.to_jf("library", "music")
    return jm.BaseItemDtoQueryResult(
        Items=[_music_view(library_id)], TotalRecordCount=1, StartIndex=0
    )


@router.get("/Users/{user_id}/Views")
async def user_views_legacy(
    user_id: str, request: Request,
    services: CompatServices = Depends(get_compat_services),
) -> Response:
    return await _handle(request, services, _views)


@router.get("/UserViews")
async def user_views_modern(
    request: Request, services: CompatServices = Depends(get_compat_services)
) -> Response:
    return await _handle(request, services, _views)


def _primary_type(types: set[str], parent_kind: str | None) -> str:
    for t in ("MusicArtist", "MusicAlbum", "Audio", "Playlist", "MusicGenre"):
        if t in types:
            return t
    return "MusicAlbum"


async def _browse(request, services, user, **_) -> jm.BaseItemDtoQueryResult:
    q = _params(request)
    b = _builder(services)
    start = max(_qint(request, "StartIndex", 0), 0)
    limit = max(_qint(request, "Limit", 100), 0)
    search = q.get("SearchTerm") or None
    types = set(_csv_param(request, "IncludeItemTypes"))
    ids = _csv_param(request, "Ids")
    album_artist_ids = _csv_param(request, "AlbumArtistIds")
    artist_ids = _csv_param(request, "ArtistIds")

    if ids:
        items = await _items_by_ids(services, b, ids, user)
        return await _build_page(_passthrough, items, start, limit)

    parent = q.get("ParentId")
    parent_kind = parent_internal = None
    if parent:
        try:
            parent_kind, parent_internal = await services.id_map.from_jf(parent)
        except JellyfinError:
            return jm.BaseItemDtoQueryResult(Items=[], TotalRecordCount=0, StartIndex=start)

    if _wants_favorites(request):
        return await _favorite_items(services, b, user, types, start, limit)

    if parent_kind == "album":
        tracks = await services.view.get_album_tracks(parent_internal, user=user)
        return await _build_page(b.audio, tracks, start, limit)

    primary = _primary_type(types, parent_kind)

    if primary == "MusicArtist":
        artists, total = await services.view.get_artists(
            limit=limit or 100_000, offset=start, q=search, user=user
        )
        return await _build_qr(b.artist, artists, total, start)

    if primary == "MusicGenre":
        genres = await services.view.get_genres()
        return await _build_page(b.genre, genres, start, limit)

    if primary == "Playlist":
        views = await services.playlists.get_all_playlists(user)
        streamable = await services.playlists.get_streamable_counts()
        vps = [
            _to_view_playlist(v.record, streamable)
            for v in views if getattr(v, "record", None)
        ]
        return await _build_page(b.playlist, vps, start, limit)

    if primary == "Audio":
        if album_artist_ids:
            mbids = [m for i in album_artist_ids if (m := await _decode_artist(services, i))]
            tracks = await services.view.get_tracks_by_album_artist_mbids(mbids, user=user)
            return await _build_page(b.audio, tracks, start, limit)
        if artist_ids:
            mbids = [m for i in artist_ids if (m := await _decode_artist(services, i))]
            tracks = await services.view.get_tracks_by_artist_mbids(mbids, user=user)
            return await _build_page(b.audio, tracks, start, limit)
        tracks, total = await services.view.get_tracks_page(
            limit=limit or 100, offset=start, q=search, user=user
        )
        return await _build_qr(b.audio, tracks, total, start)

    if album_artist_ids or artist_ids:
        albums = []
        for jf_id in (album_artist_ids or artist_ids):
            mb = await _decode_artist(services, jf_id)
            if mb:
                albums += await services.view.get_albums_for_artist(mb, user=user)
        return await _build_page(b.album, albums, start, limit)
    page = start // limit + 1 if limit else 1
    albums, total = await services.view.get_albums(
        page=page, page_size=limit or 100, q=search, user=user
    )
    return await _build_qr(b.album, albums, total, start)


async def _passthrough(item):
    return item


async def _items_by_ids(services, b, ids, user):
    built = []
    for jf_id in ids:
        try:
            kind, internal = await services.id_map.from_jf(jf_id)
        except JellyfinError:
            continue
        item = await _single_item(services, b, kind, internal, user)
        if item is not None:
            built.append(item)
    return built


async def _favorite_items(services, b, user, types, start, limit):
    primary = _primary_type(types, None)
    kind = {"MusicArtist": "artist", "MusicAlbum": "album", "Audio": "track"}.get(
        primary, "track"
    )
    favs = await services.favorites.list(user.id, kind)
    built = []
    for internal, _ in favs:
        item = await _single_item(services, b, kind, internal, user)
        if item is not None:
            built.append(item)
    return await _build_page(_passthrough, built, start, limit)


async def _single_item(services, b, kind, internal, user):
    if kind == "track":
        t = await services.view.get_track(internal, user=user)
        return await b.audio(t) if t else None
    if kind == "album":
        a = await services.view.get_album(internal, user=user)
        return await b.album(a) if a else None
    if kind == "artist":
        res = await services.view.get_artist_with_albums(internal, user=user)
        return await b.artist(res[0]) if res else None
    if kind == "playlist":
        for v in await services.playlists.get_all_playlists(user):
            rec = getattr(v, "record", None)
            if rec and rec.id == internal:
                streamable = await services.playlists.get_streamable_counts()
                return await b.playlist(_to_view_playlist(rec, streamable))
        return None
    return None


@router.get("/Users/{user_id}/Items")
async def items_legacy(
    user_id: str, request: Request,
    services: CompatServices = Depends(get_compat_services),
) -> Response:
    return await _handle(request, services, _browse)


@router.get("/Items")
async def items_modern(
    request: Request, services: CompatServices = Depends(get_compat_services)
) -> Response:
    return await _handle(request, services, _browse)


async def _artists(request, services, user, **_) -> jm.BaseItemDtoQueryResult:
    b = _builder(services)
    start = max(_qint(request, "StartIndex", 0), 0)
    limit = max(_qint(request, "Limit", 100), 0)
    search = _params(request).get("SearchTerm") or None
    artists, total = await services.view.get_artists(
        limit=limit or 100_000, offset=start, q=search, user=user
    )
    return await _build_qr(b.artist, artists, total, start)


@router.get("/Artists")
async def artists_all(
    request: Request, services: CompatServices = Depends(get_compat_services)
) -> Response:
    return await _handle(request, services, _artists)


@router.get("/Artists/AlbumArtists")
async def album_artists(
    request: Request, services: CompatServices = Depends(get_compat_services)
) -> Response:
    return await _handle(request, services, _artists)


async def _genres(request, services, user, **_) -> jm.BaseItemDtoQueryResult:
    b = _builder(services)
    start = max(_qint(request, "StartIndex", 0), 0)
    limit = max(_qint(request, "Limit", 100), 0)
    genres = await services.view.get_genres()
    return await _build_page(b.genre, genres, start, limit)


@router.get("/Genres")
async def genres_endpoint(
    request: Request, services: CompatServices = Depends(get_compat_services)
) -> Response:
    return await _handle(request, services, _genres)


@router.get("/MusicGenres")
async def music_genres_endpoint(
    request: Request, services: CompatServices = Depends(get_compat_services)
) -> Response:
    return await _handle(request, services, _genres)


async def _items_filters(request, services, user, **_) -> dict:
    # Clients (Manet) call this before listing; a 404 here can leave the library empty.
    # Genres are the only facet modelled; rest advertised empty.
    genres = await services.view.get_genres()
    return {
        "Genres": [g.name for g in genres],
        "Tags": [],
        "OfficialRatings": [],
        "Years": [],
    }


# MUST precede /Items/{item_id} (and the legacy dialect) or "Filters" is captured as an id.
@router.get("/Items/Filters")
async def items_filters(
    request: Request, services: CompatServices = Depends(get_compat_services)
) -> Response:
    return await _handle(request, services, _items_filters)


@router.get("/Users/{user_id}/Items/Filters")
async def items_filters_legacy(
    user_id: str, request: Request,
    services: CompatServices = Depends(get_compat_services),
) -> Response:
    return await _handle(request, services, _items_filters)


async def _single_item_handler(request, services, user, *, item_id) -> jm.BaseItemDto:
    b = _builder(services)
    try:
        kind, internal = await services.id_map.from_jf(item_id)
    except JellyfinError:
        raise JellyfinError(404, "Item not found")
    item = await _single_item(services, b, kind, internal, user)
    if item is None:
        raise JellyfinError(404, "Item not found")
    return item


@router.get("/Users/{user_id}/Items/{item_id}")
async def single_item_legacy(
    user_id: str, item_id: str, request: Request,
    services: CompatServices = Depends(get_compat_services),
) -> Response:
    return await _handle(request, services, _single_item_handler, item_id=item_id)


@router.get("/Items/{item_id}")
async def single_item_modern(
    item_id: str, request: Request,
    services: CompatServices = Depends(get_compat_services),
) -> Response:
    return await _handle(request, services, _single_item_handler, item_id=item_id)


# ===== Images (unauthenticated, reference s5.1) =====

# 1x1 opaque PNG for the library view's advertised ImageTags.Primary so the request
# resolves instead of 404ing (clients scale it).
_LIBRARY_COVER_PNG = base64.b64decode(
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAC0lEQVR4nGNgYGAAAAAEAAH2FzhVAAAAAElFTkSuQmCC"
)


def _image_size(request: Request) -> str:
    p = _params(request)
    for key in ("fillWidth", "maxWidth", "width", "fillHeight", "maxHeight", "height"):
        raw = p.get(key)
        if raw and raw.isdigit():
            px = int(raw)
            return "250" if px <= 300 else "500" if px <= 750 else "1200"
    return "500"


async def _image(request, services, _user, *, item_id, image_type):
    if image_type.lower() != "primary":
        raise JellyfinError(404, "No image")  # Backdrop/etc not served in v1
    try:
        kind, internal = await services.id_map.from_jf(item_id)
    except JellyfinError:
        raise JellyfinError(404, "Item not found")
    size = _image_size(request)
    disc = request.is_disconnected
    result = None
    if kind == "library":
        result = (_LIBRARY_COVER_PNG, "image/png", "library")
    elif kind == "album":
        result = await services.coverart.get_release_group_cover(internal, size, is_disconnected=disc)
    elif kind == "track":
        track = await services.view.get_track(internal)
        if track and track.rg_mbid:
            result = await services.coverart.get_release_group_cover(track.rg_mbid, size, is_disconnected=disc)
    elif kind == "artist":
        result = await services.coverart.get_artist_image(
            internal, int(size) if size.isdigit() else None, is_disconnected=disc
        )
    if not result:
        raise JellyfinError(404, "No image")
    data, content_type, _ = result
    return Response(
        content=data, media_type=content_type,
        headers={"Cache-Control": "public, max-age=31536000, immutable"},
    )


@router.get("/Items/{item_id}/Images/{image_type}")
async def item_image(
    item_id: str, image_type: str, request: Request,
    services: CompatServices = Depends(get_compat_services),
) -> Response:
    return await _handle(request, services, _image, auth=False,
                         item_id=item_id, image_type=image_type)


@router.get("/Items/{item_id}/Images/{image_type}/{index}")
async def item_image_indexed(
    item_id: str, image_type: str, index: int, request: Request,
    services: CompatServices = Depends(get_compat_services),
) -> Response:
    return await _handle(request, services, _image, auth=False,
                         item_id=item_id, image_type=image_type)


# ===== Streaming + PlaybackInfo (05-streaming-transcoding.md) =====

def _accepted_containers(param: str | None) -> set[str]:
    out: set[str] = set()
    for entry in (param or "").split(","):
        entry = entry.strip()
        if entry:
            out.add(entry.split("|", 1)[0].strip().lower())
    return out


def _map_jf_codec(codec: str | None) -> str | None:
    if not codec:
        return None
    c = codec.lower()
    return c if c in ("mp3", "opus") else "opus"  # nearest we can produce


def _audio_stream_model(track) -> jm.MediaStream:
    return jm.MediaStream(
        Codec=track.file_format, Index=0,
        BitRate=(track.bitrate or 0) * 1000 or None,
        Channels=track.channels or 2, SampleRate=track.sample_rate,
        BitDepth=track.bit_depth, IsDefault=True,
    )


async def _serve_direct(services, file_id, request) -> Response:
    range_header = request.headers.get("Range")
    try:
        chunks, headers, status = await services.local_files.stream_track(
            file_id, range_header=range_header
        )
    except ExternalServiceError as exc:
        if "Range not satisfiable" in str(exc):
            return Response(status_code=416)
        raise
    out = {**headers, "Content-Encoding": "identity"}  # keep GZip off audio (05 s10)
    return StreamingResponse(
        chunks, status_code=status, headers=out,
        media_type=headers.get("Content-Type", "application/octet-stream"),
    )


async def _stream_decided(request, services, internal, *, req_fmt, max_kbps, start_s, force):
    from services.compat.transcode_service import decide, ffmpeg_available

    track = await services.view.get_track(internal)
    if track is None:
        raise JellyfinError(404, "Item not found")
    settings = services.preferences.get_connect_apps_settings()
    plan = decide(
        track, requested_format=req_fmt, max_bitrate_kbps=max_kbps,
        force_original=force, start_seconds=start_s, settings=settings,
        ffmpeg_available=ffmpeg_available(),
    )
    if not plan.transcode:
        return await _serve_direct(services, internal, request)
    path = await services.local_files.resolve_validated_path(internal)
    return services.transcode.stream(
        str(path), plan, is_disconnected=request.is_disconnected, estimate=False
    )


async def _decode_track(services, item_id) -> str:
    try:
        kind, internal = await services.id_map.from_jf(item_id)
    except JellyfinError:
        raise JellyfinError(404, "Item not found")
    if kind != "track":
        raise JellyfinError(404, "Not an audio item")
    return internal


async def _universal(request, services, user, *, item_id):
    internal = await _decode_track(services, item_id)
    q = _params(request)
    max_bps = _qint(request, "MaxStreamingBitrate", 0)  # tolerate non-numeric -> 0
    max_kbps = round(max_bps / 1000) if max_bps else None
    start_s = _qint(request, "StartTimeTicks", 0) / JELLYFIN_TICKS_PER_SECOND
    accepted = _accepted_containers(q.get("Container"))
    track = await services.view.get_track(internal)
    if track is None:
        raise JellyfinError(404, "Item not found")
    if accepted and track.file_format in accepted:
        req_fmt = None
    else:
        req_fmt = _map_jf_codec(q.get("AudioCodec"))
    return await _stream_decided(
        request, services, internal, req_fmt=req_fmt, max_kbps=max_kbps,
        start_s=start_s, force=False,
    )


# Streaming is anonymous (auth=False): real Jellyfin's audio routes have no [Authorize],
# and native players (Jellify/Finamp/Manet) fetch the URL with no auth header, so requiring
# auth 401s playback. Still gated by protocol-enabled + a valid opaque item id.
@router.get("/Audio/{item_id}/universal")
async def audio_universal(
    item_id: str, request: Request,
    services: CompatServices = Depends(get_compat_services),
) -> Response:
    return await _handle(request, services, _universal, auth=False, item_id=item_id)


async def _audio_stream(request, services, user, *, item_id):
    internal = await _decode_track(services, item_id)
    q = _params(request)
    if (q.get("static") or "").lower() == "true":
        return await _serve_direct(services, internal, request)
    audio_bps = _qint(request, "audioBitRate", 0)  # tolerate non-numeric -> 0
    max_kbps = round(audio_bps / 1000) if audio_bps else None
    start_s = _qint(request, "startTimeTicks", 0) / JELLYFIN_TICKS_PER_SECOND
    return await _stream_decided(
        request, services, internal, req_fmt=_map_jf_codec(q.get("audioCodec")),
        max_kbps=max_kbps, start_s=start_s, force=False,
    )


@router.get("/Audio/{item_id}/stream")
@router.get("/Audio/{item_id}/stream.{container}")
async def audio_stream(
    item_id: str, request: Request,
    services: CompatServices = Depends(get_compat_services),
    container: str | None = None,
) -> Response:
    return await _handle(request, services, _audio_stream, auth=False, item_id=item_id)


async def _audio_stream_head(request, services, user, *, item_id):
    internal = await _decode_track(services, item_id)
    headers = await services.local_files.head_track(internal)
    return Response(status_code=200, headers={**headers, "Content-Encoding": "identity"})


@router.head("/Audio/{item_id}/stream")
@router.head("/Audio/{item_id}/stream.{container}")
async def audio_stream_head(
    item_id: str, request: Request,
    services: CompatServices = Depends(get_compat_services),
    container: str | None = None,
) -> Response:
    return await _handle(request, services, _audio_stream_head, auth=False, item_id=item_id)


async def _playback_info(request, services, user, *, item_id):
    from services.compat.transcode_service import decide, ffmpeg_available
    from api.compat.jellyfin.builders import ticks

    internal = await _decode_track(services, item_id)
    track = await services.view.get_track(internal, user=user)
    if track is None:
        raise JellyfinError(404, "Item not found")
    max_bps = None
    if request.method == "POST":
        body = await decode_body(request, jm.PlaybackInfoBody)
        if body and body.MaxStreamingBitrate:
            max_bps = body.MaxStreamingBitrate
    if max_bps is None:
        raw = _params(request).get("maxStreamingBitrate")
        max_bps = int(raw) if raw and raw.isdigit() else None
    settings = services.preferences.get_connect_apps_settings()
    ffmpeg = ffmpeg_available()
    will_transcode = decide(
        track, requested_format=None,
        max_bitrate_kbps=round(max_bps / 1000) if max_bps else None,
        force_original=False, start_seconds=0.0, settings=settings,
        ffmpeg_available=ffmpeg,
    ).transcode
    psid = uuid4().hex
    token = extract_token(request)
    ext = track.file_format or "dat"
    direct_url = (
        f"{_local_address(request)}/Audio/{item_id}/stream.{ext}"
        f"?static=true&mediaSourceId={item_id}&api_key={token}"
    )
    src = jm.MediaSourceInfo(
        Id=item_id, Container=track.file_format, Size=track.file_size_bytes or None,
        Bitrate=(track.bitrate or 0) * 1000 or None,
        RunTimeTicks=ticks(track.duration_seconds),
        SupportsDirectPlay=True, SupportsDirectStream=True,
        SupportsTranscoding=settings.transcoding_enabled and ffmpeg,
        DefaultAudioStreamIndex=0, MediaStreams=[_audio_stream_model(track)],
        DirectStreamUrl=direct_url,
    )
    if will_transcode:
        out = settings.transcode_default_format
        src.TranscodingUrl = (
            f"/jellyfin/Audio/{item_id}/universal?AudioCodec={out}&Container={out}"
            f"&PlaySessionId={psid}"
        )
        src.TranscodingSubProtocol = "http"
        src.TranscodingContainer = "mp3" if out == "mp3" else "ogg"
    return jm.PlaybackInfoResponse(MediaSources=[src], PlaySessionId=psid)


@router.get("/Items/{item_id}/PlaybackInfo")
async def playback_info_get(
    item_id: str, request: Request,
    services: CompatServices = Depends(get_compat_services),
) -> Response:
    return await _handle(request, services, _playback_info, item_id=item_id)


@router.post("/Items/{item_id}/PlaybackInfo")
async def playback_info_post(
    item_id: str, request: Request,
    services: CompatServices = Depends(get_compat_services),
) -> Response:
    return await _handle(request, services, _playback_info, item_id=item_id)


# ===== Favorites + played (both dialects, 200 UserItemDataDto) =====

_FAV_KINDS = {"artist", "album", "track"}


def _user_item_data(item_id, *, is_favorite=False, played=False) -> jm.UserItemDataDto:
    return jm.UserItemDataDto(
        ItemId=item_id, Key=item_id, IsFavorite=is_favorite, Played=played
    )


async def _set_favorite(request, services, user, *, item_id, add):
    try:
        kind, internal = await services.id_map.from_jf(item_id)
    except JellyfinError:
        raise JellyfinError(404, "Item not found")
    if kind not in _FAV_KINDS:
        raise JellyfinError(400, "Item is not favoritable")
    if add:
        await services.favorites.add(user.id, kind, internal)
    else:
        await services.favorites.remove(user.id, kind, internal)
    return _user_item_data(item_id, is_favorite=add)


async def _set_played(request, services, user, *, item_id, played):
    # Marker only: play counting goes via Sessions/Playing/Stopped, so don't write
    # play_history here (avoids unwanted scrobble forwards).
    try:
        await services.id_map.from_jf(item_id)
    except JellyfinError:
        raise JellyfinError(404, "Item not found")
    return _user_item_data(item_id, played=played)


def _favorite_routes(path: str) -> None:
    async def add_route(item_id: str, request: Request,
                        services: CompatServices = Depends(get_compat_services),
                        user_id: str = "") -> Response:
        return await _handle(request, services, _set_favorite, item_id=item_id, add=True)

    async def del_route(item_id: str, request: Request,
                        services: CompatServices = Depends(get_compat_services),
                        user_id: str = "") -> Response:
        return await _handle(request, services, _set_favorite, item_id=item_id, add=False)

    router.add_api_route(path, add_route, methods=["POST"])
    router.add_api_route(path, del_route, methods=["DELETE"])


def _played_routes(path: str) -> None:
    async def add_route(item_id: str, request: Request,
                        services: CompatServices = Depends(get_compat_services),
                        user_id: str = "") -> Response:
        return await _handle(request, services, _set_played, item_id=item_id, played=True)

    async def del_route(item_id: str, request: Request,
                        services: CompatServices = Depends(get_compat_services),
                        user_id: str = "") -> Response:
        return await _handle(request, services, _set_played, item_id=item_id, played=False)

    router.add_api_route(path, add_route, methods=["POST"])
    router.add_api_route(path, del_route, methods=["DELETE"])


_favorite_routes("/Users/{user_id}/FavoriteItems/{item_id}")  # legacy (Finamp)
_favorite_routes("/UserFavoriteItems/{item_id}")               # modern (Jellify)
_played_routes("/Users/{user_id}/PlayedItems/{item_id}")
_played_routes("/UserPlayedItems/{item_id}")


# ===== Playback reporting / scrobbling (all 204, lenient bodies) =====

def _session_key(body) -> str | None:
    return (body.PlaySessionId or body.ItemId) if body else None


async def _track_from_item_id(services, item_id: str) -> str | None:
    try:
        kind, internal = await services.id_map.from_jf(item_id)
    except JellyfinError:
        return None
    return internal if kind == "track" else None


async def _playing_start(request, services, user, **_):
    from core.exceptions import ResourceNotFoundError

    body = await decode_body(request, jm.PlaybackStartInfo)
    if not body or not body.ItemId:
        return None
    key = _session_key(body)
    if key:
        services.scrobble.mark_started(user.id, key)
    file_id = await _track_from_item_id(services, body.ItemId)
    if file_id:
        try:
            await services.scrobble.now_playing(
                file_id, user_id=user.id, client=extract_client(request),
                user_name=getattr(user, "display_name", ""),
            )
        except ResourceNotFoundError:
            pass
    return None  # 204


async def _playing_progress(request, services, user, **_):
    # position update: drives the live now-playing presence scrubber + heartbeat.
    # No scrobble forwarding (that happens on Stopped past threshold).
    from core.exceptions import ResourceNotFoundError

    body = await decode_body(request, jm.PlaybackProgressInfo)
    if not body or not body.ItemId:
        return None  # 204
    file_id = await _track_from_item_id(services, body.ItemId)
    if file_id:
        position_ms = (
            body.PositionTicks // (JELLYFIN_TICKS_PER_SECOND // 1000)
            if body.PositionTicks is not None
            else None
        )
        try:
            await services.scrobble.progress(
                file_id,
                user_id=user.id,
                user_name=getattr(user, "display_name", ""),
                client=extract_client(request),
                position_ms=position_ms,
                is_paused=body.IsPaused,
            )
        except ResourceNotFoundError:
            pass
    return None  # 204


def _should_scrobble(position_ticks, runtime_ticks) -> bool:
    if position_ticks is None:  # position omitted -> count it (reference s6)
        return True
    if runtime_ticks and runtime_ticks > 0:
        if position_ticks / runtime_ticks * 100 > 90:
            return True
        if position_ticks >= runtime_ticks - JELLYFIN_TICKS_PER_SECOND:
            return True
    return False


async def _playing_stopped(request, services, user, **_):
    from core.exceptions import ResourceNotFoundError
    from api.compat.jellyfin.builders import ticks

    # the session stopped (any reason) -> drop its now-playing presence, even when the
    # play didn't reach the scrobble threshold below (otherwise it lingers until TTL)
    await services.scrobble.clear_presence(user.id, extract_client(request))

    body = await decode_body(request, jm.PlaybackStopInfo)
    if not body or body.Failed or not body.ItemId:
        return None
    file_id = await _track_from_item_id(services, body.ItemId)
    if not file_id:
        return None
    track = await services.view.get_track(file_id)
    runtime = body.RunTimeTicks or (ticks(track.duration_seconds) if track else None)
    if not _should_scrobble(body.PositionTicks, runtime):
        return None
    started_at = services.scrobble.pop_started(user.id, _session_key(body) or "")
    try:
        await services.scrobble.scrobble(
            file_id, user_id=user.id, client=extract_client(request),
            played_at=started_at, user_name=getattr(user, "display_name", ""),
        )
    except ResourceNotFoundError:
        pass  # file gone since start - benign; still 204
    return None  # 204


@router.post("/Sessions/Playing")
async def sessions_playing(
    request: Request, services: CompatServices = Depends(get_compat_services)
) -> Response:
    return await _handle(request, services, _playing_start)


@router.post("/Sessions/Playing/Progress")
async def sessions_progress(
    request: Request, services: CompatServices = Depends(get_compat_services)
) -> Response:
    return await _handle(request, services, _playing_progress)


@router.post("/Sessions/Playing/Stopped")
async def sessions_stopped(
    request: Request, services: CompatServices = Depends(get_compat_services)
) -> Response:
    return await _handle(request, services, _playing_stopped)


@router.post("/Sessions/Playing/Ping")
async def sessions_ping(
    request: Request, services: CompatServices = Depends(get_compat_services)
) -> Response:
    return await _handle(request, services, lambda r, s, u: None)


@router.post("/Sessions/Capabilities/Full")
async def sessions_capabilities(
    request: Request, services: CompatServices = Depends(get_compat_services)
) -> Response:
    # No session registry to store reported capabilities; accept and 204 to avoid a 404.
    return await _handle(request, services, lambda r, s, u: None)


# ===== Playlists (06-data-mapping.md s10) =====

def _to_view_playlist(record, streamable_counts):
    from services.compat.view_models import ViewPlaylist

    # served-entry counts, not record.track_count: /Playlists/{id}/Items filters
    # to library-linked entries, so the advertised ChildCount must match (issue #181)
    count, duration = streamable_counts.get(record.id, (0, 0))
    return ViewPlaylist(
        id=record.id, name=record.name, is_public=record.is_public,
        owner_id=record.user_id or "", track_count=count,
        total_duration_seconds=float(duration) if duration else None,
    )


async def _decode_playlist(services, jf_id) -> str:
    try:
        kind, internal = await services.id_map.from_jf(jf_id)
    except JellyfinError:
        raise JellyfinError(404, "Playlist not found")
    if kind != "playlist":
        raise JellyfinError(404, "Not a playlist")
    return internal


def _ids_param(request: Request, *keys: str) -> list[str]:
    # Lookups are case-insensitive: dedupe key spellings ("ids"/"Ids") or each id is
    # collected once per spelling.
    out: list[str] = []
    p = _params(request)
    for key in {k.lower() for k in keys}:
        for value in p.getlist(key):
            out.extend(x for x in value.split(",") if x)
    return out


async def _create_playlist(request, services, user, **_):
    body = await decode_body(request, jm.CreatePlaylistDto)
    name = (body.Name if body else None) or _params(request).get("name") or "Playlist"
    track_jf_ids = (body.Ids if body else None) or _ids_param(request, "ids")
    record = await services.playlists.create_playlist(name, user_id=user.id)
    for jf_id in track_jf_ids:
        fid = await _track_from_item_id(services, jf_id)
        if fid:
            await services.playlists.add_file_id_entry(record.id, fid, requesting=user)
    return {"Id": await services.id_map.to_jf("playlist", record.id)}


@router.post("/Playlists")
async def create_playlist(
    request: Request, services: CompatServices = Depends(get_compat_services)
) -> Response:
    return await _handle(request, services, _create_playlist)


async def _playlist_detail(services, user, internal):
    from services.playlist_service import PlaylistDetailView

    detail = await services.playlists.get_playlist_with_tracks(internal, user)
    if not isinstance(detail, PlaylistDetailView):
        raise JellyfinError(404, "Playlist not found")
    return detail


async def _get_playlist(request, services, user, *, playlist_id):
    internal = await _decode_playlist(services, playlist_id)
    detail = await _playlist_detail(services, user, internal)
    return {
        "Id": playlist_id, "Name": detail.record.name, "Type": "Playlist",
        "ServerId": jm.SERVER_ID,
        "ItemIds": [
            await services.id_map.to_jf("track", e.library_file_id)
            for e in detail.tracks if e.library_file_id
        ],
    }


@router.get("/Playlists/{playlist_id}")
async def get_playlist(
    playlist_id: str, request: Request,
    services: CompatServices = Depends(get_compat_services),
) -> Response:
    return await _handle(request, services, _get_playlist, playlist_id=playlist_id)


async def _playlist_items(request, services, user, *, playlist_id):
    internal = await _decode_playlist(services, playlist_id)
    detail = await _playlist_detail(services, user, internal)
    b = _builder(services)
    start = max(_qint(request, "startIndex", 0) or _qint(request, "StartIndex", 0), 0)
    limit = max(_qint(request, "limit", 0) or _qint(request, "Limit", 0), 0)
    streamable = [e for e in detail.tracks if e.library_file_id]
    items = []
    for entry in streamable:
        track = await services.view.get_track(entry.library_file_id, user=user)
        if track is None:
            continue
        dto = await b.audio(track)
        dto.PlaylistItemId = entry.id  # per-entry handle for remove/reorder
        items.append(dto)
    total = len(items)
    page = items[start: start + limit] if limit else items[start:]
    return jm.BaseItemDtoQueryResult(Items=page, TotalRecordCount=total, StartIndex=start)


@router.get("/Playlists/{playlist_id}/Items")
async def playlist_items(
    playlist_id: str, request: Request,
    services: CompatServices = Depends(get_compat_services),
) -> Response:
    return await _handle(request, services, _playlist_items, playlist_id=playlist_id)


async def _playlist_add(request, services, user, *, playlist_id):
    internal = await _decode_playlist(services, playlist_id)
    for jf_id in _ids_param(request, "ids", "Ids"):
        fid = await _track_from_item_id(services, jf_id)
        if fid:
            await services.playlists.add_file_id_entry(internal, fid, requesting=user)
    return None  # 204


@router.post("/Playlists/{playlist_id}/Items")
async def playlist_add_items(
    playlist_id: str, request: Request,
    services: CompatServices = Depends(get_compat_services),
) -> Response:
    return await _handle(request, services, _playlist_add, playlist_id=playlist_id)


async def _playlist_remove(request, services, user, *, playlist_id):
    internal = await _decode_playlist(services, playlist_id)
    entry_ids = _ids_param(request, "entryIds", "EntryIds")
    if entry_ids:
        await services.playlists.remove_tracks(internal, user, entry_ids)
    return None  # 204


@router.delete("/Playlists/{playlist_id}/Items")
async def playlist_remove_items(
    playlist_id: str, request: Request,
    services: CompatServices = Depends(get_compat_services),
) -> Response:
    return await _handle(request, services, _playlist_remove, playlist_id=playlist_id)


async def _playlist_move(request, services, user, *, playlist_id, entry_id, new_index):
    internal = await _decode_playlist(services, playlist_id)
    await services.playlists.reorder_track(internal, user, entry_id, new_index)
    return None  # 204


@router.post("/Playlists/{playlist_id}/Items/{entry_id}/Move/{new_index}")
async def playlist_move_item(
    playlist_id: str, entry_id: str, new_index: int, request: Request,
    services: CompatServices = Depends(get_compat_services),
) -> Response:
    return await _handle(
        request, services, _playlist_move, playlist_id=playlist_id,
        entry_id=entry_id, new_index=new_index,
    )


# ===== Discovery (owned-only) =====

async def _resolve_artist_mbid(services, kind, internal) -> str | None:
    if kind == "artist":
        return internal
    if kind == "track":
        t = await services.view.get_track(internal)
        return t.artist_mbid if t else None
    if kind == "album":
        a = await services.view.get_album(internal)
        return a.artist_mbid if a else None
    return None


async def _similar(request, services, user, *, item_id):
    b = _builder(services)
    limit = max(_qint(request, "Limit", 50) or 50, 1)
    try:
        kind, internal = await services.id_map.from_jf(item_id)
    except JellyfinError:
        return jm.BaseItemDtoQueryResult(Items=[], TotalRecordCount=0, StartIndex=0)
    artist_mbid = await _resolve_artist_mbid(services, kind, internal)
    if not artist_mbid:
        return jm.BaseItemDtoQueryResult(Items=[], TotalRecordCount=0, StartIndex=0)
    tracks = await services.discover.get_similar_songs(
        artist_mbid, user_id=user.id, count=limit, user=user
    )
    return await _build_qr(b.audio, tracks, len(tracks), 0)


@router.get("/Items/{item_id}/Similar")
async def items_similar(
    item_id: str, request: Request,
    services: CompatServices = Depends(get_compat_services),
) -> Response:
    return await _handle(request, services, _similar, item_id=item_id)


@router.get("/Items/{item_id}/InstantMix")
async def items_instant_mix(
    item_id: str, request: Request,
    services: CompatServices = Depends(get_compat_services),
) -> Response:
    return await _handle(request, services, _similar, item_id=item_id)


@router.get("/Artists/{item_id}/InstantMix")
async def artist_instant_mix(
    item_id: str, request: Request,
    services: CompatServices = Depends(get_compat_services),
) -> Response:
    return await _handle(request, services, _similar, item_id=item_id)
