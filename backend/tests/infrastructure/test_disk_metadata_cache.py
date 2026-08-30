import asyncio
import hashlib
import time
import json
from pathlib import Path

import pytest

from api.v1.schemas.album import AlbumInfo
from infrastructure.cache.disk_cache import DiskMetadataCache
from repositories.audiodb_models import AudioDBArtistImages, AudioDBAlbumImages


def _cache_hash(identifier: str) -> str:
    return hashlib.sha1(f"{DiskMetadataCache._CACHE_VERSION}:{identifier}".encode()).hexdigest()


@pytest.mark.asyncio
async def test_set_album_serializes_msgspec_struct_as_mapping(tmp_path):
    cache = DiskMetadataCache(base_path=tmp_path)
    mbid = "4549a80c-efe6-4386-b3a2-4b4a918eb31f"
    album_info = AlbumInfo(
        title="The Moon Song",
        musicbrainz_id=mbid,
        artist_name="beabadoobee",
        artist_id="88d17133-abbc-42db-9526-4e2c1db60336",
        in_library=True,
        selected_release_mbid="2a835d2e-907e-4b9c-8b36-e8ad4c2e8257",
    )

    await cache.set_album(mbid, album_info, is_monitored=True)

    cache_hash = _cache_hash(mbid)
    cache_file = tmp_path / "persistent" / "albums" / f"{cache_hash}.json"
    payload = json.loads(cache_file.read_text())

    assert isinstance(payload, dict)
    assert payload["musicbrainz_id"] == mbid
    assert payload["selected_release_mbid"] == "2a835d2e-907e-4b9c-8b36-e8ad4c2e8257"

    cached = await cache.get_album(mbid)
    assert isinstance(cached, dict)
    assert cached["title"] == "The Moon Song"
    assert cached["selected_release_mbid"] == "2a835d2e-907e-4b9c-8b36-e8ad4c2e8257"


@pytest.mark.asyncio
async def test_get_album_deletes_corrupt_string_payload(tmp_path):
    cache = DiskMetadataCache(base_path=tmp_path)
    mbid = "8e1e9e51-38dc-4df3-8027-a0ada37d4674"

    cache_hash = _cache_hash(mbid)
    cache_file = tmp_path / "persistent" / "albums" / f"{cache_hash}.json"
    cache_file.parent.mkdir(parents=True, exist_ok=True)
    cache_file.write_text(json.dumps("AlbumInfo(title='Corrupt')"))

    cached = await cache.get_album(mbid)

    assert cached is None
    assert not cache_file.exists()


@pytest.mark.asyncio
async def test_audiodb_artist_entity_routing(tmp_path):
    cache = DiskMetadataCache(base_path=tmp_path)
    mbid = "a1b2c3d4-e5f6-7890-abcd-ef1234567890"
    images = AudioDBArtistImages(
        thumb_url="https://example.com/thumb.jpg",
        fanart_url="https://example.com/fanart.jpg",
        lookup_source="mbid",
        matched_mbid=mbid,
    )

    await cache._set_entity("audiodb_artist", mbid, images, is_monitored=False, ttl_seconds=None)

    result = await cache._get_entity("audiodb_artist", mbid)
    assert result is not None
    assert result["thumb_url"] == "https://example.com/thumb.jpg"
    assert result["fanart_url"] == "https://example.com/fanart.jpg"
    assert result["lookup_source"] == "mbid"

    cache_hash = _cache_hash(mbid)
    data_file = tmp_path / "recent" / "audiodb_artists" / f"{cache_hash}.json"
    assert data_file.exists()


@pytest.mark.asyncio
async def test_audiodb_album_entity_routing(tmp_path):
    cache = DiskMetadataCache(base_path=tmp_path)
    mbid = "b2c3d4e5-f6a7-8901-bcde-f12345678901"
    images = AudioDBAlbumImages(
        album_thumb_url="https://example.com/album_thumb.jpg",
        album_back_url="https://example.com/album_back.jpg",
        lookup_source="name",
        matched_mbid=mbid,
    )

    await cache._set_entity("audiodb_album", mbid, images, is_monitored=True, ttl_seconds=None)

    result = await cache._get_entity("audiodb_album", mbid)
    assert result is not None
    assert result["album_thumb_url"] == "https://example.com/album_thumb.jpg"
    assert result["album_back_url"] == "https://example.com/album_back.jpg"
    assert result["lookup_source"] == "name"

    cache_hash = _cache_hash(mbid)
    persistent_file = tmp_path / "persistent" / "audiodb_albums" / f"{cache_hash}.json"
    assert persistent_file.exists()


