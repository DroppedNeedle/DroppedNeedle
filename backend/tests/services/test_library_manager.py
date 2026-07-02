"""Tests for LibraryManager - aggregation-on-read CRUD over library_files."""

import asyncio
import threading
from pathlib import Path

import pytest

from infrastructure.persistence.library_db import LibraryDB
from models.audio import AudioInfo, AudioTag
from services.native.library_manager import LibraryManager


@pytest.fixture
def manager(tmp_path: Path) -> LibraryManager:
    db = LibraryDB(db_path=tmp_path / "library.db", write_lock=threading.Lock())
    return LibraryManager(db)


def _tag(**overrides) -> AudioTag:
    base = dict(
        title="Airbag",
        artist="Radiohead",
        album="OK Computer",
        album_artist="Radiohead",
        track_number=1,
        disc_number=1,
        year=1997,
        musicbrainz_release_group_id="rg-1",
        musicbrainz_artist_id="art-1",
    )
    base.update(overrides)
    return AudioTag(**base)


def _info(**overrides) -> AudioInfo:
    base = dict(
        duration_seconds=260.0,
        bitrate=900,
        sample_rate=44100,
        channels=2,
        file_format="flac",
        file_size_bytes=1000,
        bit_depth=16,
    )
    base.update(overrides)
    return AudioInfo(**base)


async def _upsert(manager, path, *, rg="rg-1", rec="rec-1", track=1, disc=1, tag=None, info=None):
    return await manager.upsert_file(
        Path(path),
        tag or _tag(track_number=track, disc_number=disc),
        info or _info(),
        release_group_mbid=rg,
        recording_mbid=rec,
    )


@pytest.mark.asyncio
async def test_get_albums_empty_returns_empty_list(manager):
    assert await manager.get_albums() == []


@pytest.mark.asyncio
async def test_upsert_new_file_appears_in_albums(manager, tmp_path):
    await _upsert(manager, tmp_path / "a.flac")
    albums = await manager.get_albums()
    assert len(albums) == 1
    assert albums[0].release_group_mbid == "rg-1"
    assert albums[0].track_count == 1
    assert await manager.has_album("rg-1") is True


@pytest.mark.asyncio
async def test_has_any_files_reflects_library_contents(manager, tmp_path):
    # gates the Local Files tab and the integration-status `localfiles` flag
    assert await manager.has_any_files() is False
    await _upsert(manager, tmp_path / "a.flac")
    assert await manager.has_any_files() is True


@pytest.mark.asyncio
async def test_album_quality_format_is_highest_present_not_alphabetical(manager, tmp_path):
    # badge must report highest-quality format present (wav), not MIN() which is alphabetical (mp3 < wav)
    await _upsert(manager, tmp_path / "01.mp3", track=1, rec="rec-1", info=_info(file_format="mp3"))
    await _upsert(manager, tmp_path / "02.wav", track=2, rec="rec-2", info=_info(file_format="wav"))
    albums = await manager.get_albums()
    assert len(albums) == 1
    assert albums[0].quality_format == "wav"


@pytest.mark.asyncio
async def test_upsert_new_path_inserts_fresh_uuid(manager, tmp_path):
    id_a = await _upsert(manager, tmp_path / "a.flac", rec="rec-a")
    id_b = await _upsert(manager, tmp_path / "b.flac", rec="rec-b")
    assert id_a != id_b
    assert len(id_a) == 32  # uuid4().hex


@pytest.mark.asyncio
async def test_upsert_existing_path_updates_preserving_id(manager, tmp_path):
    path = tmp_path / "a.flac"
    first = await _upsert(manager, path, tag=_tag(title="Old"))
    second = await _upsert(manager, path, tag=_tag(title="New", track_number=1))
    assert first == second  # same row identity preserved across update
    tracks = await manager.get_tracks("rg-1")
    assert len(tracks) == 1
    assert tracks[0].track_title == "New"


@pytest.mark.asyncio
async def test_upsert_compilation_sets_various_artists(manager, tmp_path):
    tag = _tag(compilation=True, artist="Track Artist", album_artist=None)
    await manager.upsert_file(
        tmp_path / "comp.flac", tag, _info(), release_group_mbid="rg-comp", recording_mbid="rec-comp"
    )
    tracks = await manager.get_tracks("rg-comp")
    assert tracks[0].artist_name == "Track Artist"  # per-track artist
    albums = await manager.get_albums()
    summary = next(a for a in albums if a.release_group_mbid == "rg-comp")
    assert summary.is_compilation is True


@pytest.mark.asyncio
async def test_soft_delete_removes_from_reads(manager, tmp_path):
    path = tmp_path / "a.flac"
    await _upsert(manager, path)
    await manager.soft_delete_file(str(path))
    assert await manager.get_albums() == []
    assert await manager.get_tracks("rg-1") == []
    assert await manager.has_album("rg-1") is False


