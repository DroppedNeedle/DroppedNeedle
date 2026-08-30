import pytest
from unittest.mock import AsyncMock, MagicMock
import asyncio
from types import SimpleNamespace

from api.v1.schemas.search import SearchResult, SuggestResponse
from infrastructure.degradation import (
    clear_degradation_context,
    init_degradation_context,
)
from infrastructure.integration_result import IntegrationResult
from services.search_service import SearchService


@pytest.fixture(autouse=True)
def clear_search_state():
    SearchService.clear_cached_results()
    clear_degradation_context()
    yield
    SearchService.clear_cached_results()
    clear_degradation_context()


def _make_search_result(
    type: str,
    title: str,
    score: int = 0,
    musicbrainz_id: str = "",
    artist: str | None = None,
    year: int | None = None,
    disambiguation: str | None = None,
) -> SearchResult:
    return SearchResult(
        type=type,
        title=title,
        musicbrainz_id=musicbrainz_id or f"mbid-{title.lower().replace(' ', '-')}",
        score=score,
        artist=artist,
        year=year,
        disambiguation=disambiguation,
        in_library=False,
        requested=False,
    )


def _make_preferences(
    secondary_types: list[str] | None = None,
    primary_types: list[str] | None = None,
) -> MagicMock:
    prefs = MagicMock()
    prefs.secondary_types = secondary_types or []
    prefs.primary_types = primary_types or [
        "album",
        "single",
        "ep",
        "broadcast",
        "other",
    ]
    return prefs


def _make_service(
    grouped: dict[str, list[SearchResult]] | None = None,
    library_mbids: set[str] | None = None,
    mb_error: Exception | None = None,
    library_error: Exception | None = None,
) -> SearchService:
    mb_repo = MagicMock()
    if mb_error:
        mb_repo.search_grouped = AsyncMock(side_effect=mb_error)
    else:
        mb_repo.search_grouped = AsyncMock(
            return_value=grouped or {"artists": [], "albums": []}
        )

    library_repo = MagicMock()
    if library_error:
        library_repo.get_library_mbids = AsyncMock(side_effect=library_error)
    else:
        library_repo.get_library_mbids = AsyncMock(return_value=library_mbids or set())

    coverart_repo = MagicMock()
    preferences_service = MagicMock()
    preferences_service.get_preferences.return_value = _make_preferences()

    return SearchService(
        mb_repo=mb_repo,
        library_repo=library_repo,
        coverart_repo=coverart_repo,
        preferences_service=preferences_service,
    )


@pytest.mark.asyncio
async def test_suggest_returns_suggest_response():
    artists = [_make_search_result("artist", "Muse", score=90)]
    albums = [
        _make_search_result("album", "Origin of Symmetry", score=85, artist="Muse")
    ]
    svc = _make_service(grouped={"artists": artists, "albums": albums})

    result = await svc.suggest(query="muse", limit=5)

    assert isinstance(result, SuggestResponse)
    assert len(result.results) == 2
    assert result.results[0].title == "Muse"
    assert result.results[1].title == "Origin of Symmetry"


@pytest.mark.asyncio
async def test_target_suggest_projects_only_returned_albums() -> None:
    album = _make_search_result(
        "album", "Origin of Symmetry", artist="Muse", musicbrainz_id="rg-1"
    )
    service = _make_service(grouped={"artists": [], "albums": [album]})
    ownership = AsyncMock()
    ownership.project_albums.return_value = [SimpleNamespace(owned=True)]
    service._ownership = ownership

    result = await service.suggest(query="muse", limit=5)

    assert result.results[0].in_library is True
    service._library_repo.get_library_mbids.assert_not_awaited()
    candidates = ownership.project_albums.await_args.args[0]
    assert [candidate.release_group_mbid for candidate in candidates] == ["rg-1"]