@pytest.mark.asyncio
async def test_get_stats_counts_audiodb_entries(tmp_path):
    cache = DiskMetadataCache(base_path=tmp_path)

    artist_images = AudioDBArtistImages(thumb_url="https://example.com/a.jpg")
    album_images = AudioDBAlbumImages(album_thumb_url="https://example.com/b.jpg")

    await cache._set_entity("audiodb_artist", "artist-1", artist_images, is_monitored=False, ttl_seconds=None)
    await cache._set_entity("audiodb_artist", "artist-2", artist_images, is_monitored=True, ttl_seconds=None)
    await cache._set_entity("audiodb_album", "album-1", album_images, is_monitored=False, ttl_seconds=None)

    stats = cache.get_stats()
    assert stats["audiodb_artist_count"] == 2
    assert stats["audiodb_album_count"] == 1
    assert stats["album_count"] == 0
    assert stats["artist_count"] == 0
    assert stats["total_count"] == 3


@pytest.mark.asyncio
async def test_clear_audiodb_isolates_from_other_entities(tmp_path):
    cache = DiskMetadataCache(base_path=tmp_path)
    album_mbid = "c3d4e5f6-a7b8-9012-cdef-123456789012"
    album_info = AlbumInfo(
        title="Regular Album",
        musicbrainz_id=album_mbid,
        artist_name="Test Artist",
        artist_id="d4e5f6a7-b8c9-0123-defa-234567890123",
        in_library=False,
    )
    await cache.set_album(album_mbid, album_info, is_monitored=False)

    artist_images = AudioDBArtistImages(thumb_url="https://example.com/thumb.jpg")
    album_images = AudioDBAlbumImages(album_thumb_url="https://example.com/album.jpg")
    await cache._set_entity("audiodb_artist", "adb-artist-1", artist_images, is_monitored=False, ttl_seconds=None)
    await cache._set_entity("audiodb_album", "adb-album-1", album_images, is_monitored=True, ttl_seconds=None)

    stats_before = cache.get_stats()
    assert stats_before["audiodb_artist_count"] == 1
    assert stats_before["audiodb_album_count"] == 1
    assert stats_before["album_count"] == 1

    await cache.clear_audiodb()

    stats_after = cache.get_stats()
    assert stats_after["audiodb_artist_count"] == 0
    assert stats_after["audiodb_album_count"] == 0
    assert stats_after["album_count"] == 1

    regular_album = await cache.get_album(album_mbid)
    assert regular_album is not None
    assert regular_album["title"] == "Regular Album"


@pytest.mark.asyncio
async def test_audiodb_monitored_persistent_vs_recent(tmp_path):
    cache = DiskMetadataCache(base_path=tmp_path)
    mbid = "e5f6a7b8-c9d0-1234-efab-567890123456"
    images = AudioDBArtistImages(thumb_url="https://example.com/t.jpg")

    await cache._set_entity("audiodb_artist", mbid, images, is_monitored=True, ttl_seconds=None)

    cache_hash = _cache_hash(mbid)
    persistent_file = tmp_path / "persistent" / "audiodb_artists" / f"{cache_hash}.json"
    recent_file = tmp_path / "recent" / "audiodb_artists" / f"{cache_hash}.json"
    assert persistent_file.exists()
    assert not recent_file.exists()

    await cache._set_entity("audiodb_artist", mbid, images, is_monitored=False, ttl_seconds=None)

    assert not persistent_file.exists()
    assert recent_file.exists()


def _album_payload(mbid: str) -> AlbumInfo:
    return AlbumInfo(
        title="The Moon Song",
        musicbrainz_id=mbid,
        artist_name="beabadoobee",
        artist_id="88d17133-abbc-42db-9526-4e2c1db60336",
        in_library=True,
        selected_release_mbid="2a835d2e-907e-4b9c-8b36-e8ad4c2e8257",
    )


class _SidecarSpy:
    """Counts write_text calls landing on any .meta.json sidecar."""

    def __init__(self) -> None:
        self.meta_writes: list[Path] = []
        self._original = Path.write_text
        self.fail_next = False

    def __enter__(self):
        state = self

        def _spy(path_self: Path, text, *args, **kwargs):
            # Bound on Path itself, so ``path_self`` is the file instance.
            if path_self.name.endswith(".meta.json"):
                if state.fail_next:
                    state.fail_next = False
                    raise OSError("simulated sidecar write failure")
                state.meta_writes.append(path_self)
            return state._original(path_self, text, *args, **kwargs)

        Path.write_text = _spy  # type: ignore[method-assign]
        return self

    def __exit__(self, *exc_info):
        Path.write_text = self._original  # type: ignore[method-assign]


INTERVAL = DiskMetadataCache._ACCESS_TOUCH_INTERVAL_SECONDS


