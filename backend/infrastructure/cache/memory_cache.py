import asyncio
import sys
import time
from typing import Any, Optional
from abc import ABC, abstractmethod
from collections import OrderedDict


class CacheInterface(ABC):
    @abstractmethod
    async def get(self, key: str) -> Optional[Any]:
        pass

    @abstractmethod
    async def set(self, key: str, value: Any, ttl_seconds: int = 60) -> None:
        pass

    @abstractmethod
    async def delete(self, key: str) -> None:
        pass

    @abstractmethod
    async def clear(self) -> None:
        pass

    @abstractmethod
    async def cleanup_expired(self) -> int:
        pass

    @abstractmethod
    async def peek(self, key: str) -> Optional[Any]:
        """Expired-tolerant read: return the entry value even when its TTL has
        passed, without evicting it, refreshing LRU order, or extending TTL.
        Returns None only when the key is absent. Used for stale-serving
        fallbacks (QW11 Part 3); ordinary reads must use get()."""
        pass

    @abstractmethod
    def size(self) -> int:
        pass

    @abstractmethod
    def estimate_memory_bytes(self) -> int:
        pass


class CacheEntry:
    __slots__ = ("value", "expires_at")

    def __init__(self, value: Any, ttl_seconds: int):
        self.value = value
        self.expires_at = time.time() + ttl_seconds

    def is_expired(self) -> bool:
        return time.time() > self.expires_at


class InMemoryCache(CacheInterface):
    def __init__(self, max_entries: int = 10000):
        self._cache: OrderedDict[str, CacheEntry] = OrderedDict()
        self._lock = asyncio.Lock()
        self._max_entries = max_entries
        self._evictions = 0
        self._hits = 0
        self._misses = 0

    @staticmethod
    def _source_key(key: str) -> str:
        try:
            from repositories.musicbrainz_base import namespace_mb_cache_key

            return namespace_mb_cache_key(key)
        except (ImportError, RuntimeError):
            return key

    async def get(self, key: str) -> Optional[Any]:
        key = self._source_key(key)
        async with self._lock:
            entry = self._cache.get(key)
            if entry is None:
                self._misses += 1
                return None

            if entry.is_expired():
                self._cache.pop(key, None)
                self._misses += 1
                return None

            self._cache.move_to_end(key)
            self._hits += 1
            return entry.value

    async def peek(self, key: str) -> Optional[Any]:
        # Deliberately no move_to_end (LRU untouched) and no pop: a peek is a
        # read-only observation, even of expired entries.
        key = self._source_key(key)
        async with self._lock:
            entry = self._cache.get(key)
            if entry is None:
                return None
            return entry.value

    async def set(self, key: str, value: Any, ttl_seconds: int = 60) -> None:
        key = self._source_key(key)
        async with self._lock:
            if key not in self._cache and len(self._cache) >= self._max_entries:
                oldest_key, _ = self._cache.popitem(last=False)
                self._evictions += 1

            self._cache[key] = CacheEntry(value, ttl_seconds)
            self._cache.move_to_end(key)

    async def delete(self, key: str) -> None:
        key = self._source_key(key)
        async with self._lock:
            self._cache.pop(key, None)

    async def clear(self) -> None:
        async with self._lock:
            self._cache.clear()

    async def clear_prefix(self, prefix: str) -> int:
        async with self._lock:
            keys_to_remove = [k for k in self._cache.keys() if k.startswith(prefix)]
            for key in keys_to_remove:
                self._cache.pop(key, None)

        return len(keys_to_remove)

    async def cleanup_expired(self) -> int:
        now = time.time()

        async with self._lock:
            expired_keys = [
                key for key, entry in self._cache.items() if now > entry.expires_at
            ]
            for key in expired_keys:
                self._cache.pop(key, None)

        return len(expired_keys)

    def size(self) -> int:
        return len(self._cache)

    def estimate_memory_bytes(self) -> int:
        total_size = 0

        total_size += sys.getsizeof(self._cache)

        for key, entry in self._cache.items():
            total_size += sys.getsizeof(key)
            total_size += sys.getsizeof(entry)
            total_size += sys.getsizeof(entry.value)

        return total_size

    def get_stats(self) -> dict[str, Any]:
        total = self._hits + self._misses
        hit_rate = (self._hits / total * 100) if total > 0 else 0.0
        return {
            "size": len(self._cache),
            "max_entries": self._max_entries,
            "hits": self._hits,
            "misses": self._misses,
            "hit_rate_percent": round(hit_rate, 2),
            "evictions": self._evictions,
            "memory_bytes": self.estimate_memory_bytes(),
        }
