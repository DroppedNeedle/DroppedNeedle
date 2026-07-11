import json
import threading

import pytest

from repositories.playlist_repository import PlaylistRepository


@pytest.fixture
def repo(tmp_path):
    db = tmp_path / "test.db"
    return PlaylistRepository(db_path=db)


class TestEnsureTables:
    def test_idempotent(self, tmp_path):
        db = tmp_path / "test.db"
        repo1 = PlaylistRepository(db_path=db)
        repo2 = PlaylistRepository(db_path=db)
        assert repo1.get_all_playlists() == []
        assert repo2.get_all_playlists() == []

    def test_foreign_key_cascade(self, repo):
        playlist = repo.create_playlist("Test")
        repo.add_tracks(playlist.id, [
            {"track_name": "T1", "artist_name": "A1", "album_name": "AL1", "source_type": "local"},
        ])
        assert len(repo.get_tracks(playlist.id)) == 1
        repo.delete_playlist(playlist.id)
        assert repo.get_tracks(playlist.id) == []

    def test_backfills_local_library_file_id(self, tmp_path):
        # web-UI-era local rows carry the file id only in track_source_id; the
        # ensure-tables ratchet links them so compat clients can stream them
        # (issue #181), and re-running is a no-op.
        import sqlite3

        db = tmp_path / "test.db"
        repo1 = PlaylistRepository(db_path=db)
        playlist = repo1.create_playlist("Legacy")
        conn = sqlite3.connect(db)
        rows = [
            ("t-local", "local", "file-1", None),
            ("t-howler", "howler", "file-2", None),
            ("t-linked", "local", "file-3", "already-linked"),
            ("t-foreign", "navidrome", "nd-4", None),
            ("t-empty", "local", "", None),
        ]
        for i, (tid, source, src_id, lfid) in enumerate(rows):
            conn.execute(
                "INSERT INTO playlist_tracks "
                "(id, playlist_id, position, track_name, artist_name, album_name, "
                " source_type, track_source_id, library_file_id, created_at) "
                "VALUES (?, ?, ?, 'T', 'A', 'AL', ?, ?, ?, '2025-01-01')",
                (tid, playlist.id, i, source, src_id, lfid),
            )
        conn.commit()
        conn.close()

        for _ in range(2):  # construct twice: backfill applies once, stays stable
            PlaylistRepository(db_path=db)
            by_id = {t.id: t for t in repo1.get_tracks(playlist.id)}
            assert by_id["t-local"].library_file_id == "file-1"
            assert by_id["t-howler"].library_file_id == "file-2"
            assert by_id["t-linked"].library_file_id == "already-linked"
            assert by_id["t-foreign"].library_file_id is None
            assert by_id["t-empty"].library_file_id is None

    def test_unique_position_constraint(self, repo):
        playlist = repo.create_playlist("Test")
        repo.add_tracks(playlist.id, [
            {"track_name": "T1", "artist_name": "A1", "album_name": "AL1", "source_type": "local"},
        ])
        import sqlite3
        with pytest.raises(sqlite3.IntegrityError):
            conn = repo._get_connection()
            conn.execute(
                "INSERT INTO playlist_tracks "
                "(id, playlist_id, position, track_name, artist_name, album_name, source_type, created_at) "
                "VALUES (?, ?, 0, 'dup', 'dup', 'dup', 'local', '2025-01-01')",
                ("dup-id", playlist.id),
            )


class TestCreatePlaylist:
    def test_returns_record(self, repo):
        result = repo.create_playlist("My Playlist")
        assert result.name == "My Playlist"
        assert result.id
        assert result.created_at
        assert result.updated_at
        assert result.cover_image_path is None


class TestGetPlaylist:
    def test_existing(self, repo):
        created = repo.create_playlist("Test")
        fetched = repo.get_playlist(created.id)
        assert fetched is not None
        assert fetched.id == created.id
        assert fetched.name == "Test"

    def test_non_existent(self, repo):
        assert repo.get_playlist("nonexistent") is None


