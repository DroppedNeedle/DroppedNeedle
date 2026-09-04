"""NavidromePlaylistExportService tests.

The export writes into a directory that is not exclusively ours - usually
Navidrome's music folder - so these concentrate on the properties that make
that safe: relative entries, marker-proven ownership, and a failed export never
becoming a deletion.
"""

import asyncio
from pathlib import Path
from unittest.mock import AsyncMock

import pytest

from services.navidrome_playlist_export_service import (
    OWNER_MARKER,
    NavidromePlaylistExportService,
    owned_playlist_id,
    render_playlist,
    safe_playlist_filename,
)


def _playlist(pid: str, name: str, *, public: bool = True) -> dict:
    return {"id": pid, "name": name, "is_public": 1 if public else 0}


def _entry(position: int, path: str | None, *, title: str = "Track") -> dict:
    return {
        "position": position,
        "track_name": title,
        "artist_name": "Artist",
        "duration": 180,
        "file_path": path,
        "relative_path": path,
        "root_id": "root-1",
    }


def _service(playlists: list[dict], entries: dict[str, list[dict]]):
    store = AsyncMock()
    store.list_target_playlists = AsyncMock(return_value=playlists)

    async def rows(playlist_id: str):
        return entries.get(playlist_id, [])

    store.list_target_playlist_export_rows = AsyncMock(side_effect=rows)
    return NavidromePlaylistExportService(store)


# Filename safety


def test_filename_is_unique_per_playlist_when_names_collide():
    first = safe_playlist_filename("Favourites", "aaaaaaaa-1111")
    second = safe_playlist_filename("Favourites", "bbbbbbbb-2222")
    assert first != second


@pytest.mark.parametrize(
    "name",
    ["CON", "back/slash", "colon:name", "trailing.", "  ", ""],
)
def test_filename_is_portable_for_hostile_names(name):
    result = safe_playlist_filename(name, "abcdefgh-1234")
    stem = result.rsplit(".", 1)[0]
    assert not set(result) & set('<>:"/\\|?*')
    assert stem.upper().split(" ")[0] not in {"CON", "PRN", "AUX", "NUL"}
    assert result.endswith(".m3u8")
    assert result.strip() == result


def test_filename_is_length_bounded():
    result = safe_playlist_filename("x" * 500, "abcdefgh-1234")
    assert len(result) < 160


# Entry rendering


def test_entries_are_relative_to_the_playlist_directory(tmp_path: Path):
    playlist_dir = tmp_path / "music" / "playlists"
    text, missing, _unrepresentable = render_playlist(
        "Mix",
        "p1",
        [_entry(1, str(tmp_path / "music" / "A" / "Artist" / "Album" / "01.flac"))],
        playlist_dir,
    )
    assert missing == 0
    assert "../A/Artist/Album/01.flac" in text
    # An absolute path here would break the moment Navidrome mounts the library
    # somewhere other than DroppedNeedle does.
    assert str(tmp_path) not in text


def test_entries_use_forward_slashes(tmp_path: Path):
    text, _missing, _unrepresentable = render_playlist(
        "Mix", "p1", [_entry(1, str(tmp_path / "A" / "B" / "01.flac"))], tmp_path
    )
    path_line = [line for line in text.splitlines() if not line.startswith("#")][0]
    assert "\\" not in path_line


def test_extinf_carries_duration_and_label(tmp_path: Path):
    text, _missing, _unrepresentable = render_playlist(
        "Mix", "p1", [_entry(1, str(tmp_path / "01.flac"), title="Song")], tmp_path
    )
    assert "#EXTM3U" in text
    assert "#PLAYLIST:Mix" in text
    assert "#EXTINF:180,Artist - Song" in text


def test_entries_without_a_local_file_are_counted_not_dropped_silently(tmp_path: Path):
    text, missing, _unrepresentable = render_playlist(
        "Mix",
        "p1",
        [_entry(1, str(tmp_path / "01.flac")), _entry(2, None)],
        tmp_path,
    )
    assert missing == 1
    assert text.count("#EXTINF") == 1


# Sync behaviour


