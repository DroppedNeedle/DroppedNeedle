"""Project accepted local identities into immutable Picard-style metadata."""

from __future__ import annotations

import hashlib
import json
import re
import time
import uuid
from collections.abc import Callable

import msgspec

from api.v1.schemas.library_management import LibraryManagementProfile
from core.exceptions import ProviderIdentityRequiredError, ResourceNotFoundError
from infrastructure.persistence.native_library_store import NativeLibraryStore
from infrastructure.queue.priority_queue import RequestPriority
from models.library_management import LibraryManagementMetadataSnapshot
from models.edition_management import CustomEditionManifest
from models.library_management_canonical import (
    AcceptedAlbumManagementIdentity,
    AcceptedTrackManagementIdentity,
    CanonicalArtistCredit,
    CanonicalDate,
    CanonicalIdentifierSet,
    CanonicalGenre,
    CanonicalLabel,
    CanonicalMedium,
    CanonicalRelationshipCredit,
    CanonicalReleaseDocument,
    CanonicalReleaseProjection,
    CanonicalTrackDocument,
    IncomingTrackManagementMapping,
)
from repositories.musicbrainz_base import MbSourceContext
from repositories.protocols.musicbrainz_management import (
    CanonicalMusicBrainzRepositoryProtocol,
    MbManagementArtist,
    MbManagementArtistCredit,
    MbManagementRelation,
    MbManagementRelease,
    MbManagementTrack,
)

_SNAPSHOT_NAMESPACE = uuid.UUID("46b91823-1eb7-50cf-9fed-522768569a67")
_VARIOUS_ARTISTS_MBID = "89ad4ac3-39f7-470e-963a-56509c546377"
_PROVIDER_NOTES = "MusicBrainz JSON API; live surface verified 2026-07-21"
_DATE_PATTERN = re.compile(r"^(\d{4})(?:-(\d{2})(?:-(\d{2}))?)?$")
_BASE_INCLUDES = {"artist-credits", "recordings", "release-groups"}
_RELATIONSHIP_INCLUDES = {
    "artist-rels",
    "recording-level-rels",
    "recording-rels",
    "release-group-level-rels",
    "work-level-rels",
    "work-rels",
}
_RELATIONSHIP_ROLE = {
    "arranger": "arranger",
    "composer": "composer",
    "conductor": "conductor",
    "instrument": "performer",
    "lyricist": "lyricist",
    "performer": "performer",
    "producer": "producer",
    "remixer": "remixer",
    "vocal": "performer",
}
# Matches Lidarr SkyHook's organizer media filter at develop commit
# d88cbcec050c04b3064c9f36221edac152e241e2, verified 2026-08-12.
_NON_AUDIO_ORGANIZATION_FORMATS = frozenset(
    {"DVD", "DVD-Video", "Blu-ray", "HD-DVD", "VCD", "SVCD", "UMD", "VHS"}
)


def _organization_audio_medium_count(release: MbManagementRelease) -> int:
    return sum(
        1
        for medium in release.media
        if medium.format not in _NON_AUDIO_ORGANIZATION_FORMATS
        and any(track.title != "[data track]" for track in medium.tracks)
    )


def _canonical_json(value: object) -> str:
    return json.dumps(
        msgspec.to_builtins(value),
        ensure_ascii=False,
        separators=(",", ":"),
        sort_keys=True,
    )


def _canonical_date(value: str | None) -> CanonicalDate | None:
    if not value:
        return None
    match = _DATE_PATTERN.fullmatch(value)
    if match is None:
        return None
    if match.group(3) is not None:
        precision = "day"
    elif match.group(2) is not None:
        precision = "month"
    else:
        precision = "year"
    return CanonicalDate(value=value, precision=precision)


def _unique(values: list[str]) -> tuple[str, ...]:
    return tuple(dict.fromkeys(value for value in values if value))