class TestGetAllPlaylists:
    def test_empty(self, repo):
        assert repo.get_all_playlists() == []

    def test_with_playlists_and_tracks(self, repo):
        p1 = repo.create_playlist("P1")
        p2 = repo.create_playlist("P2")
        repo.add_tracks(p1.id, [
            {"track_name": "T1", "artist_name": "A", "album_name": "AL",
             "source_type": "local", "cover_url": "http://a.jpg", "duration": 200},
            {"track_name": "T2", "artist_name": "A", "album_name": "AL",
             "source_type": "local", "cover_url": "http://b.jpg", "duration": 300},
        ])
        summaries = repo.get_all_playlists()
        assert len(summaries) == 2

        p1_summary = next(s for s in summaries if s.id == p1.id)
        assert p1_summary.track_count == 2
        assert p1_summary.total_duration == 500
        assert len(p1_summary.cover_urls) == 2

        p2_summary = next(s for s in summaries if s.id == p2.id)
        assert p2_summary.track_count == 0

    def test_ordered_by_updated_at_desc(self, repo):
        import time
        p1 = repo.create_playlist("First")
        time.sleep(0.05)
        p2 = repo.create_playlist("Second")
        time.sleep(0.05)
        repo.update_playlist(p1.id, name="First Updated")
        summaries = repo.get_all_playlists()
        assert len(summaries) == 2
        assert summaries[0].id == p1.id
        assert summaries[1].id == p2.id


class TestUpdatePlaylist:
    def test_update_name(self, repo):
        p = repo.create_playlist("Old Name")
        updated = repo.update_playlist(p.id, name="New Name")
        assert updated is not None
        assert updated.name == "New Name"
        assert updated.updated_at > p.updated_at

    def test_non_existent(self, repo):
        assert repo.update_playlist("nonexistent", name="X") is None

    def test_update_cover_path(self, repo):
        p = repo.create_playlist("Test")
        updated = repo.update_playlist(p.id, cover_image_path="/some/path.jpg")
        assert updated is not None
        assert updated.cover_image_path == "/some/path.jpg"

    def test_clear_cover_path(self, repo):
        p = repo.create_playlist("Test")
        repo.update_playlist(p.id, cover_image_path="/old.jpg")
        updated = repo.update_playlist(p.id, cover_image_path=None)
        assert updated is not None
        assert updated.cover_image_path is None


class TestDeletePlaylist:
    def test_existing(self, repo):
        p = repo.create_playlist("Test")
        assert repo.delete_playlist(p.id) is True
        assert repo.get_playlist(p.id) is None

    def test_non_existent(self, repo):
        assert repo.delete_playlist("nonexistent") is False