@pytest.mark.asyncio
async def test_sync_writes_one_file_per_playlist(tmp_path: Path):
    service = _service(
        [_playlist("p1", "Alpha"), _playlist("p2", "Beta")],
        {
            "p1": [_entry(1, str(tmp_path / "a.flac"))],
            "p2": [_entry(1, str(tmp_path / "b.flac"))],
        },
    )

    result = await service.sync(target_dir=str(tmp_path))

    assert result.success
    assert result.written == 2
    assert len(list(tmp_path.glob("*.m3u8"))) == 2


@pytest.mark.asyncio
async def test_public_scope_excludes_private_playlists(tmp_path: Path):
    service = _service(
        [_playlist("p1", "Shared"), _playlist("p2", "Private", public=False)],
        {
            "p1": [_entry(1, str(tmp_path / "a.flac"))],
            "p2": [_entry(1, str(tmp_path / "b.flac"))],
        },
    )

    result = await service.sync(target_dir=str(tmp_path), scope="public")

    assert result.written == 1
    assert not list(tmp_path.glob("Private*"))


@pytest.mark.asyncio
async def test_all_scope_includes_private_playlists(tmp_path: Path):
    service = _service(
        [_playlist("p1", "Shared"), _playlist("p2", "Private", public=False)],
        {
            "p1": [_entry(1, str(tmp_path / "a.flac"))],
            "p2": [_entry(1, str(tmp_path / "b.flac"))],
        },
    )

    result = await service.sync(target_dir=str(tmp_path), scope="all")

    assert result.written == 2


@pytest.mark.asyncio
async def test_playlist_with_no_local_tracks_is_skipped_not_written_empty(
    tmp_path: Path,
):
    service = _service([_playlist("p1", "Streaming only")], {"p1": [_entry(1, None)]})

    result = await service.sync(target_dir=str(tmp_path))

    assert result.skipped_empty == 1
    assert result.written == 0
    assert not list(tmp_path.glob("*.m3u8"))


@pytest.mark.asyncio
async def test_removal_deletes_only_files_this_service_wrote(tmp_path: Path):
    foreign = tmp_path / "my own playlist.m3u8"
    foreign.write_text("#EXTM3U\n", encoding="utf-8")

    service = _service([_playlist("p1", "Alpha")], {"p1": [_entry(1, str(tmp_path / "a.flac"))]})
    await service.sync(target_dir=str(tmp_path))

    # Alpha disappears from DroppedNeedle; the hand-made file must survive.
    gone = _service([], {})
    result = await gone.sync(target_dir=str(tmp_path), remove_deleted=True)

    assert result.removed == 1
    assert foreign.exists()
    assert not list(tmp_path.glob("Alpha*"))


@pytest.mark.asyncio
async def test_never_remove_leaves_orphaned_exports_in_place(tmp_path: Path):
    service = _service([_playlist("p1", "Alpha")], {"p1": [_entry(1, str(tmp_path / "a.flac"))]})
    await service.sync(target_dir=str(tmp_path))

    gone = _service([], {})
    result = await gone.sync(target_dir=str(tmp_path), remove_deleted=False)

    assert result.removed == 0
    assert list(tmp_path.glob("Alpha*"))



@pytest.mark.asyncio
async def test_missing_directory_is_created(tmp_path: Path):
    target = tmp_path / "not" / "there" / "yet"
    service = _service([_playlist("p1", "Alpha")], {"p1": [_entry(1, str(tmp_path / "a.flac"))]})

    result = await service.sync(target_dir=str(target))

    assert result.success
    assert target.is_dir()


@pytest.mark.asyncio
async def test_empty_path_fails_with_a_message_not_an_exception(tmp_path: Path):
    service = _service([], {})

    result = await service.sync(target_dir="")

    assert not result.success
    assert "Set a playlist folder" in result.message


@pytest.mark.asyncio
async def test_unicode_names_survive_the_round_trip(tmp_path: Path):
    service = _service(
        [_playlist("p1", "RÜFÜS DU SOL — 소리")],
        {"p1": [_entry(1, str(tmp_path / "Ain’t.flac"), title="Ain’t Gonna Die")]},
    )

    result = await service.sync(target_dir=str(tmp_path))

    assert result.written == 1
    written = next(tmp_path.glob("*.m3u8"))
    assert "Ain’t Gonna Die" in written.read_text(encoding="utf-8")


