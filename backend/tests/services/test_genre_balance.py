"""Genre balance: breadth-based seeding, per-zone/page genre-family share caps,
and user reduce/mute preferences across Discover and the Taste Graph."""

import pytest
from unittest.mock import AsyncMock, MagicMock, patch

from api.v1.schemas.discover import DiscoverResponse
from api.v1.schemas.home import HomeArtist, HomeSection
from infrastructure.cache.memory_cache import InMemoryCache
from repositories.listenbrainz_models import ListenBrainzArtist
from services.discover.genre_balance import (
    GenrePrefs,
    balanced_seed_selection,
    cap_genre_share,
    diverse_genre_selection,
    genre_family,
)
from services.discover.homepage_service import DiscoverHomepageService
from services.taste_graph_service import TasteGraphService


# ------------------------------------------------------------------ families


class TestGenreFamily:
    def test_kpop_spellings_are_one_family(self) -> None:
        assert genre_family("k-pop") == "k-pop"
        assert genre_family("kpop") == "k-pop"
        assert genre_family("K Pop") == "k-pop"
        assert genre_family("korean pop") == "k-pop"
        assert genre_family("Korean Ballad") == "k-pop"

    def test_distinct_genres_stay_distinct(self) -> None:
        assert genre_family("rock") == "rock"
        assert genre_family("jazz") == "jazz"
        assert genre_family("rock") != genre_family("k-pop")

    def test_unknown_or_blank_is_empty(self) -> None:
        assert genre_family(None) == ""
        assert genre_family("   ") == ""

    def test_prefs_normalize_family_spellings(self) -> None:
        prefs = GenrePrefs({"kpop": "mute", "hip-hop": "reduce"})
        assert prefs.is_muted("k-pop")
        assert prefs.multiplier(genre_family("korean pop")) == 0.0
        assert prefs.multiplier(genre_family("hip hop")) == 0.5


# ------------------------------------------------------------- seed balancing


def _skewed_candidates() -> tuple[list[str], dict[str, list[str]]]:
    """10 candidates in play-count order: 7 K-pop-family, 2 rock, 1 jazz."""
    mbids = [f"a{i}" for i in range(10)]
    kpop_spellings = ["k-pop", "kpop", "korean pop", "k-pop", "kpop", "korean pop", "k-pop"]
    genres = {f"a{i}": [kpop_spellings[i]] for i in range(7)}
    genres["a7"] = ["rock"]
    genres["a8"] = ["rock"]
    genres["a9"] = ["jazz"]
    return mbids, genres


class TestBalancedSeedSelection:
    def test_skewed_library_yields_one_seed_per_family(self) -> None:
        mbids, genres = _skewed_candidates()
        picked = balanced_seed_selection(mbids, lambda m: genres[m], 3)
        families = [genre_family(genres[m][0]) for m in picked]
        # breadth: a 70% K-pop pool still seeds three DISTINCT families
        assert sorted(families) == ["jazz", "k-pop", "rock"]

    def test_mute_excludes_the_family(self) -> None:
        mbids, genres = _skewed_candidates()
        prefs = GenrePrefs({"k-pop": "mute"})
        picked = balanced_seed_selection(mbids, lambda m: genres[m], 3, prefs)
        assert picked and all(genre_family(genres[m][0]) != "k-pop" for m in picked)

    def test_reduce_halves_the_family_weight(self) -> None:
        # 9 kpop vs 4 rock: raw damped weights sqrt(9)=3 > sqrt(4)=2, but reduce
        # halves kpop to 1.5 so rock leads the round-robin
        candidates = [f"k{i}" for i in range(9)] + [f"r{i}" for i in range(4)]
        genres = {m: ["k-pop"] if m.startswith("k") else ["rock"] for m in candidates}
        prefs = GenrePrefs({"k-pop": "reduce"})
        picked = balanced_seed_selection(candidates, lambda m: genres[m], 2, prefs)
        assert genres[picked[0]] == ["rock"]

    def test_no_genre_data_degrades_to_first_n(self) -> None:
        picked = balanced_seed_selection(["a", "b", "c", "d"], lambda m: [], 3)
        assert picked == ["a", "b", "c"]


# ------------------------------------------------------------------ zone caps


