"""MusicBrainzAlbumMixin.search_albums / search_recordings honour the request priority.

The scan path passes ``BACKGROUND_SYNC`` so a library refresh yields to live user searches
on the shared 1/s MusicBrainz limiter; every other (user-facing) caller keeps the
``USER_INITIATED`` default. These assert the param is threaded to ``mb_api_get`` unchanged.
"""

import asyncio
from types import SimpleNamespace
from unittest.mock import AsyncMock, patch

import httpx
import pytest

from core.exceptions import ConfigurationError, ExternalServiceError
from infrastructure.queue.priority_queue import RequestPriority
import repositories.musicbrainz_base as mb_base
from repositories.musicbrainz_album import (
    MusicBrainzAlbumMixin,
    _RecordingSearchPayload,
    _ReleaseGroupSearchPayload,
)
from repositories.musicbrainz_release_search_models import (
    MbReleaseSearchArtist,
    MbReleaseSearchArtistCredit,
    MbReleaseSearchGroup,
    MbReleaseSearchLabel,
    MbReleaseSearchLabelInfo,
    MbReleaseSearchMedium,
    MbReleaseSearchRelease,
    MbReleaseSearchResponse,
)


class _Repo(MusicBrainzAlbumMixin):
    def __init__(self) -> None:
        self._cache = AsyncMock()
        self._cache.get = AsyncMock(return_value=None)
        self._cache.set = AsyncMock()
        self._preferences_service = SimpleNamespace(
            get_advanced_settings=lambda: SimpleNamespace(cache_ttl_search=3600)
        )


@pytest.mark.asyncio
async def test_grouped_search_rejects_mixed_source_generations():
    from repositories.musicbrainz_repository import MusicBrainzRepository

    repository = MusicBrainzRepository.__new__(MusicBrainzRepository)
    original_source = mb_base.capture_mb_source_context()
    original_source_id = mb_base.get_mb_source_id()
    original_runtime = mb_base.brainzmash_runtime_enabled()
    old_generation = original_source.generation + 1
    mb_base.set_mb_api_base(
        "https://old.example/ws/2",
        source_mode="mirror",
        source_id="old-grouped",
        generation=old_generation,
    )

    async def search_artists(*_args, **_kwargs):
        mb_base.set_mb_api_base(
            "https://new.example/ws/2",
            source_mode="mirror",
            source_id="new-grouped",
            generation=old_generation + 1,
        )
        return []

    async def search_albums(*_args, **_kwargs):
        return []

    repository.search_artists = search_artists
    repository.search_albums = search_albums
    try:
        with pytest.raises(ConfigurationError, match="grouped search"):
            await repository.search_grouped(
                "query",
                {"artists": 1, "albums": 1},
            )
    finally:
        mb_base.set_mb_api_base(
            original_source.source_url,
            source_mode=original_source.source_mode,
            source_id=original_source_id,
            generation=original_source.generation,
            brainzmash_binding_valid=original_runtime,
        )


@pytest.mark.asyncio
async def test_search_albums_defaults_to_user_initiated():
    with patch(
        "repositories.musicbrainz_album.mb_api_get",
        AsyncMock(return_value=_ReleaseGroupSearchPayload()),
    ) as mock_get:
        await _Repo().search_albums("query")
    assert mock_get.await_args.kwargs["priority"] == RequestPriority.USER_INITIATED


@pytest.mark.asyncio
async def test_search_albums_forwards_background_priority():
    with patch(
        "repositories.musicbrainz_album.mb_api_get",
        AsyncMock(return_value=_ReleaseGroupSearchPayload()),
    ) as mock_get:
        await _Repo().search_albums("query", priority=RequestPriority.BACKGROUND_SYNC)
    assert mock_get.await_args.kwargs["priority"] == RequestPriority.BACKGROUND_SYNC


@pytest.mark.asyncio
async def test_release_edition_search_parses_facets_escapes_and_pages():
    payload = MbReleaseSearchResponse(
        count=27,
        offset=12,
        releases=[
            MbReleaseSearchRelease(
                id="release-1",
                score=98,
                title="Album (Deluxe)",
                artist_credit=[
                    MbReleaseSearchArtistCredit(
                        name="Artist", artist=MbReleaseSearchArtist(id="artist-1")
                    )
                ],
                release_group=MbReleaseSearchGroup(id="group-1", title="Album"),
                date="2026-01-02",
                country="GB",
                status="Official",
                packaging=None,
                media=[
                    MbReleaseSearchMedium(format="CD", track_count=10),
                    MbReleaseSearchMedium(format="CD", track_count=4),
                ],
                label_info=[
                    MbReleaseSearchLabelInfo(
                        catalog_number="CAT-1",
                        label=MbReleaseSearchLabel(id="label-1", name="Label"),
                    )
                ],
                barcode="123456",
                disambiguation="bonus disc",
            )
        ],
    )
    with patch(
        "repositories.musicbrainz_album.mb_api_get", AsyncMock(return_value=payload)
    ) as mock_get:
        page = await _Repo().search_release_editions(
            'Album: "Deluxe"', "Artist+ Co", limit=12, offset=12
        )

    assert page.total == 27
    assert page.offset == 12
    assert page.items[0].release_mbid == "release-1"
    assert page.items[0].release_group_mbid == "group-1"
    assert page.items[0].media_formats == ["CD"]
    assert page.items[0].disc_count == 2
    assert page.items[0].track_count == 14
    assert page.items[0].label == "Label"
    assert page.items[0].catalogue_number == "CAT-1"
    assert page.items[0].packaging is None
    assert page.items[0].musicbrainz_url.endswith("/release/release-1")
    assert mock_get.await_args.kwargs["params"] == {
        "query": 'release:"Album\\: \\"Deluxe\\"" AND artist:"Artist\\+ Co"',
        "limit": 12,
        "offset": 12,
    }
    assert mock_get.await_args.kwargs["priority"] is RequestPriority.USER_INITIATED


