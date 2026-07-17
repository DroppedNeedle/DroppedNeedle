from uuid import UUID

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


def _mbid(value: int) -> str:
    return str(UUID(int=value))


def _album(mbid):
    return HomeAlbum(name="album", mbid=mbid)


def _artist(mbid):
    return HomeArtist(name="artist", mbid=mbid)


def test_collect_covers_walks_all_sections_and_dedups_first_seen():
    album_top, album_one, album_two = (_mbid(i) for i in range(1, 4))
    artist_seed, artist_one = (_mbid(i) for i in range(101, 103))
    resp = DiscoverResponse(
        top_picks=TopPicksSection(items=[TopPickItem(album=_album(album_top), match_pct=90)]),
        because_you_listen_to=[
            BecauseYouListenTo(
                seed_artist="Seed",
                seed_artist_mbid=artist_seed,
                section=HomeSection(title="t", type="albums", items=[_album(album_one)]),
            )
        ],
        artists_you_might_like=HomeSection(
            title="Artists", type="artists", items=[_artist(artist_one), _artist(artist_seed)]
        ),
        globally_trending=HomeSection(
            title="Trending", type="albums", items=[_album(album_one), _album(album_two)]
        ),
        fresh_releases=HomeSection(
            title="Fresh", type="tracks", items=[HomeTrack(name="track")]
        ),
    )

    albums, artists = _collect_cover_prewarm_mbids(resp)

    # Top picks first, then rows top-to-bottom, duplicates dropped, tracks carry no cover.
    assert albums == [album_top, album_one, album_two]
    assert artists == [artist_seed, artist_one]


def test_collect_covers_ignores_missing_mbids():
    album_mbid = _mbid(1)
    resp = DiscoverResponse(
        globally_trending=HomeSection(
            title="Trending",
            type="albums",
            items=[_album(None), _album(""), _album(album_mbid)],
        ),
    )
    albums, artists = _collect_cover_prewarm_mbids(resp)
    assert albums == [album_mbid]
    assert artists == []


def test_collect_covers_includes_weekly_exploration_tracks():
    from api.v1.schemas.weekly_exploration import (
        WeeklyExplorationSection,
        WeeklyExplorationTrack,
    )

    album_mbid = _mbid(1)
    artist_mbid = _mbid(2)
    resp = DiscoverResponse(
        weekly_exploration=WeeklyExplorationSection(
            title="Weekly Exploration",
            playlist_date="2026-07-05",
            tracks=[
                WeeklyExplorationTrack(
                    title="t",
                    artist_name="a",
                    album_name="al",
                    release_group_mbid=album_mbid,
                    artist_mbid=artist_mbid,
                )
            ],
        ),
    )
    albums, artists = _collect_cover_prewarm_mbids(resp)
    assert album_mbid in albums
    assert artist_mbid in artists


def test_collect_covers_caps_album_count():
    resp = DiscoverResponse(
        globally_trending=HomeSection(
            title="Trending",
            type="albums",
            items=[_album(_mbid(i + 1)) for i in range(_PREWARM_MAX_ALBUMS + 50)],
        ),
    )
    albums, _ = _collect_cover_prewarm_mbids(resp)
    assert len(albums) == _PREWARM_MAX_ALBUMS