class TestCapGenreShare:
    def test_zone_cap_holds_for_dominant_family(self) -> None:
        items = [("k", i) for i in range(12)] + [("r", i) for i in range(3)]
        genres_of = lambda item: ["korean pop"] if item[0] == "k" else ["rock"]  # noqa: E731
        kept = cap_genre_share(items, genres_of)
        kpop_kept = [i for i in kept if i[0] == "k"]
        # cap = ceil(0.35 * 15) = 6
        assert len(kpop_kept) <= 6
        # backfill: every other-genre item survives
        assert [i for i in kept if i[0] == "r"] == [("r", 0), ("r", 1), ("r", 2)]

    def test_mute_excludes_all_spellings_of_a_family(self) -> None:
        items = [("a", ["kpop"]), ("b", ["korean pop"]), ("c", ["k-pop"]), ("d", ["rock"])]
        kept = cap_genre_share(items, lambda i: i[1], prefs=GenrePrefs({"k-pop": "mute"}))
        assert kept == [("d", ["rock"])]

    def test_unknown_genre_items_pass_through(self) -> None:
        items = list(range(10))
        assert cap_genre_share(items, lambda i: []) == items

    def test_page_wide_budget_is_shared_across_zones(self) -> None:
        counts: dict[str, int] = {}
        zone = [("k", i) for i in range(6)]
        genres_of = lambda item: ["k-pop"]  # noqa: E731
        # target_size 20 -> per-zone cap of 7 doesn't bite; the page budget does
        first = cap_genre_share(
            zone, genres_of, target_size=20, counts=counts, total_allowed=8
        )
        second = cap_genre_share(
            zone, genres_of, target_size=20, counts=counts, total_allowed=8
        )
        # 6 from the first zone, only 2 left in the page budget for the second
        assert len(first) == 6
        assert len(second) == 2


class TestDiverseGenreSelection:
    def test_family_capped_and_muted_dropped(self) -> None:
        top = [("k-pop", 30), ("kpop", 20), ("korean pop", 15), ("rock", 8), ("jazz", 5)]
        rows = diverse_genre_selection(top, limit=10, max_per_family=2)
        kpop_rows = [g for g, _ in rows if genre_family(g) == "k-pop"]
        assert len(kpop_rows) == 2
        assert {"rock", "jazz"} <= {g for g, _ in rows}

        muted = diverse_genre_selection(
            top, limit=10, prefs=GenrePrefs({"k-pop": "mute"}), max_per_family=2
        )
        assert all(genre_family(g) != "k-pop" for g, _ in muted)


# --------------------------------------------------- homepage service seeding


def _make_homepage_service(
    genre_index: AsyncMock | None = None,
    genre_prefs_store: AsyncMock | None = None,
    lb_repo: AsyncMock | None = None,
) -> DiscoverHomepageService:
    return DiscoverHomepageService(
        listenbrainz_repo=lb_repo or AsyncMock(),
        jellyfin_repo=AsyncMock(),
        library_repo=AsyncMock(),
        musicbrainz_repo=AsyncMock(),
        integration=MagicMock(),
        mbid_resolution=MagicMock(),
        memory_cache=None,
        genre_index=genre_index,
        genre_prefs_store=genre_prefs_store,
    )


def _lb_artist(mbid: str, plays: int) -> ListenBrainzArtist:
    return ListenBrainzArtist(
        artist_name=f"Artist {mbid}", listen_count=plays, artist_mbids=[mbid]
    )