class TestAddTracks:
    def test_append(self, repo):
        p = repo.create_playlist("Test")
        tracks = repo.add_tracks(p.id, [
            {"track_name": "T1", "artist_name": "A1", "album_name": "AL1", "source_type": "local"},
            {"track_name": "T2", "artist_name": "A2", "album_name": "AL2", "source_type": "jellyfin"},
        ])
        assert len(tracks) == 2
        assert tracks[0].position == 0
        assert tracks[1].position == 1

    def test_append_to_existing(self, repo):
        p = repo.create_playlist("Test")
        repo.add_tracks(p.id, [
            {"track_name": "T1", "artist_name": "A1", "album_name": "AL1", "source_type": "local"},
        ])
        new_tracks = repo.add_tracks(p.id, [
            {"track_name": "T2", "artist_name": "A2", "album_name": "AL2", "source_type": "local"},
        ])
        assert new_tracks[0].position == 1

    def test_insert_at_position(self, repo):
        p = repo.create_playlist("Test")
        repo.add_tracks(p.id, [
            {"track_name": "T1", "artist_name": "A1", "album_name": "AL1", "source_type": "local"},
            {"track_name": "T2", "artist_name": "A2", "album_name": "AL2", "source_type": "local"},
        ])
        inserted = repo.add_tracks(p.id, [
            {"track_name": "T_INS", "artist_name": "A", "album_name": "AL", "source_type": "local"},
        ], position=1)
        assert inserted[0].position == 1

        all_tracks = repo.get_tracks(p.id)
        assert all_tracks[0].track_name == "T1"
        assert all_tracks[1].track_name == "T_INS"
        assert all_tracks[2].track_name == "T2"
        assert [t.position for t in all_tracks] == [0, 1, 2]

    def test_insert_at_position_zero(self, repo):
        p = repo.create_playlist("Test")
        repo.add_tracks(p.id, [
            {"track_name": "T1", "artist_name": "A1", "album_name": "AL1", "source_type": "local"},
            {"track_name": "T2", "artist_name": "A2", "album_name": "AL2", "source_type": "local"},
        ])
        inserted = repo.add_tracks(p.id, [
            {"track_name": "T_FIRST", "artist_name": "A", "album_name": "AL", "source_type": "local"},
        ], position=0)
        assert inserted[0].position == 0

        all_tracks = repo.get_tracks(p.id)
        assert all_tracks[0].track_name == "T_FIRST"
        assert all_tracks[1].track_name == "T1"
        assert all_tracks[2].track_name == "T2"
        assert [t.position for t in all_tracks] == [0, 1, 2]

    def test_empty_list(self, repo):
        p = repo.create_playlist("Test")
        assert repo.add_tracks(p.id, []) == []

    def test_available_sources_roundtrip(self, repo):
        p = repo.create_playlist("Test")
        tracks = repo.add_tracks(p.id, [
            {"track_name": "T1", "artist_name": "A1", "album_name": "AL1",
             "source_type": "local", "available_sources": ["local", "jellyfin"]},
        ])
        assert tracks[0].available_sources == ["local", "jellyfin"]

        fetched = repo.get_tracks(p.id)
        assert fetched[0].available_sources == ["local", "jellyfin"]

    def test_disc_number_roundtrip(self, repo):
        p = repo.create_playlist("Test")
        tracks = repo.add_tracks(p.id, [
            {"track_name": "T1", "artist_name": "A1", "album_name": "AL1",
             "source_type": "local", "track_number": 1, "disc_number": 2},
        ])
        assert tracks[0].disc_number == 2

        fetched = repo.get_tracks(p.id)
        assert fetched[0].disc_number == 2


class TestRemoveTrack:
    def test_remove_and_recompact(self, repo):
        p = repo.create_playlist("Test")
        tracks = repo.add_tracks(p.id, [
            {"track_name": f"T{i}", "artist_name": "A", "album_name": "AL", "source_type": "local"}
            for i in range(3)
        ])
        assert repo.remove_track(p.id, tracks[1].id) is True

        remaining = repo.get_tracks(p.id)
        assert len(remaining) == 2
        assert [t.position for t in remaining] == [0, 1]
        assert remaining[0].track_name == "T0"
        assert remaining[1].track_name == "T2"

    def test_non_existent(self, repo):
        p = repo.create_playlist("Test")
        assert repo.remove_track(p.id, "nonexistent") is False


