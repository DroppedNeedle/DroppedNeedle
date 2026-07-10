"""TasteGraphService — seeds weighted from the user's own signals only,
expanded through MusicBrainz relations/labels/tags, with novelty + diversity."""

import pytest
from unittest.mock import AsyncMock

from infrastructure.cache.memory_cache import InMemoryCache
from infrastructure.persistence.follow_store import FollowedArtist
from infrastructure.persistence.play_history_store import PlayHistoryRecord
from models.search import SearchResult
from services.taste_graph_service import TasteGraphService

_SEED_A = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa"
_SEED_B = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"
_FOLLOWED = "ffffffff-ffff-ffff-ffff-ffffffffffff"


def _followed(mbid: str, name: str) -> FollowedArtist:
    return FollowedArtist(
        artist_mbid=mbid,
        artist_name=name,
        auto_download=False,
        auto_download_state="none",
        followed_at=1000.0,
    )


def _play(artist_name: str, i: int) -> PlayHistoryRecord:
    return PlayHistoryRecord(
        id=f"play-{artist_name}-{i}",
        user_id="u1",
        track_name=f"Track {i}",
        artist_name=artist_name,
        played_at=f"2026-07-0{(i % 9) + 1}T00:00:00Z",
    )


def _library_db(artists=(), albums_for_matching=(), album_mbids=()):
    db = AsyncMock()
    db.get_artists = AsyncMock(return_value=list(artists))
    db.get_all_albums_for_matching = AsyncMock(return_value=list(albums_for_matching))
    db.get_all_album_mbids = AsyncMock(return_value=set(album_mbids))
    return db


def _mb_repo(expansions=None, label_releases=None, tag_results=None):
    repo = AsyncMock()
    repo.get_artist_expansion = AsyncMock(
        side_effect=lambda mbid: (expansions or {}).get(mbid)
    )
    repo.get_label_releases = AsyncMock(
        side_effect=lambda label_mbid, limit=25: (label_releases or {}).get(label_mbid, [])
    )
    repo.search_release_groups_by_tag = AsyncMock(
        side_effect=lambda tag, limit=15: (tag_results or {}).get(tag, [])
    )
    return repo


def _service(library_db=None, followed=(), plays=(), mb_repo=None):
    follow_store = AsyncMock()
    follow_store.list_followed_artists = AsyncMock(return_value=list(followed))
    play_history = AsyncMock()
    play_history.recent = AsyncMock(return_value=list(plays))
    return TasteGraphService(
        library_db=library_db or _library_db(),
        follow_store=follow_store,
        play_history_store=play_history,
        mb_repo=mb_repo or _mb_repo(),
        cache=InMemoryCache(max_entries=100),
    )


class TestColdStart:
    @pytest.mark.asyncio
    async def test_empty_everything_is_graceful_cold_start(self):
        service = _service()
        result = await service.get_taste_graph("u1")
        assert result.cold_start is True
        assert result.seeds == []
        assert result.items == []
        assert result.generated_at


class TestSeedWeighting:
    @pytest.mark.asyncio
    async def test_followed_outweighs_played_outweighs_library(self):
        library_db = _library_db(
            artists=[
                {"mbid": _SEED_A, "name": "Library Only"},
                {"mbid": _SEED_B, "name": "Library Played"},
            ],
        )
        service = _service(
            library_db=library_db,
            followed=[_followed(_FOLLOWED, "Followed Artist")],
            plays=[_play("Library Played", i) for i in range(5)],
        )
        result = await service.get_taste_graph("u1")
        assert result.cold_start is False
        ordered = [s.artist_mbid for s in result.seeds]
        assert ordered == [_FOLLOWED, _SEED_B, _SEED_A]
        assert result.seeds[0].weight == 1.0  # normalized to the top seed


