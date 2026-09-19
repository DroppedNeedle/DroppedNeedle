import copy
import json
from pathlib import Path
from unittest.mock import AsyncMock

import msgspec
import pytest

from api.v1.schemas.library_management import (
    ArtistCreditSettings,
    GenreManagementSettings,
    LibraryManagementProfile,
    ManagedFieldSettings,
    MetadataManagementSettings,
    RelationshipCreditSettings,
    picard_style_organizer_profile,
)
from core.exceptions import ProviderIdentityRequiredError, ResourceNotFoundError
from infrastructure.queue.priority_queue import RequestPriority
from models.library_management_canonical import (
    AcceptedAlbumManagementIdentity,
    AcceptedTrackManagementIdentity,
)
from repositories.musicbrainz_management_models import MbManagementRelease
from services.native.canonical_release_metadata_service import (
    CanonicalReleaseMetadataService,
    _BASE_INCLUDES,
    _RELATIONSHIP_INCLUDES,
    _organization_audio_medium_count,
    _required_includes,
)

_FIXTURE = (
    Path(__file__).resolve().parents[2]
    / "fixtures"
    / "musicbrainz"
    / "management_release.json"
)


def _release() -> MbManagementRelease:
    return msgspec.json.decode(_FIXTURE.read_bytes(), type=MbManagementRelease)


def _identity(
    *,
    release_mbid: str | None = "aff0622e-7bd3-4fb6-9ca3-0fa19dd2340b",
    release_track_mbid: str | None = "22222222-2222-4222-8222-222222222222",
    recording_mbid: str | None = "33333333-3333-4333-8333-333333333333",
) -> AcceptedAlbumManagementIdentity:
    return AcceptedAlbumManagementIdentity(
        local_album_id="album-1",
        album_revision=4,
        identity_revision=3,
        release_group_mbid="dcff25f1-702d-3b5e-b0da-d48172e6e62a",
        release_mbid=release_mbid,
        tracks=(
            AcceptedTrackManagementIdentity(
                local_track_id="track-1",
                track_revision=7,
                identity_revision=2,
                recording_mbid=recording_mbid,
                release_mbid=release_mbid,
                release_track_mbid=release_track_mbid,
                medium_position=1,
                release_track_position=1,
            ),
        ),
    )


def _service(identity, release=None, *, clock=lambda: 100.0):
    store = AsyncMock()
    store.get_accepted_library_management_identity.return_value = identity
    store.put_management_metadata_snapshot.side_effect = lambda snapshot: snapshot
    musicbrainz = AsyncMock()
    musicbrainz.get_canonical_release.return_value = release or _release()
    return (
        CanonicalReleaseMetadataService(store, musicbrainz, clock=clock),
        store,
        musicbrainz,
    )


@pytest.mark.asyncio
async def test_requires_existing_album_and_specific_accepted_edition() -> None:
    missing_service, _, _ = _service(None)
    with pytest.raises(ResourceNotFoundError, match="album not found"):
        await missing_service.build(
            local_album_id="missing", profile=picard_style_organizer_profile()
        )

    edition_service, _, musicbrainz = _service(_identity(release_mbid=None))
    with pytest.raises(ProviderIdentityRequiredError, match="specific MusicBrainz"):
        await edition_service.build(
            local_album_id="album-1", profile=picard_style_organizer_profile()
        )
    musicbrainz.get_canonical_release.assert_not_awaited()


@pytest.mark.asyncio
async def test_requires_release_track_and_recording_mapping_for_every_file() -> None:
    service, _, _ = _service(_identity(release_track_mbid=None))
    with pytest.raises(ProviderIdentityRequiredError, match="Every selected file"):
        await service.build(
            local_album_id="album-1", profile=picard_style_organizer_profile()
        )


