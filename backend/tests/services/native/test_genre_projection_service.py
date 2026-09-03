import json
from pathlib import Path
from unittest.mock import AsyncMock

import pytest

from api.v1.schemas.library_management import (
    GenreAliasSettings,
    GenreManagementSettings,
)
from core.exceptions import ConfigurationError, ExternalServiceError, RateLimitedError
from infrastructure.resilience.retry import CircuitOpenError
from models.library_management_canonical import (
    CanonicalArtistCredit,
    CanonicalDate,
    CanonicalGenre,
    CanonicalIdentifierSet,
    CanonicalMedium,
    CanonicalReleaseDocument,
    CanonicalTrackDocument,
)
from models.library_management_genres import GenreCandidate
from services.native.genre_normalizer import GenreNormalizer
from services.native.genre_projection_service import GenreProjectionService

_RG = "dcff25f1-702d-3b5e-b0da-d48172e6e62a"


def _release(
    release_genres: tuple[CanonicalGenre, ...] = (),
    track_genres: tuple[CanonicalGenre, ...] = (),
) -> CanonicalReleaseDocument:
    identifiers = CanonicalIdentifierSet(
        release_group_mbid=_RG,
        release_mbid="aff0622e-7bd3-4fb6-9ca3-0fa19dd2340b",
    )
    credit = CanonicalArtistCredit(
        display_name="Artist",
        credited_name="Artist",
        canonical_name="Artist",
        sort_name="Artist",
        artist_mbid="artist-id",
    )
    track = CanonicalTrackDocument(
        local_track_id="track-1",
        source_track_revision=1,
        source_identity_revision=1,
        title="Track",
        artist_credits=(credit,),
        relationship_credits=(),
        identifiers=CanonicalIdentifierSet(
            release_group_mbid=_RG,
            release_mbid=identifiers.release_mbid,
            release_track_mbid="release-track",
            recording_mbid="recording",
        ),
        track_number=1,
        track_number_text="1",
        total_tracks=1,
        disc_number=1,
        total_discs=1,
        genres=track_genres,
    )
    return CanonicalReleaseDocument(
        local_album_id="album-1",
        source_album_revision=1,
        source_identity_revision=1,
        title="Album",
        artist_credits=(credit,),
        identifiers=identifiers,
        date=CanonicalDate(value="2020", precision="year"),
        original_date=None,
        release_status="Official",
        release_country="GB",
        primary_release_type="Album",
        secondary_release_types=(),
        packaging=None,
        barcode=None,
        asin=None,
        language="eng",
        script="Latn",
        compilation=False,
        total_discs=1,
        labels=(),
        genres=release_genres,
        media=(
            CanonicalMedium(
                position=1,
                title=None,
                format="CD",
                track_count=1,
                tracks=(track,),
            ),
        ),
    )


def _candidate(
    name: str,
    *,
    curated: bool = True,
    count: int = 2,
) -> GenreCandidate:
    return GenreCandidate(
        display_name=name,
        folded_name=name.casefold(),
        provider="listenbrainz",
        provider_entity="release_group",
        genre_mbid="genre-id" if curated else None,
        count=count,
        curated=curated,
    )


def _lastfm_candidate(name: str, weight: int, entity: str) -> GenreCandidate:
    return GenreCandidate(
        display_name=name,
        folded_name=name.casefold(),
        provider="lastfm",
        provider_entity=entity,
        weight=weight,
    )


def test_official_asset_is_versioned_and_complete() -> None:
    normalizer = GenreNormalizer()
    asset = json.loads(
        (
            Path(__file__).resolve().parents[3]
            / "assets"
            / "library_management_genres.json"
        ).read_text(encoding="utf-8")
    )
    assert asset["source"] == "https://musicbrainz.org/ws/2/genre/all?fmt=txt"
    assert asset["retrieved_at"] == "2026-07-21"
    assert normalizer.vocabulary_size > 2100