class TestReorderTrack:
    def test_move_forward(self, repo):
        p = repo.create_playlist("Test")
        tracks = repo.add_tracks(p.id, [
            {"track_name": f"T{i}", "artist_name": "A", "album_name": "AL", "source_type": "local"}
            for i in range(4)
        ])
        assert repo.reorder_track(p.id, tracks[0].id, 2) == 2

        result = repo.get_tracks(p.id)
        names = [t.track_name for t in result]
        assert names == ["T1", "T2", "T0", "T3"]
        assert [t.position for t in result] == [0, 1, 2, 3]

    def test_move_backward(self, repo):
        p = repo.create_playlist("Test")
        tracks = repo.add_tracks(p.id, [
            {"track_name": f"T{i}", "artist_name": "A", "album_name": "AL", "source_type": "local"}
            for i in range(4)
        ])
        assert repo.reorder_track(p.id, tracks[3].id, 1) == 1

        result = repo.get_tracks(p.id)
        names = [t.track_name for t in result]
        assert names == ["T0", "T3", "T1", "T2"]

    def test_same_position(self, repo):
        p = repo.create_playlist("Test")
        tracks = repo.add_tracks(p.id, [
            {"track_name": "T0", "artist_name": "A", "album_name": "AL", "source_type": "local"},
        ])
        assert repo.reorder_track(p.id, tracks[0].id, 0) == 0

    def test_move_to_end(self, repo):
        p = repo.create_playlist("Test")
        tracks = repo.add_tracks(p.id, [
            {"track_name": f"T{i}", "artist_name": "A", "album_name": "AL", "source_type": "local"}
            for i in range(3)
        ])
        assert repo.reorder_track(p.id, tracks[0].id, 2) == 2

        result = repo.get_tracks(p.id)
        names = [t.track_name for t in result]
        assert names == ["T1", "T2", "T0"]

    def test_non_existent(self, repo):
        p = repo.create_playlist("Test")
        assert repo.reorder_track(p.id, "nonexistent", 0) is None

    def test_clamps_out_of_range(self, repo):
        p = repo.create_playlist("Test")
        tracks = repo.add_tracks(p.id, [
            {"track_name": f"T{i}", "artist_name": "A", "album_name": "AL", "source_type": "local"}
            for i in range(3)
        ])
        actual = repo.reorder_track(p.id, tracks[0].id, 9999)
        assert actual == 2
        result = repo.get_tracks(p.id)
        assert [t.track_name for t in result] == ["T1", "T2", "T0"]


class TestUpdateTrackSource:
    def test_update_source_type(self, repo):
        p = repo.create_playlist("Test")
        tracks = repo.add_tracks(p.id, [
            {"track_name": "T1", "artist_name": "A1", "album_name": "AL1", "source_type": "local"},
        ])
        result = repo.update_track_source(p.id, tracks[0].id, source_type="jellyfin")
        assert result is not None
        assert result.source_type == "jellyfin"

    def test_update_available_sources(self, repo):
        p = repo.create_playlist("Test")
        tracks = repo.add_tracks(p.id, [
            {"track_name": "T1", "artist_name": "A1", "album_name": "AL1", "source_type": "local"},
        ])
        result = repo.update_track_source(
            p.id, tracks[0].id, available_sources=["local", "jellyfin"],
        )
        assert result is not None
        assert result.available_sources == ["local", "jellyfin"]

    def test_non_existent(self, repo):
        p = repo.create_playlist("Test")
        assert repo.update_track_source(p.id, "nonexistent") is None


class TestBatchLinkLibraryFiles:
    def test_links_only_null_rows(self, repo):
        p = repo.create_playlist("Test")
        tracks = repo.add_tracks(p.id, [
            {"track_name": "T1", "artist_name": "A", "album_name": "AL", "source_type": ""},
            {"track_name": "T2", "artist_name": "A", "album_name": "AL",
             "source_type": "local", "library_file_id": "keep-me"},
        ])
        updated = repo.batch_link_library_files(
            p.id, {tracks[0].id: "file-1", tracks[1].id: "file-2"},
        )
        assert updated == 1  # linked row is left alone
        by_id = {t.id: t for t in repo.get_tracks(p.id)}
        assert by_id[tracks[0].id].library_file_id == "file-1"
        assert by_id[tracks[1].id].library_file_id == "keep-me"

    def test_empty_updates_noop(self, repo):
        p = repo.create_playlist("Test")
        assert repo.batch_link_library_files(p.id, {}) == 0


class TestGetStreamableCounts:
    def test_counts_linked_entries_only(self, repo):
        p = repo.create_playlist("Test")
        repo.add_tracks(p.id, [
            {"track_name": "T1", "artist_name": "A", "album_name": "AL",
             "source_type": "local", "library_file_id": "f1", "duration": 100},
            {"track_name": "T2", "artist_name": "A", "album_name": "AL",
             "source_type": "local", "library_file_id": "f2", "duration": None},
            {"track_name": "T3", "artist_name": "A", "album_name": "AL",
             "source_type": ""},
        ])
        empty = repo.create_playlist("Empty")
        counts = repo.get_streamable_counts()
        assert counts[p.id] == (2, 100)  # NULL duration counts as 0
        assert empty.id not in counts


