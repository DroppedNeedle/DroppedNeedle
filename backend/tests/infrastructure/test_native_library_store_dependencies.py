import sqlite3
from pathlib import Path
from types import SimpleNamespace
from typing import get_args

from core.dependencies import auth_providers, cache_providers
from core.dependencies.type_aliases import NativeLibraryStoreDep
from infrastructure.library_management_blob_store import LibraryManagementBlobStore
from infrastructure.persistence.native_library_store import NativeLibraryStore


def test_native_store_provider_is_singleton_and_clearable(
    monkeypatch, tmp_path: Path
) -> None:
    database = tmp_path / "library.db"
    with sqlite3.connect(database) as connection:
        connection.execute("CREATE TABLE auth_users (id TEXT PRIMARY KEY)")
    monkeypatch.setattr(
        cache_providers,
        "get_settings",
        lambda: SimpleNamespace(library_db_path=database),
    )
    monkeypatch.setattr(auth_providers, "get_auth_store", lambda: object())
    cache_providers.get_native_library_store.cache_clear()
    cache_providers.get_persistence_write_lock.cache_clear()

    first = cache_providers.get_native_library_store()
    second = cache_providers.get_native_library_store()
    cache_providers.get_native_library_store.cache_clear()
    third = cache_providers.get_native_library_store()

    assert isinstance(first, NativeLibraryStore)
    assert first is second
    assert third is not first
    assert get_args(NativeLibraryStoreDep)[0] is NativeLibraryStore

    cache_providers.get_native_library_store.cache_clear()
    cache_providers.get_persistence_write_lock.cache_clear()


def test_management_blob_store_uses_persistent_cache_volume(
    monkeypatch, tmp_path: Path
) -> None:
    cache_dir = tmp_path / "cache"
    ledger = object()
    monkeypatch.setattr(
        cache_providers,
        "get_settings",
        lambda: SimpleNamespace(cache_dir=cache_dir),
    )
    monkeypatch.setattr(cache_providers, "get_native_library_store", lambda: ledger)
    cache_providers.get_library_management_blob_store.cache_clear()

    store = cache_providers.get_library_management_blob_store()

    assert isinstance(store, LibraryManagementBlobStore)
    assert store.root == (cache_dir / "library-management" / "blobs").resolve()

    cache_providers.get_library_management_blob_store.cache_clear()


def test_target_provider_is_only_referenced_by_isolated_target_composition() -> None:
    root = Path(__file__).parents[2]
    production_roots = [root / "main.py", root / "api", root / "services"]
    references: list[str] = []
    for production_root in production_roots:
        paths = (
            [production_root]
            if production_root.is_file()
            else production_root.rglob("*.py")
        )
        for path in paths:
            if "get_native_library_store" in path.read_text(encoding="utf-8"):
                references.append(str(path.relative_to(root)))

    assert references == ["services/native/target_application_lifecycle.py"]
    assert "target_application" not in (root / "main.py").read_text(encoding="utf-8")
    assert "target_application_lifecycle" in (root / "target_application.py").read_text(
        encoding="utf-8"
    )