@pytest.mark.asyncio
async def test_release_edition_search_uses_dedicated_cache():
    repo = _Repo()
    with patch(
        "repositories.musicbrainz_album.mb_api_get",
        AsyncMock(return_value=MbReleaseSearchResponse()),
    ):
        cached = await repo.search_release_editions("Album", "Artist")
    repo._cache.get.return_value = cached

    with patch("repositories.musicbrainz_album.mb_api_get", AsyncMock()) as mock_get:
        repeated = await repo.search_release_editions("Album", "Artist")

    assert repeated is cached
    mock_get.assert_not_awaited()


@pytest.mark.asyncio
async def test_release_edition_search_normalizes_provider_failures():
    request = httpx.Request("GET", "https://musicbrainz.org/ws/2/release")
    with patch(
        "repositories.musicbrainz_album.mb_api_get",
        AsyncMock(
            side_effect=httpx.ConnectError("private host detail", request=request)
        ),
    ):
        with pytest.raises(
            ExternalServiceError,
            match="MusicBrainz release search is temporarily unavailable",
        ) as raised:
            await _Repo().search_release_editions("Album", "Artist")

    assert "private host detail" not in str(raised.value)


@pytest.mark.asyncio
async def test_structured_release_group_search_keeps_artist_and_title_separate():
    with patch(
        "repositories.musicbrainz_album.mb_api_get",
        AsyncMock(return_value=_ReleaseGroupSearchPayload()),
    ) as mock_get:
        await _Repo().search_release_groups(
            "Clairo + band", 'Originals: "Deluxe"', include_all_types=True
        )

    assert mock_get.await_args.kwargs["params"]["query"] == (
        '(releasegroup:"Originals\\: \\"Deluxe\\"" '
        'OR release:"Originals\\: \\"Deluxe\\"") '
        'AND artist:"Clairo \\+ band"'
    )


@pytest.mark.asyncio
async def test_search_recordings_defaults_to_user_initiated():
    with patch(
        "repositories.musicbrainz_album.mb_api_get",
        AsyncMock(return_value=_RecordingSearchPayload()),
    ) as mock_get:
        await _Repo().search_recordings("artist", "title")
    assert mock_get.await_args.kwargs["priority"] == RequestPriority.USER_INITIATED


@pytest.mark.asyncio
async def test_search_recordings_forwards_background_priority():
    with patch(
        "repositories.musicbrainz_album.mb_api_get",
        AsyncMock(return_value=_RecordingSearchPayload()),
    ) as mock_get:
        await _Repo().search_recordings(
            "artist", "title", priority=RequestPriority.BACKGROUND_SYNC
        )
    assert mock_get.await_args.kwargs["priority"] == RequestPriority.BACKGROUND_SYNC


@pytest.mark.asyncio
async def test_recording_search_escapes_structured_artist_and_title_fields():
    with patch(
        "repositories.musicbrainz_album.mb_api_get",
        AsyncMock(return_value=_RecordingSearchPayload()),
    ) as mock_get:
        await _Repo().search_recordings("Artist + Co", 'Song: "Live"')

    assert mock_get.await_args.kwargs["params"]["query"] == (
        'recording:"Song\\: \\"Live\\"" AND artist:"Artist \\+ Co"'
    )


@pytest.mark.asyncio
async def test_mixed_priority_release_group_callers_get_separate_leaders():
    repo = _Repo()
    started = asyncio.Event()
    release = asyncio.Event()
    priorities = []

    async def provider(*_args, **kwargs):
        priorities.append(kwargs["priority"])
        started.set()
        await release.wait()
        return _ReleaseGroupSearchPayload()

    mock_get = AsyncMock(side_effect=provider)
    with patch("repositories.musicbrainz_album.mb_api_get", mock_get):
        background = asyncio.create_task(
            repo.search_release_groups(
                "Artist",
                "query",
                priority=RequestPriority.BACKGROUND_SYNC,
            )
        )
        await started.wait()
        foreground = asyncio.create_task(
            repo.search_release_groups(
                "Artist",
                "query",
                priority=RequestPriority.USER_INITIATED,
            )
        )
        await asyncio.sleep(0)
        assert not foreground.done()
        release.set()
        await asyncio.gather(background, foreground)

    assert mock_get.await_count == 2
    assert sorted(priorities) == sorted(
        [RequestPriority.BACKGROUND_SYNC, RequestPriority.USER_INITIATED]
    )
