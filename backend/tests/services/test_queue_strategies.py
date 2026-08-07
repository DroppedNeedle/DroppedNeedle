"""Unit tests for queue_strategies - pure functions extracted from DiscoverQueueService."""

import random
from unittest.mock import AsyncMock, MagicMock

import pytest

from api.v1.schemas.discover import DiscoverQueueItemLight
from repositories.listenbrainz_models import (
    ListenBrainzArtist,
    ListenBrainzReleaseGroup,
    ListenBrainzSimilarArtist,
)
from services.discover.queue_strategies import (
    VARIOUS_ARTISTS_MBID,
    build_similar_artist_pools,
    discover_by_genres,
    get_artist_deep_cuts,
    get_trending_filler,
    interleave_at_positions,
    round_robin_dedup_select,
)


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _item(rg_mbid: str, artist_mbid: str = "artist-1") -> DiscoverQueueItemLight:
    return DiscoverQueueItemLight(
        release_group_mbid=rg_mbid,
        album_name="Album",
        artist_name="Artist",
        artist_mbid=artist_mbid,
        recommendation_reason="test",
        cover_url="",
        is_wildcard=False,
        in_library=False,
    )


def _make_mbid_svc() -> MagicMock:
    """Return a lightweight MbidResolutionService fake."""
    svc = MagicMock()
    svc.normalize_mbid = lambda x: x.strip().lower() if x else None
    svc.make_queue_item = lambda **kw: DiscoverQueueItemLight(
        release_group_mbid=kw["release_group_mbid"],
        album_name=kw["album_name"],
        artist_name=kw["artist_name"],
        artist_mbid=kw["artist_mbid"],
        recommendation_reason=kw["reason"],
        cover_url=f"/api/v1/covers/release-group/{kw['release_group_mbid']}?size=500",
        is_wildcard=kw.get("is_wildcard", False),
        in_library=False,
    )
    return svc


# ===================================================================
# round_robin_dedup_select
# ===================================================================


class TestRoundRobinDedupSelect:
    def test_respects_count_cap(self) -> None:
        pools = [
            [_item(f"rg-{i}", f"a-{i}") for i in range(5)],
            [_item(f"rg-{i+10}", f"a-{i+10}") for i in range(5)],
            [_item(f"rg-{i+20}", f"a-{i+20}") for i in range(5)],
        ]
        random.seed(42)
        result = round_robin_dedup_select(pools, 4)
        assert len(result) == 4

    def test_dedup_by_mbid(self) -> None:
        pools = [
            [_item("dup-rg", "a-1")],
            [_item("dup-rg", "a-2")],
        ]
        random.seed(42)
        result = round_robin_dedup_select(pools, 10)
        assert len(result) == 1

    def test_max_per_artist(self) -> None:
        pool = [_item(f"rg-{i}", "same-artist") for i in range(5)]
        random.seed(42)
        result = round_robin_dedup_select([pool], 10)
        assert len(result) == 2

    def test_empty_pools(self) -> None:
        result = round_robin_dedup_select([], 5)
        assert result == []

    def test_round_robin_balance(self) -> None:
        pool_a = [_item(f"a-rg-{i}", f"art-a-{i}") for i in range(3)]
        pool_b = [_item(f"b-rg-{i}", f"art-b-{i}") for i in range(3)]
        random.seed(42)
        result = round_robin_dedup_select([pool_a, pool_b], 4)
        assert len(result) == 4
        pool_a_ids = {it.release_group_mbid for it in pool_a}
        pool_b_ids = {it.release_group_mbid for it in pool_b}
        from_a = sum(1 for it in result if it.release_group_mbid in pool_a_ids)
        from_b = sum(1 for it in result if it.release_group_mbid in pool_b_ids)
        assert from_a >= 1
        assert from_b >= 1


# ===================================================================
# interleave_at_positions
# ===================================================================