def test_normalizer_applies_alias_allow_deny_casing_and_hierarchy() -> None:
    settings = GenreManagementSettings(
        aliases=[GenreAliasSettings(source="Alt", target="alternative rock")],
        allowlist=["alternative rock"],
        denylist=["metal"],
        preferred_casing=["Alternative Rock"],
    )
    normalized = GenreNormalizer().normalize(
        _candidate("Alt", curated=False),
        settings,
        require_canonical_vocabulary=True,
    )
    denied = GenreNormalizer().normalize(
        _candidate("Metal"), settings, require_canonical_vocabulary=False
    )
    assert normalized is not None
    assert normalized.display_name == "Alternative Rock"
    assert normalized.canonicalization_path == ("Alternative Rock", "rock")
    assert denied is None


def test_normalizer_rejects_hierarchy_cycles(tmp_path: Path) -> None:
    asset = tmp_path / "genres.json"
    asset.write_text(
        json.dumps(
            {
                "genres": ["a", "b"],
                "aliases": {},
                "parents": {"a": "b", "b": "a"},
            }
        ),
        encoding="utf-8",
    )
    with pytest.raises(ConfigurationError, match="cycle"):
        GenreNormalizer(asset)


@pytest.mark.asyncio
async def test_projection_respects_source_order_thresholds_curated_gate_and_dedupe() -> (
    None
):
    listenbrainz = AsyncMock()
    listenbrainz.get_release_group_genres_batch.return_value = {
        _RG: (
            _candidate("Classical", count=5),
            _candidate("Keyboard", curated=False, count=5),
            _candidate("Baroque", count=0),
        )
    }
    settings = GenreManagementSettings(
        sources=["listenbrainz", "musicbrainz"],
        maximum_count=4,
        listenbrainz_minimum_count=1,
        musicbrainz_minimum_count=1,
        listenbrainz_curated_only=True,
    )
    release = _release(
        release_genres=(
            CanonicalGenre(
                display_name="classical",
                provider_entity="release",
                genre_mbid="mb-classical",
                count=2,
            ),
            CanonicalGenre(
                display_name="Baroque",
                provider_entity="release_group",
                genre_mbid="mb-baroque",
                count=3,
            ),
        )
    )
    projection = await GenreProjectionService(
        GenreNormalizer(), listenbrainz=listenbrainz
    ).project(settings=settings, canonical_release=release, existing_genres=[])

    assert projection.names == ("classical", "baroque")
    assert projection.genres[0].provider == "listenbrainz"
    assert projection.genres[1].provider == "musicbrainz"


@pytest.mark.asyncio
async def test_noncurated_listenbrainz_requires_canonical_vocabulary() -> None:
    listenbrainz = AsyncMock()
    listenbrainz.get_release_group_genres_batch.return_value = {
        _RG: (
            _candidate("not-a-reviewed-genre", curated=False),
            _candidate("dnb", curated=False),
        )
    }
    settings = GenreManagementSettings(
        sources=["listenbrainz"], listenbrainz_curated_only=False
    )
    projection = await GenreProjectionService(
        GenreNormalizer(), listenbrainz=listenbrainz
    ).project(settings=settings, canonical_release=_release(), existing_genres=[])
    assert projection.names == ("drum and bass",)


@pytest.mark.asyncio
async def test_lastfm_uses_album_then_artist_weights_threshold_and_whitelist() -> None:
    lastfm = AsyncMock()
    lastfm.get_album_top_genres.return_value = (
        _lastfm_candidate("Alternative Rock", 100, "album"),
        _lastfm_candidate("Albums I Own", 99, "album"),
        _lastfm_candidate("Rock", 5, "album"),
    )
    lastfm.get_artist_top_genres.return_value = (
        _lastfm_candidate("Rock", 90, "artist"),
        _lastfm_candidate("Electronic", 80, "artist"),
    )
    settings = GenreManagementSettings(
        sources=["lastfm"],
        maximum_count=3,
        lastfm_minimum_weight=10,
        lastfm_whitelist_only=True,
    )

    projection = await GenreProjectionService(GenreNormalizer(), lastfm=lastfm).project(
        settings=settings, canonical_release=_release(), existing_genres=[]
    )

    assert projection.names == ("alternative rock", "rock", "electronic")
    assert [value.provider_entity for value in projection.genres] == [
        "album",
        "artist",
        "artist",
    ]
    lastfm.get_album_top_genres.assert_awaited_once_with(
        artist_name="Artist", album_title="Album"
    )
    lastfm.get_artist_top_genres.assert_awaited_once_with(artist_name="Artist")


