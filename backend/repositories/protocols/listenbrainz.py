from typing import Any, Protocol

from repositories.listenbrainz_models import (
    ListenBrainzArtist,
    ListenBrainzFeedbackRecording,
    ListenBrainzRecording,
    ListenBrainzReleaseGroup,
    ListenBrainzSimilarArtist,
)


class ListenBrainzRepositoryProtocol(Protocol):

    def is_configured(self) -> bool:
        ...

    async def get_user_loved_recordings(
        self,
        username: str | None = None,
        count: int = 25,
        offset: int = 0,
    ) -> list[ListenBrainzFeedbackRecording]:
        ...

    async def submit_now_playing(
        self,
        artist_name: str,
        track_name: str,
        release_name: str = "",
        duration_ms: int = 0,
    ) -> bool:
        ...

    async def submit_single_listen(
        self,
        artist_name: str,
        track_name: str,
        listened_at: int,
        release_name: str = "",
        duration_ms: int = 0,
    ) -> bool:
        ...

    async def get_trending_artists(
        self,
        time_range: str = "this_week",
        limit: int = 20,
        offset: int = 0
    ) -> list[ListenBrainzArtist]:
        ...

    async def get_popular_release_groups(
        self,
        time_range: str = "this_week",
        limit: int = 20,
        offset: int = 0
    ) -> list[ListenBrainzReleaseGroup]:
        ...

    async def get_fresh_releases(
        self,
        limit: int = 20
    ) -> list[ListenBrainzReleaseGroup]:
        ...

    async def get_similar_artists(
        self,
        artist_mbid: str,
        max_similar: int = 15,
        mode: str = "easy"
    ) -> list[ListenBrainzSimilarArtist]:
        ...

    async def get_artist_top_release_groups(
        self,
        artist_mbid: str,
        count: int = 10
    ) -> list[ListenBrainzReleaseGroup]:
        ...

    async def get_artist_top_recordings(
        self,
        artist_mbid: str,
        count: int = 10
    ) -> list[ListenBrainzRecording]:
        ...

    async def get_recording_release_groups_batch(
        self,
        recording_mbids: list[str],
    ) -> dict[str, str]:
        ...

    async def get_release_group_popularity_batch(
        self,
        release_group_mbids: list[str]
    ) -> dict[str, int]:
        ...

    async def get_recommendation_playlists(
        self,
        username: str | None = None,
    ) -> list[dict[str, Any]]:
        ...

    async def get_playlist_tracks(
        self,
        playlist_id: str,
    ) -> Any:
        ...