@pytest.mark.asyncio
async def test_warm_hits_within_interval_write_zero_sidecars(tmp_path):
    clock = {"now": time.time()}
    cache = DiskMetadataCache(base_path=tmp_path, clock=lambda: clock["now"])
    mbid = "4549a80c-efe6-4386-b3a2-4b4a918eb31f"
    await cache.set_album(mbid, _album_payload(mbid))
    clock["now"] = time.time()  # align with the real fill stamp

    with _SidecarSpy() as spy:
        for step in range(5):
            clock["now"] += INTERVAL / 10  # stay inside the touch window
            cached = await cache.get_album(mbid)
            assert cached is not None and cached["title"] == "The Moon Song"

    assert spy.meta_writes == []  # zero rewrites for warm hits


@pytest.mark.asyncio
async def test_read_after_interval_touches_once_and_updates_durable_value(tmp_path):
    clock = {"now": time.time()}
    cache = DiskMetadataCache(base_path=tmp_path, clock=lambda: clock["now"])
    mbid = "4549a80c-efe6-4386-b3a2-4b4a918eb31f"
    await cache.set_album(mbid, _album_payload(mbid))
    clock["now"] = time.time()
    meta_path = (
        tmp_path / "recent" / "albums" / f"{_cache_hash(mbid)}.meta.json"
    )

    with _SidecarSpy() as spy:
        clock["now"] += INTERVAL + 1  # outside the window
        cached = await cache.get_album(mbid)
        assert cached is not None

    assert len(spy.meta_writes) == 1  # exactly one throttled touch
    durable = json.loads(meta_path.read_text())
    assert durable["last_accessed"] == pytest.approx(clock["now"])

    # a second immediate read stays inside the new window: no more writes
    with _SidecarSpy() as spy_two:
        clock["now"] += INTERVAL / 2
        assert await cache.get_album(mbid) is not None
    assert spy_two.meta_writes == []


@pytest.mark.asyncio
async def test_concurrent_burst_after_interval_coalesces_to_one_touch(tmp_path):
    clock = {"now": time.time()}
    cache = DiskMetadataCache(base_path=tmp_path, clock=lambda: clock["now"])
    mbid = "4549a80c-efe6-4386-b3a2-4b4a918eb31f"
    await cache.set_album(mbid, _album_payload(mbid))
    clock["now"] = time.time()

    with _SidecarSpy() as spy:
        clock["now"] += INTERVAL + 1
        results = await asyncio.gather(
            *[cache.get_album(mbid) for _ in range(10)]
        )

    assert all(result is not None for result in results)
    assert len(spy.meta_writes) <= 1  # coalesced, not one write per worker


@pytest.mark.asyncio
async def test_old_sidecar_with_created_at_only_reads_and_evicts(tmp_path):
    cache = DiskMetadataCache(base_path=tmp_path)
    mbid = "4549a80c-efe6-4386-b3a2-4b4a918eb31f"
    await cache.set_album(mbid, _album_payload(mbid))
    meta_path = tmp_path / "recent" / "albums" / f"{_cache_hash(mbid)}.meta.json"

    # legacy sidecar shape: created_at only, no last_accessed key at all
    legacy = json.loads(meta_path.read_text())
    legacy.pop("last_accessed", None)
    meta_path.write_text(json.dumps(legacy))

    restarted = DiskMetadataCache(base_path=tmp_path, clock=lambda: 1_000_000.0)
    cached = await restarted.get_album(mbid)
    assert cached is not None and cached["musicbrainz_id"] == mbid

    # eviction remains deterministic from the durable fallback values
    freed = await restarted.enforce_recent_size_limits()
    assert isinstance(freed, int)
    assert (meta_path.parent / f"{_cache_hash(mbid)}.json").exists()


@pytest.mark.asyncio
async def test_expiry_still_removes_pairs_and_failed_touch_keeps_payload(tmp_path):
    clock = {"now": 2_000_000_000.0}
    cache = DiskMetadataCache(base_path=tmp_path, clock=lambda: clock["now"])
    mbid = "4549a80c-efe6-4386-b3a2-4b4a918eb31f"
    await cache.set_album(mbid, _album_payload(mbid), ttl_seconds=60)
    data_file = tmp_path / "recent" / "albums" / f"{_cache_hash(mbid)}.json"
    meta_path = cache._meta_path(data_file)

    # expiry path untouched: advancing past expires_at removes the pair
    clock["now"] += 120
    removed = await cache.cleanup_expired_recent()
    assert removed >= 1 and not data_file.exists()

    # refill; a failing sidecar touch must not hide the valid payload
    await cache.set_album(mbid, _album_payload(mbid), ttl_seconds=600)
    clock["now"] += INTERVAL + 1
    with _SidecarSpy() as spy:
        spy.fail_next = True
        cached = await cache.get_album(mbid)
    assert cached is not None and cached["title"] == "The Moon Song"
    assert data_file.exists()