@pytest.mark.asyncio
async def test_get_tracks_ordered_by_disc_then_track(manager, tmp_path):
    await _upsert(manager, tmp_path / "d2t1.flac", rec="r3", track=1, disc=2)
    await _upsert(manager, tmp_path / "d1t2.flac", rec="r2", track=2, disc=1)
    await _upsert(manager, tmp_path / "d1t1.flac", rec="r1", track=1, disc=1)
    tracks = await manager.get_tracks("rg-1")
    assert [(t.disc_number, t.track_number) for t in tracks] == [(1, 1), (1, 2), (2, 1)]


@pytest.mark.asyncio
async def test_search_tracks_matches_title_and_album(manager, tmp_path):
    # guards the LibraryManager -> LibraryDB passthrough against going missing
    await _upsert(
        manager, tmp_path / "airbag.flac", rec="r1", track=1,
        tag=_tag(title="Airbag", album="OK Computer"),
    )
    await _upsert(
        manager, tmp_path / "karma.flac", rec="r2", track=2,
        tag=_tag(title="Karma Police", album="OK Computer"),
    )
    by_title = await manager.search_tracks("airbag")
    assert [r["track_title"] for r in by_title] == ["Airbag"]
    by_album = await manager.search_tracks("ok computer")
    assert {r["track_title"] for r in by_album} == {"Airbag", "Karma Police"}


@pytest.mark.asyncio
async def test_has_track(manager, tmp_path):
    await _upsert(manager, tmp_path / "a.flac", rec="rec-xyz")
    assert await manager.has_track("rec-xyz") is True
    assert await manager.has_track("rec-missing") is False


@pytest.mark.asyncio
async def test_get_albums_pagination(manager, tmp_path):
    for i in range(5):
        await _upsert(manager, tmp_path / f"{i}.flac", rg=f"rg-{i}", rec=f"rec-{i}")
    page1 = await manager.get_albums(page=1, page_size=2)
    page3 = await manager.get_albums(page=3, page_size=2)
    assert len(page1) == 2
    assert len(page3) == 1  # 5 albums, last page has the remainder


@pytest.mark.asyncio
async def test_ten_concurrent_upserts_no_database_locked(manager, tmp_path):
    results = await asyncio.gather(
        *[_upsert(manager, tmp_path / f"track{i}.flac", rec=f"rec-{i}", track=i) for i in range(10)]
    )
    assert len(set(results)) == 10  # ten distinct rows, no lost writes
    albums = await manager.get_albums()
    assert albums[0].track_count == 10


@pytest.mark.asyncio
async def test_reconcile_soft_deletes_missing_files(manager, tmp_path):
    music = tmp_path / "music"
    music.mkdir()
    present = music / "present.flac"
    missing = music / "missing.flac"
    present.write_bytes(b"x")
    missing.write_bytes(b"x")
    await _upsert(manager, present, rec="rec-present")
    await _upsert(manager, missing, rec="rec-missing")
    missing.unlink()  # gone from disk

    deleted = await manager.reconcile_with_filesystem([music])
    assert deleted == 1
    tracks = await manager.get_tracks("rg-1")
    assert {t.file_path for t in tracks} == {str(present)}


@pytest.mark.asyncio
async def test_reconcile_with_no_targets_is_safe_noop(manager, tmp_path):
    await _upsert(manager, tmp_path / "a.flac")
    assert await manager.reconcile_with_filesystem(None) == 0
    assert len(await manager.get_albums()) == 1  # nothing mass-deleted


@pytest.mark.asyncio
async def test_reconcile_skips_when_target_missing_no_mass_delete(manager, tmp_path):
    # a missing/unmounted target makes os.walk yield nothing; must NOT soft-delete
    # the whole library against an empty present-set
    await _upsert(manager, tmp_path / "a.flac")
    deleted = await manager.reconcile_with_filesystem([tmp_path / "not-mounted"])
    assert deleted == 0
    assert len(await manager.get_albums()) == 1