class TestGetTracks:
    def test_ordered(self, repo):
        p = repo.create_playlist("Test")
        repo.add_tracks(p.id, [
            {"track_name": f"T{i}", "artist_name": "A", "album_name": "AL", "source_type": "local"}
            for i in range(3)
        ])
        tracks = repo.get_tracks(p.id)
        assert [t.position for t in tracks] == [0, 1, 2]

    def test_empty_playlist(self, repo):
        p = repo.create_playlist("Test")
        assert repo.get_tracks(p.id) == []

    def test_non_existent_playlist(self, repo):
        assert repo.get_tracks("nonexistent") == []


class TestConcurrency:
    def test_concurrent_writes(self, repo):
        p = repo.create_playlist("Test")
        errors: list[Exception] = []

        def add_track(idx: int):
            try:
                repo.add_tracks(p.id, [
                    {"track_name": f"T{idx}", "artist_name": "A", "album_name": "AL", "source_type": "local"},
                ])
            except Exception as e:  # noqa: BLE001
                errors.append(e)

        threads = [threading.Thread(target=add_track, args=(i,)) for i in range(10)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()

        assert errors == []
        tracks = repo.get_tracks(p.id)
        assert len(tracks) == 10


class TestCheckTrackMembership:
    def test_empty_tracks(self, repo):
        result = repo.check_track_membership([])
        assert result == {}

    def test_no_playlists(self, repo):
        result = repo.check_track_membership([("Song", "Artist", "Album")])
        assert result == {}

    def test_full_overlap(self, repo):
        p = repo.create_playlist("Test")
        repo.add_tracks(p.id, [
            {"track_name": "Song A", "artist_name": "Artist X", "album_name": "Album 1", "source_type": "local"},
            {"track_name": "Song B", "artist_name": "Artist Y", "album_name": "Album 2", "source_type": "local"},
        ])
        result = repo.check_track_membership([
            ("Song A", "Artist X", "Album 1"),
            ("Song B", "Artist Y", "Album 2"),
        ])
        assert result == {p.id: [0, 1]}

    def test_partial_overlap(self, repo):
        p = repo.create_playlist("Test")
        repo.add_tracks(p.id, [
            {"track_name": "Song A", "artist_name": "Artist X", "album_name": "Album 1", "source_type": "local"},
        ])
        result = repo.check_track_membership([
            ("Song A", "Artist X", "Album 1"),
            ("Song C", "Artist Z", "Album 3"),
        ])
        assert result == {p.id: [0]}

    def test_no_overlap(self, repo):
        p = repo.create_playlist("Test")
        repo.add_tracks(p.id, [
            {"track_name": "Song A", "artist_name": "Artist X", "album_name": "Album 1", "source_type": "local"},
        ])
        result = repo.check_track_membership([
            ("Different", "Other", "Nope"),
        ])
        assert result == {}

    def test_case_insensitive(self, repo):
        p = repo.create_playlist("Test")
        repo.add_tracks(p.id, [
            {"track_name": "Hello World", "artist_name": "ARTIST", "album_name": "Album", "source_type": "local"},
        ])
        result = repo.check_track_membership([
            ("hello world", "artist", "album"),
        ])
        assert result == {p.id: [0]}

    def test_multiple_playlists(self, repo):
        p1 = repo.create_playlist("One")
        p2 = repo.create_playlist("Two")
        repo.add_tracks(p1.id, [
            {"track_name": "Song A", "artist_name": "Art", "album_name": "Alb", "source_type": "local"},
        ])
        repo.add_tracks(p2.id, [
            {"track_name": "Song A", "artist_name": "Art", "album_name": "Alb", "source_type": "local"},
            {"track_name": "Song B", "artist_name": "Art2", "album_name": "Alb2", "source_type": "local"},
        ])
        result = repo.check_track_membership([
            ("Song A", "Art", "Alb"),
            ("Song B", "Art2", "Alb2"),
        ])
        assert sorted(result[p1.id]) == [0]
        assert sorted(result[p2.id]) == [0, 1]