# Failure handling and ownership


@pytest.mark.asyncio
async def test_read_failure_does_not_delete_the_prior_export(tmp_path: Path):
    """A transient error must not be read as "the playlist is gone"."""
    service = _service(
        [_playlist("p1", "Alpha")], {"p1": [_entry(1, str(tmp_path / "a.flac"))]}
    )
    await service.sync(target_dir=str(tmp_path))
    exported = next(tmp_path.glob("Alpha*"))

    broken = _service([_playlist("p1", "Alpha")], {})
    broken._store.list_target_playlist_export_rows = AsyncMock(
        side_effect=RuntimeError("database is locked")
    )
    result = await broken.sync(target_dir=str(tmp_path), remove_deleted=True)

    assert not result.success
    assert result.removed == 0
    assert exported.exists()
    # Ownership lives in the file, so the next run still recognises it.
    assert owned_playlist_id(exported) == "p1"


@pytest.mark.asyncio
async def test_write_failure_does_not_delete_the_prior_export(tmp_path: Path):
    """The same guarantee when the write fails rather than the read."""
    service = _service(
        [_playlist("p1", "Alpha")], {"p1": [_entry(1, str(tmp_path / "a.flac"))]}
    )
    await service.sync(target_dir=str(tmp_path))
    exported = next(tmp_path.glob("Alpha*"))
    original = exported.read_text(encoding="utf-8")

    # The track title must differ, or the rendered text matches what is already
    # on disk, no write is attempted and this asserts nothing.
    failing = _service(
        [_playlist("p1", "Alpha")],
        {"p1": [_entry(1, str(tmp_path / "a.flac"), title="Renamed")]},
    )
    attempted = False

    def explode(path, text):
        nonlocal attempted
        attempted = True
        raise OSError("No space left on device")

    failing._write_atomic = explode
    result = await failing.sync(target_dir=str(tmp_path), remove_deleted=True)

    assert attempted
    assert not result.success
    assert result.removed == 0
    assert exported.read_text(encoding="utf-8") == original


@pytest.mark.asyncio
async def test_a_colliding_hand_made_file_is_never_overwritten(tmp_path: Path):
    """The manifest is an index, not proof that we wrote a file."""
    service = _service(
        [_playlist("p1", "Alpha")], {"p1": [_entry(1, str(tmp_path / "a.flac"))]}
    )
    filename = safe_playlist_filename("Alpha", "p1")
    handmade = tmp_path / filename
    handmade.write_text("#EXTM3U\n/somewhere/else.flac\n", encoding="utf-8")

    result = await service.sync(target_dir=str(tmp_path))

    assert result.skipped_not_ours == 1
    assert result.written == 0
    assert handmade.read_text(encoding="utf-8") == "#EXTM3U\n/somewhere/else.flac\n"


@pytest.mark.asyncio
async def test_a_file_replaced_by_hand_after_export_is_not_deleted(tmp_path: Path):
    """Ownership is re-checked at deletion, not trusted from the manifest."""
    service = _service(
        [_playlist("p1", "Alpha")], {"p1": [_entry(1, str(tmp_path / "a.flac"))]}
    )
    await service.sync(target_dir=str(tmp_path))
    exported = next(tmp_path.glob("Alpha*"))
    exported.write_text("#EXTM3U\n# mine now\n", encoding="utf-8")

    gone = _service([], {})
    result = await gone.sync(target_dir=str(tmp_path), remove_deleted=True)

    assert result.removed == 0
    assert exported.exists()


@pytest.mark.asyncio
async def test_exported_files_carry_the_ownership_marker(tmp_path: Path):
    service = _service(
        [_playlist("p1", "Alpha")], {"p1": [_entry(1, str(tmp_path / "a.flac"))]}
    )

    await service.sync(target_dir=str(tmp_path))

    exported = next(tmp_path.glob("Alpha*"))
    assert f"{OWNER_MARKER}p1" in exported.read_text(encoding="utf-8")
    assert owned_playlist_id(exported) == "p1"


