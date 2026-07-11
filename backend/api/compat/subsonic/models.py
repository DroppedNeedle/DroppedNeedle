"""Subsonic response object structs + View DTO -> object formatters.

Field names are EXACT Subsonic/OpenSubsonic camelCase names so msgspec.to_builtins
emits the right keys. Optional list fields default None so the None-strip omits
them in list vs detail contexts.
"""

from __future__ import annotations

import re
from datetime import datetime, timezone

import msgspec

from api.compat.subsonic.ids import encode
from services.compat.view_models import ViewAlbum, ViewArtist, ViewGenre, ViewTrack

_MIME = {
    "flac": "audio/flac", "mp3": "audio/mpeg", "ogg": "audio/ogg",
    "opus": "audio/opus", "m4a": "audio/mp4", "aac": "audio/aac",
    "wav": "audio/wav", "wma": "audio/x-ms-wma",
}


def mime_for(file_format: str) -> str:
    return _MIME.get((file_format or "").lower(), "application/octet-stream")


def iso(ts: float | int | None) -> str | None:
    if ts is None:
        return None
    return datetime.fromtimestamp(float(ts), tz=timezone.utc).strftime(
        "%Y-%m-%dT%H:%M:%SZ"
    )


def genre_slug(name: str) -> str:
    return re.sub(r"[^a-z0-9]+", "-", (name or "").lower()).strip("-")


class SArtistID3(msgspec.Struct, kw_only=True):
    id: str
    name: str
    coverArt: str | None = None
    albumCount: int | None = None
    starred: str | None = None
    musicBrainzId: str | None = None
    sortName: str | None = None
    album: list["SAlbumID3"] | None = None  # populated by getArtist only


class SArtist(msgspec.Struct, kw_only=True):
    """File-structure artist (getIndexes / search2 / getStarred)."""

    id: str
    name: str
    starred: str | None = None
    coverArt: str | None = None


class SAlbumID3(msgspec.Struct, kw_only=True):
    id: str
    name: str
    artist: str | None = None
    artistId: str | None = None
    coverArt: str | None = None
    songCount: int | None = None
    duration: int | None = None
    playCount: int | None = None
    created: str | None = None
    starred: str | None = None
    year: int | None = None
    genre: str | None = None
    isCompilation: bool | None = None
    musicBrainzId: str | None = None
    song: list["SChild"] | None = None  # populated by getAlbum only


class SChild(msgspec.Struct, kw_only=True):
    id: str
    isDir: bool = False
    title: str
    parent: str | None = None
    album: str | None = None
    artist: str | None = None
    track: int | None = None
    year: int | None = None
    genre: str | None = None
    coverArt: str | None = None
    size: int | None = None
    contentType: str | None = None
    suffix: str | None = None
    transcodedContentType: str | None = None
    transcodedSuffix: str | None = None
    duration: int | None = None
    bitRate: int | None = None
    path: str | None = None
    discNumber: int | None = None
    created: str | None = None
    starred: str | None = None
    albumId: str | None = None
    artistId: str | None = None
    type: str | None = None
    playCount: int | None = None
    # OpenSubsonic required-on-song fields
    mediaType: str | None = None
    bitDepth: int | None = None
    samplingRate: int | None = None
    channelCount: int | None = None
    musicBrainzId: str | None = None


class SNowPlayingEntry(SChild, kw_only=True):
    """getNowPlaying entry: a song Child plus session attribution."""

    username: str = ""
    minutesAgo: int = 0
    playerId: int = 0
    playerName: str | None = None


class SGenre(msgspec.Struct, kw_only=True):
    value: str
    songCount: int = 0
    albumCount: int = 0


class SPlaylist(msgspec.Struct, kw_only=True):
    id: str
    name: str
    comment: str | None = None
    owner: str | None = None
    public: bool | None = None
    songCount: int = 0
    duration: int | None = None
    created: str | None = None
    changed: str | None = None
    coverArt: str | None = None
    entry: list["SChild"] | None = None  # populated by getPlaylist only


class SIndexID3(msgspec.Struct, kw_only=True):
    name: str
    artist: list[SArtistID3] = []


class SArtistsID3(msgspec.Struct, kw_only=True):
    ignoredArticles: str = "The El La Los Las Le Les"
    index: list[SIndexID3] = []


class SIndex(msgspec.Struct, kw_only=True):
    name: str
    artist: list[SArtist] = []


