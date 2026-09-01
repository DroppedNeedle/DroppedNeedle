import asyncio

import httpx
import pytest

import repositories.musicbrainz_base as mb_base
from core.exceptions import ConfigurationError
from infrastructure.cache.memory_cache import InMemoryCache


class _SwitchingClient:
    def __init__(self, old_started: asyncio.Event, release_old: asyncio.Event) -> None:
        self.old_started = old_started
        self.release_old = release_old
        self.contexts = []
        self.urls = []

    async def get(self, url: str, params=None):
        self.urls.append(url)
        context = mb_base.get_mb_response_context()
        self.contexts.append(context)
        if len(self.contexts) == 1:
            self.old_started.set()
            await self.release_old.wait()
        return httpx.Response(200, json={"artist": []})


@pytest.mark.asyncio
async def test_old_source_answer_cannot_overwrite_new_source_cache(monkeypatch):
    old_started = asyncio.Event()
    release_old = asyncio.Event()
    client = _SwitchingClient(old_started, release_old)
    original_source = mb_base.capture_mb_source_context()
    original_source_id = mb_base.get_mb_source_id()
    original_runtime = mb_base.brainzmash_runtime_enabled()
    monkeypatch.setattr(mb_base, "_http_client", client)
    monkeypatch.setattr(mb_base, "_mb_limiter_bypassed", True)
    old_generation = original_source.generation + 1
    mb_base.set_mb_api_base(
        "https://old.example/ws/2",
        source_mode="mirror",
        source_id="old-source",
        generation=old_generation,
    )
    cache = InMemoryCache()

    try:
        old_task = asyncio.create_task(mb_base.mb_api_get("/artist"))
        await old_started.wait()
        mb_base.set_mb_api_base(
            "https://new.example/ws/2",
            source_mode="mirror",
            source_id="new-source",
            generation=old_generation + 1,
        )
        new_result = await mb_base.mb_api_get("/artist")
        release_old.set()
        with pytest.raises(ConfigurationError, match="source changed"):
            await old_task
        assert new_result == {"artist": []}
        assert client.urls == [
            "https://old.example/ws/2/artist",
            "https://new.example/ws/2/artist",
        ]
        old_context, new_context = client.contexts
        assert old_context is not None and new_context is not None
        assert old_context.source_url.endswith("old.example/ws/2")
        assert new_context.source_url.endswith("new.example/ws/2")

        assert (
            await mb_base.mb_cache_set_if_current(
                cache,
                "artist",
                {"artist": []},
                ttl_seconds=60,
                context=old_context,
            )
            is False
        )
        assert await cache.get("artist") is None
        assert (
            await mb_base.mb_cache_set_if_current(
                cache,
                "artist",
                new_result,
                ttl_seconds=60,
                context=new_context,
            )
            is True
        )
        assert await cache.get("artist") == {"artist": []}
    finally:
        release_old.set()
        mb_base.set_mb_api_base(
            original_source.source_url,
            source_mode=original_source.source_mode,
            source_id=original_source_id,
            generation=original_source.generation,
            brainzmash_binding_valid=original_runtime,
        )