@pytest.mark.asyncio
async def test_suggest_score_interleaving():
    artists = [
        _make_search_result("artist", "Artist A", score=90),
        _make_search_result("artist", "Artist B", score=80),
    ]
    albums = [
        _make_search_result("album", "Album X", score=95, artist="X"),
        _make_search_result("album", "Album Y", score=85, artist="Y"),
    ]
    svc = _make_service(grouped={"artists": artists, "albums": albums})

    result = await svc.suggest(query="test", limit=5)

    assert len(result.results) == 4
    assert result.results[0].title == "Album X"
    assert result.results[0].score == 95
    assert result.results[1].title == "Artist A"
    assert result.results[1].score == 90
    assert result.results[2].title == "Album Y"
    assert result.results[2].score == 85
    assert result.results[3].title == "Artist B"
    assert result.results[3].score == 80


@pytest.mark.asyncio
async def test_suggest_equal_score_artist_before_album():
    artists = [_make_search_result("artist", "Bee", score=80)]
    albums = [_make_search_result("album", "Ant", score=80, artist="Someone")]
    svc = _make_service(grouped={"artists": artists, "albums": albums})

    result = await svc.suggest(query="test", limit=5)

    assert len(result.results) == 2
    assert result.results[0].type == "artist"
    assert result.results[0].title == "Bee"
    assert result.results[1].type == "album"
    assert result.results[1].title == "Ant"


@pytest.mark.asyncio
async def test_suggest_alphabetical_tiebreak_within_same_type():
    artists = [
        _make_search_result("artist", "Zebra", score=80),
        _make_search_result("artist", "Alpha", score=80),
    ]
    svc = _make_service(grouped={"artists": artists, "albums": []})

    result = await svc.suggest(query="test", limit=5)

    assert len(result.results) == 2
    assert result.results[0].title == "Alpha"
    assert result.results[1].title == "Zebra"


@pytest.mark.asyncio
async def test_suggest_truncates_to_limit():
    artists = [
        _make_search_result("artist", f"Artist {i}", score=100 - i) for i in range(3)
    ]
    albums = [
        _make_search_result("album", f"Album {i}", score=99 - i, artist="X")
        for i in range(3)
    ]
    svc = _make_service(grouped={"artists": artists, "albums": albums})

    result = await svc.suggest(query="test", limit=4)

    assert len(result.results) == 4


@pytest.mark.asyncio
async def test_suggest_library_failure_returns_default_flags():
    artists = [_make_search_result("artist", "Muse", score=90)]
    albums = [
        _make_search_result(
            "album", "Absolution", score=85, artist="Muse", musicbrainz_id="album-1"
        ),
    ]
    svc = _make_service(
        grouped={"artists": artists, "albums": albums},
        library_error=Exception("library unavailable"),
    )

    result = await svc.suggest(query="muse", limit=5)

    assert len(result.results) == 2
    for r in result.results:
        assert r.in_library is False
        assert r.requested is False


@pytest.mark.asyncio
async def test_suggest_musicbrainz_failure_returns_empty():
    svc = _make_service(mb_error=Exception("MusicBrainz down"))

    result = await svc.suggest(query="muse", limit=5)

    assert isinstance(result, SuggestResponse)
    assert len(result.results) == 0


@pytest.mark.asyncio
async def test_suggest_query_normalization():
    artists = [_make_search_result("artist", "Muse", score=90)]
    svc = _make_service(grouped={"artists": artists, "albums": []})

    result = await svc.suggest(query="  muse  ", limit=5)

    assert len(result.results) == 1
    assert result.results[0].title == "Muse"
    svc._mb_repo.search_grouped.assert_called_once()
    call_args = svc._mb_repo.search_grouped.call_args
    assert call_args[0][0] == "muse"


