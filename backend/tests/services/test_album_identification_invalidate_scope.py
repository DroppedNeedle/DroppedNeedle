"""GH-349: identification-commit invalidate hook uses scoped catalog keys.

The ``invalidate`` closure built by
``get_target_album_identification_service`` must await the async
``store.album_catalog_scope_ids`` and delete exactly the entity-keyed
caches via ``invalidate_catalog_scope``. Without the ``await`` the unpack
raises ``TypeError`` (caught by the fallback ``except``), so every commit
silently degrades to the wholesale bulk sweep.
"""

from __future__ import annotations

from typing import Any

import pytest

from core.dependencies import cache_providers, service_providers
from infrastructure.cache.cache_keys import library_identification_prefixes

RG = "667DAB68-E5F0-40A8-8BF1-7AC99E35CEBE"
ARTIST = "88d17133-abbc-42db-9526-4e2c1db60336"


class _FakeStore:
    """Mirrors the real store contract: scope resolution is async."""

    def __init__(self) -> None:
        self.resolved: list[str] = []

    async def album_catalog_scope_ids(
        self, local_album_id: str
    ) -> tuple[set[str], set[str]]:
        self.resolved.append(local_album_id)
        return {RG}, {ARTIST}


class _FailingStore(_FakeStore):
    async def album_catalog_scope_ids(
        self, local_album_id: str
    ) -> tuple[set[str], set[str]]:
        raise RuntimeError("scope lookup failed")


class _FakeCache:
    def __init__(self) -> None:
        self.cleared: list[str] = []

    async def clear_prefix(self, prefix: str) -> int:
        self.cleared.append(prefix)
        return 0


class _FakeSnapshotStore:
    def __init__(self) -> None:
        self.stale_marks = 0

    async def mark_discover_stale(self) -> None:
        self.stale_marks += 1


def _build_service(
    monkeypatch: pytest.MonkeyPatch,
    store: _FakeStore,
    cache: _FakeCache,
    snapshot: _FakeSnapshotStore,
    scoped_calls: list[dict[str, Any]],
) -> Any:
    async def _record_scope(cache: Any, **kwargs: Any) -> None:
        scoped_calls.append({"cache": cache, **kwargs})

    monkeypatch.setattr(cache_providers, "get_native_library_store", lambda: store)
    monkeypatch.setattr(service_providers, "get_cache", lambda: cache)
    monkeypatch.setattr(
        service_providers, "get_discovery_snapshot_store", lambda: snapshot
    )
    monkeypatch.setattr(
        service_providers, "get_target_identification_queue", lambda: object()
    )
    monkeypatch.setattr(
        service_providers,
        "get_musicbrainz_identification_repository",
        lambda: object(),
    )
    monkeypatch.setattr(service_providers, "get_audio_fingerprinter", lambda: object())
    monkeypatch.setattr(
        service_providers, "get_mb_provider_availability", lambda: (lambda: True)
    )
    monkeypatch.setattr(
        service_providers, "invalidate_catalog_scope", _record_scope
    )
    provider = service_providers.get_target_album_identification_service
    provider.cache_clear()
    try:
        return provider()
    finally:
        provider.cache_clear()


@pytest.mark.asyncio
async def test_scoped_path_passes_entity_ids_through(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """AC2: resolved rg/artist ids reach invalidate_catalog_scope, no sweep."""
    store, cache, snapshot = _FakeStore(), _FakeCache(), _FakeSnapshotStore()
    scoped_calls: list[dict[str, Any]] = []
    service = _build_service(monkeypatch, store, cache, snapshot, scoped_calls)

    await service._invalidate({"library", "artist"}, ["local-album-1"])

    # The coroutine was actually awaited (pre-fix it never ran past unpack).
    assert store.resolved == ["local-album-1"]
    assert len(scoped_calls) == 1
    call = scoped_calls[0]
    assert call["cache"] is cache
    assert call["album_mbids"] == {RG}
    assert call["artist_mbids"] == {ARTIST}
    assert call["include_lists"] is True
    # Scoped path must NOT bulk-sweep the identification prefixes.
    assert cache.cleared == []
    assert snapshot.stale_marks == 1


@pytest.mark.asyncio
async def test_resolution_failure_falls_back_to_bulk_sweep(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """AC3: scope lookup raising keeps bulk sweep + discover-stale mark."""
    store, cache, snapshot = _FailingStore(), _FakeCache(), _FakeSnapshotStore()
    scoped_calls: list[dict[str, Any]] = []
    service = _build_service(monkeypatch, store, cache, snapshot, scoped_calls)

    await service._invalidate({"library"}, ["local-album-9"])

    assert scoped_calls == []
    assert cache.cleared == library_identification_prefixes()
    assert snapshot.stale_marks == 1