class TestInterleaveAtPositions:
    def test_inserts_at_correct_indices(self) -> None:
        base = [_item(f"base-{i}") for i in range(10)]
        ins = [_item("ins-0", "w"), _item("ins-1", "w")]
        result = interleave_at_positions(base, ins)
        assert result[2].release_group_mbid == "ins-0"
        assert result[7].release_group_mbid == "ins-1"
        assert len(result) == 12

    def test_empty_insertions(self) -> None:
        base = [_item(f"b-{i}") for i in range(5)]
        result = interleave_at_positions(base, [])
        assert len(result) == 5

    def test_more_insertions_than_positions(self) -> None:
        base = [_item(f"b-{i}") for i in range(10)]
        ins = [_item("i-0", "w"), _item("i-1", "w"), _item("i-2", "w")]
        result = interleave_at_positions(base, ins)
        assert len(result) == 13
        assert result[-1].release_group_mbid == "i-2"

    def test_short_base(self) -> None:
        base = [_item("only")]
        ins = [_item("i-0", "w"), _item("i-1", "w")]
        result = interleave_at_positions(base, ins)
        assert len(result) == 3

    def test_custom_positions(self) -> None:
        base = [_item(f"b-{i}") for i in range(10)]
        ins = [_item("i-0", "w"), _item("i-1", "w")]
        result = interleave_at_positions(base, ins, positions=[0, 5])
        assert result[0].release_group_mbid == "i-0"
        assert result[5].release_group_mbid == "i-1"


# ===================================================================
# build_similar_artist_pools
# ===================================================================


class TestBuildSimilarArtistPools:
    @pytest.mark.asyncio
    async def test_returns_one_pool_per_seed(self) -> None:
        lb_repo = AsyncMock()
        lb_repo.get_similar_artists.return_value = [
            ListenBrainzSimilarArtist(artist_mbid="sim-1", artist_name="Sim1", listen_count=100),
        ]
        lb_repo.get_artist_top_release_groups.return_value = [
            ListenBrainzReleaseGroup(
                release_group_name="Album1", artist_name="Sim1",
                listen_count=50, release_group_mbid="rg-1",
            ),
        ]
        mbid_svc = _make_mbid_svc()
        seeds = [
            ListenBrainzArtist(artist_name="Seed1", listen_count=200, artist_mbids=["seed-1"]),
            ListenBrainzArtist(artist_name="Seed2", listen_count=150, artist_mbids=["seed-2"]),
        ]
        pools = await build_similar_artist_pools(
            seeds, set(), 5, 3, lb_repo=lb_repo, mbid_svc=mbid_svc,
        )
        assert len(pools) == 2
        assert all(len(p) > 0 for p in pools)

    @pytest.mark.asyncio
    async def test_skips_various_artists(self) -> None:
        lb_repo = AsyncMock()
        lb_repo.get_similar_artists.return_value = [
            ListenBrainzSimilarArtist(
                artist_mbid=VARIOUS_ARTISTS_MBID, artist_name="VA", listen_count=100
            ),
        ]
        mbid_svc = _make_mbid_svc()
        seeds = [ListenBrainzArtist(artist_name="S", listen_count=1, artist_mbids=["s1"])]
        pools = await build_similar_artist_pools(
            seeds, set(), 5, 3, lb_repo=lb_repo, mbid_svc=mbid_svc,
        )
        assert pools == [[]]

    @pytest.mark.asyncio
    async def test_dedup_within_pool(self) -> None:
        lb_repo = AsyncMock()
        lb_repo.get_similar_artists.return_value = [
            ListenBrainzSimilarArtist(artist_mbid="sim-a", artist_name="SimA", listen_count=100),
            ListenBrainzSimilarArtist(artist_mbid="sim-b", artist_name="SimB", listen_count=80),
        ]
        lb_repo.get_artist_top_release_groups.return_value = [
            ListenBrainzReleaseGroup(
                release_group_name="Same Album", artist_name="SimA",
                listen_count=50, release_group_mbid="dup-rg",
            ),
        ]
        mbid_svc = _make_mbid_svc()
        seeds = [ListenBrainzArtist(artist_name="S", listen_count=1, artist_mbids=["s1"])]
        pools = await build_similar_artist_pools(
            seeds, set(), 5, 3, lb_repo=lb_repo, mbid_svc=mbid_svc,
        )
        assert len(pools[0]) == 1

    @pytest.mark.asyncio
    async def test_excludes_excluded_mbids(self) -> None:
        lb_repo = AsyncMock()
        lb_repo.get_similar_artists.return_value = [
            ListenBrainzSimilarArtist(artist_mbid="sim-1", artist_name="Sim1", listen_count=100),
        ]
        lb_repo.get_artist_top_release_groups.return_value = [
            ListenBrainzReleaseGroup(
                release_group_name="Excluded", artist_name="Sim1",
                listen_count=50, release_group_mbid="excluded-rg",
            ),
        ]
        mbid_svc = _make_mbid_svc()
        seeds = [ListenBrainzArtist(artist_name="S", listen_count=1, artist_mbids=["s1"])]
        pools = await build_similar_artist_pools(
            seeds, {"excluded-rg"}, 5, 3, lb_repo=lb_repo, mbid_svc=mbid_svc,
        )
        assert pools[0] == []

    @pytest.mark.asyncio
    async def test_seed_without_mbid(self) -> None:
        lb_repo = AsyncMock()
        mbid_svc = _make_mbid_svc()
        seeds = [ListenBrainzArtist(artist_name="NoMBID", listen_count=1, artist_mbids=[])]
        pools = await build_similar_artist_pools(
            seeds, set(), 5, 3, lb_repo=lb_repo, mbid_svc=mbid_svc,
        )
        assert pools == [[]]
        lb_repo.get_similar_artists.assert_not_called()