@pytest.mark.asyncio
async def test_suggest_in_library_flag():
    albums = [
        _make_search_result(
            "album", "Absolution", score=85, artist="Muse", musicbrainz_id="album-lib-1"
        ),
    ]
    svc = _make_service(
        grouped={"artists": [], "albums": albums},
        library_mbids={"album-lib-1"},
    )

    result = await svc.suggest(query="absolution", limit=5)

    assert len(result.results) == 1
    assert result.results[0].in_library is True
    assert result.results[0].requested is False


@pytest.mark.asyncio
async def test_suggest_whitespace_only_query_returns_empty():
    svc = _make_service(grouped={"artists": [], "albums": []})

    result = await svc.suggest(query="  a  ", limit=5)

    assert isinstance(result, SuggestResponse)
    assert len(result.results) == 0
    svc._mb_repo.search_grouped.assert_not_called()


@pytest.mark.asyncio
async def test_suggest_single_char_after_strip_returns_empty():
    svc = _make_service(grouped={"artists": [], "albums": []})

    result = await svc.suggest(query="x", limit=5)

    assert isinstance(result, SuggestResponse)
    assert len(result.results) == 0
    svc._mb_repo.search_grouped.assert_not_called()


@pytest.mark.asyncio
async def test_suggest_case_insensitive_alphabetical_tiebreak():
    artists = [
        _make_search_result("artist", "Bravo", score=80),
        _make_search_result("artist", "alpha", score=80),
    ]
    svc = _make_service(grouped={"artists": artists, "albums": []})

    result = await svc.suggest(query="test", limit=5)

    assert len(result.results) == 2
    assert result.results[0].title == "alpha"
    assert result.results[1].title == "Bravo"


@pytest.mark.asyncio
async def test_suggest_deduplication_single_mb_call():
    artists = [_make_search_result("artist", "Muse", score=90)]

    call_event = asyncio.Event()
    call_count = 0

    async def slow_search_grouped(*args, **kwargs):
        nonlocal call_count
        call_count += 1
        await call_event.wait()
        return {"artists": artists, "albums": []}

    mb_repo = MagicMock()
    mb_repo.search_grouped = slow_search_grouped

    library_repo = MagicMock()
    library_repo.get_library_mbids = AsyncMock(return_value=set())
    library_repo.get_queue = AsyncMock(return_value=[])

    coverart_repo = MagicMock()
    preferences_service = MagicMock()
    preferences_service.get_preferences.return_value = _make_preferences()

    svc = SearchService(
        mb_repo=mb_repo,
        library_repo=library_repo,
        coverart_repo=coverart_repo,
        preferences_service=preferences_service,
    )

    task1 = asyncio.create_task(svc.suggest(query="muse", limit=5))
    task2 = asyncio.create_task(svc.suggest(query="muse", limit=5))
    await asyncio.sleep(0.05)
    call_event.set()

    r1, r2 = await asyncio.gather(task1, task2)

    assert call_count == 1
    assert len(r1.results) == 1
    assert len(r2.results) == 1


@pytest.mark.asyncio
async def test_suggest_reports_partial_results_after_one_provider_bucket_fails():
    artist = _make_search_result("artist", "Muse", score=95)
    service = _make_service(grouped={"artists": [artist], "albums": []})

    async def partial_search(*args, **kwargs):
        context = init_degradation_context()
        context.record(
            IntegrationResult.error(source="musicbrainz", msg="release search 503")
        )
        return {"artists": [artist], "albums": []}

    service._mb_repo.search_grouped = AsyncMock(side_effect=partial_search)

    result = await service.suggest(query="muse", limit=5)

    assert [item.title for item in result.results] == ["Muse"]
    assert result.remote_status == "partial"


@pytest.mark.asyncio
async def test_suggest_timeout_is_terminal_and_explicit(monkeypatch):
    service = _make_service()

    async def never_returns(*args, **kwargs):
        await asyncio.Event().wait()

    service._mb_repo.search_grouped = AsyncMock(side_effect=never_returns)
    monkeypatch.setattr("services.search_service.SUGGEST_TIMEOUT_SECONDS", 0.01)
    init_degradation_context()

    result = await service.suggest(query="unfamiliar", limit=5)

    assert result.results == []
    assert result.remote_status == "timeout"