def _select_alias(artist: MbManagementArtist, preferred_locales: tuple[str, ...]):
    for preferred in preferred_locales:
        wanted = preferred.casefold()
        language = wanted.split("-", 1)[0]
        candidates = [
            alias
            for alias in artist.aliases
            if alias.locale
            and (
                alias.locale.casefold() == wanted
                or alias.locale.casefold().split("-", 1)[0] == language
            )
        ]
        if candidates:
            return min(
                enumerate(candidates),
                key=lambda value: (not bool(value[1].primary), value[0]),
            )[1]
    return None


def _artist_credit(
    credit: MbManagementArtistCredit,
    *,
    standardization: str,
    translate_names: bool,
    preferred_locales: tuple[str, ...],
) -> CanonicalArtistCredit:
    artist = credit.artist
    credited_name = credit.name or artist.name
    alias = _select_alias(artist, preferred_locales) if translate_names else None
    if alias is not None:
        display_name = alias.name or credited_name or artist.name
        sort_name = alias.sort_name or artist.sort_name or display_name
    elif standardization == "canonical":
        display_name = artist.name or credited_name
        sort_name = artist.sort_name or display_name
    else:
        display_name = credited_name or artist.name
        sort_name = artist.sort_name or display_name
    return CanonicalArtistCredit(
        display_name=display_name,
        credited_name=credited_name,
        canonical_name=artist.name or credited_name,
        sort_name=sort_name,
        artist_mbid=artist.id,
        join_phrase=credit.joinphrase,
    )


def _artist_credits(
    credits: list[MbManagementArtistCredit], profile: LibraryManagementProfile
) -> tuple[CanonicalArtistCredit, ...]:
    settings = profile.metadata.artist_credits
    locales = tuple(settings.preferred_locales)
    return tuple(
        _artist_credit(
            credit,
            standardization=settings.standardization,
            translate_names=settings.translate_names,
            preferred_locales=locales,
        )
        for credit in credits
        if credit.name or credit.artist.name
    )


def _relationship_credit(
    relation: MbManagementRelation,
    *,
    role: str,
    profile: LibraryManagementProfile,
) -> CanonicalRelationshipCredit | None:
    artist = relation.artist
    if artist is None or not (artist.id or artist.name):
        return None
    settings = profile.metadata.artist_credits
    alias = (
        _select_alias(artist, tuple(settings.preferred_locales))
        if settings.translate_names
        else None
    )
    display_name = (
        alias.name
        if alias is not None and alias.name
        else artist.name or relation.target_credit
    )
    sort_name = (
        alias.sort_name
        if alias is not None and alias.sort_name
        else artist.sort_name or display_name
    )
    return CanonicalRelationshipCredit(
        role=role,
        source_type=relation.type,
        display_name=display_name,
        canonical_name=artist.name or display_name,
        sort_name=sort_name,
        artist_mbid=artist.id,
        attributes=tuple(relation.attributes),
        begin_date=relation.begin,
        end_date=relation.end,
    )


def _track_relationships(
    track: MbManagementTrack, profile: LibraryManagementProfile
) -> tuple[CanonicalRelationshipCredit, ...]:
    settings = profile.metadata.relationships
    if not profile.metadata.enabled or not settings.enabled:
        return ()
    enabled = set(settings.types)
    result: list[CanonicalRelationshipCredit] = []
    relations = list(track.recording.relations)
    for relation in track.recording.relations:
        if relation.work is not None:
            relations.extend(relation.work.relations)
    for relation in relations:
        role = _RELATIONSHIP_ROLE.get(relation.type, "other")
        if role not in enabled:
            continue
        value = _relationship_credit(relation, role=role, profile=profile)
        if value is not None and value not in result:
            result.append(value)
    return tuple(result)


def _work_values(track: MbManagementTrack) -> tuple[str | None, tuple[str, ...]]:
    works = [
        relation.work
        for relation in track.recording.relations
        if relation.work is not None and relation.work.id
    ]
    title = works[0].title if works and works[0].title else None
    return title, _unique([work.id for work in works])


