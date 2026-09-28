from typing import Protocol

from repositories.lastfm_models import (
    LastFmAlbum,
    LastFmAlbumInfo,
    LastFmArtist,
    LastFmArtistInfo,
    LastFmLovedTrack,
    LastFmRecentTrack,
    LastFmSession,
    LastFmSimilarArtist,
    LastFmToken,
    LastFmTrack,
)


class LastFmRepositoryProtocol(Protocol):

    def configure(self, api_key: str, shared_secret: str, session_key: str = "") -> None:
        ...

    @staticmethod
    def reset_circuit_breaker() -> None:
        ...

    async def get_token(self) -> LastFmToken:
        ...

    async def get_session(self, token: str) -> LastFmSession:
        ...

    async def validate_api_key(self) -> tuple[bool, str]:
        ...

    async def validate_session(self) -> tuple[bool, str]:
        ...

    async def update_now_playing(
        self,
        artist: str,
        track: str,
        album: str = "",
        duration: int = 0,
        mbid: str | None = None,
    ) -> bool:
        ...

    async def scrobble(
        self,
        artist: str,
        track: str,
        timestamp: int,
        album: str = "",
        duration: int = 0,
        mbid: str | None = None,
    ) -> bool:
        ...

    async def get_user_top_artists(
        self, username: str, period: str = "overall", limit: int = 50
    ) -> list[LastFmArtist]:
        ...

    async def get_user_top_albums(
        self, username: str, period: str = "overall", limit: int = 50
    ) -> list[LastFmAlbum]:
        ...

    async def get_user_top_tracks(
        self, username: str, period: str = "overall", limit: int = 50
    ) -> list[LastFmTrack]:
        ...

    async def get_user_recent_tracks(
        self, username: str, limit: int = 50
    ) -> list[LastFmRecentTrack]:
        ...

    async def get_user_loved_tracks(
        self, username: str, limit: int = 50
    ) -> list[LastFmLovedTrack]:
        ...

    async def get_user_weekly_artist_chart(
        self, username: str
    ) -> list[LastFmArtist]:
        ...

    async def get_user_weekly_album_chart(
        self, username: str
    ) -> list[LastFmAlbum]:
        ...

    async def get_artist_top_tracks(
        self, artist: str, mbid: str | None = None, limit: int = 10
    ) -> list[LastFmTrack]:
        ...

    async def get_artist_top_albums(
        self, artist: str, mbid: str | None = None, limit: int = 10
    ) -> list[LastFmAlbum]:
        ...

    async def get_artist_info(
        self, artist: str, mbid: str | None = None, username: str | None = None
    ) -> LastFmArtistInfo | None:
        ...

    async def get_album_info(
        self,
        artist: str,
        album: str,
        mbid: str | None = None,
        username: str | None = None,
    ) -> LastFmAlbumInfo | None:
        ...

    async def get_track_album(self, artist: str, track: str) -> str | None:
        ...

    async def get_similar_artists(
        self, artist: str, mbid: str | None = None, limit: int = 30
    ) -> list[LastFmSimilarArtist]:
        ...

    async def get_global_top_artists(self, limit: int = 50) -> list[LastFmArtist]:
        ...

    async def get_global_top_tracks(self, limit: int = 50) -> list[LastFmTrack]:
        ...

    async def get_tag_top_artists(
        self, tag: str, limit: int = 50
    ) -> list[LastFmArtist]:
        ...
