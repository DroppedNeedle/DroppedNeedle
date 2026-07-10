"""Top Picks: pure scoring (services/discover/top_picks.py) and the section
builders added alongside it (listeners-like-you, anniversaries, new-from-followed)."""

import asyncio
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from api.v1.schemas.discover import TopPicksSection
from infrastructure.service_health import service_health
from services.discover.homepage_service import DiscoverHomepageService
from services.discover.top_picks import TopPickCandidate, score_candidates

_UID = "user-1"
_DATE = "2026-07-03"


def make_candidate(**overrides) -> TopPickCandidate:
    defaults = dict(
        release_group_mbid="rg-1",
        album_name="Album",
        artist_name="Artist",
        artist_mbid="artist-1",
        sim=0.0,
        listen_count=0,
        seed_artist=None,
        from_trending=False,
    )
    defaults.update(overrides)
    return TopPickCandidate(**defaults)


class TestScoreCandidates:
    def test_match_pct_bounds(self):
        low = make_candidate(release_group_mbid="rg-low")
        high = make_candidate(
            release_group_mbid="rg-high",
            artist_mbid="artist-2",
            sim=1.0,
            listen_count=10_000_000,
            seed_artist="Seed",
        )
        picks = score_candidates(
            [low, high],
            user_id=_UID,
            date_iso=_DATE,
            user_genres={"rock"},
            genres_by_artist={"artist-2": ["rock", "indie"]},
        )
        assert all(40 <= p.match_pct <= 98 for p in picks)
        by_mbid = {p.candidate.release_group_mbid: p for p in picks}
        assert by_mbid["rg-high"].match_pct > by_mbid["rg-low"].match_pct

    def test_deterministic_within_a_day_varies_across_days(self):
        candidates = [make_candidate(release_group_mbid=f"rg-{i}", artist_mbid=f"a-{i}") for i in range(8)]
        kwargs = dict(user_id=_UID, user_genres=set(), genres_by_artist={})
        day1a = score_candidates(list(candidates), date_iso="2026-07-03", **kwargs)
        day1b = score_candidates(list(candidates), date_iso="2026-07-03", **kwargs)
        day2 = score_candidates(list(candidates), date_iso="2026-07-04", **kwargs)
        assert [p.candidate.release_group_mbid for p in day1a] == [
            p.candidate.release_group_mbid for p in day1b
        ]
        assert [p.score for p in day1a] != [p.score for p in day2]

    def test_personalised_picks_rank_ahead_of_trending(self):
        # a high-popularity trending album (no similarity) must NOT outrank a personalised
        # similar-artist pick, even one with a modest match and no listen-count (the LB-outage
        # shape) - trending only fills the tail
        personalised = [
            make_candidate(
                release_group_mbid=f"p-{i}", artist_mbid=f"pa-{i}",
                sim=0.2, seed_artist="Seed", listen_count=0,
            )
            for i in range(3)
        ]
        trending = [
            make_candidate(
                release_group_mbid=f"t-{i}", artist_mbid=f"ta-{i}",
                sim=0.0, from_trending=True, listen_count=50_000_000,
            )
            for i in range(3)
        ]
        picks = score_candidates(
            trending + personalised,  # trending first in the input on purpose
            user_id=_UID, date_iso=_DATE, user_genres=set(), genres_by_artist={}, count=4,
        )
        # all three personalised come before any trending
        assert [p.candidate.from_trending for p in picks] == [False, False, False, True]

    def test_max_two_albums_per_artist(self):
        candidates = [
            make_candidate(release_group_mbid=f"rg-{i}", artist_mbid="same-artist", sim=0.9)
            for i in range(4)
        ] + [make_candidate(release_group_mbid="rg-other", artist_mbid="other", sim=0.5)]
        picks = score_candidates(
            candidates, user_id=_UID, date_iso=_DATE, user_genres=set(), genres_by_artist={}
        )
        artists = [p.candidate.artist_mbid for p in picks]
        assert artists.count("same-artist") == 2  # at most two per artist
        assert "other" in artists

    def test_reasons_reflect_signals(self):
        seed_pick = make_candidate(
            release_group_mbid="rg-a", artist_mbid="a-1", sim=0.8, seed_artist="Radiohead"
        )
        genre_pick = make_candidate(release_group_mbid="rg-b", artist_mbid="a-2")
        trending_pick = make_candidate(
            release_group_mbid="rg-c", artist_mbid="a-3", from_trending=True
        )
        picks = score_candidates(
            [seed_pick, genre_pick, trending_pick],
            user_id=_UID,
            date_iso=_DATE,
            user_genres={"shoegaze", "dream pop"},
            genres_by_artist={"a-2": ["shoegaze", "noise"]},
        )
        by_mbid = {p.candidate.release_group_mbid: p for p in picks}
        assert by_mbid["rg-a"].reasons[0] == "Because you listen to Radiohead"
        assert by_mbid["rg-b"].reasons == ["You love shoegaze"]
        assert by_mbid["rg-c"].reasons == ["Trending worldwide"]

    def test_count_caps_output(self):
        candidates = [
            make_candidate(release_group_mbid=f"rg-{i}", artist_mbid=f"a-{i}") for i in range(30)
        ]
        picks = score_candidates(
            candidates, user_id=_UID, date_iso=_DATE, user_genres=set(), genres_by_artist={},
            count=5,
        )
        assert len(picks) == 5

    def test_empty_input(self):
        assert score_candidates(
            [], user_id=_UID, date_iso=_DATE, user_genres=set(), genres_by_artist={}
        ) == []


