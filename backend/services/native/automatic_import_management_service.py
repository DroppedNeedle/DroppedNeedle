"""Prepare immutable canonical projections for automatic import publication."""

from __future__ import annotations

import asyncio
import fnmatch
import hashlib
import os
from pathlib import Path, PurePosixPath
import shutil
import unicodedata
import uuid

import msgspec

from api.v1.schemas.library_management import profile_revision, settings_revision
from core.exceptions import (
    AudioFormatError,
    AutomaticManagementHoldError,
    ConfigurationError,
    ExternalServiceError,
    PathLimitExceededError,
    ProviderIdentityRequiredError,
    ScriptValidationError,
    StaleRevisionError,
    ValidationError,
)
from repositories.musicbrainz_base import MbSourceContext, capture_mb_source_context
from infrastructure.audio.metadata_engine import (
    AUDIO_EXTENSION_FORMATS,
    AudioMetadataEngine,
)
from infrastructure.queue.priority_queue import RequestPriority
from models.audio_metadata import (
    AudioMetadataDocument,
    AudioSemanticField,
    DesiredAudioDocument,
    DesiredAudioField,
)
from models.library_management import (
    BUNDLE_TOO_LARGE,
    FIELD_UNSUPPORTED_BY_FORMAT,
    FORMAT_UNSUPPORTED,
    INSUFFICIENT_SPACE,
    METADATA_UNAVAILABLE,
    PATH_TOO_LONG,
    PROFILE_CHANGED,
    ROOT_READ_ONLY,
    ROOT_UNAVAILABLE,
    SCRIPT_VALIDATION_FAILED,
    SIDECAR_COLLISION,
    SYMLINK_UNSUPPORTED,
    TRACK_NOT_MAPPED,
    LibraryManagementImportArtifact,
    LibraryManagementImportBundle,
    LibraryManagementImportFile,
    MANAGEMENT_RECYCLE_ROOT_ID,
)
from models.library_management_artwork import ExistingArtworkDescriptor, ArtworkOutput
from models.library_management_enrichment import (
    LyricsProjection,
    ReplayGainAnalysis,
    ReplayGainTrackResult,
)
from models.library_management_canonical import IncomingTrackManagementMapping
from models.library_management_planning import naming_policy_revision
from services.native.artwork_projection_service import (
    ArtworkProjectionService,
    desired_embedded_artwork,
)
from services.native.audio_write_planning_service import AudioWritePlanningService
from services.native.canonical_release_metadata_service import (
    CanonicalReleaseMetadataService,
)
from services.native.effective_metadata_projection_service import (
    EffectiveMetadataProjectionService,
)
from services.native.genre_projection_service import GenreProjectionService
from services.native.library_management_planner import LibraryManagementPlanner
from services.native.library_management_naming_policy import (
    management_naming_context,
    select_naming_script,
)
from services.native.library_management_profile_service import (
    LibraryManagementProfileService,
)
from services.native.lyrics_management_policy import (
    planned_lyrics_outputs,
    required_lyrics_outputs_available,
    synchronized_lyrics_supported,
)
from services.native.lyrics_projection_service import LyricsProjectionService
from services.native.managed_field_registry import canonical_track_values
from services.native.naming import NamingTemplateEngine
from services.native.replaygain_analysis_service import ReplayGainAnalysisService
from services.native.tagging_scripts import TaggingScriptEngine

_IMPORT_IDENTITY_NAMESPACE = uuid.UUID("04fd3ed7-4452-4ae9-a61f-12bdf9a78888")
_MAX_SIDECAR_ENTRIES = 10_000
_DISK_SAFETY_BYTES = 64 * 1024 * 1024