def test_owned_playlist_id_rejects_a_foreign_file(tmp_path: Path):
    foreign = tmp_path / "theirs.m3u8"
    foreign.write_text("#EXTM3U\n#EXTINF:1,A\n/a.flac\n", encoding="utf-8")
    assert owned_playlist_id(foreign) is None


# Input handling


@pytest.mark.asyncio
async def test_relative_target_directory_is_refused(tmp_path: Path):
    service = _service([], {})
    result = await service.sync(target_dir="playlists")
    assert not result.success
    assert "full path" in result.message


def test_control_characters_cannot_inject_m3u_lines(tmp_path: Path):
    """Assert the exact lines: a count check alone can pass on a layout where
    the injected text landed somewhere unexpected."""
    text, _missing, _unrepresentable = render_playlist(
        "Evil\n#EXTINF:1,injected",
        "p1",
        [_entry(1, str(tmp_path / "a.flac"), title="Title\n#EXTINF:9,other")],
        tmp_path,
    )

    assert text.splitlines() == [
        "#EXTM3U",
        f"{OWNER_MARKER}p1",
        "#PLAYLIST:Evil #EXTINF:1,injected",
        "#EXTINF:180,Artist - Title #EXTINF:9,other",
        "a.flac",
    ]
    # The injected text survives only as inert content inside the expected
    # lines, never as a directive of its own.
    assert sum(line.startswith("#EXTINF:") for line in text.splitlines()) == 1
    assert "\r" not in text


def test_temporary_files_are_unique_per_write(tmp_path: Path):
    service = NavidromePlaylistExportService(AsyncMock())
    names = set()
    original = Path.write_text

    def capture(self, *args, **kwargs):
        names.add(self.name)
        return original(self, *args, **kwargs)

    Path.write_text = capture
    try:
        service._write_atomic(tmp_path / "a.m3u8", "#EXTM3U\n")
        service._write_atomic(tmp_path / "a.m3u8", "#EXTM3U\n")
    finally:
        Path.write_text = original

    assert len(names) == 2


@pytest.mark.asyncio
async def test_unchanged_playlist_is_not_rewritten(tmp_path: Path):
    """No mtime churn on a steady library, so Navidrome does not rescan."""
    service = _service(
        [_playlist("p1", "Alpha")], {"p1": [_entry(1, str(tmp_path / "a.flac"))]}
    )
    await service.sync(target_dir=str(tmp_path))
    exported = next(tmp_path.glob("Alpha*"))
    before = exported.stat().st_mtime_ns

    result = await service.sync(target_dir=str(tmp_path))

    assert result.unchanged == 1
    assert result.written == 0
    assert exported.stat().st_mtime_ns == before


@pytest.mark.asyncio
async def test_a_changed_track_path_is_rewritten(tmp_path: Path):
    """A library rename moves a file without touching the playlist's id,
    timestamp or track count."""
    service = _service(
        [_playlist("p1", "Alpha")], {"p1": [_entry(1, str(tmp_path / "old.flac"))]}
    )
    await service.sync(target_dir=str(tmp_path))

    moved = _service(
        [_playlist("p1", "Alpha")],
        {"p1": [_entry(1, str(tmp_path / "renamed" / "new.flac"))]},
    )
    result = await moved.sync(target_dir=str(tmp_path))

    assert result.written == 1
    exported = next(tmp_path.glob("Alpha*"))
    assert "renamed/new.flac" in exported.read_text(encoding="utf-8")


@pytest.mark.asyncio
async def test_a_changed_target_directory_is_populated(tmp_path: Path):
    """Changing the folder must export there immediately."""
    first = tmp_path / "one"
    second = tmp_path / "two"
    service = _service(
        [_playlist("p1", "Alpha")], {"p1": [_entry(1, str(tmp_path / "a.flac"))]}
    )

    await service.sync(target_dir=str(first))
    result = await service.sync(target_dir=str(second))

    assert result.written == 1
    assert list(second.glob("Alpha*"))


@pytest.mark.asyncio
async def test_a_previously_skipped_playlist_is_retried(tmp_path: Path):
    """A skip must not be recorded as done."""
    empty = _service([_playlist("p1", "Alpha")], {"p1": [_entry(1, None)]})
    first = await empty.sync(target_dir=str(tmp_path))
    assert first.skipped_empty == 1

    available = _service(
        [_playlist("p1", "Alpha")], {"p1": [_entry(1, str(tmp_path / "a.flac"))]}
    )
    result = await available.sync(target_dir=str(tmp_path))

    assert result.written == 1