@pytest.mark.asyncio
async def test_reconcile_protects_unmounted_root_while_sibling_reconciles(manager, tmp_path):
    # root_a healthy (one genuinely-deleted file), root_b unmounted so its walk yields
    # nothing: the missing file under root_a must be soft-deleted, but root_b's tracks
    # must be protected from mass-delete, not wiped because the mount is down
    import shutil

    root_a = tmp_path / "a"
    root_b = tmp_path / "b"
    root_a.mkdir()
    root_b.mkdir()
    a_present = root_a / "present.flac"
    a_missing = root_a / "missing.flac"
    b_track = root_b / "song.flac"
    for f in (a_present, a_missing, b_track):
        f.write_bytes(b"x")
    await _upsert(manager, a_present, rg="rg-a", rec="rec-a1")
    await _upsert(manager, a_missing, rg="rg-a", rec="rec-a2")
    await _upsert(manager, b_track, rg="rg-b", rec="rec-b1")
    a_missing.unlink()  # genuinely gone from the healthy root
    shutil.rmtree(root_b)  # root_b unmounted: its walk yields nothing

    deleted = await manager.reconcile_with_filesystem([root_a, root_b])
    assert deleted == 1  # only the genuinely-missing file under the healthy root
    assert {t.file_path for t in await manager.get_tracks("rg-a")} == {str(a_present)}
    # root_b's track survives: protected, not soft-deleted on transient mount loss
    assert {t.file_path for t in await manager.get_tracks("rg-b")} == {str(b_track)}


@pytest.mark.asyncio
async def test_get_tracks_page_flat_list_with_album_context(manager, tmp_path):
    await _upsert(manager, tmp_path / "01.flac", rg="rg-1", rec="rec-1", track=1)
    await _upsert(manager, tmp_path / "02.flac", rg="rg-1", rec="rec-2", track=2)
    items, total = await manager.get_tracks_page(limit=10, offset=0)
    assert total == 2
    assert len(items) == 2
    first = items[0]
    assert first.title == "Airbag"
    assert first.album_name == "OK Computer"
    assert first.artist_name == "Radiohead"
    assert first.album_mbid == "rg-1"
    assert first.track_file_id  # the library_files UUID the player streams by


@pytest.mark.asyncio
async def test_get_tracks_page_paginates_and_searches(manager, tmp_path):
    for i in range(5):
        await _upsert(
            manager, tmp_path / f"{i}.flac", rg=f"rg-{i}", rec=f"rec-{i}", tag=_tag(title=f"Song {i}")
        )
    page1, total = await manager.get_tracks_page(limit=2, offset=0)
    assert total == 5
    assert len(page1) == 2
    page3, _ = await manager.get_tracks_page(limit=2, offset=4)
    assert len(page3) == 1  # 5 tracks, last page has the remainder

    found, found_total = await manager.get_tracks_page(limit=10, offset=0, q="Song 3")
    assert found_total == 1
    assert found[0].title == "Song 3"


@pytest.mark.asyncio
async def test_upsert_requires_mbid_unless_manual_review(manager, tmp_path):
    with pytest.raises(ValueError):
        await manager.upsert_file(tmp_path / "x.flac", _tag(), _info(), release_group_mbid=None)
    # source='manual_review' is the sanctioned None-mbid path (Tier 4)
    file_id = await manager.upsert_file(
        tmp_path / "y.flac", _tag(), _info(), release_group_mbid=None, source="manual_review"
    )
    assert file_id


@pytest.mark.asyncio
async def test_get_library_mbids_empty_when_no_files(manager):
    assert await manager.get_library_mbids() == set()


@pytest.mark.asyncio
async def test_get_library_mbids_returns_native_release_groups(manager, tmp_path):
    # Overrides the empty stub so /library/mbids, artist in-library flags, and the
    # request-completion check reflect native imports.
    await _upsert(manager, tmp_path / "a.flac", rg="rg-1", rec="rec-1")
    await _upsert(manager, tmp_path / "b.flac", rg="rg-2", rec="rec-2")
    assert await manager.get_library_mbids(include_release_ids=False) == {"rg-1", "rg-2"}


@pytest.mark.asyncio
async def test_get_library_mbids_includes_release_ids_when_requested(manager, tmp_path):
    await manager.upsert_file(
        tmp_path / "a.flac",
        _tag(),
        _info(),
        release_group_mbid="rg-1",
        release_mbid="rel-1",
        recording_mbid="rec-1",
    )
    assert await manager.get_library_mbids(include_release_ids=False) == {"rg-1"}
    assert await manager.get_library_mbids(include_release_ids=True) == {"rg-1", "rel-1"}


@pytest.mark.asyncio
async def test_get_library_mbids_excludes_soft_deleted(manager, tmp_path):
    gone = tmp_path / "gone.flac"
    await _upsert(manager, gone, rg="rg-del", rec="rec-del")
    await manager.soft_delete_file(str(gone))
    assert "rg-del" not in await manager.get_library_mbids()


