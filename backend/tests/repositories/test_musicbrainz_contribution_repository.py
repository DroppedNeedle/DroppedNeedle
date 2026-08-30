import asyncio
import inspect
from pathlib import Path
from unittest.mock import AsyncMock

import msgspec
import httpx
import pytest

from core.exceptions import ExternalServiceError, InvalidExternalPayloadError
from infrastructure.queue.priority_queue import RequestPriority
from infrastructure.resilience.retry import CircuitOpenError
from models.library_contribution import MusicBrainzDuplicateFacts
from repositories.musicbrainz_album import MusicBrainzAlbumMixin
from repositories.musicbrainz_contribution_models import (
    MbContributionRelease,
    MbContributionReleaseSearch,
    MbContributionUrl,
)
from repositories.musicbrainz_repository import MusicBrainzRepository
from repositories.protocols.musicbrainz import MusicBrainzRepositoryProtocol

_FIXTURES = Path(__file__).resolve().parents[1] / "fixtures" / "musicbrainz"


class _Repo(MusicBrainzAlbumMixin):
    def __init__(self) -> None:
        self._cache = AsyncMock()
        self._cache.get.return_value = None


def _decoded(name: str, model_type: type):
    return msgspec.json.decode((_FIXTURES / name).read_bytes(), type=model_type)


@pytest.mark.asyncio
async def test_url_resolution_retains_unique_and_ambiguous_release_targets(
    monkeypatch,
) -> None:
    import repositories.musicbrainz_album as module

    api = AsyncMock(
        side_effect=[
            _decoded("contribution_url_release.json", MbContributionUrl),
            _decoded("contribution_url_multiple.json", MbContributionUrl),
        ]
    )
    monkeypatch.setattr(module, "mb_api_get", api)
    repo = _Repo()
    one = await repo.resolve_url(
        "https://www.discogs.com/release/3562468",
        includes=("release-rels",),
        priority=RequestPriority.USER_INITIATED,
    )
    multiple = await repo.resolve_url(
        "https://www.discogs.com/release/999",
        includes=("release-rels",),
        priority=RequestPriority.USER_INITIATED,
    )
    assert one.release_mbids == ["aff0622e-7bd3-4fb6-9ca3-0fa19dd2340b"]
    assert multiple.release_mbids == [
        "11111111-1111-4111-8111-111111111111",
        "22222222-2222-4222-8222-222222222222",
    ]
    assert api.await_args_list[0].kwargs["priority"] is RequestPriority.USER_INITIATED


@pytest.mark.asyncio
async def test_release_verification_normalizes_tracks_and_artist(monkeypatch) -> None:
    import repositories.musicbrainz_album as module

    monkeypatch.setattr(
        module,
        "mb_api_get",
        AsyncMock(
            return_value=_decoded("contribution_release.json", MbContributionRelease)
        ),
    )
    release = await _Repo().get_release_for_verification(
        "aff0622e-7bd3-4fb6-9ca3-0fa19dd2340b",
        priority=RequestPriority.BACKGROUND_SYNC,
    )
    assert release is not None
    assert release.release_group_mbid == "dcff25f1-702d-3b5e-b0da-d48172e6e62a"
    assert release.artist_name == "Glenn Gould"
    assert release.tracks[0].duration_seconds == 188
    assert release.tracks[0].recording_mbid == "33333333-3333-4333-8333-333333333333"


@pytest.mark.asyncio
async def test_duplicate_search_uses_typed_facts_and_cache(monkeypatch) -> None:
    import repositories.musicbrainz_album as module

    api = AsyncMock(
        return_value=_decoded("contribution_search.json", MbContributionReleaseSearch)
    )
    monkeypatch.setattr(module, "mb_api_get", api)
    repo = _Repo()
    facts = MusicBrainzDuplicateFacts(
        title="Goldberg Variations, BWV 988",
        artist_name='Glenn "Gould" + Co',
        barcode="5099705264827",
    )
    results = await repo.search_duplicate_releases(
        facts, priority=RequestPriority.USER_INITIATED, limit=8
    )
    assert [release.release_mbid for release in results] == [
        "aff0622e-7bd3-4fb6-9ca3-0fa19dd2340b"
    ]
    query = api.await_args.kwargs["params"]["query"]
    assert 'release:"Goldberg Variations, BWV 988"' in query
    assert 'artist:"Glenn \\"Gould\\" \\+ Co"' in query
    assert 'barcode:"5099705264827"' in query


@pytest.mark.parametrize(
    "failure",
    [
        httpx.ConnectError("connection failed"),
        CircuitOpenError("circuit open", "musicbrainz"),
    ],
)
@pytest.mark.asyncio
async def test_verification_normalizes_transport_and_circuit_failures(
    monkeypatch, failure: Exception
) -> None:
    import repositories.musicbrainz_album as module

    monkeypatch.setattr(module, "mb_api_get", AsyncMock(side_effect=failure))

    with pytest.raises(ExternalServiceError, match="temporarily unavailable"):
        await _Repo().get_release_for_verification(
            "aff0622e-7bd3-4fb6-9ca3-0fa19dd2340b",
            priority=RequestPriority.BACKGROUND_SYNC,
            bypass_cache=True,
        )