@pytest.mark.asyncio
async def test_projects_credited_picard_metadata_and_identity_alignment() -> None:
    service, store, musicbrainz = _service(_identity())
    projection = await service.build(
        local_album_id="album-1",
        local_track_ids=("track-1",),
        profile=picard_style_organizer_profile(),
        priority=RequestPriority.BACKGROUND_SYNC,
    )
    document = projection.document
    track = document.media[0].tracks[0]

    assert document.artist_credits[0].display_name == "Johann Sebastian Bach"
    assert document.artist_credits[0].sort_name == "Bach, Johann Sebastian"
    assert document.date is not None and document.date.value == "1982-10"
    assert document.date.precision == "month"
    assert document.original_date is not None
    assert document.original_date.value == "1956"
    assert document.original_date.precision == "year"
    assert document.total_discs == 1
    assert document.album_disambiguation == ""
    assert document.organization_audio_medium_count == 1
    assert track.total_tracks == 1
    assert track.disc_subtitle == "Aria and Variations"
    assert track.track_number_text == "A1"
    assert track.identifiers.release_track_mbid == (
        "22222222-2222-4222-8222-222222222222"
    )
    assert track.identifiers.recording_mbid == ("33333333-3333-4333-8333-333333333333")
    assert track.identifiers.isrcs == ("USSM18200001",)
    assert track.work_title == "Goldberg Variations, BWV 988: Aria"
    assert {credit.role for credit in track.relationship_credits} == {
        "composer",
        "performer",
    }
    assert musicbrainz.get_canonical_release.await_args.kwargs["priority"] is (
        RequestPriority.BACKGROUND_SYNC
    )
    includes = musicbrainz.get_canonical_release.await_args.kwargs["includes"]
    assert "recordings" in includes
    assert "work-rels" in includes
    snapshot = store.put_management_metadata_snapshot.await_args.args[0]
    payload = json.loads(snapshot.canonical_payload_json)
    assert payload["media"][0]["tracks"][0]["local_track_id"] == "track-1"
    assert snapshot.payload_sha256 == projection.payload_sha256


def test_organization_medium_count_matches_lidarr_audio_filter() -> None:
    release = _release()
    cd = release.media[0]
    dvd = copy.deepcopy(cd)
    dvd.position = 2
    dvd.format = "DVD"
    data_only = copy.deepcopy(cd)
    data_only.position = 3
    data_only.tracks[0].title = "[data track]"
    empty = copy.deepcopy(cd)
    empty.position = 4
    empty.tracks = []
    dvd_audio = copy.deepcopy(cd)
    dvd_audio.position = 5
    dvd_audio.format = "DVD-Audio"
    release.media = [cd, dvd, data_only, empty, dvd_audio]

    assert _organization_audio_medium_count(release) == 2


def test_non_audio_medium_does_not_make_a_single_cd_release_multi_disc() -> None:
    release = _release()
    dvd = copy.deepcopy(release.media[0])
    dvd.position = 2
    dvd.format = "DVD"
    release.media.append(dvd)

    assert _organization_audio_medium_count(release) == 1


def test_audio_medium_count_preserves_positions_around_non_audio_media() -> None:
    release = _release()
    dvd = copy.deepcopy(release.media[0])
    dvd.position = 2
    dvd.format = "DVD"
    third_cd = copy.deepcopy(release.media[0])
    third_cd.position = 3
    release.media = [release.media[0], dvd, third_cd]

    assert _organization_audio_medium_count(release) == 2
    assert [medium.position for medium in release.media] == [1, 2, 3]


@pytest.mark.asyncio
async def test_release_group_disambiguation_wins_over_exact_release_text() -> None:
    release = _release()
    release.disambiguation = "Exact release text"
    release.release_group.disambiguation = "Release group text"
    service, _, _ = _service(_identity(), release)

    projection = await service.build(
        local_album_id="album-1",
        local_track_ids=("track-1",),
        profile=picard_style_organizer_profile(),
    )

    assert projection.document.album_disambiguation == "Release group text"


