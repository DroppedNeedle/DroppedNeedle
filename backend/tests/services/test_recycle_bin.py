"""CollectionManagement Phase 0: the upgrade-only recycle bin (D4/D19).

``recycle()`` moves (never deletes), survives basename collisions, and
``prune()`` removes only entries past the retention window.
"""

from datetime import datetime, timedelta, timezone
from pathlib import Path

import pytest

from services.native.recycle_bin import (
    _ENTRY_STAMP_FORMAT,
    prune,
    recycle,
    resolve_bin_path,
)


@pytest.fixture
def bin_path(tmp_path: Path) -> Path:
    return tmp_path / ".recycle"


def _make_file(tmp_path: Path, name: str, content: bytes = b"audio") -> Path:
    path = tmp_path / name
    path.write_bytes(content)
    return path


def test_recycle_moves_file_into_bin(tmp_path: Path, bin_path: Path):
    source = _make_file(tmp_path, "track.mp3", b"old bytes")

    destination = recycle(source, bin_path)

    assert not source.exists()
    assert destination.exists()
    assert destination.read_bytes() == b"old bytes"
    assert bin_path in destination.parents


def test_recycle_same_basename_twice_never_collides(tmp_path: Path, bin_path: Path):
    first = recycle(_make_file(tmp_path, "track.mp3", b"one"), bin_path)
    second = recycle(_make_file(tmp_path, "track.mp3", b"two"), bin_path)

    assert first != second
    assert first.read_bytes() == b"one"
    assert second.read_bytes() == b"two"


def test_prune_removes_only_entries_past_retention(tmp_path: Path, bin_path: Path):
    fresh = recycle(_make_file(tmp_path, "fresh.mp3"), bin_path)
    # Forge an old entry by renaming its stamp 40 days into the past (prune ages by
    # the name stamp, not mtime, so an EXDEV copy can't resurrect an old entry).
    old_entry = recycle(_make_file(tmp_path, "old.mp3"), bin_path).parent
    old_stamp = (datetime.now(timezone.utc) - timedelta(days=40)).strftime(_ENTRY_STAMP_FORMAT)
    aged = old_entry.rename(bin_path / f"{old_stamp}-{old_entry.name.split('-', 1)[1]}")

    removed = prune(bin_path, retention_days=30)

    assert removed == 1
    assert not aged.exists()
    assert fresh.exists()


def test_prune_missing_bin_is_noop(tmp_path: Path):
    assert prune(tmp_path / "nope", retention_days=30) == 0


def test_resolve_bin_path_prefers_configured_then_library_default():
    assert resolve_bin_path("/custom/bin", ["/music"]) == Path("/custom/bin")
    assert resolve_bin_path("", ["/music"]) == Path("/music/.recycle")
    assert resolve_bin_path("   ", ["/music", "/more"]) == Path("/music/.recycle")
    assert resolve_bin_path("", []) is None