@pytest.mark.asyncio
async def test_search_bucket_timeout_does_not_return_a_false_empty_state(monkeypatch):
    service = _make_service()

    async def never_returns(*args, **kwargs):
        await asyncio.Event().wait()

    service._mb_repo.search_artists = AsyncMock(side_effect=never_returns)
    monkeypatch.setattr("services.search_service.FULL_SEARCH_TIMEOUT_SECONDS", 0.01)
    init_degradation_context()

    results, top_result, status = await service.search_bucket(
        "artists", "unfamiliar", limit=10
    )

    assert results == []
    assert top_result is None
    assert status == "timeout"


@pytest.mark.asyncio
async def test_search_bucket_provider_failure_is_explicit():
    service = _make_service()
    service._mb_repo.search_albums = AsyncMock(
        side_effect=RuntimeError("provider returned 503")
    )
    init_degradation_context()

    results, top_result, status = await service.search_bucket(
        "albums", "unfamiliar", limit=10
    )

    assert results == []
    assert top_result is None
    assert status == "error"


@pytest.mark.asyncio
async def test_search_bucket_preserves_success_when_other_bucket_is_unavailable():
    service = _make_service()
    artist = _make_search_result("artist", "Alice Coltrane", score=100)
    service._mb_repo.search_artists = AsyncMock(return_value=[artist])
    init_degradation_context()

    results, top_result, status = await service.search_bucket(
        "artists", "Alice Coltrane", limit=10
    )

    assert results == [artist]
    assert top_result == artist
    assert status == "ok"


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("failed_bucket", "expected_status"),
    [
        ("artists", {"artists": "error", "albums": "ok"}),
        ("albums", {"artists": "ok", "albums": "error"}),
    ],
)
async def test_combined_search_reports_each_bucket_status(
    failed_bucket: str, expected_status: dict[str, str]
):
    artist = _make_search_result("artist", "Alice Coltrane", score=100)
    album = _make_search_result("album", "Journey in Satchidananda", score=95)
    service = _make_service()
    service._mb_repo.search_grouped = AsyncMock(
        return_value=(
            {
                "artists": [] if failed_bucket == "artists" else [artist],
                "albums": [] if failed_bucket == "albums" else [album],
            },
            {failed_bucket},
        )
    )
    init_degradation_context()

    result = await service.search("Alice Coltrane")

    assert result.bucket_status == expected_status
    assert SearchService._search_cache == {}


@pytest.mark.asyncio
async def test_successful_combined_search_is_cached():
    artist = _make_search_result("artist", "Alice Coltrane", score=100)
    service = _make_service(grouped={"artists": [artist], "albums": []})

    first = await service.search("Alice Coltrane")
    second = await service.search("Alice Coltrane")

    assert first == second
    service._mb_repo.search_grouped.assert_awaited_once()


@pytest.mark.asyncio
async def test_timed_out_combined_search_is_not_cached(monkeypatch):
    service = _make_service()

    async def never_returns(*args, **kwargs):
        await asyncio.Event().wait()

    service._mb_repo.search_grouped = AsyncMock(side_effect=never_returns)
    monkeypatch.setattr("services.search_service.FULL_SEARCH_TIMEOUT_SECONDS", 0.01)
    init_degradation_context()

    first = await service.search("unfamiliar")
    second = await service.search("unfamiliar")

    assert first.bucket_status == {"artists": "timeout", "albums": "timeout"}
    assert second.bucket_status == {"artists": "timeout", "albums": "timeout"}
    assert service._mb_repo.search_grouped.await_count == 2