@pytest.mark.asyncio
async def test_canonical_standardization_and_locale_translation_are_independent() -> (
    None
):
    release = _release()
    release.artist_credit[0].name = "J. S. Bach (credited)"

    canonical_profile = picard_style_organizer_profile()
    canonical_profile.metadata.artist_credits.standardization = "canonical"
    canonical_service, _, _ = _service(_identity(), release)
    canonical = await canonical_service.build(
        local_album_id="album-1", profile=canonical_profile
    )
    assert canonical.document.artist_credits[0].display_name == "Johann Sebastian Bach"
    assert canonical.document.artist_credits[0].credited_name == "J. S. Bach (credited)"

    translated_profile = copy.deepcopy(canonical_profile)
    translated_profile.metadata.artist_credits.translate_names = True
    translated_profile.metadata.artist_credits.preferred_locales = ["en-GB"]
    translated_service, _, musicbrainz = _service(_identity(), release)
    translated = await translated_service.build(
        local_album_id="album-1", profile=translated_profile
    )
    assert translated.document.artist_credits[0].display_name == "J. S. Bach"
    assert musicbrainz.get_canonical_release.await_args.kwargs["preferred_locales"] == (
        "en-GB",
    )
    assert "aliases" in musicbrainz.get_canonical_release.await_args.kwargs["includes"]


@pytest.mark.asyncio
async def test_relationship_controls_can_disable_projection_and_includes() -> None:
    profile = picard_style_organizer_profile()
    profile.metadata.relationships.enabled = False
    service, _, musicbrainz = _service(_identity())

    projection = await service.build(local_album_id="album-1", profile=profile)

    assert projection.document.media[0].tracks[0].relationship_credits == ()
    includes = musicbrainz.get_canonical_release.await_args.kwargs["includes"]
    assert "work-rels" not in includes
    assert "recording-level-rels" not in includes


@pytest.mark.asyncio
async def test_compilation_uses_musicbrainz_various_artists_identity() -> None:
    release = _release()
    release.artist_credit = [release.artist_credit[0]]
    release.artist_credit[0].artist.id = "89ad4ac3-39f7-470e-963a-56509c546377"
    release.artist_credit[0].artist.name = "Various Artists"
    release.artist_credit[0].name = "Various Artists"
    service, _, _ = _service(_identity(), release)

    projection = await service.build(
        local_album_id="album-1", profile=picard_style_organizer_profile()
    )
    assert projection.document.compilation is True


@pytest.mark.asyncio
async def test_missing_provider_entity_and_changed_mapping_return_to_review() -> None:
    missing_service, _, musicbrainz = _service(_identity())
    musicbrainz.get_canonical_release.return_value = None
    with pytest.raises(ProviderIdentityRequiredError, match="no longer exists"):
        await missing_service.build(
            local_album_id="album-1", profile=picard_style_organizer_profile()
        )

    changed_service, _, _ = _service(_identity(recording_mbid="different-recording"))
    with pytest.raises(ProviderIdentityRequiredError, match="no longer matches"):
        await changed_service.build(
            local_album_id="album-1", profile=picard_style_organizer_profile()
        )


@pytest.mark.asyncio
async def test_snapshot_identity_and_payload_are_deterministic() -> None:
    clock_values = iter((100.0, 200.0))
    service, store, _ = _service(_identity(), clock=lambda: next(clock_values))
    profile = picard_style_organizer_profile()

    first = await service.build(local_album_id="album-1", profile=profile)
    second = await service.build(local_album_id="album-1", profile=profile)

    assert first.metadata_snapshot_id == second.metadata_snapshot_id
    assert first.input_hash == second.input_hash
    assert first.payload_sha256 == second.payload_sha256
    first_snapshot = store.put_management_metadata_snapshot.await_args_list[0].args[0]
    second_snapshot = store.put_management_metadata_snapshot.await_args_list[1].args[0]
    assert (
        first_snapshot.canonical_payload_json == second_snapshot.canonical_payload_json
    )
    assert first_snapshot.fetched_at != second_snapshot.fetched_at


