import time


from infrastructure.msgspec_fastapi import AppStruct


class NowPlayingRequest(AppStruct):
    track_name: str
    artist_name: str
    album_name: str = ""
    duration_ms: int = 0
    mbid: str | None = None
    # native playback source, or normalized client name from the compat adapter
    source: str | None = None
    release_group_mbid: str | None = None

    def __post_init__(self) -> None:
        if self.duration_ms < 0:
            raise ValueError("duration_ms must be >= 0")


class ScrobbleRequest(AppStruct):
    track_name: str
    artist_name: str
    timestamp: int
    album_name: str = ""
    duration_ms: int = 0
    mbid: str | None = None
    # native playback source, or normalized client name from the compat adapter
    source: str | None = None
    release_group_mbid: str | None = None

    def __post_init__(self) -> None:
        now = int(time.time())
        max_age = 14 * 24 * 60 * 60
        if self.duration_ms < 0:
            raise ValueError("duration_ms must be >= 0")
        if self.timestamp > now + 60:
            raise ValueError("Timestamp cannot be in the future")
        if self.timestamp < now - max_age:
            raise ValueError("Timestamp cannot be older than 14 days")


class ServiceResult(AppStruct):
    success: bool
    error: str | None = None


class ScrobbleResponse(AppStruct):
    accepted: bool
    services: dict[str, ServiceResult] = {}