@pytest.mark.asyncio
async def test_search_source_generation_blocks_late_fresh_cache_write(monkeypatch):
    generation = 0
    monkeypatch.setattr(
        "services.search_service.get_mb_source_generation",
        lambda: generation,
    )
    started = asyncio.Event()
    release = asyncio.Event()
    artist = _make_search_result("artist", "Muse", score=90)
    service = _make_service()

    async def delayed_search(*_args, **_kwargs):
        started.set()
        await release.wait()
        return {"artists": [artist], "albums": []}

    service._mb_repo.search_grouped = AsyncMock(side_effect=delayed_search)
    pending = asyncio.create_task(service.search("Muse"))
    await started.wait()
    generation = 1
    release.set()
    result = await pending

    assert result.artists == []
    assert result.albums == []
    assert result.top_artist is None
    assert result.top_album is None
    assert result.bucket_status == {"artists": "error", "albums": "error"}
    assert SearchService._search_cache == {}


@pytest.mark.asyncio
async def test_search_source_generation_prevents_inflight_cross_source_coalescing(
    monkeypatch,
):
    generation = 0
    monkeypatch.setattr(
        "services.search_service.get_mb_source_generation",
        lambda: generation,
    )
    started = [asyncio.Event(), asyncio.Event()]
    releases = [asyncio.Event(), asyncio.Event()]
    calls = 0
    artist = _make_search_result("artist", "Muse", score=90)
    service = _make_service()

    async def delayed_search(*_args, **_kwargs):
        nonlocal calls
        index = calls
        calls += 1
        started[index].set()
        await releases[index].wait()
        return {"artists": [artist], "albums": []}

    service._mb_repo.search_grouped = AsyncMock(side_effect=delayed_search)
    first = asyncio.create_task(service.search("Muse"))
    await started[0].wait()
    generation = 1
    second = asyncio.create_task(service.search("Muse"))
    await started[1].wait()
    assert calls == 2

    releases[0].set()
    releases[1].set()
    first_result, second_result = await asyncio.gather(first, second)
    assert first_result.artists == []
    assert first_result.top_artist is None
    assert first_result.bucket_status == {"artists": "error", "albums": "error"}
    assert second_result.artists == [artist]
    assert any(key.startswith("g1:") for key in SearchService._search_cache)
    assert not any(key.startswith("g0:") for key in SearchService._search_cache)


@pytest.mark.asyncio
async def test_search_bucket_source_generation_blocks_late_success_result(monkeypatch):
    generation = 0
    monkeypatch.setattr(
        "services.search_service.get_mb_source_generation",
        lambda: generation,
    )
    started = asyncio.Event()
    release = asyncio.Event()
    artist = _make_search_result("artist", "Muse", score=90)
    service = _make_service()

    async def delayed_search(*_args, **_kwargs):
        started.set()
        await release.wait()
        return [artist]

    service._mb_repo.search_artists = AsyncMock(side_effect=delayed_search)
    pending = asyncio.create_task(service.search_bucket("artists", "Muse", limit=5))
    await started.wait()
    generation = 1
    release.set()
    results, top_result, status = await pending

    assert results == []
    assert top_result is None
    assert status == "error"
    assert SearchService._stale_bucket_cache == {}


@pytest.mark.asyncio
async def test_search_bucket_does_not_fallback_to_previous_source_stale_cache(
    monkeypatch,
):
    generation = 0
    monkeypatch.setattr(
        "services.search_service.get_mb_source_generation",
        lambda: generation,
    )
    artist = _make_search_result("artist", "Muse", score=90)
    service = _make_service()
    service._mb_repo.search_artists = AsyncMock(return_value=[artist])

    results, _top, status = await service.search_bucket("artists", "Muse", limit=5)
    assert results == [artist]
    assert status == "ok"

    generation = 1
    service._mb_repo.search_artists = AsyncMock(
        side_effect=RuntimeError("new source unavailable")
    )
    results, top, status = await service.search_bucket("artists", "Muse", limit=5)

    assert results == []
    assert top is None
    assert status == "error"