@pytest.mark.asyncio
async def test_lastfm_rate_limit_preserves_existing_and_marks_source_deferred() -> None:
    lastfm = AsyncMock()
    lastfm.get_album_top_genres.side_effect = RateLimitedError("slow down")
    projection = await GenreProjectionService(GenreNormalizer(), lastfm=lastfm).project(
        settings=GenreManagementSettings(sources=["lastfm"], mode="replace"),
        canonical_release=_release(),
        existing_genres=["Existing Genre"],
    )

    assert projection.names == ("Existing Genre",)
    assert projection.preserved_existing is True
    assert projection.deferred_sources == ("lastfm",)


@pytest.mark.asyncio
async def test_fill_and_merge_use_existing_without_heuristic_splitting() -> None:
    release = _release(
        release_genres=(
            CanonicalGenre(display_name="Rock", provider_entity="release", count=3),
        )
    )
    fill = GenreManagementSettings(mode="fill_missing", sources=["musicbrainz"])
    merge = GenreManagementSettings(mode="merge", sources=["musicbrainz"])
    service = GenreProjectionService(GenreNormalizer())
    filled = await service.project(
        settings=fill,
        canonical_release=release,
        existing_genres=["Jazz; Fusion"],
    )
    merged = await service.project(
        settings=merge,
        canonical_release=release,
        existing_genres=["Jazz", "rock"],
    )
    assert filled.names == ("Jazz; Fusion",)
    assert filled.preserved_existing is True
    assert merged.names == ("rock", "jazz")


@pytest.mark.asyncio
async def test_all_enabled_remote_outage_preserves_existing_and_marks_deferred() -> (
    None
):
    listenbrainz = AsyncMock()
    listenbrainz.get_release_group_genres_batch.side_effect = ExternalServiceError(
        "offline"
    )
    settings = GenreManagementSettings(sources=["listenbrainz"], mode="replace")
    projection = await GenreProjectionService(
        GenreNormalizer(), listenbrainz=listenbrainz
    ).project(
        settings=settings,
        canonical_release=_release(),
        existing_genres=["Existing Genre"],
    )
    assert projection.names == ("Existing Genre",)
    assert projection.preserved_existing is True
    assert projection.deferred_sources == ("listenbrainz",)

@pytest.mark.asyncio
async def test_listenbrainz_open_breaker_marks_source_deferred() -> None:
    listenbrainz = AsyncMock()
    listenbrainz.get_release_group_genres_batch.side_effect = CircuitOpenError(
        "Circuit breaker 'listenbrainz' is OPEN"
    )
    settings = GenreManagementSettings(sources=["listenbrainz"], mode="replace")
    projection = await GenreProjectionService(
        GenreNormalizer(), listenbrainz=listenbrainz
    ).project(
        settings=settings,
        canonical_release=_release(),
        existing_genres=["Existing Genre"],
    )
    assert projection.names == ("Existing Genre",)
    assert projection.preserved_existing is True
    assert projection.deferred_sources == ("listenbrainz",)


@pytest.mark.asyncio
async def test_lastfm_open_breaker_marks_source_deferred() -> None:
    lastfm = AsyncMock()
    lastfm.get_album_top_genres.side_effect = CircuitOpenError(
        "Circuit breaker 'lastfm' is OPEN"
    )
    projection = await GenreProjectionService(GenreNormalizer(), lastfm=lastfm).project(
        settings=GenreManagementSettings(sources=["lastfm"], mode="replace"),
        canonical_release=_release(),
        existing_genres=["Existing Genre"],
    )
    assert projection.names == ("Existing Genre",)
    assert projection.preserved_existing is True
    assert projection.deferred_sources == ("lastfm",)