# ===================================================================
# discover_by_genres
# ===================================================================


class TestDiscoverByGenres:
    @pytest.mark.asyncio
    async def test_returns_items_from_tag_search(self) -> None:
        release = MagicMock()
        release.musicbrainz_id = "rg-rock-1"
        release.title = "Rock Album"
        release.artist = "Rock Artist"
        mb_repo = AsyncMock()
        mb_repo.search_release_groups_by_tag.return_value = [release]
        mbid_svc = _make_mbid_svc()

        result = await discover_by_genres(
            ["rock"], set(),
            mb_repo=mb_repo, mbid_svc=mbid_svc,
        )
        assert len(result) == 1
        assert result[0].release_group_mbid == "rg-rock-1"

    @pytest.mark.asyncio
    async def test_returns_empty_no_genres(self) -> None:
        mb_repo = AsyncMock()
        mbid_svc = _make_mbid_svc()

        result = await discover_by_genres(
            [], set(),
            mb_repo=mb_repo, mbid_svc=mbid_svc,
        )
        assert result == []

    @pytest.mark.asyncio
    async def test_dedup_across_genres(self) -> None:
        release = MagicMock()
        release.musicbrainz_id = "rg-shared"
        release.title = "Shared Album"
        release.artist = "Shared Artist"
        mb_repo = AsyncMock()
        mb_repo.search_release_groups_by_tag.return_value = [release]
        mbid_svc = _make_mbid_svc()

        result = await discover_by_genres(
            ["rock", "indie"], set(),
            mb_repo=mb_repo, mbid_svc=mbid_svc,
        )
        assert len(result) == 1


# ===================================================================
# get_artist_deep_cuts
# ===================================================================