def _required_includes(profile: LibraryManagementProfile) -> tuple[str, ...]:
    includes = set(_BASE_INCLUDES)
    fields = {
        field.field
        for field in profile.metadata.fields
        if field.mode != "disabled"
        and field.field not in profile.metadata.preserve_fields
    }
    if fields.intersection({"label", "catalog_number"}):
        includes.add("labels")
    if "isrc" in fields:
        includes.add("isrcs")
    if (
        profile.metadata.artist_credits.translate_names
        and profile.metadata.artist_credits.preferred_locales
    ):
        includes.add("aliases")
    if profile.metadata.relationships.enabled and profile.metadata.relationships.types:
        includes.update(_RELATIONSHIP_INCLUDES)
    if profile.genres.enabled and "musicbrainz" in profile.genres.sources:
        includes.add("genres")
    return tuple(sorted(includes))


class CanonicalReleaseMetadataService:
    def __init__(
        self,
        store: NativeLibraryStore,
        musicbrainz: CanonicalMusicBrainzRepositoryProtocol,
        *,
        clock: Callable[[], float] = time.time,
    ) -> None:
        self._store = store
        self._musicbrainz = musicbrainz
        self._clock = clock

    async def build(
        self,
        *,
        local_album_id: str,
        profile: LibraryManagementProfile,
        local_track_ids: tuple[str, ...] | None = None,
        priority: RequestPriority = RequestPriority.USER_INITIATED,
        bypass_cache: bool = False,
    ) -> CanonicalReleaseProjection:
        identity = await self._store.get_accepted_library_management_identity(
            local_album_id, local_track_ids=local_track_ids
        )
        if identity is None:
            raise ResourceNotFoundError("Library album not found.")

        return await self.build_from_identity(
            identity=identity,
            profile=profile,
            priority=priority,
            bypass_cache=bypass_cache,
        )

    async def build_from_identity(
        self,
        *,
        identity: AcceptedAlbumManagementIdentity,
        profile: LibraryManagementProfile,
        priority: RequestPriority = RequestPriority.USER_INITIATED,
        bypass_cache: bool = False,
    ) -> CanonicalReleaseProjection:
        if identity.identity_kind == "custom_edition":
            self._require_current_custom_identity(identity)
            state = await self._store.get_custom_edition_state(identity.local_album_id)
            if (
                state is None
                or state.stale
                or state.manifest.id != identity.custom_manifest_id
            ):
                raise ProviderIdentityRequiredError(
                    "The Custom edition changed after it was sealed; review and reseal it before managing this album."
                )
            document = self._project_custom(identity, state.manifest)
            return await self._snapshot(identity, profile, (), document)
        self._require_complete_identity(identity, None)
        assert identity.release_mbid is not None
        includes = _required_includes(profile)
        release = await self._fetch_release(
            identity.release_mbid,
            profile=profile,
            includes=includes,
            priority=priority,
            bypass_cache=bypass_cache,
        )
        if release is None:
            raise ProviderIdentityRequiredError(
                "The selected MusicBrainz release no longer exists; review this album's identity."
            )
        document = self._project(identity, release, profile)
        return await self._snapshot(identity, profile, includes, document)

    async def build_for_import(
        self,
        *,
        local_album_id: str,
        release_group_mbid: str,
        release_mbid: str,
        mappings: tuple[IncomingTrackManagementMapping, ...],
        profile: LibraryManagementProfile,
        priority: RequestPriority = RequestPriority.BACKGROUND_SYNC,
        source_context: MbSourceContext | None = None,
    ) -> CanonicalReleaseProjection:
        """Resolve verified incoming positions to exact release-track identities."""

        if not mappings:
            raise ProviderIdentityRequiredError(
                "Every automatic import file needs an accepted release-track mapping."
            )
        includes = _required_includes(profile)
        release = await self._fetch_release(
            release_mbid,
            profile=profile,
            includes=includes,
            priority=priority,
            bypass_cache=False,
            source_context=source_context,
        )
        if release is None:
            raise ProviderIdentityRequiredError(
                "The selected MusicBrainz release no longer exists; review this import."
            )
        if release.release_group.id != release_group_mbid:
            raise ProviderIdentityRequiredError(
                "The selected release no longer belongs to the requested release group."
            )
        by_position = {
            (medium.position, track.position): track
            for medium in release.media
            for track in medium.tracks
        }
        by_id = {
            track.id: (medium.position, track)
            for medium in release.media
            for track in medium.tracks
            if track.id
        }
        accepted: list[AcceptedTrackManagementIdentity] = []
        used_release_tracks: set[str] = set()
        for mapping in mappings:
            positioned = by_position.get(
                (mapping.medium_position, mapping.release_track_position)
            )
            identified = by_id.get(mapping.release_track_mbid or "")
            if identified is not None:
                identified_medium, provider_track = identified
                if (
                    identified_medium != mapping.medium_position
                    or provider_track.position != mapping.release_track_position
                ):
                    raise ProviderIdentityRequiredError(
                        "An incoming release-track mapping conflicts with its position."
                    )
            else:
                provider_track = positioned
            if provider_track is None or not provider_track.id:
                raise ProviderIdentityRequiredError(
                    "An incoming file could not be mapped to the selected release."
                )
            if (
                positioned is not None
                and positioned.id
                and positioned.id != provider_track.id
            ):
                raise ProviderIdentityRequiredError(
                    "An incoming release-track mapping is ambiguous."
                )
            if (
                mapping.recording_mbid
                and provider_track.recording.id != mapping.recording_mbid
            ):
                raise ProviderIdentityRequiredError(
                    "An incoming recording does not match the selected release track."
                )
            if provider_track.id in used_release_tracks:
                raise ProviderIdentityRequiredError(
                    "Two incoming files map to the same selected release track."
                )
            used_release_tracks.add(provider_track.id)
            accepted.append(
                AcceptedTrackManagementIdentity(
                    local_track_id=mapping.local_track_id,
                    track_revision=1,
                    identity_revision=1,
                    recording_mbid=provider_track.recording.id,
                    release_mbid=release.id,
                    release_track_mbid=provider_track.id,
                    medium_position=mapping.medium_position,
                    release_track_position=mapping.release_track_position,
                )
            )
        identity = AcceptedAlbumManagementIdentity(
            local_album_id=local_album_id,
            album_revision=1,
            identity_revision=1,
            release_group_mbid=release_group_mbid,
            release_mbid=release.id,
            tracks=tuple(accepted),
        )
        document = self._project(identity, release, profile)
        return await self._snapshot(identity, profile, includes, document)

    async def _fetch_release(
        self,
        release_mbid: str,
        *,
        profile: LibraryManagementProfile,
        includes: tuple[str, ...],
        priority: RequestPriority,
        bypass_cache: bool,
        source_context: MbSourceContext | None = None,
    ) -> MbManagementRelease | None:
        locales = tuple(profile.metadata.artist_credits.preferred_locales)
        return await self._musicbrainz.get_canonical_release(
            release_mbid,
            includes=includes,
            preferred_locales=locales,
            artist_standardization=profile.metadata.artist_credits.standardization,
            priority=priority,
            bypass_cache=bypass_cache,
            source_context=source_context,
        )

    @staticmethod
    def _require_complete_identity(
        identity: AcceptedAlbumManagementIdentity,
        requested_track_ids: tuple[str, ...] | None,
    ) -> None:
        if identity.identity_kind != "exact_release":
            raise ProviderIdentityRequiredError(
                "This operation requires an accepted exact MusicBrainz release."
            )
        if not identity.release_group_mbid or not identity.release_mbid:
            raise ProviderIdentityRequiredError(
                "Select and accept a specific MusicBrainz release before managing this album."
            )
        if identity.identity_revision is None:
            raise ProviderIdentityRequiredError(
                "The selected MusicBrainz release has no accepted identity revision."
            )
        if requested_track_ids is not None and {
            track.local_track_id for track in identity.tracks
        } != set(requested_track_ids):
            raise ProviderIdentityRequiredError(
                "One or more selected files are not part of this accepted album."
            )
        if not identity.tracks:
            raise ProviderIdentityRequiredError(
                "The album has no files eligible for Library Management."
            )
        release_tracks: set[str] = set()
        for track in identity.tracks:
            if (
                track.identity_revision is None
                or not track.recording_mbid
                or not track.release_track_mbid
                or track.release_mbid != identity.release_mbid
            ):
                raise ProviderIdentityRequiredError(
                    "Every selected file needs an accepted release-track mapping for the selected MusicBrainz release."
                )
            if track.release_track_mbid in release_tracks:
                raise ProviderIdentityRequiredError(
                    "Two selected files map to the same MusicBrainz release track."
                )
            release_tracks.add(track.release_track_mbid)

    @staticmethod
    def _require_current_custom_identity(
        identity: AcceptedAlbumManagementIdentity,
    ) -> None:
        if (
            not identity.release_group_mbid
            or identity.identity_revision is None
            or not identity.custom_manifest_id
            or identity.custom_manifest_stale
            or not identity.tracks
        ):
            raise ProviderIdentityRequiredError(
                "The Custom edition changed after it was sealed; review and reseal it before managing this album."
            )
        if any(
            track.identity_revision is None and track.recording_mbid
            for track in identity.tracks
        ):
            raise ProviderIdentityRequiredError(
                "The Custom edition's recognized recording evidence changed after sealing."
            )

    @staticmethod
    def _project_custom(
        identity: AcceptedAlbumManagementIdentity,
        manifest: CustomEditionManifest,
    ) -> CanonicalReleaseDocument:
        selected_ids = {value.local_track_id for value in identity.tracks}
        manifest_tracks = [
            value for value in manifest.tracks if value.local_track_id in selected_ids
        ]
        if len(manifest_tracks) != len(selected_ids):
            raise ProviderIdentityRequiredError(
                "One or more selected files are not part of the sealed Custom edition."
            )
        try:
            album_metadata = json.loads(manifest.album_metadata_json)
            track_metadata = {
                value.local_track_id: json.loads(value.metadata_json)
                for value in manifest_tracks
            }
        except (TypeError, ValueError) as error:
            raise ProviderIdentityRequiredError(
                "The sealed Custom edition metadata is invalid; review and reseal it."
            ) from error
        if not isinstance(album_metadata, dict) or any(
            not isinstance(value, dict) for value in track_metadata.values()
        ):
            raise ProviderIdentityRequiredError(
                "The sealed Custom edition metadata is invalid; review and reseal it."
            )

        album_artist = CanonicalArtistCredit(
            display_name=manifest.album_artist_name,
            credited_name=manifest.album_artist_name,
            canonical_name=manifest.album_artist_name,
            sort_name=str(
                album_metadata.get("album_artist_sort_name")
                or manifest.album_artist_name
            ),
            artist_mbid=manifest.artist_mbid,
        )
        disc_counts: dict[int, int] = {}
        for value in manifest.tracks:
            disc_counts[value.disc_number] = disc_counts.get(value.disc_number, 0) + 1
        total_discs = len(disc_counts)
        by_disc: dict[int, list[CanonicalTrackDocument]] = {}
        accepted = {value.local_track_id: value for value in identity.tracks}
        for value in manifest_tracks:
            metadata = track_metadata[value.local_track_id]
            mapped = accepted[value.local_track_id]
            artist = CanonicalArtistCredit(
                display_name=value.artist_name,
                credited_name=value.artist_name,
                canonical_name=value.artist_name,
                sort_name=str(metadata.get("artist_sort") or value.artist_name),
                artist_mbid=value.artist_mbid,
            )
            by_disc.setdefault(value.disc_number, []).append(
                CanonicalTrackDocument(
                    local_track_id=value.local_track_id,
                    source_track_revision=value.source_track_revision,
                    source_identity_revision=value.source_identity_revision or 0,
                    title=value.title,
                    artist_credits=(artist,),
                    relationship_credits=(),
                    identifiers=CanonicalIdentifierSet(
                        release_group_mbid=manifest.release_group_mbid,
                        release_mbid=None,
                        recording_mbid=value.recording_mbid,
                        album_artist_mbids=(manifest.artist_mbid,)
                        if manifest.artist_mbid
                        else (),
                        artist_mbids=(value.artist_mbid,) if value.artist_mbid else (),
                    ),
                    track_number=value.track_number,
                    track_number_text=str(value.track_number),
                    total_tracks=disc_counts[value.disc_number],
                    disc_number=value.disc_number,
                    total_discs=total_discs,
                    disc_subtitle=(
                        str(metadata["disc_subtitle"])
                        if metadata.get("disc_subtitle")
                        else None
                    ),
                    media_format=value.file_format or None,
                    duration_milliseconds=(
                        round(value.duration_seconds * 1000)
                        if value.duration_seconds is not None
                        else None
                    ),
                    genres=(
                        (
                            CanonicalGenre(
                                display_name=str(metadata["genre"]),
                                provider_entity="custom_edition",
                            ),
                        )
                        if metadata.get("genre")
                        else ()
                    ),
                )
            )
        media = tuple(
            CanonicalMedium(
                position=disc_number,
                title=None,
                format=None,
                track_count=disc_counts[disc_number],
                tracks=tuple(
                    sorted(
                        by_disc.get(disc_number, []),
                        key=lambda track: track.track_number,
                    )
                ),
            )
            for disc_number in sorted(by_disc)
        )
        year = album_metadata.get("year")
        original_date = album_metadata.get("original_release_date")
        primary_genre = album_metadata.get("primary_genre")
        return CanonicalReleaseDocument(
            local_album_id=identity.local_album_id,
            source_album_revision=manifest.source_album_revision,
            source_identity_revision=manifest.source_identity_revision or 0,
            title=manifest.album_title,
            artist_credits=(album_artist,),
            identifiers=CanonicalIdentifierSet(
                release_group_mbid=manifest.release_group_mbid,
                release_mbid=None,
                album_artist_mbids=(manifest.artist_mbid,)
                if manifest.artist_mbid
                else (),
            ),
            date=_canonical_date(str(year)) if year else None,
            original_date=_canonical_date(str(original_date))
            if original_date
            else None,
            release_status=None,
            release_country=None,
            primary_release_type=None,
            secondary_release_types=(),
            packaging=None,
            barcode=None,
            asin=None,
            language=None,
            script=None,
            compilation=bool(album_metadata.get("is_compilation")),
            total_discs=total_discs,
            labels=(),
            genres=(
                (
                    CanonicalGenre(
                        display_name=str(primary_genre),
                        provider_entity="custom_edition",
                    ),
                )
                if primary_genre
                else ()
            ),
            media=media,
            organization_audio_medium_count=total_discs,
            identity_kind="custom_edition",
            custom_manifest_id=manifest.id,
        )

    def _project(
        self,
        identity: AcceptedAlbumManagementIdentity,
        release: MbManagementRelease,
        profile: LibraryManagementProfile,
    ) -> CanonicalReleaseDocument:
        if release.id != identity.release_mbid:
            raise ProviderIdentityRequiredError(
                "MusicBrainz returned a different release than the accepted selection."
            )
        if release.release_group.id != identity.release_group_mbid:
            raise ProviderIdentityRequiredError(
                "The accepted release no longer belongs to the selected release group."
            )

        provider_tracks = {
            track.id: (medium, track)
            for medium in release.media
            for track in medium.tracks
            if track.id
        }
        mappings = {
            track.release_track_mbid: track
            for track in identity.tracks
            if track.release_track_mbid
        }
        total_discs = len(release.media)
        album_credits = _artist_credits(release.artist_credit, profile)
        album_artist_ids = _unique([credit.artist_mbid for credit in album_credits])
        projected_media: list[CanonicalMedium] = []
        for medium in release.media:
            projected_tracks: list[CanonicalTrackDocument] = []
            for provider_track in medium.tracks:
                mapping = mappings.get(provider_track.id)
                if mapping is None:
                    continue
                self._validate_mapping(mapping, medium.position, provider_track)
                track_credits = _artist_credits(
                    provider_track.artist_credit
                    or provider_track.recording.artist_credit
                    or release.artist_credit,
                    profile,
                )
                work_title, work_ids = _work_values(provider_track)
                projected_tracks.append(
                    CanonicalTrackDocument(
                        local_track_id=mapping.local_track_id,
                        source_track_revision=mapping.track_revision,
                        source_identity_revision=mapping.identity_revision or 0,
                        title=provider_track.title or provider_track.recording.title,
                        artist_credits=track_credits,
                        relationship_credits=_track_relationships(
                            provider_track, profile
                        ),
                        identifiers=CanonicalIdentifierSet(
                            release_group_mbid=release.release_group.id,
                            release_mbid=release.id,
                            release_track_mbid=provider_track.id,
                            recording_mbid=provider_track.recording.id,
                            album_artist_mbids=album_artist_ids,
                            artist_mbids=_unique(
                                [credit.artist_mbid for credit in track_credits]
                            ),
                            work_mbids=work_ids,
                            isrcs=_unique(provider_track.recording.isrcs),
                        ),
                        track_number=provider_track.position,
                        track_number_text=provider_track.number
                        or str(provider_track.position),
                        total_tracks=medium.track_count or len(medium.tracks),
                        disc_number=medium.position,
                        total_discs=total_discs,
                        disc_subtitle=medium.title or None,
                        media_format=medium.format,
                        duration_milliseconds=provider_track.length
                        or provider_track.recording.length,
                        work_title=work_title,
                        genres=tuple(
                            CanonicalGenre(
                                display_name=genre.name,
                                provider_entity="recording",
                                genre_mbid=genre.id or None,
                                count=genre.count,
                            )
                            for genre in provider_track.recording.genres
                            if genre.name
                        ),
                    )
                )
            if projected_tracks:
                projected_media.append(
                    CanonicalMedium(
                        position=medium.position,
                        title=medium.title or None,
                        format=medium.format,
                        track_count=medium.track_count or len(medium.tracks),
                        tracks=tuple(projected_tracks),
                    )
                )

        if sum(len(medium.tracks) for medium in projected_media) != len(
            identity.tracks
        ):
            missing = sorted(set(mappings) - set(provider_tracks))
            raise ProviderIdentityRequiredError(
                "Accepted release-track mappings are missing from the selected MusicBrainz release.",
                details={"missing_release_track_mbids": missing},
            )

        return CanonicalReleaseDocument(
            local_album_id=identity.local_album_id,
            source_album_revision=identity.album_revision,
            source_identity_revision=identity.identity_revision or 0,
            title=release.title,
            artist_credits=album_credits,
            identifiers=CanonicalIdentifierSet(
                release_group_mbid=release.release_group.id,
                release_mbid=release.id,
                album_artist_mbids=album_artist_ids,
            ),
            date=_canonical_date(release.date),
            original_date=_canonical_date(release.release_group.first_release_date),
            release_status=release.status,
            release_country=release.country,
            primary_release_type=release.release_group.primary_type,
            secondary_release_types=tuple(release.release_group.secondary_types),
            packaging=release.packaging,
            barcode=release.barcode,
            asin=release.asin,
            language=release.text_representation.language,
            script=release.text_representation.script,
            compilation=album_artist_ids == (_VARIOUS_ARTISTS_MBID,),
            total_discs=total_discs,
            labels=tuple(
                CanonicalLabel(
                    name=value.label.name if value.label is not None else None,
                    label_mbid=(value.label.id if value.label is not None else None),
                    catalog_number=value.catalog_number or None,
                )
                for value in release.label_info
            ),
            genres=tuple(
                CanonicalGenre(
                    display_name=genre.name,
                    provider_entity=entity,
                    genre_mbid=genre.id or None,
                    count=genre.count,
                )
                for entity, values in (
                    ("release", release.genres),
                    ("release_group", release.release_group.genres),
                )
                for genre in values
                if genre.name
            ),
            media=tuple(projected_media),
            album_disambiguation=release.release_group.disambiguation.strip(),
            organization_audio_medium_count=_organization_audio_medium_count(release),
        )

    @staticmethod
    def _validate_mapping(
        mapping: AcceptedTrackManagementIdentity,
        medium_position: int,
        track: MbManagementTrack,
    ) -> None:
        if track.recording.id != mapping.recording_mbid:
            raise ProviderIdentityRequiredError(
                "An accepted recording mapping no longer matches its MusicBrainz release track."
            )
        if (
            mapping.medium_position is not None
            and mapping.medium_position != medium_position
        ) or (
            mapping.release_track_position is not None
            and mapping.release_track_position != track.position
        ):
            raise ProviderIdentityRequiredError(
                "An accepted track position no longer matches the selected MusicBrainz release."
            )

    async def _snapshot(
        self,
        identity: AcceptedAlbumManagementIdentity,
        profile: LibraryManagementProfile,
        includes: tuple[str, ...],
        document: CanonicalReleaseDocument,
    ) -> CanonicalReleaseProjection:
        input_json = _canonical_json(
            {
                "identity": identity,
                "includes": includes,
                "artist_credits": profile.metadata.artist_credits,
                "relationships": profile.metadata.relationships,
                "musicbrainz_genres": profile.genres.enabled
                and "musicbrainz" in profile.genres.sources,
            }
        )
        input_hash = hashlib.sha256(input_json.encode("utf-8")).hexdigest()
        payload_json = _canonical_json(document)
        payload_hash = hashlib.sha256(payload_json.encode("utf-8")).hexdigest()
        snapshot_id = str(
            uuid.uuid5(
                _SNAPSHOT_NAMESPACE,
                f"{document.identity_kind}:{document.identifiers.release_mbid or document.custom_manifest_id}:{input_hash}:{payload_hash}",
            )
        )
        now = self._clock()
        custom = document.identity_kind == "custom_edition"
        snapshot = await self._store.put_management_metadata_snapshot(
            LibraryManagementMetadataSnapshot(
                id=snapshot_id,
                provider="droppedneedle" if custom else "musicbrainz",
                entity_kind="custom_edition" if custom else "release",
                entity_id=(
                    document.custom_manifest_id
                    if custom
                    else document.identifiers.release_mbid
                )
                or "",
                input_hash=input_hash,
                canonical_payload_json=payload_json,
                payload_sha256=payload_hash,
                fetched_at=now,
                expires_at=None if custom else now + 3600,
                provider_version_notes=(
                    "Immutable DroppedNeedle Custom edition manifest"
                    if custom
                    else _PROVIDER_NOTES
                ),
            )
        )
        return CanonicalReleaseProjection(
            document=document,
            metadata_snapshot_id=snapshot.id,
            input_hash=input_hash,
            payload_sha256=payload_hash,
        )