def _svc() -> DiscoverHomepageService:
    svc = DiscoverHomepageService.__new__(DiscoverHomepageService)
    svc._memory_cache = None
    svc._mbid_store = None
    svc._genre_index = None
    svc._genre_prefs_store = None
    svc._library_db = None
    svc._follow_service = None
    svc._integration = MagicMock()
    svc._integration.is_library_configured.return_value = True
    svc._integration.get_discover_picks_settings.return_value = (0.7, 12)
    svc._mbid = MagicMock()
    svc._mbid.normalize_mbid = staticmethod(lambda m: m.lower() if m else None)
    svc._mbid.get_library_album_mbids = AsyncMock(return_value=set())
    svc._mbid.get_user_listened_release_group_mbids = AsyncMock(return_value=set())
    svc._lb_repo = MagicMock()
    svc._lb_repo.get_sitewide_top_release_groups = AsyncMock(return_value=[])
    svc._lb_repo.get_artist_top_release_groups = AsyncMock(return_value=[])
    return svc


def _rg(mbid: str, name: str = "Album", artist: str = "Artist", artist_mbids=None, listens=100):
    return SimpleNamespace(
        release_group_mbid=mbid,
        release_group_name=name,
        artist_name=artist,
        artist_mbids=artist_mbids or ["a-1"],
        listen_count=listens,
    )