@pytest.mark.asyncio
async def test_targeted_reconcile_does_not_touch_other_dirs(manager, tmp_path):
    # Regression: a reconcile scoped to ONE album folder (the post-import targeted
    # reconcile) must NOT soft-delete files that live in OTHER folders. Previously
    # importing one album soft-deleted the entire rest of the library.
    dir_a = tmp_path / "Album A"
    dir_b = tmp_path / "Album B"
    dir_a.mkdir()
    dir_b.mkdir()
    (dir_a / "a1.flac").write_bytes(b"x")
    (dir_b / "b1.flac").write_bytes(b"x")
    await _upsert(manager, dir_a / "a1.flac", rg="rg-a", rec="rec-a")
    await _upsert(manager, dir_b / "b1.flac", rg="rg-b", rec="rec-b")

    await manager.reconcile_with_filesystem(targets=[dir_a])

    mbids = await manager.get_library_mbids(include_release_ids=False)
    assert "rg-a" in mbids  # on disk + in scope -> kept
    assert "rg-b" in mbids  # outside the reconcile scope -> NOT soft-deleted (the fix)


@pytest.mark.asyncio
async def test_targeted_reconcile_soft_deletes_missing_within_scope(manager, tmp_path):
    dir_a = tmp_path / "Album A"
    dir_a.mkdir()
    (dir_a / "present.flac").write_bytes(b"x")
    await _upsert(manager, dir_a / "present.flac", rg="rg-present", rec="rec-1")
    # a row whose file is gone from disk but lives UNDER the reconcile scope
    await _upsert(manager, dir_a / "gone.flac", rg="rg-gone", rec="rec-2")

    await manager.reconcile_with_filesystem(targets=[dir_a])

    mbids = await manager.get_library_mbids(include_release_ids=False)
    assert "rg-present" in mbids
    assert "rg-gone" not in mbids  # genuinely missing within scope -> soft-deleted


@pytest.mark.asyncio
async def test_album_quality_tier_none_when_not_held(manager):
    assert await manager.album_quality_tier("rg-absent") is None


@pytest.mark.asyncio
async def test_album_quality_tier_is_the_worst_held_file(manager, tmp_path):
    # An album is only as good as its weakest track: a FLAC + a 320 MP3 -> mp3_320.
    await _upsert(manager, tmp_path / "a.flac", track=1, rec="rec-1",
                  info=_info(file_format="flac", bitrate=900))
    await _upsert(manager, tmp_path / "b.mp3", track=2, rec="rec-2",
                  info=_info(file_format="mp3", bitrate=320, bit_depth=None))
    assert await manager.album_quality_tier("rg-1") == "mp3_320"


@pytest.mark.asyncio
async def test_album_quality_tier_all_lossless(manager, tmp_path):
    await _upsert(manager, tmp_path / "a.flac", info=_info(file_format="flac", bitrate=900))
    assert await manager.album_quality_tier("rg-1") == "lossless"


_REAL_ARTIST_MBID = "88d17133-abbc-42db-9526-4e2c1db60336"


@pytest.mark.asyncio
async def test_upsert_inherits_album_artist_from_release_group(manager, tmp_path):
    # first file resolved the album's artist; a later tag-poor file (e.g. a held
    # import) must join that identity, not synthesize a second one that splits
    # the artist into two library entries
    await _upsert(manager, tmp_path / "a.flac", track=1, rec="rec-1",
                  tag=_tag(track_number=1, musicbrainz_album_artist_id=_REAL_ARTIST_MBID))
    await _upsert(manager, tmp_path / "b.mp3", track=2, rec="rec-2",
                  tag=_tag(track_number=2))
    artists, total = await manager.get_artists()
    assert total == 1
    assert artists[0].artist_mbid == _REAL_ARTIST_MBID
    assert artists[0].track_count == 2


@pytest.mark.asyncio
async def test_upsert_compilation_does_not_inherit_album_artist(manager, tmp_path):
    # a compilation file must stay under Various Artists even when its release
    # group has a resolved single artist on other files
    await _upsert(manager, tmp_path / "a.flac", track=1, rec="rec-1",
                  tag=_tag(track_number=1, musicbrainz_album_artist_id=_REAL_ARTIST_MBID))
    await _upsert(manager, tmp_path / "b.mp3", track=2, rec="rec-2",
                  tag=_tag(track_number=2, album_artist="Various Artists"))
    artists, _total = await manager.get_artists()
    by_name = {a.artist_name: a for a in artists}
    assert by_name["Radiohead"].artist_mbid == _REAL_ARTIST_MBID
    assert by_name["Various Artists"].artist_mbid is None


@pytest.mark.asyncio
async def test_get_artists_never_exposes_synthetic_mbid(manager, tmp_path):
    # no MB album-artist id anywhere: the synthetic (dashless) Q14 id must stay
    # internal - leaking it gives the UI a MusicBrainz link that can only 404
    await _upsert(manager, tmp_path / "a.flac")
    artists, total = await manager.get_artists()
    assert total == 1
    assert artists[0].artist_name == "Radiohead"
    assert artists[0].artist_mbid is None