class TestGetArtistDeepCuts:
    @pytest.mark.asyncio
    async def test_excludes_current_top_and_listened(self) -> None:
        lb_repo = AsyncMock()
        top_rg = ListenBrainzReleaseGroup(
            release_group_name="Top Album", artist_name="ArtistA",
            listen_count=200, release_group_mbid="top-rg",
            artist_mbids=["artist-a"],
        )
        lb_repo.get_user_top_release_groups.return_value = [top_rg]
        deep_rg = ListenBrainzReleaseGroup(
            release_group_name="Deep Cut", artist_name="ArtistA",
            listen_count=20, release_group_mbid="deep-rg",
        )
        listened_rg = ListenBrainzReleaseGroup(
            release_group_name="Listened", artist_name="ArtistA",
            listen_count=30, release_group_mbid="listened-rg",
        )
        lb_repo.get_artist_top_release_groups.return_value = [top_rg, deep_rg, listened_rg]
        mbid_svc = _make_mbid_svc()

        result = await get_artist_deep_cuts(
            "user1", set(), {"listened-rg"}, 3,
            lb_repo=lb_repo, mbid_svc=mbid_svc,
        )
        rg_ids = [it.release_group_mbid for it in result]
        assert "deep-rg" in rg_ids
        assert "top-rg" not in rg_ids
        assert "listened-rg" not in rg_ids

    @pytest.mark.asyncio
    async def test_returns_empty_no_top_release_groups(self) -> None:
        lb_repo = AsyncMock()
        lb_repo.get_user_top_release_groups.return_value = []
        mbid_svc = _make_mbid_svc()

        result = await get_artist_deep_cuts(
            "user1", set(), set(), 3,
            lb_repo=lb_repo, mbid_svc=mbid_svc,
        )
        assert result == []

    @pytest.mark.asyncio
    async def test_caps_artist_seeds_at_six(self) -> None:
        lb_repo = AsyncMock()
        top_rgs = [
            ListenBrainzReleaseGroup(
                release_group_name=f"Album{i}", artist_name=f"Artist{i}",
                listen_count=200 - i, release_group_mbid=f"rg-{i}",
                artist_mbids=[f"artist-{i}"],
            )
            for i in range(10)
        ]
        lb_repo.get_user_top_release_groups.return_value = top_rgs
        lb_repo.get_artist_top_release_groups.return_value = []
        mbid_svc = _make_mbid_svc()

        await get_artist_deep_cuts(
            "user1", set(), set(), 3,
            lb_repo=lb_repo, mbid_svc=mbid_svc,
        )
        assert lb_repo.get_artist_top_release_groups.call_count == 6


# ===================================================================
# get_trending_filler
# ===================================================================