# Step 05-3 audit: shipped defaults are deliberate (picard preset wires 12
# parts with aliases off; user JSON profiles and root overrides are genuine
# user choice). Labels, ISRCs, and work values feed tags AND naming:
# project() gates on per-field modes (never metadata.enabled) and the
# planner renders naming scripts from desired_metadata, so modes alone imply
# consumption. Only the legacy path-only seed shape (metadata off, empty
# fields) skips parts: unlisted modes default to disabled, so the projector
# ignores every candidate. Relationship credits are the one
# metadata.enabled-gated projection (_track_relationships returns () with
# metadata off); the rels still flow for metadata-off profiles with work
# modes on. The table below pins profile -> includes, consumer per part.
def _includes_profile(
    *,
    metadata_enabled: bool = True,
    fields: tuple[ManagedFieldSettings, ...] = (),
    preserve_fields: tuple[str, ...] = (),
    relationships_enabled: bool = False,
    relationship_types: tuple[str, ...] = (),
    translate_names: bool = False,
    preferred_locales: tuple[str, ...] = (),
    genres_enabled: bool = False,
    genre_sources: tuple[str, ...] = ("musicbrainz", "listenbrainz"),
) -> LibraryManagementProfile:
    return LibraryManagementProfile(
        id="includes-audit",
        name="includes-audit",
        metadata=MetadataManagementSettings(
            enabled=metadata_enabled,
            fields=list(fields),
            artist_credits=ArtistCreditSettings(
                translate_names=translate_names,
                preferred_locales=list(preferred_locales),
            ),
            relationships=RelationshipCreditSettings(
                enabled=relationships_enabled,
                types=list(relationship_types),
            ),
            preserve_fields=list(preserve_fields),
        ),
        genres=GenreManagementSettings(
            enabled=genres_enabled,
            sources=list(genre_sources),
        ),
    )


def _everything_profile() -> LibraryManagementProfile:
    profile = picard_style_organizer_profile()
    profile.metadata.artist_credits.translate_names = True
    profile.metadata.artist_credits.preferred_locales = ["en"]
    return profile


