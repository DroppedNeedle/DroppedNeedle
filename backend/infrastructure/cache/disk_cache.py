import asyncio
import hashlib
import json
import shutil
import time
from pathlib import Path
from typing import Any

from infrastructure.serialization import to_jsonable


def _encode_json(value: Any) -> str:
    return json.dumps(value, ensure_ascii=True, separators=(",", ":"))


def _decode_json(text: str) -> Any:
    return json.loads(text)


class DiskMetadataCache:
    _CACHE_VERSION = "v3"

    def __init__(
        self,
        base_path: Path,
        recent_metadata_max_size_mb: int = 128,
        recent_covers_max_size_mb: int = 0,
        persistent_metadata_ttl_hours: int = 24,
    ):
        self.base_path = Path(base_path)
        self.recent_metadata_max_size_bytes = max(recent_metadata_max_size_mb, 0) * 1024 * 1024
        self.recent_covers_max_size_bytes = max(recent_covers_max_size_mb, 0) * 1024 * 1024
        self.default_ttl_seconds = max(persistent_metadata_ttl_hours, 1) * 3600

        self._recent_albums_dir = self.base_path / "recent" / "albums"
        self._recent_artists_dir = self.base_path / "recent" / "artists"
        self._recent_covers_dir = self.base_path / "recent" / "covers"
        self._persistent_albums_dir = self.base_path / "persistent" / "albums"
        self._persistent_artists_dir = self.base_path / "persistent" / "artists"
        self._recent_audiodb_artists_dir = self.base_path / "recent" / "audiodb_artists"
        self._recent_audiodb_albums_dir = self.base_path / "recent" / "audiodb_albums"
        self._persistent_audiodb_artists_dir = self.base_path / "persistent" / "audiodb_artists"
        self._persistent_audiodb_albums_dir = self.base_path / "persistent" / "audiodb_albums"
        # per-user Discover pages: persistent so a restart reloads them instantly
        self._recent_discover_dir = self.base_path / "recent" / "discover"
        self._persistent_discover_dir = self.base_path / "persistent" / "discover"
        self._ensure_dirs()

    def _ensure_dirs(self) -> None:
        for path in (
            self._recent_albums_dir,
            self._recent_artists_dir,
            self._recent_covers_dir,
            self._persistent_albums_dir,
            self._persistent_artists_dir,
            self._recent_audiodb_artists_dir,
            self._recent_audiodb_albums_dir,
            self._persistent_audiodb_artists_dir,
            self._persistent_audiodb_albums_dir,
            self._recent_discover_dir,
            self._persistent_discover_dir,
        ):
            path.mkdir(parents=True, exist_ok=True)

    @staticmethod
    def _cache_hash(identifier: str) -> str:
        return hashlib.sha1(f"{DiskMetadataCache._CACHE_VERSION}:{identifier}".encode()).hexdigest()

    @staticmethod
    def _meta_path(file_path: Path) -> Path:
        return file_path.with_suffix(".meta.json")

    def _entity_paths(self, entity_type: str, identifier: str) -> tuple[Path, Path]:
        cache_hash = self._cache_hash(identifier)
        if entity_type == "album":
            return (
                self._recent_albums_dir / f"{cache_hash}.json",
                self._persistent_albums_dir / f"{cache_hash}.json",
            )
        if entity_type == "artist":
            return (
                self._recent_artists_dir / f"{cache_hash}.json",
                self._persistent_artists_dir / f"{cache_hash}.json",
            )
        if entity_type == "audiodb_artist":
            return (
                self._recent_audiodb_artists_dir / f"{cache_hash}.json",
                self._persistent_audiodb_artists_dir / f"{cache_hash}.json",
            )
        if entity_type == "audiodb_album":
            return (
                self._recent_audiodb_albums_dir / f"{cache_hash}.json",
                self._persistent_audiodb_albums_dir / f"{cache_hash}.json",
            )
        if entity_type == "discover":
            return (
                self._recent_discover_dir / f"{cache_hash}.json",
                self._persistent_discover_dir / f"{cache_hash}.json",
            )
        raise ValueError(f"Unsupported entity type: {entity_type}")

    def _delete_file_pair(self, file_path: Path) -> None:
        file_path.unlink(missing_ok=True)
        self._meta_path(file_path).unlink(missing_ok=True)

    def _load_meta(self, meta_path: Path) -> dict[str, Any]:
        if not meta_path.exists():
            return {}
        try:
            payload = _decode_json(meta_path.read_text())
        except (json.JSONDecodeError, OSError, TypeError):
            return {}
        return payload if isinstance(payload, dict) else {}

    @staticmethod
    def _is_expired(meta: dict[str, Any]) -> bool:
        expires_at = meta.get("expires_at")
        return isinstance(expires_at, (int, float)) and time.time() > float(expires_at)

    def _cleanup_expired_directory(self, directory: Path) -> int:
        removed = 0
        handled_meta_paths: set[Path] = set()

        for data_path in directory.iterdir():
            if not data_path.is_file() or data_path.name.endswith(".meta.json"):
                continue
            meta_path = self._meta_path(data_path)
            handled_meta_paths.add(meta_path)
            if self._is_expired(self._load_meta(meta_path)):
                self._delete_file_pair(data_path)
                removed += 1

        for meta_path in directory.glob("*.meta.json"):
            if meta_path in handled_meta_paths:
                continue
            if self._is_expired(self._load_meta(meta_path)):
                meta_path.unlink(missing_ok=True)
                removed += 1

        return removed

    def _enforce_size_limit_for_directory(self, directory: Path, max_size_bytes: int) -> int:
        if max_size_bytes <= 0:
            return 0

        candidates: list[tuple[float, Path, int]] = []
        total_size = 0
        for data_path in directory.iterdir():
            if not data_path.is_file() or data_path.name.endswith(".meta.json"):
                continue
            try:
                size_bytes = data_path.stat().st_size
            except FileNotFoundError:
                continue
            meta = self._load_meta(self._meta_path(data_path))
            last_accessed = float(meta.get("last_accessed", meta.get("created_at", 0.0)) or 0.0)
            total_size += size_bytes
            candidates.append((last_accessed, data_path, size_bytes))

        if total_size <= max_size_bytes:
            return 0

        bytes_to_free = total_size - max_size_bytes
        freed = 0
        for _, data_path, size_bytes in sorted(candidates, key=lambda item: item[0]):
            self._delete_file_pair(data_path)
            freed += size_bytes
            if freed >= bytes_to_free:
                break
        return freed

    def _write_json_entry(self, file_path: Path, payload: dict[str, Any], expires_at: float | None) -> None:
        file_path.parent.mkdir(parents=True, exist_ok=True)
        now = time.time()
        file_path.write_text(_encode_json(payload))
        meta = {
            "created_at": now,
            "last_accessed": now,
        }
        if expires_at is not None:
            meta["expires_at"] = expires_at
        self._meta_path(file_path).write_text(_encode_json(meta))

    def _read_json_entry(self, file_path: Path, honor_expiry: bool) -> dict[str, Any] | None:
        if not file_path.exists():
            return None

        meta_path = self._meta_path(file_path)
        meta: dict[str, Any] = {}
        if meta_path.exists():
            try:
                meta = _decode_json(meta_path.read_text())
            except (json.JSONDecodeError, OSError, TypeError):
                meta = {}

        if honor_expiry:
            expires_at = meta.get("expires_at")
            if isinstance(expires_at, (int, float)) and time.time() > float(expires_at):
                self._delete_file_pair(file_path)
                return None

        try:
            payload = _decode_json(file_path.read_text())
        except (json.JSONDecodeError, OSError, TypeError):
            self._delete_file_pair(file_path)
            return None

        if not isinstance(payload, dict):
            self._delete_file_pair(file_path)
            return None

        if meta_path.exists():
            meta["last_accessed"] = time.time()
            try:
                meta_path.write_text(_encode_json(meta))
            except OSError:
                pass

        return payload

    async def _set_entity(
        self,
        entity_type: str,
        identifier: str,
        payload: Any,
        is_monitored: bool,
        ttl_seconds: int | None,
    ) -> None:
        builtins = to_jsonable(payload)
        if not isinstance(builtins, dict):
            raise TypeError(f"Expected mapping payload for {entity_type} cache, got {type(builtins)!r}")

        recent_path, persistent_path = self._entity_paths(entity_type, identifier)

        def operation() -> None:
            target_path = persistent_path if is_monitored else recent_path
            other_path = recent_path if is_monitored else persistent_path
            self._delete_file_pair(other_path)
            expires_at = None
            if ttl_seconds is not None:
                expires_at = time.time() + max(ttl_seconds, 1)
            elif not is_monitored:
                expires_at = time.time() + max(self.default_ttl_seconds, 1)
            self._write_json_entry(target_path, builtins, expires_at)

        await asyncio.to_thread(operation)

    async def _get_entity(self, entity_type: str, identifier: str) -> dict[str, Any] | None:
        recent_path, persistent_path = self._entity_paths(entity_type, identifier)

        def operation() -> dict[str, Any] | None:
            persistent_payload = self._read_json_entry(persistent_path, honor_expiry=True)
            if persistent_payload is not None:
                return persistent_payload
            return self._read_json_entry(recent_path, honor_expiry=True)

        return await asyncio.to_thread(operation)

    async def set_album(
        self,
        musicbrainz_id: str,
        album_info: Any,
        is_monitored: bool = False,
        ttl_seconds: int | None = None,
    ) -> None:
        await self._set_entity("album", musicbrainz_id, album_info, is_monitored, ttl_seconds)

    async def get_album(self, musicbrainz_id: str) -> dict[str, Any] | None:
        return await self._get_entity("album", musicbrainz_id)

    async def set_artist(
        self,
        musicbrainz_id: str,
        artist_info: Any,
        is_monitored: bool = False,
        ttl_seconds: int | None = None,
    ) -> None:
        await self._set_entity("artist", musicbrainz_id, artist_info, is_monitored, ttl_seconds)

    async def get_artist(self, musicbrainz_id: str) -> dict[str, Any] | None:
        return await self._get_entity("artist", musicbrainz_id)

    async def set_audiodb_artist(
        self,
        identifier: str,
        payload: Any,
        is_monitored: bool = False,
        ttl_seconds: int | None = None,
    ) -> None:
        await self._set_entity("audiodb_artist", identifier, payload, is_monitored, ttl_seconds)

    async def get_audiodb_artist(self, identifier: str) -> dict[str, Any] | None:
        return await self._get_entity("audiodb_artist", identifier)

    async def set_audiodb_album(
        self,
        identifier: str,
        payload: Any,
        is_monitored: bool = False,
        ttl_seconds: int | None = None,
    ) -> None:
        await self._set_entity("audiodb_album", identifier, payload, is_monitored, ttl_seconds)

    async def get_audiodb_album(self, identifier: str) -> dict[str, Any] | None:
        return await self._get_entity("audiodb_album", identifier)

    async def set_discover(
        self,
        identifier: str,
        payload: Any,
        ttl_seconds: int | None = None,
    ) -> None:
        """Persist a built per-user Discover page. Always stored on the persistent side
        (it must survive restarts) but with an explicit TTL, matching the discover
        cache's freshness semantics."""
        await self._set_entity("discover", identifier, payload, is_monitored=True, ttl_seconds=ttl_seconds)

    async def get_discover(self, identifier: str) -> dict[str, Any] | None:
        return await self._get_entity("discover", identifier)

    async def delete_album(self, musicbrainz_id: str) -> None:
        recent_path, persistent_path = self._entity_paths("album", musicbrainz_id)
        await asyncio.to_thread(self._delete_file_pair, recent_path)
        await asyncio.to_thread(self._delete_file_pair, persistent_path)

    async def delete_artist(self, musicbrainz_id: str) -> None:
        recent_path, persistent_path = self._entity_paths("artist", musicbrainz_id)
        await asyncio.to_thread(self._delete_file_pair, recent_path)
        await asyncio.to_thread(self._delete_file_pair, persistent_path)

    async def delete_entity(self, entity_type: str, identifier: str) -> None:
        recent_path, persistent_path = self._entity_paths(entity_type, identifier)
        await asyncio.to_thread(self._delete_file_pair, recent_path)
        await asyncio.to_thread(self._delete_file_pair, persistent_path)

    async def promote_to_persistent(self, identifier: str, identifier_type: str) -> bool:
        entity_type = "artist" if identifier_type == "artist" else "album"
        recent_path, persistent_path = self._entity_paths(entity_type, identifier)

        def operation() -> bool:
            if persistent_path.exists():
                return True
            if not recent_path.exists():
                return False
            persistent_path.parent.mkdir(parents=True, exist_ok=True)
            shutil.move(str(recent_path), str(persistent_path))
            recent_meta = self._meta_path(recent_path)
            persistent_meta = self._meta_path(persistent_path)
            if recent_meta.exists():
                meta = _decode_json(recent_meta.read_text())
                meta.pop("expires_at", None)
                meta["last_accessed"] = time.time()
                persistent_meta.write_text(_encode_json(meta))
                recent_meta.unlink(missing_ok=True)
            return True

        return await asyncio.to_thread(operation)

    async def promote_album_to_persistent(self, musicbrainz_id: str) -> bool:
        return await self.promote_to_persistent(musicbrainz_id, "album")

    async def promote_artist_to_persistent(self, musicbrainz_id: str) -> bool:
        return await self.promote_to_persistent(musicbrainz_id, "artist")

    async def cleanup_expired_recent(self) -> int:
        def operation() -> int:
            removed = 0
            for base_dir in (
                self._recent_albums_dir,
                self._recent_artists_dir,
                self._recent_audiodb_artists_dir,
                self._recent_audiodb_albums_dir,
            ):
                removed += self._cleanup_expired_directory(base_dir)
            return removed

        return await asyncio.to_thread(operation)

    async def enforce_recent_size_limits(self) -> int:
        if self.recent_metadata_max_size_bytes <= 0:
            return 0

        def operation() -> int:
            candidates: list[tuple[float, Path, int]] = []
            total_size = 0
            for base_dir in (
                self._recent_albums_dir,
                self._recent_artists_dir,
                self._recent_audiodb_artists_dir,
                self._recent_audiodb_albums_dir,
            ):
                for data_path in base_dir.glob("*.json"):
                    if data_path.name.endswith(".meta.json"):
                        continue
                    try:
                        size_bytes = data_path.stat().st_size
                    except FileNotFoundError:
                        continue
                    meta_path = self._meta_path(data_path)
                    meta: dict[str, Any] = {}
                    if meta_path.exists():
                        try:
                            meta = _decode_json(meta_path.read_text())
                        except Exception:  # noqa: BLE001
                            meta = {}
                    last_accessed = float(meta.get("last_accessed", meta.get("created_at", 0.0)) or 0.0)
                    total_size += size_bytes
                    candidates.append((last_accessed, data_path, size_bytes))

            if total_size <= self.recent_metadata_max_size_bytes:
                return 0

            bytes_to_free = total_size - self.recent_metadata_max_size_bytes
            freed = 0
            for _, data_path, size_bytes in sorted(candidates, key=lambda item: item[0]):
                self._delete_file_pair(data_path)
                freed += size_bytes
                if freed >= bytes_to_free:
                    break
            return freed

        return await asyncio.to_thread(operation)

    async def cleanup_expired_covers(self) -> int:
        return await asyncio.to_thread(self._cleanup_expired_directory, self._recent_covers_dir)

    async def enforce_cover_size_limits(self) -> int:
        return await asyncio.to_thread(
            self._enforce_size_limit_for_directory,
            self._recent_covers_dir,
            self.recent_covers_max_size_bytes,
        )

    def get_stats(self) -> dict[str, Any]:
        total_count = 0
        album_count = 0
        artist_count = 0
        audiodb_artist_count = 0
        audiodb_album_count = 0
        total_size_bytes = 0

        for base_dir, counter_name in (
            (self._recent_albums_dir, "album"),
            (self._persistent_albums_dir, "album"),
            (self._recent_artists_dir, "artist"),
            (self._persistent_artists_dir, "artist"),
            (self._recent_audiodb_artists_dir, "audiodb_artist"),
            (self._persistent_audiodb_artists_dir, "audiodb_artist"),
            (self._recent_audiodb_albums_dir, "audiodb_album"),
            (self._persistent_audiodb_albums_dir, "audiodb_album"),
        ):
            for data_path in base_dir.glob("*.json"):
                if data_path.name.endswith(".meta.json"):
                    continue
                total_count += 1
                if counter_name == "album":
                    album_count += 1
                elif counter_name == "artist":
                    artist_count += 1
                elif counter_name == "audiodb_artist":
                    audiodb_artist_count += 1
                elif counter_name == "audiodb_album":
                    audiodb_album_count += 1
                try:
                    total_size_bytes += data_path.stat().st_size
                except FileNotFoundError:
                    pass

        return {
            "total_count": total_count,
            "album_count": album_count,
            "artist_count": artist_count,
            "audiodb_artist_count": audiodb_artist_count,
            "audiodb_album_count": audiodb_album_count,
            "total_size_bytes": total_size_bytes,
        }

    async def clear_all(self) -> None:
        def operation() -> None:
            if self.base_path.exists():
                shutil.rmtree(self.base_path)
            self._ensure_dirs()

        await asyncio.to_thread(operation)

    async def clear_audiodb(self) -> None:
        def operation() -> None:
            for d in (
                self._recent_audiodb_artists_dir,
                self._recent_audiodb_albums_dir,
                self._persistent_audiodb_artists_dir,
                self._persistent_audiodb_albums_dir,
            ):
                if d.exists():
                    shutil.rmtree(d)
            self._ensure_dirs()

        await asyncio.to_thread(operation)
