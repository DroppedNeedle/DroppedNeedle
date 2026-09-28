import asyncio
import inspect
from pathlib import Path
from unittest.mock import AsyncMock

import httpx
import pytest

import repositories.coverart_repository as coverart_module
from core.exceptions import ArtworkProcessingError, ExternalServiceError
from infrastructure.cache.cache_keys import (
    CAA_MANAGEMENT_PREFIX,
    coverart_management_key,
    coverart_prefixes,
)
from infrastructure.cache.memory_cache import InMemoryCache
from infrastructure.queue.priority_queue import RequestPriority
from models.library_management_artwork import ArtworkCandidate
from repositories.coverart_repository import CoverArtRepository
from repositories.protocols.coverart_management import (
    ManagementCoverArtRepositoryProtocol,
)

_RELEASE = "aff0622e-7bd3-4fb6-9ca3-0fa19dd2340b"
_RG = "dcff25f1-702d-3b5e-b0da-d48172e6e62a"
_FIXTURE = (
    Path(__file__).parents[1] / "fixtures" / "coverart" / "management_release.json"
).read_bytes()


@pytest.fixture(autouse=True)
def no_cover_rate_wait(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(coverart_module._coverart_rate_limiter, "acquire", AsyncMock())


@pytest.mark.asyncio
async def test_exact_release_candidates_keep_types_approval_and_selected_size(
    tmp_path: Path,
) -> None:
    requests: list[str] = []

    def handler(request: httpx.Request) -> httpx.Response:
        requests.append(str(request.url))
        return httpx.Response(200, content=_FIXTURE, request=request)

    async with httpx.AsyncClient(transport=httpx.MockTransport(handler)) as client:
        repository = CoverArtRepository(client, InMemoryCache(), cache_dir=tmp_path)
        result = await repository.list_management_artwork(
            entity_kind="release",
            mbid=_RELEASE,
            download_size="500",
            priority=RequestPriority.BACKGROUND_SYNC,
        )
        cached = await repository.list_management_artwork(
            entity_kind="release",
            mbid=_RELEASE,
            download_size="500",
            priority=RequestPriority.BACKGROUND_SYNC,
        )

    assert result == cached
    assert len(requests) == 1
    assert result[0].source == "cover_art_archive_release"
    assert result[0].source_is_exact_release is True
    assert result[0].image_types == ("front",)
    assert result[0].locator.endswith("/111-500.jpg")
    assert result[0].locator.startswith("https://coverartarchive.org/")
    assert result[1].approved is False
    assert result[1].image_types == ("back", "spine")
    assert result[2].image_types == ("other",)


@pytest.mark.asyncio
async def test_release_group_candidates_are_explicitly_fallback_labelled(
    tmp_path: Path,
) -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(200, content=_FIXTURE, request=request)

    async with httpx.AsyncClient(transport=httpx.MockTransport(handler)) as client:
        repository = CoverArtRepository(client, InMemoryCache(), cache_dir=tmp_path)
        result = await repository.list_management_artwork(
            entity_kind="release-group",
            mbid=_RG,
            download_size="full",
            priority=RequestPriority.USER_INITIATED,
        )

    assert result[0].source == "cover_art_archive_release_group"
    assert result[0].source_is_exact_release is False
    assert result[0].source_entity_mbid == _RG


@pytest.mark.asyncio
async def test_identical_metadata_requests_are_coalesced(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    started = asyncio.Event()
    release = asyncio.Event()

    async def load(*args, **kwargs):
        del args, kwargs
        started.set()
        await release.wait()
        return 200, _FIXTURE, "application/json"

    async with httpx.AsyncClient() as client:
        repository = CoverArtRepository(client, InMemoryCache(), cache_dir=tmp_path)
        stream = AsyncMock(side_effect=load)
        monkeypatch.setattr(repository, "_stream_management_artwork", stream)
        first = asyncio.create_task(
            repository.list_management_artwork(
                entity_kind="release",
                mbid=_RELEASE,
                download_size="full",
                priority=RequestPriority.BACKGROUND_SYNC,
            )
        )
        await started.wait()
        second = asyncio.create_task(
            repository.list_management_artwork(
                entity_kind="release",
                mbid=_RELEASE,
                download_size="full",
                priority=RequestPriority.BACKGROUND_SYNC,
            )
        )
        await asyncio.sleep(0)
        release.set()
        assert await first == await second

    assert stream.await_count == 1


@pytest.mark.asyncio
async def test_404_is_cached_as_authoritative_absence(tmp_path: Path) -> None:
    handler = AsyncMock(
        side_effect=lambda request: httpx.Response(404, request=request)
    )
    async with httpx.AsyncClient(transport=httpx.MockTransport(handler)) as client:
        repository = CoverArtRepository(client, InMemoryCache(), cache_dir=tmp_path)
        first = await repository.list_management_artwork(
            entity_kind="release",
            mbid=_RELEASE,
            download_size="full",
            priority=RequestPriority.USER_INITIATED,
        )
        second = await repository.list_management_artwork(
            entity_kind="release",
            mbid=_RELEASE,
            download_size="full",
            priority=RequestPriority.USER_INITIATED,
        )

    assert first == second == ()
    assert handler.await_count == 1


@pytest.mark.asyncio
async def test_invalid_json_and_untrusted_image_hosts_are_provider_errors(
    tmp_path: Path,
) -> None:
    responses = iter(
        [
            b"not-json",
            b'{"images":[{"approved":true,"id":1,"image":"https://example.com/a.jpg","types":["Front"]}]}',
        ]
    )

    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(200, content=next(responses), request=request)

    async with httpx.AsyncClient(transport=httpx.MockTransport(handler)) as client:
        repository = CoverArtRepository(client, InMemoryCache(), cache_dir=tmp_path)
        with pytest.raises(ExternalServiceError, match="invalid artwork metadata"):
            await repository.list_management_artwork(
                entity_kind="release",
                mbid=_RELEASE,
                download_size="full",
                priority=RequestPriority.USER_INITIATED,
            )
        with pytest.raises(ExternalServiceError, match="invalid artwork location"):
            await repository.list_management_artwork(
                entity_kind="release-group",
                mbid=_RG,
                download_size="full",
                priority=RequestPriority.USER_INITIATED,
            )


@pytest.mark.asyncio
async def test_string_image_ids_decode_like_integer_ids(tmp_path: Path) -> None:
    payload = (
        b'{"images":[{"approved":true,"back":false,"comment":"","front":true,'
        b'"id":"10118092077",'
        b'"image":"https://coverartarchive.org/release/'
        + _RELEASE.encode()
        + b'/10118092077.jpg",'
        b'"thumbnails":{"500":"https://coverartarchive.org/release/'
        + _RELEASE.encode()
        + b'/10118092077-500.jpg"},"types":["Front"]}]}'
    )

    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(200, content=payload, request=request)

    async with httpx.AsyncClient(transport=httpx.MockTransport(handler)) as client:
        repository = CoverArtRepository(client, InMemoryCache(), cache_dir=tmp_path)
        result = await repository.list_management_artwork(
            entity_kind="release",
            mbid=_RELEASE,
            download_size="500",
            priority=RequestPriority.BACKGROUND_SYNC,
        )

    assert len(result) == 1
    assert result[0].candidate_id == f"caa:release:{_RELEASE}:10118092077:500"
    assert result[0].locator.endswith("/10118092077-500.jpg")
    assert result[0].primary is True


@pytest.mark.asyncio
async def test_undecodable_metadata_is_logged_as_a_warning(
    tmp_path: Path, caplog: pytest.LogCaptureFixture
) -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(
            200, content=b'{"images":[{"id":{"nested":true}}]}', request=request
        )

    async with httpx.AsyncClient(transport=httpx.MockTransport(handler)) as client:
        repository = CoverArtRepository(client, InMemoryCache(), cache_dir=tmp_path)
        with caplog.at_level("WARNING", logger=coverart_module.__name__):
            with pytest.raises(ExternalServiceError, match="invalid artwork metadata"):
                await repository.list_management_artwork(
                    entity_kind="release-group",
                    mbid=_RG,
                    download_size="full",
                    priority=RequestPriority.USER_INITIATED,
                )

    assert any(
        "could not be decoded" in record.getMessage() and _RG in record.getMessage()
        for record in caplog.records
    )


@pytest.mark.asyncio
async def test_download_enforces_declared_and_actual_byte_bounds(
    tmp_path: Path,
) -> None:
    responses = iter(
        [
            httpx.Response(200, content=b"123", headers={"Content-Length": "99"}),
            httpx.Response(200, content=b"123456"),
            httpx.Response(200, content=b"png", headers={"Content-Type": "image/png"}),
        ]
    )

    def handler(request: httpx.Request) -> httpx.Response:
        response = next(responses)
        response.request = request
        return response

    candidate = ArtworkCandidate(
        candidate_id="candidate",
        source="cover_art_archive_release",
        locator=f"https://coverartarchive.org/release/{_RELEASE}/1.png",
        image_types=("front",),
        approved=True,
        primary=True,
    )
    async with httpx.AsyncClient(transport=httpx.MockTransport(handler)) as client:
        repository = CoverArtRepository(client, InMemoryCache(), cache_dir=tmp_path)
        with pytest.raises(ArtworkProcessingError, match="safety limit"):
            await repository.download_management_artwork(
                candidate, maximum_bytes=10, priority=RequestPriority.USER_INITIATED
            )
        with pytest.raises(ArtworkProcessingError, match="safety limit"):
            await repository.download_management_artwork(
                candidate, maximum_bytes=5, priority=RequestPriority.USER_INITIATED
            )
        assert await repository.download_management_artwork(
            candidate, maximum_bytes=10, priority=RequestPriority.USER_INITIATED
        ) == (b"png", "image/png")


def test_cache_key_and_protocol_contract() -> None:
    assert coverart_management_key("release", _RELEASE, "full").startswith(
        CAA_MANAGEMENT_PREFIX
    )
    assert CAA_MANAGEMENT_PREFIX in coverart_prefixes()
    assert inspect.signature(
        ManagementCoverArtRepositoryProtocol.list_management_artwork
    ) == inspect.signature(CoverArtRepository.list_management_artwork)
    assert inspect.signature(
        ManagementCoverArtRepositoryProtocol.download_management_artwork
    ) == inspect.signature(CoverArtRepository.download_management_artwork)
