import msgspec

from api.v1.schemas.discover import (
    BecauseYouListenTo,
    DiscoverResponse,
    TopPickItem,
    TopPicksSection,
)
from api.v1.schemas.home import HomeAlbum, HomeArtist, HomeGenre, HomeSection, HomeTrack
from api.v1.schemas.weekly_exploration import (
    WeeklyExplorationSection,
    WeeklyExplorationTrack,
)
from services.discover.snapshot_codec import decode_discover_response


def test_discover_snapshot_round_trip_preserves_each_card_shape() -> None:
    response = DiscoverResponse(
        because_you_listen_to=[
            BecauseYouListenTo(
                seed_artist="Cocteau Twins",
                seed_artist_mbid="artist-1",
                section=HomeSection(
                    title="Because You Listen To Cocteau Twins",
                    type="artists",
                    items=[HomeArtist(name="This Mortal Coil", mbid="artist-2")],
                ),
            )
        ],
        fresh_releases=HomeSection(
            title="Fresh",
            type="albums",
            items=[HomeAlbum(name="New Album", artist_name="Artist", mbid="rg-1")],
        ),
        lastfm_recent_scrobbles=HomeSection(
            title="Recent",
            type="tracks",
            items=[HomeTrack(name="Song", artist_name="Artist", album_name="Album")],
        ),
        genre_list=HomeSection(
            title="Genres",
            type="genres",
            items=[HomeGenre(name="Dream Pop", artist_count=12)],
        ),
        top_picks=TopPicksSection(
            items=[
                TopPickItem(
                    album=HomeAlbum(name="Pick", artist_name="Artist", mbid="rg-2"),
                    match_pct=91,
                    reasons=["Close to your favourites"],
                )
            ]
        ),
        weekly_exploration=WeeklyExplorationSection(
            title="Weekly Exploration",
            playlist_date="2026-07-17",
            tracks=[
                WeeklyExplorationTrack(
                    title="Track", artist_name="Artist", album_name="Album"
                )
            ],
        ),
        generated_at=123.0,
        section_status={"fresh": "ready"},
    )

    restored = decode_discover_response(msgspec.json.encode(response))

    assert isinstance(restored.because_you_listen_to[0].section.items[0], HomeArtist)
    assert isinstance(restored.fresh_releases.items[0], HomeAlbum)
    assert isinstance(restored.lastfm_recent_scrobbles.items[0], HomeTrack)
    assert isinstance(restored.genre_list.items[0], HomeGenre)
    assert restored.top_picks.items[0].match_pct == 91
    assert restored.weekly_exploration.tracks[0].title == "Track"
    assert restored.generated_at == 123.0