@pytest.mark.asyncio
async def test_renaming_a_playlist_removes_its_previous_file(tmp_path: Path):
    """The filename follows the title, so a rename orphans the old file."""
    service = _service(
        [_playlist("p1", "Alpha")], {"p1": [_entry(1, str(tmp_path / "a.flac"))]}
    )
    await service.sync(target_dir=str(tmp_path))
    assert list(tmp_path.glob("Alpha*"))

    renamed = _service(
        [_playlist("p1", "Beta")], {"p1": [_entry(1, str(tmp_path / "a.flac"))]}
    )
    result = await renamed.sync(target_dir=str(tmp_path), remove_deleted=True)

    assert result.removed == 1
    assert list(tmp_path.glob("Beta*"))
    assert not list(tmp_path.glob("Alpha*"))


@pytest.mark.asyncio
async def test_renaming_leaves_the_old_file_when_removal_is_off(tmp_path: Path):
    service = _service(
        [_playlist("p1", "Alpha")], {"p1": [_entry(1, str(tmp_path / "a.flac"))]}
    )
    await service.sync(target_dir=str(tmp_path))

    renamed = _service(
        [_playlist("p1", "Beta")], {"p1": [_entry(1, str(tmp_path / "a.flac"))]}
    )
    result = await renamed.sync(target_dir=str(tmp_path), remove_deleted=False)

    assert result.removed == 0
    assert list(tmp_path.glob("Alpha*"))
    assert list(tmp_path.glob("Beta*"))



@pytest.mark.asyncio
async def test_concurrent_syncs_are_serialised(tmp_path: Path):
    """Sync Now and the periodic task share one service instance."""
    service = _service(
        [_playlist("p1", "Alpha")], {"p1": [_entry(1, str(tmp_path / "a.flac"))]}
    )
    overlaps = 0
    active = 0
    original = service._store.list_target_playlists

    async def watched():
        nonlocal overlaps, active
        active += 1
        if active > 1:
            overlaps += 1
        await asyncio.sleep(0)
        active -= 1
        return await original()

    service._store.list_target_playlists = watched

    await asyncio.gather(
        service.sync(target_dir=str(tmp_path)),
        service.sync(target_dir=str(tmp_path)),
    )

    assert overlaps == 0


@pytest.mark.asyncio
async def test_a_failed_removal_is_retried_on_the_next_sync(tmp_path: Path):
    """A read-only mount must not leave a deleted playlist exposed forever."""
    service = _service(
        [_playlist("p1", "Alpha")], {"p1": [_entry(1, str(tmp_path / "a.flac"))]}
    )
    await service.sync(target_dir=str(tmp_path))
    exported = next(tmp_path.glob("Alpha*"))

    gone = _service([], {})
    real_unlink = Path.unlink

    def refuse(self, *args, **kwargs):
        if self.name == exported.name:
            raise OSError("Read-only file system")
        return real_unlink(self, *args, **kwargs)

    Path.unlink = refuse
    try:
        first = await gone.sync(target_dir=str(tmp_path), remove_deleted=True)
    finally:
        Path.unlink = real_unlink

    assert first.removed == 0
    assert first.removal_failures == 1
    assert exported.exists()

    second = await gone.sync(target_dir=str(tmp_path), remove_deleted=True)

    assert second.removed == 1
    assert not exported.exists()