_INCLUDES_CASES = [
        ("empty-profile-base-only", _includes_profile(), set(_BASE_INCLUDES)),
        (
            "metadata-off-with-modes-off-base-only-for-legacy-path-only-seed",
            _includes_profile(
                metadata_enabled=False,
                relationships_enabled=True,
                relationship_types=("composer", "performer"),
                translate_names=False,
                genres_enabled=False,
            ),
            set(_BASE_INCLUDES),
        ),
        (
            "metadata-off-with-label-isrc-work-modes-on-requests-parts-for-naming",
            _includes_profile(
                metadata_enabled=False,
                fields=(
                    ManagedFieldSettings(field="label"),
                    ManagedFieldSettings(field="isrc"),
                    ManagedFieldSettings(field="work"),
                ),
                relationships_enabled=True,
                relationship_types=("composer", "performer"),
                translate_names=False,
                genres_enabled=False,
            ),
            set(_BASE_INCLUDES)
            | {"labels", "isrcs"}
            | set(_RELATIONSHIP_INCLUDES),
        ),
        (
            "metadata-off-with-work-mode-on-requests-rels-for-naming",
            _includes_profile(
                metadata_enabled=False,
                fields=(ManagedFieldSettings(field="work"),),
                relationships_enabled=True,
                relationship_types=("composer", "performer"),
            ),
            set(_BASE_INCLUDES) | set(_RELATIONSHIP_INCLUDES),
        ),
        (
            "metadata-off-with-label-mode-on-requests-labels-for-naming",
            _includes_profile(
                metadata_enabled=False,
                fields=(ManagedFieldSettings(field="label"),),
            ),
            set(_BASE_INCLUDES) | {"labels"},
        ),
        (
            "tag-edit-shape-with-narrow-fields-requests-only-consumed-parts",
            _includes_profile(
                metadata_enabled=True,
                fields=(ManagedFieldSettings(field="label"),),
                relationships_enabled=True,
                relationship_types=("composer", "performer"),
            ),
            set(_BASE_INCLUDES) | {"labels"} | set(_RELATIONSHIP_INCLUDES),
        ),
        (
            "label-field-adds-labels-for-tags-and-naming",
            _includes_profile(fields=(ManagedFieldSettings(field="label"),)),
            set(_BASE_INCLUDES) | {"labels"},
        ),
        (
            "catalog-number-field-adds-labels-for-tags-and-naming",
            _includes_profile(
                fields=(ManagedFieldSettings(field="catalog_number"),)
            ),
            set(_BASE_INCLUDES) | {"labels"},
        ),
        (
            "isrc-field-adds-isrcs-for-tags-and-naming",
            _includes_profile(fields=(ManagedFieldSettings(field="isrc"),)),
            set(_BASE_INCLUDES) | {"isrcs"},
        ),
        (
            "disabled-mode-field-adds-nothing",
            _includes_profile(
                fields=(ManagedFieldSettings(field="label", mode="disabled"),)
            ),
            set(_BASE_INCLUDES),
        ),
        (
            "preserved-field-adds-nothing",
            _includes_profile(
                fields=(ManagedFieldSettings(field="isrc"),),
                preserve_fields=("isrc",),
            ),
            set(_BASE_INCLUDES),
        ),
        (
            "translate-with-locales-adds-aliases-for-naming-and-artist-credits",
            _includes_profile(
                translate_names=True, preferred_locales=("en-GB",)
            ),
            set(_BASE_INCLUDES) | {"aliases"},
        ),
        (
            "translate-without-locales-adds-nothing",
            _includes_profile(translate_names=True),
            set(_BASE_INCLUDES),
        ),
        (
            "locales-without-translate-add-nothing",
            _includes_profile(preferred_locales=("en-GB",)),
            set(_BASE_INCLUDES),
        ),
        (
            "relationships-add-6-rels-for-credits-and-work-fields",
            _includes_profile(
                relationships_enabled=True,
                relationship_types=("composer", "performer"),
            ),
            set(_BASE_INCLUDES) | set(_RELATIONSHIP_INCLUDES),
        ),
        (
            "relationships-without-types-add-nothing",
            _includes_profile(relationships_enabled=True),
            set(_BASE_INCLUDES),
        ),
        (
            "musicbrainz-genres-add-genres-for-genre-projection",
            _includes_profile(genres_enabled=True),
            set(_BASE_INCLUDES) | {"genres"},
        ),
        (
            "genres-without-musicbrainz-source-add-nothing",
            _includes_profile(
                genres_enabled=True, genre_sources=("listenbrainz",)
            ),
            set(_BASE_INCLUDES),
        ),
        (
            "everything-adds-all-13",
            _everything_profile(),
            set(_BASE_INCLUDES)
            | {"labels", "isrcs", "aliases", "genres"}
            | set(_RELATIONSHIP_INCLUDES),
        ),
        (
            "metadata-off-keeps-aliases-and-genres-for-naming-and-genre-projection",
            _includes_profile(
                metadata_enabled=False,
                translate_names=True,
                preferred_locales=("en",),
                genres_enabled=True,
            ),
            set(_BASE_INCLUDES) | {"aliases", "genres"},
        ),
    ]


@pytest.mark.parametrize(
    ("case", "profile", "expected"),
    _INCLUDES_CASES,
    ids=[case[0] for case in _INCLUDES_CASES],
)
def test_required_includes_table(case, profile, expected) -> None:
    includes = set(_required_includes(profile))
    assert includes == expected
    if case == "everything-adds-all-13":
        assert len(includes) == 13


@pytest.mark.asyncio
async def test_metadata_disabled_build_still_wires_mode_consumed_parts_for_naming() -> (
    None
):
    profile = picard_style_organizer_profile()
    profile.metadata.enabled = False
    service, _, musicbrainz = _service(_identity())

    await service.build(local_album_id="album-1", profile=profile)

    includes = set(musicbrainz.get_canonical_release.await_args.kwargs["includes"])
    assert includes == (
        set(_BASE_INCLUDES)
        | {"labels", "isrcs", "genres"}
        | set(_RELATIONSHIP_INCLUDES)
    )