@pytest.mark.asyncio
async def test_recent_eviction_prefers_least_recently_touched_entry(tmp_path):
    clock = {"now": time.time()}
    small = DiskMetadataCache(
        base_path=tmp_path,
        recent_metadata_max_size_mb=0,  # limit enforced manually below
        clock=lambda: clock["now"],
    )
    old_id = "aaaaaaa8-1111-4386-b3a2-4b4a918eb31f"
    fresh_id = "bbbbbbb8-2222-4386-b3a2-4b4a918eb31f"
    await small.set_album(old_id, _album_payload(old_id))
    clock["now"] = time.time() + INTERVAL + 1
    await small.set_album(fresh_id, _album_payload(fresh_id))

    # touch the FRESH entry inside its window (in-memory only, no sidecar
    # rewrite): LRU choice must still see it as newest.
    clock["now"] += 1
    await small.get_album(fresh_id)

    old_data = tmp_path / "recent" / "albums" / f"{_cache_hash(old_id)}.json"
    fresh_data = tmp_path / "recent" / "albums" / f"{_cache_hash(fresh_id)}.json"
    # budget equals the fresh entry's size: deleting exactly ONE entry frees
    # enough, so the recency ordering alone decides which entry that is.
    total_size = old_data.stat().st_size + fresh_data.stat().st_size
    freed = small._enforce_size_limit_for_directory(
        tmp_path / "recent" / "albums",
        max_size_bytes=total_size - old_data.stat().st_size,
    )
    assert freed > 0
    assert not old_data.exists(), "stale entry must be the eviction victim"
    assert fresh_data.exists(), "freshly touched entry must survive"


@pytest.mark.asyncio
async def test_restart_eviction_falls_back_to_durable_sidecars(tmp_path):
    clock = {"now": 2_000_000_000.0}
    cache = DiskMetadataCache(base_path=tmp_path, clock=lambda: clock["now"])
    touched_id = "ccccccc8-3333-4386-b3a2-4b4a918eb31f"
    stale_id = "ddddddd8-4444-4386-b3a2-4b4a918eb31f"
    await cache.set_album(stale_id, _album_payload(stale_id))
    clock["now"] += INTERVAL + 1
    await cache.set_album(touched_id, _album_payload(touched_id))
    clock["now"] += INTERVAL + 1
    await cache.get_album(touched_id)  # durable touch recorded on sidecar

    # a brand-new instance has no in-memory map: durable sidecars decide
    restarted = DiskMetadataCache(base_path=tmp_path, clock=lambda: clock["now"])
    stale_data = tmp_path / "recent" / "albums" / f"{_cache_hash(stale_id)}.json"
    touched_data = tmp_path / "recent" / "albums" / f"{_cache_hash(touched_id)}.json"
    total_size = stale_data.stat().st_size + touched_data.stat().st_size
    freed = restarted._enforce_size_limit_for_directory(
        tmp_path / "recent" / "albums",
        max_size_bytes=total_size - stale_data.stat().st_size,
    )
    assert freed > 0
    assert (tmp_path / "recent" / "albums" / f"{_cache_hash(touched_id)}.json").exists()
    assert not (tmp_path / "recent" / "albums" / f"{_cache_hash(stale_id)}.json").exists()


@pytest.mark.asyncio
async def test_artist_profiles_persist_across_restart_and_musicbrainz_clear(tmp_path):
    mbid = "eeeeeee8-5555-4386-b3a2-4b4a918eb31f"
    full_payload = {
        "musicbrainz_id": mbid,
        "name": "Full Artist",
        "release_groups": [{"id": "rg-full"}],
    }
    basic_payload = {
        "musicbrainz_id": mbid,
        "name": "Basic Artist",
        "release_groups": [],
    }

    cache = DiskMetadataCache(base_path=tmp_path)
    await cache.set_artist(mbid, full_payload, is_monitored=True)
    await cache.set_artist(mbid, basic_payload, profile="basic")

    assert (await cache.get_artist(mbid))["name"] == "Full Artist"
    assert (await cache.get_artist(mbid, profile="basic"))["name"] == "Basic Artist"

    restarted = DiskMetadataCache(base_path=tmp_path)
    assert (await restarted.get_artist(mbid))["name"] == "Full Artist"
    assert (
        await restarted.get_artist(mbid, profile="basic")
    )["name"] == "Basic Artist"

    await restarted.delete_artist(mbid, profile="basic")
    assert await restarted.get_artist(mbid, profile="basic") is None
    assert (await restarted.get_artist(mbid))["name"] == "Full Artist"

    await restarted.clear_musicbrainz()
    assert await restarted.get_artist(mbid) is None