@pytest.mark.asyncio
async def test_a_failed_rename_removal_is_retried_on_the_next_sync(tmp_path: Path):
    """The pending entry carries its own filename, so a rename retries too."""
    service = _service(
        [_playlist("p1", "Alpha")], {"p1": [_entry(1, str(tmp_path / "a.flac"))]}
    )
    await service.sync(target_dir=str(tmp_path))
    old = next(tmp_path.glob("Alpha*"))

    renamed = _service(
        [_playlist("p1", "Beta")], {"p1": [_entry(1, str(tmp_path / "a.flac"))]}
    )
    real_unlink = Path.unlink

    def refuse(self, *args, **kwargs):
        if self.name == old.name:
            raise OSError("Device or resource busy")
        return real_unlink(self, *args, **kwargs)

    Path.unlink = refuse
    try:
        first = await renamed.sync(target_dir=str(tmp_path), remove_deleted=True)
    finally:
        Path.unlink = real_unlink

    assert first.removal_failures == 1
    assert old.exists()
    assert list(tmp_path.glob("Beta*"))

    second = await renamed.sync(target_dir=str(tmp_path), remove_deleted=True)

    assert second.removed == 1
    assert not old.exists()
    assert list(tmp_path.glob("Beta*"))



@pytest.mark.asyncio
async def test_a_deferred_removal_reports_as_a_failure(tmp_path: Path):
    """The count must reach the caller: a deferred removal means a playlist is
    still readable in Navidrome that should not be."""
    service = _service(
        [_playlist("p1", "Alpha")], {"p1": [_entry(1, str(tmp_path / "a.flac"))]}
    )
    await service.sync(target_dir=str(tmp_path))
    exported = next(tmp_path.glob("Alpha*"))

    gone = _service([], {})
    real_unlink = Path.unlink

    def refuse(self, *args, **kwargs):
        if self.name == exported.name:
            raise OSError("Read-only file system")
        return real_unlink(self, *args, **kwargs)

    Path.unlink = refuse
    try:
        result = await gone.sync(target_dir=str(tmp_path), remove_deleted=True)
    finally:
        Path.unlink = real_unlink

    assert result.removal_failures == 1
    assert result.success is False
    assert "will retry next sync" in result.message


@pytest.mark.asyncio
async def test_ownership_survives_a_corrupt_leftover_index(tmp_path: Path):
    """A stale or corrupt index file from any source must not affect cleanup."""
    service = _service(
        [_playlist("p1", "Alpha")], {"p1": [_entry(1, str(tmp_path / "a.flac"))]}
    )
    await service.sync(target_dir=str(tmp_path))
    (tmp_path / ".droppedneedle-playlists.json").write_text(
        "{ this is not json", encoding="utf-8"
    )

    gone = _service([], {})
    result = await gone.sync(target_dir=str(tmp_path), remove_deleted=True)

    assert result.removed == 1
    assert not list(tmp_path.glob("Alpha*"))


@pytest.mark.asyncio
async def test_cleanup_works_with_no_prior_state_at_all(tmp_path: Path):
    """A file written by an earlier install, with nothing else on disk, is still
    recognised as ours and cleaned up when its playlist goes."""
    orphan = tmp_path / safe_playlist_filename("Alpha", "p1")
    orphan.write_text(f"#EXTM3U\n{OWNER_MARKER}p1\n#PLAYLIST:Alpha\na.flac\n", encoding="utf-8")

    service = _service([], {})
    result = await service.sync(target_dir=str(tmp_path), remove_deleted=True)

    assert result.removed == 1
    assert not orphan.exists()


@pytest.mark.asyncio
async def test_a_read_only_directory_reports_rather_than_silently_skipping(
    tmp_path: Path,
):
    """A removal that cannot happen must fail the run, every run, until it can."""
    service = _service(
        [_playlist("p1", "Alpha")], {"p1": [_entry(1, str(tmp_path / "a.flac"))]}
    )
    await service.sync(target_dir=str(tmp_path))
    exported = next(tmp_path.glob("Alpha*"))

    gone = _service([], {})
    real_unlink = Path.unlink

    def refuse(self, *args, **kwargs):
        if self.name == exported.name:
            raise OSError("Read-only file system")
        return real_unlink(self, *args, **kwargs)

    Path.unlink = refuse
    try:
        first = await gone.sync(target_dir=str(tmp_path), remove_deleted=True)
        second = await gone.sync(target_dir=str(tmp_path), remove_deleted=True)
    finally:
        Path.unlink = real_unlink

    # Reported both times: nothing was remembered that could mask the second.
    assert first.removal_failures == 1 and first.success is False
    assert second.removal_failures == 1 and second.success is False

    third = await gone.sync(target_dir=str(tmp_path), remove_deleted=True)
    assert third.removed == 1
    assert third.success is True