class TestBuildTopPicks:
    @pytest.mark.asyncio
    async def test_builds_from_similar_pools_with_seed_reason(self):
        svc = _svc()
        seed = SimpleNamespace(artist_name="Radiohead", artist_mbids=["seed-1"], listen_count=10)
        results = {
            "similar_0": [
                SimpleNamespace(artist_mbid="sim-1", artist_name="The Verve", listen_count=5, score=100.0)
            ]
        }
        svc._lb_repo.get_artist_top_release_groups = AsyncMock(
            return_value=[_rg("rg-1", "Urban Hymns", "The Verve", ["sim-1"])]
        )

        section = await svc._build_top_picks(_UID, "listenbrainz", True, "u", results, [seed])

        assert isinstance(section, TopPicksSection)
        assert section.items
        assert section.personalizing is False  # healthy LB path: real personalised picks
        item = section.items[0]
        assert item.album.mbid == "rg-1"
        assert item.seed_artist == "Radiohead"
        assert 40 <= item.match_pct <= 98
        assert "Because you listen to Radiohead" in item.reasons

    @pytest.mark.asyncio
    async def test_excludes_library_listened_and_ignored(self):
        svc = _svc()
        svc._mbid.get_library_album_mbids = AsyncMock(return_value={"rg-lib"})
        svc._mbid.get_user_listened_release_group_mbids = AsyncMock(return_value={"rg-heard"})
        svc._lb_repo.get_sitewide_top_release_groups = AsyncMock(
            return_value=[
                _rg("rg-lib"),
                _rg("rg-heard", artist_mbids=["a-2"]),
                _rg("rg-new", artist_mbids=["a-3"]),
            ]
        )

        section = await svc._build_top_picks(_UID, "listenbrainz", True, "u", {}, [])

        assert section is not None
        mbids = [i.album.mbid for i in section.items]
        assert mbids == ["rg-new"]

    @pytest.mark.asyncio
    async def test_returns_none_when_no_candidates(self):
        svc = _svc()
        section = await svc._build_top_picks(_UID, "listenbrainz", True, "u", {}, [])
        assert section is None

    @pytest.mark.asyncio
    async def test_thorough_build_ignores_cached_section(self):
        # A cold on-visit build can cache a trending-only section; the warmer's thorough
        # build must REBUILD rather than return that cache, or the warm is a no-op.
        from services.discover.mbid_resolution_service import discover_build_thorough

        svc = _svc()
        stale = TopPicksSection(items=[], source="listenbrainz", personalizing=True)
        svc._memory_cache = MagicMock()
        svc._memory_cache.get = AsyncMock(return_value={"section": stale})
        svc._memory_cache.set = AsyncMock()
        svc._top_picks_cache_key = MagicMock(return_value="tp:u")
        seed = SimpleNamespace(artist_name="Radiohead", artist_mbids=["seed-1"], listen_count=10)
        results = {
            "similar_0": [
                SimpleNamespace(artist_mbid="sim-1", artist_name="The Verve", listen_count=5, score=100.0)
            ]
        }
        svc._lb_repo.get_artist_top_release_groups = AsyncMock(
            return_value=[_rg("rg-1", "Urban Hymns", "The Verve", ["sim-1"])]
        )

        # on-visit: short-circuits to the cached section
        assert await svc._build_top_picks(_UID, "listenbrainz", True, "u", results, [seed]) is stale

        # thorough: ignores the cache and rebuilds fresh
        token = discover_build_thorough.set(True)
        try:
            rebuilt = await svc._build_top_picks(_UID, "listenbrainz", True, "u", results, [seed])
        finally:
            discover_build_thorough.reset(token)
        assert rebuilt is not stale
        assert rebuilt is not None and rebuilt.items  # actually built picks

    @pytest.mark.asyncio
    async def test_populates_from_trending_when_lastfm_similarity_starves(self, monkeypatch):
        # LB popularity degraded -> Top Picks sources similarity from Last.fm, which
        # resolves via MusicBrainz (1/s) and can be starved during the outage. The
        # section must still populate from the MB-free trending pool rather than being
        # cancelled empty at the task budget.
        import services.discover.homepage_service as mod

        service_health.mark_degraded(
            "listenbrainz", "popularity", message="down", fallback="lastfm"
        )
        try:
            svc = _svc()
            svc._lfm_repo = MagicMock()  # non-None so the popularity gate uses Last.fm
            monkeypatch.setattr(mod, "TOP_PICKS_SIMILARITY_BUDGET_SECONDS", 0.05)

            async def _hang(*_a, **_k):
                await asyncio.sleep(1)  # never completes within the tiny budget

            svc._add_lastfm_top_pick_candidates = _hang
            svc._lb_repo.get_sitewide_top_release_groups = AsyncMock(
                return_value=[_rg("rg-trend", "Trend Album", "Trend Artist", ["a-trend"])]
            )

            seed = SimpleNamespace(artist_name="Radiohead", artist_mbids=["seed-1"], listen_count=10)
            results = {
                "similar_0": [
                    SimpleNamespace(artist_mbid="sim-1", artist_name="X", listen_count=5, score=100.0)
                ]
            }
            section = await svc._build_top_picks(_UID, "lastfm", True, "u", results, [seed], lfm_enabled=True)

            assert section is not None
            assert any(i.album.mbid == "rg-trend" for i in section.items)
        finally:
            service_health.clear()

    @pytest.mark.asyncio
    async def test_trending_only_result_cached_briefly_for_retry(self, monkeypatch):
        # A degraded (trending-only) build must NOT be frozen for the full 4h - it should
        # cache briefly so the next build retries as MusicBrainz warms and personalised
        # picks converge (fixes "Top Picks stuck at 48%").
        import services.discover.homepage_service as mod

        service_health.mark_degraded(
            "listenbrainz", "popularity", message="down", fallback="lastfm"
        )
        try:
            svc = _svc()
            svc._lfm_repo = MagicMock()

            async def _adds_nothing(*_a, **_k):
                return  # personalisation starved: no non-trending candidates

            svc._add_lastfm_top_pick_candidates = _adds_nothing
            svc._lb_repo.get_sitewide_top_release_groups = AsyncMock(
                return_value=[_rg("rg-trend", "Trend Album", "Trend Artist", ["a-trend"])]
            )

            set_calls: list[tuple[str, int]] = []
            cache = MagicMock()
            cache.get = AsyncMock(return_value=None)
            cache.set = AsyncMock(side_effect=lambda k, v, ttl=None: set_calls.append((k, ttl)))
            svc._memory_cache = cache

            seed = SimpleNamespace(artist_name="R", artist_mbids=["s-1"], listen_count=1)
            results = {
                "similar_0": [
                    SimpleNamespace(artist_mbid="sim-1", artist_name="X", listen_count=1, score=100.0)
                ]
            }
            section = await svc._build_top_picks(_UID, "lastfm", True, "u", results, [seed], lfm_enabled=True)

            assert section is not None  # populated from trending
            assert section.personalizing is True  # drives the "still personalising" UI hint
            picks_ttls = [ttl for (k, ttl) in set_calls if k.startswith("top_picks:")]
            assert picks_ttls
            assert all(ttl == mod.DISCOVER_PICKS_DEGRADED_TTL for ttl in picks_ttls)
        finally:
            service_health.clear()

    @pytest.mark.asyncio
    async def test_result_is_cached(self):
        svc = _svc()
        store: dict = {}
        cache = MagicMock()
        cache.get = AsyncMock(side_effect=lambda k: store.get(k))
        cache.set = AsyncMock(side_effect=lambda k, v, ttl=None: store.__setitem__(k, v))
        svc._memory_cache = cache
        svc._lb_repo.get_sitewide_top_release_groups = AsyncMock(
            return_value=[_rg("rg-new", artist_mbids=["a-3"])]
        )

        first = await svc._build_top_picks(_UID, "listenbrainz", True, "u", {}, [])
        svc._lb_repo.get_sitewide_top_release_groups = AsyncMock(
            side_effect=AssertionError("must hit the cache")
        )
        second = await svc._build_top_picks(_UID, "listenbrainz", True, "u", {}, [])

        assert first is not None
        assert second == first


