"""DTOs for the live now-playing presence feed.

A `NowPlayingReport` is what the native web player POSTs as a heartbeat; the
`NowPlayingSnapshot` (list of `NowPlayingSnapshotEntry`) is what the read endpoint
and the SSE channel emit, already privacy-projected per the owner's visibility
setting. Redacted entries keep identity + progress but carry empty track fields.
"""

from infrastructure.msgspec_fastapi import AppStruct


class NowPlayingReport(AppStruct):
    track_name: str
    artist_name: str
    album_name: str | None = None
    cover_url: str = ""
    # 'local' | 'youtube' | 'jellyfin' | 'navidrome' | 'plex' (the web player's source)
    source: str = "local"
    device: str = "web"
    is_paused: bool = False
    progress_ms: int | None = None
    duration_ms: int | None = None


class NowPlayingSnapshotEntry(AppStruct):
    id: str
    user_name: str
    track_name: str
    artist_name: str
    album_name: str | None
    cover_url: str
    device_name: str
    is_paused: bool
    source: str
    progress_ms: int | None
    duration_ms: int | None
    # true when the owner chose 'track_hidden': identity + progress only, song stripped
    redacted: bool = False


class NowPlayingSnapshot(AppStruct):
    sessions: list[NowPlayingSnapshotEntry] = []
