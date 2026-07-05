from api.v1.schemas.discover import (
    BecauseYouListenTo,
    DiscoverResponse,
    TopPickItem,
    TopPicksSection,
)
from api.v1.schemas.home import HomeAlbum, HomeArtist, HomeSection, HomeTrack
from services.discover.homepage_service import (
    _PREWARM_MAX_ALBUMS,
    _collect_cover_prewarm_mbids,
)


def _album(mbid):
    return HomeAlbum(name="album", mbid=mbid)


def _artist(mbid):
    return HomeArtist(name="artist", mbid=mbid)


def test_collect_covers_walks_all_sections_and_dedups_first_seen():
    resp = DiscoverResponse(
        top_picks=TopPicksSection(items=[TopPickItem(album=_album("alb-top"), match_pct=90)]),
        because_you_listen_to=[
            BecauseYouListenTo(
                seed_artist="Seed",
                seed_artist_mbid="art-seed",
                section=HomeSection(title="t", type="albums", items=[_album("alb-1")]),
            )
        ],
        artists_you_might_like=HomeSection(
            title="Artists", type="artists", items=[_artist("art-1"), _artist("art-seed")]
        ),
        globally_trending=HomeSection(
            title="Trending", type="albums", items=[_album("alb-1"), _album("alb-2")]
        ),
        fresh_releases=HomeSection(
            title="Fresh", type="tracks", items=[HomeTrack(name="track")]
        ),
    )

    albums, artists = _collect_cover_prewarm_mbids(resp)

    # Top picks first, then rows top-to-bottom, duplicates dropped, tracks carry no cover.
    assert albums == ["alb-top", "alb-1", "alb-2"]
    assert artists == ["art-seed", "art-1"]


def test_collect_covers_ignores_missing_mbids():
    resp = DiscoverResponse(
        globally_trending=HomeSection(
            title="Trending",
            type="albums",
            items=[_album(None), _album(""), _album("alb-ok")],
        ),
    )
    albums, artists = _collect_cover_prewarm_mbids(resp)
    assert albums == ["alb-ok"]
    assert artists == []


def test_collect_covers_includes_weekly_exploration_tracks():
    from api.v1.schemas.weekly_exploration import (
        WeeklyExplorationSection,
        WeeklyExplorationTrack,
    )

    resp = DiscoverResponse(
        weekly_exploration=WeeklyExplorationSection(
            title="Weekly Exploration",
            playlist_date="2026-07-05",
            tracks=[
                WeeklyExplorationTrack(
                    title="t",
                    artist_name="a",
                    album_name="al",
                    release_group_mbid="we-album",
                    artist_mbid="we-artist",
                )
            ],
        ),
    )
    albums, artists = _collect_cover_prewarm_mbids(resp)
    assert "we-album" in albums
    assert "we-artist" in artists


def test_collect_covers_caps_album_count():
    resp = DiscoverResponse(
        globally_trending=HomeSection(
            title="Trending",
            type="albums",
            items=[_album(f"alb-{i}") for i in range(_PREWARM_MAX_ALBUMS + 50)],
        ),
    )
    albums, _ = _collect_cover_prewarm_mbids(resp)
    assert len(albums) == _PREWARM_MAX_ALBUMS