class TestGetTrendingFiller:
    @pytest.mark.asyncio
    async def test_lb_path_returns_wildcards(self) -> None:
        lb_repo = AsyncMock()
        lb_repo.get_sitewide_top_release_groups.return_value = [
            ListenBrainzReleaseGroup(
                release_group_name="Trending", artist_name="Artist",
                listen_count=500, release_group_mbid="t-1",
                artist_mbids=["a-1"],
            ),
        ]
        mb_repo = AsyncMock()
        mbid_svc = _make_mbid_svc()

        result = await get_trending_filler(
            5, set(), set(), None, "listenbrainz",
            lb_repo=lb_repo, mb_repo=mb_repo, mbid_svc=mbid_svc,
        )
        assert len(result) >= 1
        assert result[0].is_wildcard is True

    @pytest.mark.asyncio
    async def test_respects_count_cap(self) -> None:
        lb_repo = AsyncMock()
        lb_repo.get_sitewide_top_release_groups.return_value = [
            ListenBrainzReleaseGroup(
                release_group_name=f"T{i}", artist_name=f"A{i}",
                listen_count=100, release_group_mbid=f"t-{i}",
                artist_mbids=[f"a-{i}"],
            )
            for i in range(20)
        ]
        mb_repo = AsyncMock()
        mbid_svc = _make_mbid_svc()

        result = await get_trending_filler(
            3, set(), set(), None, "listenbrainz",
            lb_repo=lb_repo, mb_repo=mb_repo, mbid_svc=mbid_svc,
        )
        assert len(result) <= 3

    @pytest.mark.asyncio
    async def test_excludes_ignored_library_seen(self) -> None:
        lb_repo = AsyncMock()
        lb_repo.get_sitewide_top_release_groups.return_value = [
            ListenBrainzReleaseGroup(
                release_group_name="Ignored", artist_name="A",
                listen_count=100, release_group_mbid="ignored-1",
                artist_mbids=["a-1"],
            ),
            ListenBrainzReleaseGroup(
                release_group_name="InLib", artist_name="A",
                listen_count=100, release_group_mbid="lib-1",
                artist_mbids=["a-2"],
            ),
            ListenBrainzReleaseGroup(
                release_group_name="Seen", artist_name="A",
                listen_count=100, release_group_mbid="seen-1",
                artist_mbids=["a-3"],
            ),
            ListenBrainzReleaseGroup(
                release_group_name="OK", artist_name="A",
                listen_count=100, release_group_mbid="ok-1",
                artist_mbids=["a-4"],
            ),
        ]
        mb_repo = AsyncMock()
        mbid_svc = _make_mbid_svc()

        result = await get_trending_filler(
            10, {"ignored-1"}, {"lib-1"}, {"seen-1"}, "listenbrainz",
            lb_repo=lb_repo, mb_repo=mb_repo, mbid_svc=mbid_svc,
        )
        rg_ids = {it.release_group_mbid for it in result}
        assert "ignored-1" not in rg_ids
        assert "lib-1" not in rg_ids
        assert "seen-1" not in rg_ids
        assert "ok-1" in rg_ids

    @pytest.mark.asyncio
    async def test_dedup_repeated_release_group(self) -> None:
        dupe = ListenBrainzReleaseGroup(
            release_group_name="Repeated", artist_name="Artist",
            listen_count=500, release_group_mbid="dupe-1",
            artist_mbids=["a-1"],
        )
        lb_repo = AsyncMock()
        lb_repo.get_sitewide_top_release_groups.return_value = [dupe, dupe]
        mb_repo = AsyncMock()
        mbid_svc = _make_mbid_svc()

        result = await get_trending_filler(
            5, set(), set(), None, "listenbrainz",
            lb_repo=lb_repo, mb_repo=mb_repo, mbid_svc=mbid_svc,
        )

        keys = [it.release_group_mbid for it in result]
        assert keys == ["dupe-1"]

    @pytest.mark.asyncio
    async def test_returns_empty_when_count_zero(self) -> None:
        lb_repo = AsyncMock()
        mb_repo = AsyncMock()
        mbid_svc = _make_mbid_svc()

        result = await get_trending_filler(
            0, set(), set(), None, "listenbrainz",
            lb_repo=lb_repo, mb_repo=mb_repo, mbid_svc=mbid_svc,
        )
        assert result == []

    @pytest.mark.asyncio
    async def test_skips_various_artists(self) -> None:
        lb_repo = AsyncMock()
        lb_repo.get_sitewide_top_release_groups.return_value = [
            ListenBrainzReleaseGroup(
                release_group_name="VA Album", artist_name="Various",
                listen_count=100, release_group_mbid="va-rg",
                artist_mbids=[VARIOUS_ARTISTS_MBID],
            ),
        ]
        mb_repo = AsyncMock()
        mbid_svc = _make_mbid_svc()

        result = await get_trending_filler(
            5, set(), set(), None, "listenbrainz",
            lb_repo=lb_repo, mb_repo=mb_repo, mbid_svc=mbid_svc,
        )
        assert all(it.release_group_mbid != "va-rg" for it in result)

    @pytest.mark.asyncio
    async def test_lastfm_happy_path(self) -> None:
        """Last.fm source returns trending items when enabled and lfm_repo present."""
        lb_repo = AsyncMock()
        mb_repo = AsyncMock()
        lfm_repo = AsyncMock()

        artist = MagicMock()
        artist.name = "PopStar"
        artist.mbid = "pop-artist-1"
        lfm_repo.get_global_top_artists.return_value = [artist]

        album = MagicMock()
        album.name = "Hit Album"
        album.mbid = "lfm-album-1"
        lfm_repo.get_artist_top_albums.return_value = [album]

        mbid_svc = _make_mbid_svc()
        mbid_svc.lastfm_albums_to_queue_items = AsyncMock(return_value=[
            _item("lfm-rg-1", "pop-artist-1"),
        ])

        result = await get_trending_filler(
            5, set(), set(), None, "lastfm",
            lb_repo=lb_repo, mb_repo=mb_repo, mbid_svc=mbid_svc,
            lfm_repo=lfm_repo, is_lastfm_enabled=True,
        )
        assert len(result) >= 1
        assert result[0].release_group_mbid == "lfm-rg-1"
        lfm_repo.get_global_top_artists.assert_awaited_once()

    @pytest.mark.asyncio
    async def test_lastfm_fallback_decade_tags(self) -> None:
        """When Last.fm primary wildcard fetch returns empty, decade-tag fallback is used."""
        lb_repo = AsyncMock()
        mb_repo = AsyncMock()
        lfm_repo = AsyncMock()

        artist = MagicMock()
        artist.name = "Nobody"
        artist.mbid = "nobody-1"
        lfm_repo.get_global_top_artists.return_value = [artist]
        lfm_repo.get_artist_top_albums.return_value = []

        mbid_svc = _make_mbid_svc()
        mbid_svc.lastfm_albums_to_queue_items = AsyncMock(return_value=[])

        decade_release = MagicMock()
        decade_release.musicbrainz_id = "decade-rg-1"
        decade_release.title = "Retro Hit"
        decade_release.artist = "Retro Artist"
        mb_repo.search_release_groups_by_tag.return_value = [decade_release]

        result = await get_trending_filler(
            3, set(), set(), None, "lastfm",
            lb_repo=lb_repo, mb_repo=mb_repo, mbid_svc=mbid_svc,
            lfm_repo=lfm_repo, is_lastfm_enabled=True,
        )
        assert len(result) >= 1
        assert result[0].release_group_mbid == "decade-rg-1"
        mb_repo.search_release_groups_by_tag.assert_called()

    @pytest.mark.asyncio
    async def test_lastfm_returns_empty_gracefully_when_disabled(self) -> None:
        """When source is lastfm but is_lastfm_enabled is False, falls back to LB path."""
        lb_repo = AsyncMock()
        lb_repo.get_sitewide_top_release_groups.return_value = []
        mb_repo = AsyncMock()
        # The decade tag-search fallback now runs on every path when the primary tier is
        # empty; with no tag results it must still degrade to [].
        mb_repo.search_release_groups_by_tag.return_value = []
        mbid_svc = _make_mbid_svc()

        result = await get_trending_filler(
            3, set(), set(), None, "lastfm",
            lb_repo=lb_repo, mb_repo=mb_repo, mbid_svc=mbid_svc,
            lfm_repo=None, is_lastfm_enabled=False,
        )
        assert result == []

    @pytest.mark.asyncio
    async def test_decade_fallback_runs_without_lastfm(self) -> None:
        """LB-degraded user with Last.fm NOT linked still gets decade-tag wildcards."""
        lb_repo = AsyncMock()
        lb_repo.get_sitewide_top_release_groups.return_value = []
        mb_repo = AsyncMock()
        mbid_svc = _make_mbid_svc()

        decade_release = MagicMock()
        decade_release.musicbrainz_id = "decade-rg-9"
        decade_release.title = "Old Hit"
        decade_release.artist = "Old Artist"
        mb_repo.search_release_groups_by_tag.return_value = [decade_release]

        result = await get_trending_filler(
            3, set(), set(), None, "listenbrainz",
            lb_repo=lb_repo, mb_repo=mb_repo, mbid_svc=mbid_svc,
            lfm_repo=None, is_lastfm_enabled=False,
        )
        assert len(result) >= 1
        assert result[0].release_group_mbid == "decade-rg-9"
        mb_repo.search_release_groups_by_tag.assert_called()


# ===================================================================
# Non-mutation assertions (pure-function contract)
# ===================================================================


class TestNonMutationContract:
    def test_round_robin_dedup_select_does_not_mutate_input(self) -> None:
        pool_a = [_item(f"a-{i}", f"art-a-{i}") for i in range(5)]
        pool_b = [_item(f"b-{i}", f"art-b-{i}") for i in range(5)]
        original_a = list(pool_a)
        original_b = list(pool_b)

        round_robin_dedup_select([pool_a, pool_b], 4)

        assert pool_a == original_a, "round_robin_dedup_select mutated pool_a"
        assert pool_b == original_b, "round_robin_dedup_select mutated pool_b"

    def test_interleave_at_positions_does_not_mutate_input(self) -> None:
        base = [_item(f"base-{i}") for i in range(5)]
        insertions = [_item("ins-0", "w"), _item("ins-1", "w")]
        original_base = list(base)
        original_ins = list(insertions)

        interleave_at_positions(base, insertions)

        assert base == original_base, "interleave_at_positions mutated base"
        assert insertions == original_ins, "interleave_at_positions mutated insertions"