class TestResolveReleaseMbids:
    @pytest.mark.asyncio
    async def test_degrades_to_empty_map_when_mb_starved(self, monkeypatch):
        # under the outage the release->RG resolution competes on MB's 1/s limit; if it
        # blows its budget it returns {} so callers fall back to raw release mbids instead
        # of the whole section timing out at the 25s task budget and vanishing.
        import services.discover.homepage_service as mod

        svc = _svc()
        svc._mb_repo = MagicMock()

        async def _hang(_rid):
            await asyncio.sleep(1)

        svc._mb_repo.get_release_group_id_from_release = _hang
        monkeypatch.setattr(mod, "DISCOVER_MB_RESOLVE_BUDGET_SECONDS", 0.05)

        result = await svc._resolve_release_mbids(["rel-1", "rel-2"])
        assert result == {}


class TestListenersLikeYou:
    @pytest.mark.asyncio
    async def test_builds_from_similar_users(self):
        svc = _svc()
        lb_client = MagicMock()
        lb_client.get_similar_users = AsyncMock(
            return_value=[{"user_name": "twin", "similarity": 0.9}]
        )
        lb_client.get_user_top_release_groups = AsyncMock(
            return_value=[_rg("rg-t", "Their Album", "Their Artist", ["a-9"])]
        )

        section = await svc._build_listeners_like_you(lb_client, "me", _UID)

        assert section is not None
        assert section.title == "Listeners Like You Are Playing"
        assert section.items[0].mbid == "rg-t"
        lb_client.get_user_top_release_groups.assert_awaited_once_with(
            username="twin", range_="this_month", count=15
        )

    @pytest.mark.asyncio
    async def test_none_when_no_similar_users(self):
        svc = _svc()
        lb_client = MagicMock()
        lb_client.get_similar_users = AsyncMock(return_value=[])
        assert await svc._build_listeners_like_you(lb_client, "me", _UID) is None

    @pytest.mark.asyncio
    async def test_failure_degrades_to_none(self):
        svc = _svc()
        lb_client = MagicMock()
        lb_client.get_similar_users = AsyncMock(side_effect=RuntimeError("lb down"))
        assert await svc._build_listeners_like_you(lb_client, "me", _UID) is None