@pytest.mark.asyncio
async def test_release_verification_accepts_live_null_packaging(monkeypatch) -> None:
    import repositories.musicbrainz_album as module

    monkeypatch.setattr(
        module,
        "mb_api_get",
        AsyncMock(
            return_value=_decoded(
                "contribution_release_null_packaging.json", MbContributionRelease
            )
        ),
    )
    release = await _Repo().get_release_for_verification(
        "aff0622e-7bd3-4fb6-9ca3-0fa19dd2340b",
        priority=RequestPriority.BACKGROUND_SYNC,
    )
    assert release is not None
    assert release.packaging is None
    assert release.status == "Official"
    assert release.date == "1992-08-31"
    assert release.release_group_mbid == "dcff25f1-702d-3b5e-b0da-d48172e6e62a"


@pytest.mark.asyncio
async def test_url_resolution_accepts_live_null_status_and_packaging(
    monkeypatch,
) -> None:
    import repositories.musicbrainz_album as module

    monkeypatch.setattr(
        module,
        "mb_api_get",
        AsyncMock(
            return_value=_decoded(
                "contribution_url_release_null.json", MbContributionUrl
            )
        ),
    )
    resolution = await _Repo().resolve_url(
        "https://www.discogs.com/release/3562468",
        includes=("release-rels",),
        priority=RequestPriority.USER_INITIATED,
    )
    assert resolution.release_mbids == ["aff0622e-7bd3-4fb6-9ca3-0fa19dd2340b"]


@pytest.mark.asyncio
async def test_duplicate_search_omitted_optional_fields_decode_to_none(
    monkeypatch,
) -> None:
    import repositories.musicbrainz_album as module

    monkeypatch.setattr(
        module,
        "mb_api_get",
        AsyncMock(
            return_value=_decoded(
                "contribution_search.json", MbContributionReleaseSearch
            )
        ),
    )
    results = await _Repo().search_duplicate_releases(
        MusicBrainzDuplicateFacts(
            title="Goldberg Variations, BWV 988",
            artist_name="Glenn Gould",
        ),
        priority=RequestPriority.USER_INITIATED,
        limit=8,
    )
    assert results
    assert results[0].packaging is None
    assert results[0].status is None


@pytest.mark.asyncio
async def test_verification_raises_typed_payload_error_records_degradation_no_cache(
    monkeypatch,
) -> None:
    import repositories.musicbrainz_album as module

    monkeypatch.setattr(
        module,
        "mb_api_get",
        AsyncMock(side_effect=InvalidExternalPayloadError("unexpected payload shape")),
    )
    degraded: list[str] = []
    monkeypatch.setattr(
        module, "_record_mb_degradation", lambda msg: degraded.append(msg)
    )
    repo = _Repo()
    with pytest.raises(InvalidExternalPayloadError):
        await repo.get_release_for_verification(
            "aff0622e-7bd3-4fb6-9ca3-0fa19dd2340b",
            priority=RequestPriority.BACKGROUND_SYNC,
            bypass_cache=True,
        )
    assert degraded == [
        "MusicBrainz release verification returned an unmappable payload: "
        "unexpected payload shape"
    ]
    repo._cache.set.assert_not_awaited()


@pytest.mark.asyncio
async def test_duplicate_search_raises_typed_payload_error(monkeypatch) -> None:
    import repositories.musicbrainz_album as module

    monkeypatch.setattr(
        module,
        "mb_api_get",
        AsyncMock(side_effect=InvalidExternalPayloadError("unexpected payload shape")),
    )
    with pytest.raises(InvalidExternalPayloadError):
        await _Repo().search_duplicate_releases(
            MusicBrainzDuplicateFacts(title="Goldberg", artist_name="Gould"),
            priority=RequestPriority.USER_INITIATED,
            limit=8,
        )


@pytest.mark.asyncio
async def test_fresh_verification_requests_are_deduplicated(monkeypatch) -> None:
    import repositories.musicbrainz_album as module

    started = asyncio.Event()
    release = asyncio.Event()

    async def load(*_args, **_kwargs):
        started.set()
        await release.wait()
        return _decoded("contribution_release.json", MbContributionRelease)

    api = AsyncMock(side_effect=load)
    monkeypatch.setattr(module, "mb_api_get", api)
    repo = _Repo()
    first = asyncio.create_task(
        repo.get_release_for_verification(
            "aff0622e-7bd3-4fb6-9ca3-0fa19dd2340b",
            priority=RequestPriority.BACKGROUND_SYNC,
            bypass_cache=True,
        )
    )
    await started.wait()
    second = asyncio.create_task(
        repo.get_release_for_verification(
            "aff0622e-7bd3-4fb6-9ca3-0fa19dd2340b",
            priority=RequestPriority.BACKGROUND_SYNC,
            bypass_cache=True,
        )
    )
    await asyncio.sleep(0)
    release.set()

    await asyncio.gather(first, second)
    assert api.await_count == 1


def test_contribution_methods_conform_to_protocol() -> None:
    for name in (
        "resolve_url",
        "get_release_for_verification",
        "search_duplicate_releases",
        "search_release_groups",
        # A2: priority threading on the artist detail leg.
        "get_artist_by_id",
        "get_artist_release_groups_with_context",
    ):
        assert inspect.signature(
            getattr(MusicBrainzRepositoryProtocol, name)
        ) == inspect.signature(getattr(MusicBrainzRepository, name))