class AutomaticImportManagementService:
    def __init__(
        self,
        profiles: LibraryManagementProfileService,
        planner: LibraryManagementPlanner,
        canonical: CanonicalReleaseMetadataService,
        effective: EffectiveMetadataProjectionService,
        genres: GenreProjectionService,
        artwork: ArtworkProjectionService,
        audio: AudioMetadataEngine,
        write_planner: AudioWritePlanningService,
        naming: NamingTemplateEngine,
        tagging: TaggingScriptEngine,
        lyrics: LyricsProjectionService | None = None,
        replaygain: ReplayGainAnalysisService | None = None,
    ) -> None:
        self._profiles = profiles
        self._planner = planner
        self._canonical = canonical
        self._effective = effective
        self._genres = genres
        self._artwork = artwork
        self._audio = audio
        self._write_planner = write_planner
        self._naming = naming
        self._tagging = tagging
        self._lyrics = lyrics
        self._replaygain = replaygain

    async def prepare(
        self,
        bundle: LibraryManagementImportBundle,
        *,
        source_context: MbSourceContext | None = None,
    ) -> LibraryManagementImportBundle:
        source_context = source_context or capture_mb_source_context()
        managed = [value.pinned_profile is not None for value in bundle.files]
        if managed and all(managed):
            return bundle
        if any(managed):
            raise AutomaticManagementHoldError(
                PROFILE_CHANGED,
                "The sealed import projection is incomplete.",
            )
        trigger = bundle.origin
        prepared = list(bundle.files)
        automatic: dict[int, tuple[object, object]] = {}
        try:
            # F-NL-01 (DECISIONS-LIVE): an unmapped bonus file is imported
            # unmanaged by default and must not hold the mapped part of the
            # unit. Only management-eligible requests resolve a profile; the
            # identity check below stays at the managed-request boundary.
            managed_requests = [
                request
                for request in bundle.files
                if (
                    request.authoritative_mapping
                    and request.release_group_mbid
                    and request.release_mbid
                )
                or trigger == "edition_conversion"
            ]
            for request in managed_requests:
                root_id = (
                    request.replacement_root_id
                    if request.conversion_recycle_only
                    else request.destination_root_id
                )
                if root_id is None:
                    raise ConfigurationError(
                        "An edition-conversion recycle item lost its source root."
                    )
                resolved = (
                    self._profiles.prepare_conversion_profile(
                        root_id=root_id,
                        expected_policy_revision=bundle.policy_revision,
                    )
                    if trigger == "edition_conversion"
                    else self._profiles.prepare_automatic_profile(
                        root_id=root_id,
                        trigger=trigger,
                        expected_policy_revision=bundle.policy_revision,
                    )
                )
                if resolved is None:
                    continue
                settings, _assignment, profile, policy = resolved
                automatic[request.ordinal] = (
                    settings,
                    (profile, policy, self._planner.pin_profile(settings, profile)),
                )
            if not automatic:
                return bundle
            if any(
                value.conversion_recycle_only and value.ordinal not in automatic
                for value in bundle.files
            ):
                raise ConfigurationError(
                    "One import unit cannot cross automatic and unmanaged root assignments."
                )
            groups: dict[
                tuple[str, str, str, str, str, str],
                list[LibraryManagementImportFile],
            ] = {}
            for request in bundle.files:
                resolved = automatic.get(request.ordinal)
                if resolved is None:
                    continue
                request_settings, values = resolved
                profile, _policy, pinned = values
                if request.conversion_recycle_only:
                    continue
                if (
                    not request.authoritative_mapping
                    or not request.release_group_mbid
                    or not request.release_mbid
                    or not request.release_track_mbid
                ):
                    raise ProviderIdentityRequiredError(
                        "Every automatic import file needs an accepted release-track mapping."
                    )
                groups.setdefault(
                    (
                        request.destination_root_id,
                        request.release_group_mbid,
                        request.release_mbid,
                        profile_revision(profile),
                        settings_revision(request_settings),
                        naming_policy_revision(pinned),
                    ),
                    [],
                ).append(request)
            if len(groups) != 1:
                raise ConfigurationError(
                    "One automatic import unit must use one activated root and profile."
                )

            by_ordinal = {value.ordinal: value for value in prepared}
            for requests in groups.values():
                first = requests[0]
                settings, values = automatic[first.ordinal]
                profile, policy, pinned = values
                roots = {
                    value.id: Path(value.path)
                    for value in policy.settings.library_roots
                }
                root = roots[first.destination_root_id]
                mappings = tuple(
                    IncomingTrackManagementMapping(
                        local_track_id=self._synthetic_track_id(bundle, request),
                        medium_position=request.medium_position
                        or request.tag.disc_number
                        or 1,
                        release_track_position=request.release_track_position
                        or request.tag.track_number,
                        recording_mbid=request.recording_mbid,
                        release_track_mbid=request.release_track_mbid,
                    )
                    for request in requests
                )
                if any(value.release_track_position < 1 for value in mappings):
                    raise ProviderIdentityRequiredError(
                        "Every automatic import file needs an accepted track position."
                    )
                projection = await self._canonical.build_for_import(
                    local_album_id=self._synthetic_album_id(bundle, first),
                    release_group_mbid=str(first.release_group_mbid),
                    release_mbid=str(first.release_mbid),
                    mappings=mappings,
                    profile=profile,
                    priority=RequestPriority.BACKGROUND_SYNC,
                    source_context=source_context,
                )
                canonical_tracks = {
                    track.local_track_id: track
                    for medium in projection.document.media
                    for track in medium.tracks
                }
                current_by_ordinal = {
                    request.ordinal: await asyncio.to_thread(
                        self._audio.read, Path(request.input_path)
                    )
                    for request in sorted(requests, key=lambda value: value.ordinal)
                }
                replaygain_analysis: ReplayGainAnalysis | None = None
                replaygain_by_path: dict[str, ReplayGainTrackResult] = {}
                replaygain_settings = profile.enrichment.replaygain
                if (
                    replaygain_settings.enabled
                    and replaygain_settings.mode != "preserve"
                ):
                    if self._replaygain is None:
                        replaygain_analysis = ReplayGainAnalysis(
                            status="deferred",
                            reason="The ReplayGain analyzer is unavailable.",
                        )
                    else:
                        replaygain_analysis = await self._replaygain.analyze(
                            tuple(
                                Path(request.input_path)
                                for request in sorted(
                                    requests, key=lambda value: value.ordinal
                                )
                            ),
                            album_aware=replaygain_settings.album_aware,
                        )
                    if replaygain_analysis.status == "available":
                        replaygain_by_path = {
                            value.source_path: value
                            for value in replaygain_analysis.tracks
                        }
                    elif replaygain_settings.required:
                        raise AutomaticManagementHoldError(
                            METADATA_UNAVAILABLE,
                            "Required ReplayGain analysis failed for this import.",
                        )
                first_output_ordinal = min(value.ordinal for value in requests)
                sidecars_added = False
                for request in sorted(requests, key=lambda value: value.ordinal):
                    current = current_by_ordinal[request.ordinal]
                    canonical_track = canonical_tracks[
                        self._synthetic_track_id(bundle, request)
                    ]
                    (
                        desired_metadata,
                        desired,
                        artwork_outputs,
                        management_warnings,
                    ) = await self._project_file(
                        request=request,
                        current=current,
                        canonical_release=projection.document,
                        canonical_track=canonical_track,
                        profile=profile,
                        pinned=pinned,
                        replaygain_result=replaygain_by_path.get(request.input_path),
                        replaygain_analysis=replaygain_analysis,
                    )
                    plan = self._write_planner.plan(
                        current=current, desired=desired, profile=profile
                    )
                    if plan.blockers:
                        raise AutomaticManagementHoldError(
                            FIELD_UNSUPPORTED_BY_FORMAT,
                            "The file format cannot safely apply this profile: "
                            + "; ".join(plan.blockers),
                        )
                    destination_relative = self._destination_relative(
                        request,
                        current,
                        desired_metadata,
                        pinned,
                        root,
                        projection.document,
                        canonical_track,
                    )
                    artifacts: list[LibraryManagementImportArtifact] = []
                    if request.ordinal == first_output_ordinal:
                        artifacts.extend(
                            await self._external_artifacts(
                                artwork_outputs,
                                current,
                                desired_metadata,
                                pinned,
                                root,
                                destination_relative,
                                request.destination_root_id,
                            )
                        )
                        artifacts.extend(
                            await asyncio.to_thread(
                                self._sidecar_artifacts,
                                request,
                                profile,
                                destination_relative,
                            )
                        )
                        artifacts = self._coalesce_artifacts(artifacts)
                        sidecars_added = True
                    by_ordinal[request.ordinal] = msgspec.structs.replace(
                        request,
                        destination_relative_path=destination_relative,
                        release_group_mbid=projection.document.identifiers.release_group_mbid,
                        release_mbid=projection.document.identifiers.release_mbid,
                        recording_mbid=canonical_track.identifiers.recording_mbid,
                        release_track_mbid=canonical_track.identifiers.release_track_mbid,
                        medium_position=canonical_track.disc_number,
                        release_track_position=canonical_track.track_number,
                        baseline_relative_path=request.destination_relative_path,
                        desired_document=desired,
                        pinned_profile=pinned,
                        metadata_snapshot_id=projection.metadata_snapshot_id,
                        projection_hash=projection.payload_sha256,
                        settings_revision=settings_revision(settings),
                        naming_policy_revision=naming_policy_revision(pinned),
                        undo_retention_days=settings.undo_retention_days,
                        management_warnings=management_warnings,
                        artifacts=tuple(artifacts),
                    )
                # F-109: validated-input contracts must not vanish under -O.
                if not sidecars_added:
                    raise ValidationError(
                        "A conversion import lost its sealed sidecar artifacts."
                    )
                for request in sorted(
                    (value for value in bundle.files if value.conversion_recycle_only),
                    key=lambda value: value.ordinal,
                ):
                    recycle_settings, recycle_values = automatic[request.ordinal]
                    recycle_profile, _recycle_policy, recycle_pinned = recycle_values
                    if profile_revision(recycle_profile) != profile_revision(
                        profile
                    ) or settings_revision(recycle_settings) != settings_revision(
                        settings
                    ):
                        raise ConfigurationError(
                            "One edition conversion must use one current management profile."
                        )
                    by_ordinal[request.ordinal] = msgspec.structs.replace(
                        request,
                        baseline_relative_path=str(request.replacement_relative_path),
                        desired_document=DesiredAudioDocument(fields=()),
                        pinned_profile=recycle_pinned,
                        metadata_snapshot_id=projection.metadata_snapshot_id,
                        projection_hash=projection.payload_sha256,
                        settings_revision=settings_revision(recycle_settings),
                        naming_policy_revision=naming_policy_revision(recycle_pinned),
                        undo_retention_days=recycle_settings.undo_retention_days,
                    )
            result = msgspec.structs.replace(
                bundle,
                files=tuple(by_ordinal[value.ordinal] for value in bundle.files),
            )
            if bundle.conversion_recycle_bin_path is not None:
                roots[MANAGEMENT_RECYCLE_ROOT_ID] = Path(
                    bundle.conversion_recycle_bin_path
                )
            await self._validate_capacity(result, roots)
            contract_hash = hashlib.sha256(
                msgspec.json.encode(result.files)
            ).hexdigest()
            return msgspec.structs.replace(
                result,
                idempotency_key=(
                    f"{bundle.idempotency_key}:management:{contract_hash}"
                ),
            )
        except StaleRevisionError as error:
            raise AutomaticManagementHoldError(PROFILE_CHANGED, str(error)) from error
        except ProviderIdentityRequiredError as error:
            raise AutomaticManagementHoldError(TRACK_NOT_MAPPED, str(error)) from error
        except ExternalServiceError as error:
            raise AutomaticManagementHoldError(
                METADATA_UNAVAILABLE,
                "Required metadata is temporarily unavailable. Retry this import later.",
            ) from error
        except AudioFormatError as error:
            raise AutomaticManagementHoldError(
                FORMAT_UNSUPPORTED, str(error)
            ) from error
        except PathLimitExceededError as error:
            raise AutomaticManagementHoldError(PATH_TOO_LONG, str(error)) from error
        except ConfigurationError as error:
            raise AutomaticManagementHoldError(
                PROFILE_CHANGED,
                "The active Library Management profile needs review before this import "
                f"can continue: {error}",
            ) from error
        except ScriptValidationError as error:
            raise AutomaticManagementHoldError(
                SCRIPT_VALIDATION_FAILED,
                "The active Library Management profile could not safely process this "
                f"file: {error}",
            ) from error
        except ValidationError as error:
            raise AutomaticManagementHoldError(
                FIELD_UNSUPPORTED_BY_FORMAT, str(error)
            ) from error

    async def _project_file(
        self,
        *,
        request: LibraryManagementImportFile,
        current,
        canonical_release,
        canonical_track,
        profile,
        pinned,
        replaygain_result: ReplayGainTrackResult | None = None,
        replaygain_analysis: ReplayGainAnalysis | None = None,
    ) -> tuple[
        AudioMetadataDocument,
        DesiredAudioDocument,
        tuple[ArtworkOutput, ...],
        tuple[str, ...],
    ]:
        existing = {field.name: field.value for field in current.metadata.fields}
        canonical_values = canonical_track_values(canonical_release, canonical_track)
        genre_projection = await self._genres.project(
            settings=profile.genres,
            canonical_release=canonical_release,
            existing_genres=current.metadata.strings_for("genre"),
        )
        if profile.enrichment.lyrics.enabled and self._lyrics is not None:
            lyrics_projection = await self._lyrics.project(
                settings=profile.enrichment.lyrics,
                canonical_release=canonical_release,
                canonical_track=canonical_track,
                duration_seconds=current.technical.duration_seconds,
            )
        elif profile.enrichment.lyrics.enabled:
            lyrics_projection = LyricsProjection(
                status="deferred",
                reason="The lyrics provider is not available.",
            )
        else:
            lyrics_projection = LyricsProjection(status="disabled")
        synced_lyrics_supported = synchronized_lyrics_supported(
            current.probe.detected_format,
            wav_tag_policy=profile.metadata.format_compatibility.wav_tag_policy,
        )
        if (
            profile.enrichment.lyrics.enabled
            and profile.enrichment.lyrics.required
            and not required_lyrics_outputs_available(
                profile.enrichment.lyrics,
                lyrics_projection,
                existing,
                synchronized_supported=synced_lyrics_supported,
            )
        ):
            raise AutomaticManagementHoldError(
                METADATA_UNAVAILABLE,
                "Required lyrics are unavailable for this import.",
            )
        replaygain_settings = profile.enrichment.replaygain
        replaygain_values: tuple[tuple[str, float | None], ...] = (
            (
                "replaygain_track_gain",
                replaygain_result.track_gain_db if replaygain_result else None,
            ),
            (
                "replaygain_track_peak",
                replaygain_result.track_peak if replaygain_result else None,
            ),
            (
                "replaygain_album_gain",
                replaygain_result.album_gain_db if replaygain_result else None,
            ),
            (
                "replaygain_album_peak",
                replaygain_result.album_peak if replaygain_result else None,
            ),
        )
        required_replaygain_names = {
            "replaygain_track_gain",
            "replaygain_track_peak",
            *(
                ("replaygain_album_gain", "replaygain_album_peak")
                if replaygain_settings.album_aware
                else ()
            ),
        }
        if replaygain_settings.enabled and replaygain_settings.required:
            available_replaygain = {
                name for name, value in replaygain_values if value is not None
            } | {
                name
                for name in required_replaygain_names
                if isinstance(existing.get(name), float)
            }
            if not required_replaygain_names <= available_replaygain:
                raise AutomaticManagementHoldError(
                    METADATA_UNAVAILABLE,
                    "Required ReplayGain values are unavailable for this import.",
                )
        enriched = {
            "genre": tuple(value.display_name for value in genre_projection.genres)
        }
        preliminary = self._effective.project(
            profile=profile,
            canonical_values=canonical_values,
            existing_values=existing,
            enriched_values=enriched,
            canonical_available=True,
        )
        preliminary_document = self._metadata_document(preliminary)
        current_custom = self._write_planner.custom_tags(
            current=current, profile=profile
        )
        transformed = self._tagging.apply(
            preliminary_document,
            pinned.tagging_scripts,
            custom_tags=current_custom,
            protected_fields=frozenset(),
        )
        effective = self._effective.project(
            profile=profile,
            canonical_values=canonical_values,
            existing_values=existing,
            enriched_values=enriched,
            transformed_values=self._tagging.transformed_values(transformed),
            canonical_available=True,
        )
        desired_metadata = self._metadata_document(effective)
        # F-PERF-07: one bounded pass cache per automatic import so the
        # projection reuses this inspection instead of walking twice.
        artwork_pass_cache = self._artwork.new_pass_cache()
        existing_external = await self._artwork.inspect_existing_external(
            profile.artwork,
            Path(request.input_path).parent,
            pass_cache=artwork_pass_cache,
        )
        artwork = await self._artwork.project(
            settings=profile.artwork,
            release_mbid=canonical_release.identifiers.release_mbid,
            release_group_mbid=canonical_release.identifiers.release_group_mbid,
            album_directory=Path(request.input_path).parent,
            pass_cache=artwork_pass_cache,
            existing_embedded=tuple(
                ExistingArtworkDescriptor(
                    image_type=value.image_type,
                    mime_type=value.mime_type or "application/octet-stream",
                    width=value.width,
                    height=value.height,
                    byte_size=value.byte_size,
                    sha256=value.sha256,
                )
                for value in current.artwork
            ),
            existing_external=existing_external,
            priority=RequestPriority.BACKGROUND_SYNC,
        )
        desired_fields = list(
            self._desired_fields(profile, current.metadata, desired_metadata)
        )
        for name, value in planned_lyrics_outputs(
            profile.enrichment.lyrics,
            lyrics_projection,
            existing,
            synchronized_supported=synced_lyrics_supported,
        ):
            desired_fields.append(
                DesiredAudioField(name=name, action="set", value=value)
            )
        if replaygain_settings.enabled and replaygain_settings.mode != "preserve":
            for name, value in replaygain_values:
                if value is None or (
                    not replaygain_settings.album_aware
                    and name.startswith("replaygain_album_")
                ):
                    continue
                if replaygain_settings.mode == "fill_missing" and isinstance(
                    existing.get(name), float
                ):
                    continue
                desired_fields.append(
                    DesiredAudioField(name=name, action="set", value=value)
                )
        desired = DesiredAudioDocument(
            fields=tuple(desired_fields),
            custom_tags=self._tagging.desired_custom_tags(current_custom, transformed),
            artwork=(
                desired_embedded_artwork(current.artwork, artwork.embedded)
                if profile.artwork.embedded_enabled
                else None
            ),
            artist_display=desired_metadata.artist_display,
            album_artist_display=desired_metadata.album_artist_display,
        )
        warnings = tuple(
            [
                *(f"genre:{source}" for source in genre_projection.deferred_sources),
                *(f"artwork:{source}" for source in artwork.deferred_sources),
                *(
                    (f"lyrics:{lyrics_projection.status}",)
                    if lyrics_projection.status in {"deferred", "mismatch", "not_found"}
                    else ()
                ),
                *(
                    ("replaygain:deferred",)
                    if replaygain_analysis is not None
                    and replaygain_analysis.status == "deferred"
                    else ()
                ),
            ]
        )
        return desired_metadata, desired, artwork.external, warnings

    def _destination_relative(
        self,
        request,
        current,
        metadata,
        pinned,
        root: Path,
        canonical_release,
        canonical_track,
    ) -> str:  # noqa: ANN001
        organization = pinned.profile.organization
        if not organization.rename_enabled and not organization.move_enabled:
            return request.destination_relative_path
        named = msgspec.structs.replace(current, metadata=metadata)
        naming_context = management_naming_context(canonical_release, canonical_track)
        naming_script = select_naming_script(
            pinned, naming_context.organization_audio_medium_count
        )
        rendered = self._naming.format_management_path(
            naming_script.source,
            named,
            organization.compatibility,
            script_name=naming_script.name,
            root=root,
            naming_context=naming_context,
        )
        rendered_path = PurePosixPath(rendered.relative_path)
        original = PurePosixPath(request.destination_relative_path)
        parent = rendered_path.parent if organization.move_enabled else original.parent
        name = rendered_path.name if organization.rename_enabled else original.name
        return (parent / name).as_posix()

    async def _external_artifacts(
        self,
        outputs: tuple[ArtworkOutput, ...],
        current,
        metadata,
        pinned,
        root: Path,
        destination_relative: str,
        root_id: str,
    ) -> list[LibraryManagementImportArtifact]:  # noqa: ANN001
        named = msgspec.structs.replace(current, metadata=metadata)
        parent = PurePosixPath(destination_relative).parent
        artifacts: list[LibraryManagementImportArtifact] = []
        collision_keys: set[str] = set()
        for output in outputs:
            extension = "jpg" if output.format == "jpeg" else output.format
            if pinned.external_artwork_naming_script is not None:
                script = pinned.external_artwork_naming_script
                rendered = self._naming.format_management_path(
                    script.source,
                    named,
                    pinned.profile.organization.compatibility,
                    script_name=script.name,
                    root=root,
                    artwork_type=output.image_type,
                    artwork_comment=output.description,
                    artwork_extension=extension,
                    artwork_format=output.format,
                )
            else:
                stem = "cover" if output.image_type == "front" else output.image_type
                rendered = self._naming.format_management_literal_path(
                    (parent / f"{stem}.{extension}").as_posix(),
                    pinned.profile.organization.compatibility,
                    script_name="Default external artwork naming",
                    root=root,
                )
            if rendered.collision_key in collision_keys:
                raise AutomaticManagementHoldError(
                    SIDECAR_COLLISION,
                    "Two external artwork outputs resolve to the same destination.",
                )
            collision_keys.add(rendered.collision_key)
            _collision, identical, _existing_fingerprint = await asyncio.to_thread(
                self._planner._external_artwork_collision,
                root / PurePosixPath(rendered.relative_path),
                output.sha256,
                overwrite=pinned.profile.artwork.overwrite_external_files,
            )
            if identical:
                continue
            artifacts.append(
                LibraryManagementImportArtifact(
                    kind="external_art",
                    destination_root_id=root_id,
                    destination_relative_path=rendered.relative_path,
                    content=output.content,
                    source_fingerprint=output.sha256,
                )
            )
        return artifacts

    @staticmethod
    async def _validate_capacity(
        bundle: LibraryManagementImportBundle,
        roots: dict[str, Path],
    ) -> None:
        required: dict[str, int] = {}
        for request in bundle.files:
            try:
                source_size = (
                    await asyncio.to_thread(Path(request.input_path).stat)
                ).st_size
            except OSError as error:
                raise AutomaticManagementHoldError(
                    ROOT_UNAVAILABLE, "An automatic import source is unavailable."
                ) from error
            required[request.destination_root_id] = (
                required.get(request.destination_root_id, 0) + source_size
            )
            for artifact in request.artifacts:
                if artifact.content is not None:
                    size = len(artifact.content)
                else:
                    if artifact.source_path is None:
                        # F-109: contract check must survive python -O; the
                        # hold outcome is the honest classification here.
                        raise AutomaticManagementHoldError(
                            ROOT_UNAVAILABLE,
                            "An automatic import sidecar is unavailable.",
                        )
                    try:
                        size = (
                            await asyncio.to_thread(Path(artifact.source_path).stat)
                        ).st_size
                    except OSError as error:
                        raise AutomaticManagementHoldError(
                            ROOT_UNAVAILABLE,
                            "An automatic import sidecar is unavailable.",
                        ) from error
                required[artifact.destination_root_id] = (
                    required.get(artifact.destination_root_id, 0) + size
                )
        for root_id, byte_count in required.items():
            root = roots.get(root_id)
            if root is None or not root.is_dir():
                raise AutomaticManagementHoldError(
                    ROOT_UNAVAILABLE, "An automatic destination root is unavailable."
                )
            if not os.access(root, os.W_OK):
                raise AutomaticManagementHoldError(
                    ROOT_READ_ONLY, "An automatic destination root is read-only."
                )
            try:
                free = (await asyncio.to_thread(shutil.disk_usage, root)).free
            except OSError as error:
                raise AutomaticManagementHoldError(
                    ROOT_UNAVAILABLE,
                    "Automatic destination capacity could not be checked.",
                ) from error
            if byte_count + _DISK_SAFETY_BYTES > free:
                raise AutomaticManagementHoldError(
                    INSUFFICIENT_SPACE,
                    "The automatic destination does not have enough free space.",
                )

    @staticmethod
    def _coalesce_artifacts(
        artifacts: list[LibraryManagementImportArtifact],
    ) -> list[LibraryManagementImportArtifact]:
        selected: dict[tuple[str, str], LibraryManagementImportArtifact] = {}
        for artifact in artifacts:
            key = (
                artifact.destination_root_id,
                unicodedata.normalize(
                    "NFC", artifact.destination_relative_path
                ).casefold(),
            )
            existing = selected.get(key)
            if existing is None:
                selected[key] = artifact
                continue
            if existing.source_fingerprint != artifact.source_fingerprint:
                raise AutomaticManagementHoldError(
                    SIDECAR_COLLISION,
                    "Artwork and sidecar files resolve to the same destination.",
                )
            if artifact.kind == "external_art":
                selected[key] = artifact
        return list(selected.values())

    @staticmethod
    def _sidecar_artifacts(
        request: LibraryManagementImportFile,
        profile,
        destination_relative: str,
    ) -> list[LibraryManagementImportArtifact]:  # noqa: ANN001
        if not profile.organization.move_sidecars:
            return []
        source_directory = Path(request.input_path).parent
        destination_parent = PurePosixPath(destination_relative).parent
        patterns = tuple(
            value.casefold() for value in profile.organization.sidecar_patterns
        )
        artifacts: list[LibraryManagementImportArtifact] = []
        examined = 0
        for current_root, directories, files in os.walk(
            source_directory, followlinks=False
        ):
            current = Path(current_root)
            directories[:] = sorted(
                value for value in directories if not (current / value).is_symlink()
            )
            for name in sorted(files):
                examined += 1
                if examined > _MAX_SIDECAR_ENTRIES:
                    raise AutomaticManagementHoldError(
                        BUNDLE_TOO_LARGE,
                        "The acquisition folder contains too many sidecar entries to "
                        "organize automatically.",
                    )
                path = current / name
                relative = path.relative_to(source_directory).as_posix()
                folded = relative.casefold()
                if not any(
                    fnmatch.fnmatchcase(folded, pattern)
                    if "/" in pattern
                    else "/" not in folded and fnmatch.fnmatchcase(folded, pattern)
                    for pattern in patterns
                ):
                    continue
                if path.suffix.casefold() in AUDIO_EXTENSION_FORMATS:
                    continue
                if path.is_symlink() or not path.is_file():
                    raise AutomaticManagementHoldError(
                        SYMLINK_UNSUPPORTED,
                        "Automatic sidecars cannot be symbolic links.",
                    )
                content_hash = AutomaticImportManagementService._hash_file(path)
                artifacts.append(
                    LibraryManagementImportArtifact(
                        kind="sidecar",
                        destination_root_id=request.destination_root_id,
                        destination_relative_path=(
                            destination_parent / PurePosixPath(relative)
                        ).as_posix(),
                        source_path=str(path),
                        source_fingerprint=content_hash,
                    )
                )
        return artifacts

    @staticmethod
    def _metadata_document(projection) -> AudioMetadataDocument:  # noqa: ANN001
        fields = tuple(
            AudioSemanticField(name=value.name, value=value.value)
            for value in projection.fields
            if value.value is not None and value.value != ()
        )
        artists = next(
            (value.value for value in projection.fields if value.name == "artist"), ()
        )
        album_artists = next(
            (
                value.value
                for value in projection.fields
                if value.name == "album_artist"
            ),
            (),
        )
        return AudioMetadataDocument(
            fields=fields,
            artist_display="; ".join(artists) if isinstance(artists, tuple) else None,
            album_artist_display=(
                "; ".join(album_artists) if isinstance(album_artists, tuple) else None
            ),
        )

    @staticmethod
    def _desired_fields(profile, current, desired) -> tuple[DesiredAudioField, ...]:  # noqa: ANN001
        selected = {
            value.field
            for value in profile.metadata.fields
            if profile.metadata.enabled and value.mode not in {"disabled", "preserve"}
        }
        if profile.genres.enabled:
            selected.add("genre")
        fields: list[DesiredAudioField] = []
        for name in sorted(selected):
            before = current.value_for(name)
            after = desired.value_for(name)
            action = (
                "unchanged"
                if before == after
                else "clear"
                if after is None or after == ()
                else "set"
            )
            fields.append(DesiredAudioField(name=name, action=action, value=after))
        return tuple(fields)

    @staticmethod
    def _synthetic_track_id(
        bundle: LibraryManagementImportBundle, request: LibraryManagementImportFile
    ) -> str:
        return str(
            uuid.uuid5(
                _IMPORT_IDENTITY_NAMESPACE,
                f"{bundle.idempotency_key}:track:{request.ordinal}",
            )
        )

    @staticmethod
    def _synthetic_album_id(
        bundle: LibraryManagementImportBundle, request: LibraryManagementImportFile
    ) -> str:
        return str(
            uuid.uuid5(
                _IMPORT_IDENTITY_NAMESPACE,
                f"{bundle.idempotency_key}:album:{request.destination_root_id}:"
                f"{request.release_mbid}",
            )
        )

    @staticmethod
    def _hash_file(path: Path) -> str:
        digest = hashlib.sha256()
        with path.open("rb") as handle:
            for chunk in iter(lambda: handle.read(1024 * 1024), b""):
                digest.update(chunk)
        return digest.hexdigest()
