"""playlist_tracks song identity + resolution columns (#204)."""

import sqlite3

import pytest

from repositories.playlist_repository import PlaylistRepository, TrackResolution


@pytest.fixture
def repo(tmp_path):
    return PlaylistRepository(db_path=tmp_path / "test.db")


def _add(repo, playlist_id, **extra):
    return repo.add_tracks(
        playlist_id,
        [
            {
                "track_name": "Fortunate Son",
                "artist_name": "Creedence Clearwater Revival",
                "album_name": "Chronicle",
                "source_type": "",
                **extra,
            }
        ],
    )[0]


def test_add_tracks_round_trips_identity(repo):
    playlist = repo.create_playlist("CCR")
    created = _add(repo, playlist.id, isrc="USFI86900049", recording_mbid="rec-1")

    stored = repo.get_track(playlist.id, created.id)
    for record in (created, stored):
        assert record.isrc == "USFI86900049"
        assert record.recording_mbid == "rec-1"
        assert record.target_release_group_mbid is None
        assert record.resolution_status is None
        assert record.flag_options is None


def test_batch_update_resolutions(repo):
    playlist = repo.create_playlist("CCR")
    kept = _add(repo, playlist.id, recording_mbid="rec-1")
    swapped = _add(repo, playlist.id, recording_mbid="rec-remaster")
    flagged = _add(repo, playlist.id, recording_mbid="rec-3")
    before = repo.get_playlist(playlist.id).updated_at

    options = [
        {"recording_mbid": "rec-studio", "release_group_mbid": "rg-willy"},
        {"recording_mbid": "rec-3", "release_group_mbid": "rg-chronicle"},
    ]
    updated = repo.batch_update_resolutions(
        playlist.id,
        {
            kept.id: TrackResolution("rg-willy", "album", "resolved"),
            swapped.id: TrackResolution(
                "rg-bayou", "album", "resolved", recording_mbid="rec-studio"
            ),
            flagged.id: TrackResolution(None, None, "flagged", flag_options=options),
            "not-a-track": TrackResolution(None, None, "unresolved"),
        },
    )

    assert updated == 3
    by_id = {t.id: t for t in repo.get_tracks(playlist.id)}
    assert by_id[kept.id].recording_mbid == "rec-1"
    assert by_id[kept.id].target_release_group_mbid == "rg-willy"
    assert by_id[kept.id].home_tier == "album"
    assert by_id[swapped.id].recording_mbid == "rec-studio"
    assert by_id[flagged.id].resolution_status == "flagged"
    assert by_id[flagged.id].flag_options == options
    # Derived state is not a user edit.
    assert repo.get_playlist(playlist.id).updated_at == before


def test_resolution_is_scoped_to_playlist(repo):
    mine = repo.create_playlist("Mine")
    other = repo.create_playlist("Other")
    track = _add(repo, mine.id)

    assert (
        repo.batch_update_resolutions(
            other.id, {track.id: TrackResolution("rg", "album", "resolved")}
        )
        == 0
    )
    assert repo.get_track(mine.id, track.id).resolution_status is None


def test_update_track_source_preserves_resolution(repo):
    playlist = repo.create_playlist("CCR")
    track = _add(repo, playlist.id, isrc="USFI86900049", recording_mbid="rec-1")
    repo.batch_update_resolutions(
        playlist.id, {track.id: TrackResolution("rg-willy", "album", "resolved")}
    )

    updated = repo.update_track_source(playlist.id, track.id, source_type="local")

    assert updated.isrc == "USFI86900049"
    assert updated.target_release_group_mbid == "rg-willy"
    assert updated.resolution_status == "resolved"


def test_columns_added_to_legacy_table(tmp_path):
    db = tmp_path / "legacy.db"
    conn = sqlite3.connect(db)
    conn.execute(
        "CREATE TABLE playlists (id TEXT PRIMARY KEY, name TEXT NOT NULL, "
        "cover_image_path TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL)"
    )
    conn.execute(
        "CREATE TABLE playlist_tracks (id TEXT PRIMARY KEY, playlist_id TEXT NOT NULL, "
        "position INTEGER NOT NULL, track_name TEXT NOT NULL, artist_name TEXT NOT NULL, "
        "album_name TEXT NOT NULL, album_id TEXT, artist_id TEXT, track_source_id TEXT, "
        "cover_url TEXT, source_type TEXT NOT NULL, available_sources TEXT, format TEXT, "
        "track_number INTEGER, duration INTEGER, created_at TEXT NOT NULL)"
    )
    conn.execute(
        "INSERT INTO playlists VALUES ('p1', 'Old', NULL, '2025-01-01', '2025-01-01')"
    )
    conn.execute(
        "INSERT INTO playlist_tracks (id, playlist_id, position, track_name, "
        "artist_name, album_name, source_type, created_at) "
        "VALUES ('t1', 'p1', 0, 'T', 'A', 'AL', '', '2025-01-01')"
    )
    conn.commit()
    conn.close()

    for _ in range(2):
        repo = PlaylistRepository(db_path=db)
        legacy = repo.get_track("p1", "t1")
        assert legacy.isrc is None
        assert legacy.flag_options is None
        repo.close()


def test_corrupt_flag_options_read_as_none(repo, tmp_path):
    playlist = repo.create_playlist("CCR")
    track = _add(repo, playlist.id)
    conn = repo._get_connection()
    conn.execute(
        "UPDATE playlist_tracks SET flag_options = '{bad' WHERE id = ?", (track.id,)
    )
    conn.commit()
    assert repo.get_track(playlist.id, track.id).flag_options is None


def test_legacy_rows_resolved_earlier_get_their_filed_album_shown(repo):
    """Same refresh on the legacy table, even without a library_files table."""
    playlist = repo.create_playlist("Kungs")
    stale = "/api/v1/covers/release-group/rg-muchdance?size=250"
    resolved = _add(repo, playlist.id, album_id="rg-muchdance", cover_url=stale)
    flagged = _add(repo, playlist.id, album_id="rg-muchdance", cover_url=stale)
    repo.batch_update_resolutions(
        playlist.id,
        {
            resolved.id: TrackResolution("rg-layers", "album", "resolved"),
            flagged.id: TrackResolution(None, None, "flagged"),
        },
    )

    assert repo.link_tracks_by_recording(playlist.id) == 1

    after = {t.id: t for t in repo.get_tracks(playlist.id)}
    assert (
        after[resolved.id].cover_url
        == "/api/v1/covers/release-group/rg-layers?size=250"
    )
    assert after[resolved.id].album_id == "rg-layers"
    assert after[flagged.id].cover_url == stale
    assert repo.link_tracks_by_recording(playlist.id) == 0


def test_legacy_removed_library_file_is_unlinked(repo, tmp_path):
    playlist = repo.create_playlist("CCR")
    conn = repo._get_connection()
    conn.execute(
        "CREATE TABLE library_files (id TEXT PRIMARY KEY, recording_mbid TEXT, deleted_at TEXT)"
    )
    conn.execute("INSERT INTO library_files VALUES ('file-1', 'rec-1', '2026-10-14')")
    conn.commit()
    track = _add(
        repo,
        playlist.id,
        source_type="local",
        track_source_id="file-1",
        library_file_id="file-1",
        available_sources=["local", "navidrome"],
    )

    assert repo.link_tracks_by_recording(playlist.id) == 1

    after = repo.get_track(playlist.id, track.id)
    assert (after.library_file_id, after.source_type, after.track_source_id) == (
        None,
        "",
        None,
    )
    assert after.available_sources is None  # Navidrome served the same file