class TestAnniversaries:
    @pytest.mark.asyncio
    async def test_round_birthdays_only_roundest_first(self):
        from datetime import datetime, timezone

        this_year = datetime.now(timezone.utc).year
        svc = _svc()
        svc._library_db = MagicMock()
        svc._library_db.get_albums = AsyncMock(
            return_value=[
                {"mbid": "rg-30", "title": "Thirty", "artist_name": "A", "year": this_year - 30},
                {"mbid": "rg-10", "title": "Ten", "artist_name": "B", "year": this_year - 10},
                {"mbid": "rg-7", "title": "Seven", "artist_name": "C", "year": this_year - 7},
                {"mbid": "rg-none", "title": "NoYear", "artist_name": "D", "year": None},
            ]
        )

        section = await svc._build_anniversaries()

        assert section is not None
        assert [i.mbid for i in section.items] == ["rg-30", "rg-10"]
        assert all(i.in_library for i in section.items)

    @pytest.mark.asyncio
    async def test_none_without_library_db_or_matches(self):
        svc = _svc()
        assert await svc._build_anniversaries() is None
        svc._library_db = MagicMock()
        svc._library_db.get_albums = AsyncMock(return_value=[])
        assert await svc._build_anniversaries() is None


class TestNewFromFollowed:
    @pytest.mark.asyncio
    async def test_builds_section_from_follow_service(self):
        svc = _svc()
        svc._follow_service = MagicMock()
        svc._follow_service.list_new_releases = AsyncMock(
            return_value=(
                [
                    SimpleNamespace(
                        release_group_mbid="rg-f",
                        title="Fresh",
                        artist_name="Followed",
                        artist_mbid="a-f",
                        primary_type="Album",
                        first_release_date="2026-06-20",
                    )
                ],
                1,
            )
        )

        section = await svc._build_new_from_followed(_UID)

        assert section is not None
        assert section.title == "New From Artists You Follow"
        assert section.items[0].mbid == "rg-f"
        svc._follow_service.list_new_releases.assert_awaited_once_with(_UID, 10, 0)

    @pytest.mark.asyncio
    async def test_none_when_following_nothing(self):
        svc = _svc()
        svc._follow_service = MagicMock()
        svc._follow_service.list_new_releases = AsyncMock(return_value=([], 0))
        assert await svc._build_new_from_followed(_UID) is None