class TestSeedArtistsAreGenreBalanced:
    @pytest.mark.asyncio
    async def test_skewed_top_artists_seed_three_distinct_families(self) -> None:
        mbids, genres = _skewed_candidates()
        lb_repo = AsyncMock()
        # play-count order: the 7 K-pop artists dominate the raw top
        lb_repo.get_user_top_artists = AsyncMock(
            return_value=[_lb_artist(m, 100 - i) for i, m in enumerate(mbids)]
        )
        genre_index = AsyncMock()
        genre_index.get_genres_for_artists = AsyncMock(return_value=genres)
        service = _make_homepage_service(genre_index=genre_index, lb_repo=lb_repo)

        seeds = await service._get_seed_artists(True, "user", False)
        assert len(seeds) == 3
        families = {genre_family(genres[s.artist_mbids[0]][0]) for s in seeds}
        assert families == {"k-pop", "rock", "jazz"}

    @pytest.mark.asyncio
    async def test_mute_excludes_family_from_seeds(self) -> None:
        mbids, genres = _skewed_candidates()
        lb_repo = AsyncMock()
        lb_repo.get_user_top_artists = AsyncMock(
            return_value=[_lb_artist(m, 100 - i) for i, m in enumerate(mbids)]
        )
        genre_index = AsyncMock()
        genre_index.get_genres_for_artists = AsyncMock(return_value=genres)
        service = _make_homepage_service(genre_index=genre_index, lb_repo=lb_repo)

        seeds = await service._get_seed_artists(
            True, "user", False, genre_prefs=GenrePrefs({"k-pop": "mute"})
        )
        assert seeds
        for seed in seeds:
            assert genre_family(genres[seed.artist_mbids[0]][0]) != "k-pop"

    @pytest.mark.asyncio
    async def test_without_genre_index_keeps_original_top_three(self) -> None:
        lb_repo = AsyncMock()
        lb_repo.get_user_top_artists = AsyncMock(
            return_value=[_lb_artist(f"a{i}", 100 - i) for i in range(10)]
        )
        service = _make_homepage_service(genre_index=None, lb_repo=lb_repo)
        seeds = await service._get_seed_artists(True, "user", False)
        assert [s.artist_mbids[0] for s in seeds] == ["a0", "a1", "a2"]


class TestZoneGenreCapInResponse:
    @staticmethod
    def _response_with_section(n_kpop: int, n_rock: int) -> tuple[DiscoverResponse, dict]:
        items = [HomeArtist(mbid=f"k{i}", name=f"K{i}") for i in range(n_kpop)]
        items += [HomeArtist(mbid=f"r{i}", name=f"R{i}") for i in range(n_rock)]
        genres = {f"k{i}": ["korean pop"] for i in range(n_kpop)}
        genres.update({f"r{i}": ["rock"] for i in range(n_rock)})
        response = DiscoverResponse(
            artists_you_might_like=HomeSection(
                title="Artists You Might Like", type="artists", items=items,
            )
        )
        return response, genres

    @pytest.mark.asyncio
    async def test_zone_share_cap_holds(self) -> None:
        response, genres = self._response_with_section(12, 3)
        genre_index = AsyncMock()
        genre_index.get_genres_for_artists = AsyncMock(return_value=genres)
        service = _make_homepage_service(genre_index=genre_index)

        await service._apply_genre_balance(response, GenrePrefs())
        kept = response.artists_you_might_like.items
        kpop = [i for i in kept if i.mbid.startswith("k")]
        assert len(kpop) <= 6  # ceil(0.35 * 15)
        assert len([i for i in kept if i.mbid.startswith("r")]) == 3

    @pytest.mark.asyncio
    async def test_mute_removes_family_from_zone(self) -> None:
        response, genres = self._response_with_section(12, 3)
        genre_index = AsyncMock()
        genre_index.get_genres_for_artists = AsyncMock(return_value=genres)
        service = _make_homepage_service(genre_index=genre_index)

        await service._apply_genre_balance(response, GenrePrefs({"k-pop": "mute"}))
        kept = response.artists_you_might_like.items
        assert kept and all(i.mbid.startswith("r") for i in kept)