class TestExpansion:
    @pytest.mark.asyncio
    async def test_member_collab_label_scene_candidates(self):
        expansions = {
            _SEED_A: {
                "tags": [{"name": "shoegaze", "count": 5}],
                "relations": [
                    {
                        "type": "member of band",
                        "artist": {"id": "member-1", "name": "New Member"},
                    },
                    {
                        "type": "collaboration",
                        "artist": {"id": "collab-1", "name": "New Collaborator"},
                    },
                    {
                        # novelty: already in library, must be filtered out
                        "type": "member of band",
                        "artist": {"id": _SEED_B, "name": "Library Played"},
                    },
                ],
            },
            _SEED_B: {
                "tags": [],
                "relations": [
                    {
                        "type": "recording contract",
                        "label": {"id": "label-1", "name": "Test Label"},
                    },
                ],
            },
        }
        label_releases = {
            "label-1": [
                {
                    "title": "Label Album",
                    "release-group": {"id": "rg-label-1", "title": "Label Album"},
                    "artist-credit": [
                        {"name": "Label Mate", "artist": {"id": "mate-1", "name": "Label Mate"}}
                    ],
                }
            ]
        }
        tag_results = {
            "shoegaze": [
                SearchResult(
                    type="album",
                    title="Scene Album",
                    musicbrainz_id="rg-scene-1",
                    artist="Scene Artist",
                )
            ]
        }
        library_db = _library_db(
            artists=[
                {"mbid": _SEED_A, "name": "Seed Artist"},
                {"mbid": _SEED_B, "name": "Library Played"},
            ],
        )
        service = _service(
            library_db=library_db,
            mb_repo=_mb_repo(expansions, label_releases, tag_results),
        )
        result = await service.get_taste_graph("u1")

        by_mbid = {i.mbid: i for i in result.items}
        assert _SEED_B not in by_mbid  # novelty filter
        assert by_mbid["member-1"].kind == "artist"
        assert by_mbid["member-1"].reasons[0].type == "member"
        assert by_mbid["member-1"].reasons[0].via_mbid == _SEED_A
        assert by_mbid["collab-1"].reasons[0].type == "collaborator"
        assert by_mbid["rg-label-1"].kind == "album"
        assert by_mbid["rg-label-1"].artist_mbid == "mate-1"
        assert by_mbid["rg-label-1"].reasons[0].type == "label"
        assert by_mbid["rg-label-1"].reasons[0].via_name == "Test Label"
        assert by_mbid["rg-scene-1"].reasons[0].type == "scene"
        assert by_mbid["rg-scene-1"].reasons[0].via_name == "shoegaze"
        # member/collab outrank label-mates which outrank scene matches
        assert by_mbid["member-1"].score > by_mbid["rg-label-1"].score > by_mbid["rg-scene-1"].score
        assert all(i.in_library is False for i in result.items)
        assert len(result.items) <= 30

    @pytest.mark.asyncio
    async def test_per_seed_diversity_cap(self):
        expansions = {
            _SEED_A: {
                "tags": [],
                "relations": [
                    {"type": "member of band", "artist": {"id": f"m-{i}", "name": f"M {i}"}}
                    for i in range(6)
                ],
            },
        }
        library_db = _library_db(artists=[{"mbid": _SEED_A, "name": "Seed Artist"}])
        service = _service(library_db=library_db, mb_repo=_mb_repo(expansions))
        result = await service.get_taste_graph("u1")
        assert len(result.items) == 3  # max 3 candidates per seed

    @pytest.mark.asyncio
    async def test_graph_is_cached_per_user(self):
        library_db = _library_db(artists=[{"mbid": _SEED_A, "name": "Seed Artist"}])
        mb_repo = _mb_repo({
            _SEED_A: {
                "tags": [],
                "relations": [
                    {"type": "member of band", "artist": {"id": "m-1", "name": "M 1"}}
                ],
            }
        })
        service = _service(library_db=library_db, mb_repo=mb_repo)
        first = await service.get_taste_graph("u1")
        second = await service.get_taste_graph("u1")
        assert first == second
        mb_repo.get_artist_expansion.assert_awaited_once()

    @pytest.mark.asyncio
    async def test_mb_failure_degrades_without_500(self):
        library_db = _library_db(artists=[{"mbid": _SEED_A, "name": "Seed Artist"}])
        mb_repo = AsyncMock()
        mb_repo.get_artist_expansion = AsyncMock(side_effect=RuntimeError("mb down"))
        service = _service(library_db=library_db, mb_repo=mb_repo)
        result = await service.get_taste_graph("u1")
        assert result.cold_start is False
        assert result.items == []
        assert [s.artist_mbid for s in result.seeds] == [_SEED_A]