class SIndexes(msgspec.Struct, kw_only=True):
    lastModified: int = 0
    ignoredArticles: str = "The El La Los Las Le Les"
    index: list[SIndex] = []


class SMusicFolder(msgspec.Struct, kw_only=True):
    id: int
    name: str


class SLicense(msgspec.Struct, kw_only=True):
    valid: bool = True


class SOpenSubsonicExtension(msgspec.Struct, kw_only=True):
    name: str
    versions: list[int]


class SUser(msgspec.Struct, kw_only=True):
    username: str
    scrobblingEnabled: bool = True
    adminRole: bool = False
    settingsRole: bool = True
    downloadRole: bool = True
    uploadRole: bool = False
    playlistRole: bool = True
    coverArtRole: bool = True
    commentRole: bool = False
    podcastRole: bool = False
    streamRole: bool = True
    jukeboxRole: bool = False
    shareRole: bool = False
    videoConversionRole: bool = False
    maxBitRate: int | None = None


# ----- View DTO -> Subsonic object formatters -----


def to_artist_id3(v: ViewArtist) -> SArtistID3:
    aid = encode("artist", v.artist_mbid)
    return SArtistID3(
        id=aid,
        name=v.name,
        coverArt=aid,
        albumCount=v.album_count,
        starred=iso(v.starred_at),
        musicBrainzId=v.artist_mbid,
        sortName=v.name,
    )


def to_artist_file(v: ViewArtist) -> SArtist:
    aid = encode("artist", v.artist_mbid)
    return SArtist(id=aid, name=v.name, coverArt=aid, starred=iso(v.starred_at))


def to_album_id3(v: ViewAlbum) -> SAlbumID3:
    alid = encode("album", v.rg_mbid)
    return SAlbumID3(
        id=alid,
        name=v.title,
        artist=v.artist_name,
        artistId=encode("artist", v.artist_mbid) if v.artist_mbid else None,
        coverArt=alid,
        songCount=v.track_count,
        duration=round(v.total_duration_seconds)
        if v.total_duration_seconds is not None
        else None,
        playCount=v.play_count,
        created=iso(v.date_added),
        starred=iso(v.starred_at),
        year=v.year,
        genre=v.genre,
        isCompilation=v.is_compilation,
        musicBrainzId=v.rg_mbid,
    )


def to_child(
    v: ViewTrack,
    *,
    transcoded_content_type: str | None = None,
    transcoded_suffix: str | None = None,
) -> SChild:
    alid = encode("album", v.rg_mbid) if v.rg_mbid else None
    return SChild(
        id=encode("track", v.file_id),
        isDir=False,
        title=v.title,
        parent=alid,
        album=v.album_title,
        artist=v.artist_name,
        track=v.track_number or None,
        year=v.year,
        genre=v.genre,
        coverArt=alid,
        size=v.file_size_bytes or None,
        contentType=mime_for(v.file_format),
        suffix=v.file_format or None,
        transcodedContentType=transcoded_content_type,
        transcodedSuffix=transcoded_suffix,
        duration=round(v.duration_seconds),
        bitRate=v.bitrate,
        discNumber=v.disc_number or None,
        created=iso(v.created_at),
        starred=iso(v.starred_at),
        albumId=alid,
        artistId=encode("artist", v.artist_mbid) if v.artist_mbid else None,
        type="music",
        playCount=v.play_count,
        mediaType="song",
        bitDepth=v.bit_depth,
        samplingRate=v.sample_rate,
        channelCount=v.channels if v.channels is not None else 2,
        musicBrainzId=v.recording_mbid,
    )


def to_album_child(v: ViewAlbum) -> SChild:
    """Album as a file-structure Child (getAlbumList / getMusicDirectory dir)."""
    alid = encode("album", v.rg_mbid)
    return SChild(
        id=alid,
        isDir=True,
        title=v.title,
        album=v.title,
        artist=v.artist_name,
        artistId=encode("artist", v.artist_mbid) if v.artist_mbid else None,
        coverArt=alid,
        year=v.year,
        genre=v.genre,
        created=iso(v.date_added),
        starred=iso(v.starred_at),
        duration=round(v.total_duration_seconds)
        if v.total_duration_seconds is not None
        else None,
    )


def to_genre(v: ViewGenre) -> SGenre:
    return SGenre(value=v.name, songCount=v.song_count, albumCount=v.album_count)