class TestDailyMixClusterBalance:
    @staticmethod
    def _genre_index(top_genres: list[tuple[str, int]]) -> AsyncMock:
        genre_index = AsyncMock()
        genre_index.get_top_genres = AsyncMock(return_value=top_genres)
        counter = 0
        artists: dict[str, list[str]] = {}
        for genre, _ in top_genres:
            artists[genre] = [f"artist-{counter + j}" for j in range(4)]
            counter += 4
        genre_index.get_artists_for_genres = AsyncMock(return_value=artists)
        genre_index.get_albums_by_genre = AsyncMock(return_value=[])
        return genre_index

    _TOP = [
        ("k-pop", 30), ("kpop", 20), ("korean pop", 15), ("rock", 8), ("jazz", 5),
    ]

    @pytest.mark.asyncio
    @patch("services.discover.homepage_service.build_similar_artist_pools")
    async def test_one_family_gets_at_most_two_clusters(self, mock_pools: AsyncMock) -> None:
        mock_pools.return_value = []
        genre_index = self._genre_index(self._TOP)
        service = _make_homepage_service(genre_index=genre_index)
        # no memory cache -> no daily-mix cache read/write; library albums empty ->
        # sections need pool items, so give every cluster one album
        from api.v1.schemas.discover import DiscoverQueueItemLight
        mock_pools.return_value = [[
            DiscoverQueueItemLight(
                release_group_mbid="rg-1", album_name="A", artist_name="B",
                artist_mbid="m-1", recommendation_reason="r",
            )
        ]]

        sections = await service._build_daily_mix_sections("u1", "listenbrainz", set())
        families = [
            genre_family(section.title.split(" - ", 1)[1].lower()) for section in sections
        ]
        assert families.count("k-pop") <= 2
        assert "rock" in families and "jazz" in families

    @pytest.mark.asyncio
    @patch("services.discover.homepage_service.build_similar_artist_pools")
    async def test_muted_family_gets_no_cluster(self, mock_pools: AsyncMock) -> None:
        from api.v1.schemas.discover import DiscoverQueueItemLight
        mock_pools.return_value = [[
            DiscoverQueueItemLight(
                release_group_mbid="rg-1", album_name="A", artist_name="B",
                artist_mbid="m-1", recommendation_reason="r",
            )
        ]]
        prefs_store = AsyncMock()
        prefs_store.get_levels = AsyncMock(return_value={"k-pop": "mute"})
        genre_index = self._genre_index(self._TOP)
        service = _make_homepage_service(
            genre_index=genre_index, genre_prefs_store=prefs_store
        )

        sections = await service._build_daily_mix_sections("u1", "listenbrainz", set())
        assert sections
        for section in sections:
            genre_label = section.title.split(" - ", 1)[1].lower()
            assert genre_family(genre_label) != "k-pop"


# ------------------------------------------------------------------ taste graph


def _taste_graph_service(
    artists: list[dict],
    genres: dict[str, list[str]],
    levels: dict[str, str] | None = None,
) -> TasteGraphService:
    library_db = AsyncMock()
    library_db.get_artists = AsyncMock(return_value=artists)
    library_db.get_all_albums_for_matching = AsyncMock(return_value=[])
    library_db.get_all_album_mbids = AsyncMock(return_value=set())
    follow_store = AsyncMock()
    follow_store.list_followed_artists = AsyncMock(return_value=[])
    play_history = AsyncMock()
    play_history.recent = AsyncMock(return_value=[])
    mb_repo = AsyncMock()
    mb_repo.get_artist_expansion = AsyncMock(return_value={})
    genre_index = AsyncMock()
    genre_index.get_genres_for_artists = AsyncMock(return_value=genres)
    prefs_store = AsyncMock()
    prefs_store.get_levels = AsyncMock(return_value=levels or {})
    return TasteGraphService(
        library_db=library_db,
        follow_store=follow_store,
        play_history_store=play_history,
        mb_repo=mb_repo,
        cache=InMemoryCache(max_entries=100),
        genre_index=genre_index,
        genre_prefs_store=prefs_store,
    )


class TestTasteGraphSeedBalance:
    @staticmethod
    def _skewed_library() -> tuple[list[dict], dict[str, list[str]]]:
        artists = [{"mbid": f"k{i}", "name": f"KArtist {i}"} for i in range(12)]
        artists += [{"mbid": "r1", "name": "Rock One"}, {"mbid": "r2", "name": "Rock Two"}]
        genres = {f"k{i}": ["kpop"] for i in range(12)}
        genres["r1"] = ["rock"]
        genres["r2"] = ["rock"]
        return artists, genres

    @pytest.mark.asyncio
    async def test_minority_families_reach_the_seed_set(self) -> None:
        artists, genres = self._skewed_library()
        service = _taste_graph_service(artists, genres)
        result = await service.get_taste_graph("u1")
        seed_mbids = {s.artist_mbid for s in result.seeds}
        # raw top-10-by-weight would be K-pop only; balanced seeding pulls rock in
        assert {"r1", "r2"} <= seed_mbids

    @pytest.mark.asyncio
    async def test_muted_family_never_seeds(self) -> None:
        artists, genres = self._skewed_library()
        service = _taste_graph_service(artists, genres, levels={"k-pop": "mute"})
        result = await service.get_taste_graph("u1")
        seed_mbids = {s.artist_mbid for s in result.seeds}
        assert seed_mbids == {"r1", "r2"}
