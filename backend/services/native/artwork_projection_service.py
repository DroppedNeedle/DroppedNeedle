"""Select and process artwork without mutating files or external sidecars."""

from __future__ import annotations

import asyncio
from collections.abc import Sequence
import fnmatch
import hashlib
import os
from pathlib import Path
import stat

from api.v1.schemas.library_management import ArtworkManagementSettings
from core.exceptions import ArtworkProcessingError, ExternalServiceError
from infrastructure.audio.artwork_processor import ArtworkProcessor
from infrastructure.degradation import try_get_degradation_context
from infrastructure.integration_result import IntegrationResult
from infrastructure.queue.priority_queue import RequestPriority
from models.audio_metadata import EmbeddedArtworkDescriptor
from models.library_management_artwork import (
    ArtworkCandidate,
    ArtworkDecision,
    ArtworkImageType,
    ArtworkOutput,
    ArtworkOutputKind,
    ArtworkProcessingFormat,
    ArtworkProjection,
    ArtworkSource,
    ExistingArtworkDescriptor,
    InspectedArtwork,
)
from repositories.protocols.coverart_management import (
    ManagementCoverArtRepositoryProtocol,
)

_SOURCE = "library_management_artwork"
_CACHE_MISS = object()
_MAX_LOCAL_ENTRIES = 10_000


def merge_embedded_artwork(
    existing: Sequence[EmbeddedArtworkDescriptor],
    replacements: Sequence[EmbeddedArtworkDescriptor],
) -> tuple[EmbeddedArtworkDescriptor, ...]:
    replacement_types = {value.image_type for value in replacements}
    return (
        *replacements,
        *(value for value in existing if value.image_type not in replacement_types),
    )


def desired_embedded_artwork(
    existing: Sequence[EmbeddedArtworkDescriptor],
    replacements: Sequence[ArtworkOutput],
) -> tuple[EmbeddedArtworkDescriptor, ...]:
    return merge_embedded_artwork(
        existing,
        tuple(
            EmbeddedArtworkDescriptor(
                image_type=value.image_type,
                mime_type=value.mime_type,
                description=value.description,
                width=value.width,
                height=value.height,
                byte_size=value.byte_size,
                sha256=value.sha256,
                content=value.content,
                format_supported=True,
            )
            for value in replacements
        ),
    )


def _record_degradation(message: str) -> None:
    context = try_get_degradation_context()
    if context is not None:
        context.record(IntegrationResult.error(source=_SOURCE, msg=message))


class _LocalArtworkPassEntry:
    """One enumerated directory snapshot plus per-candidate outcomes."""

    __slots__ = ("candidates", "inspections")

    def __init__(
        self,
        candidates: tuple[ArtworkCandidate, ...],
        inspections: dict[str, "InspectedArtwork | None"],
    ) -> None:
        self.candidates = candidates
        # candidate_id -> validated inspection, or None when inspection
        # failed and the failure must be reused instead of retried.
        self.inspections = inspections


class _LocalArtworkPassCache:
    """F-PERF-07 bounded per-pass local artwork reuse.

    Created by a management caller for ONE album planning/import pass;
    carries the local-candidate enumeration and validated inspections so
    ``project`` avoids the second walk/read/inspect. Keyed by resolved
    boundary root and pattern tuple; never shared across passes, albums,
    or process restarts."""

    __slots__ = ("entries",)

    def __init__(self) -> None:
        self.entries: dict[tuple[str, tuple[str, ...]], _LocalArtworkPassEntry] = {}


class ArtworkProjectionService:
    def __init__(
        self,
        repository: ManagementCoverArtRepositoryProtocol,
        processor: ArtworkProcessor,
    ) -> None:
        self._repository = repository
        self._processor = processor

    def new_pass_cache(self) -> _LocalArtworkPassCache:
        """F-PERF-07: create one bounded pass-local artwork cache."""
        return _LocalArtworkPassCache()

    @staticmethod
    def _scoped_pass_entry(
        pass_cache: _LocalArtworkPassCache,
        album_directory: Path | None,
        patterns: tuple[str, ...],
    ) -> _LocalArtworkPassEntry | None:
        """The pass entry for this directory/pattern scope, if enumerated."""
        if album_directory is None:
            return None
        try:
            key = ArtworkProjectionService._pass_cache_key(album_directory, patterns)
        except (ArtworkProcessingError, OSError):
            return None
        return pass_cache.entries.get(key)

    @staticmethod
    def _pass_cache_key(
        album_directory: Path, patterns
    ) -> tuple[str, tuple[str, ...]]:
        """Resolved boundary root plus normalized pattern tuple."""
        root = str(album_directory.resolve(strict=True))
        normalized = tuple(pattern.casefold() for pattern in patterns)
        return root, normalized

    async def inspect_existing_external(
        self,
        settings: ArtworkManagementSettings,
        album_directory: Path | None,
        *,
        pass_cache: _LocalArtworkPassCache | None = None,
    ) -> tuple[ExistingArtworkDescriptor, ...]:
        """Inspect matching local artwork so replacement policy has real dimensions.

        F-PERF-07: with a ``pass_cache`` the enumeration and per-candidate
        inspections happen once and are reused by the following ``project``
        call within the same management pass."""

        if album_directory is None or not settings.external_enabled:
            return ()
        patterns = tuple(settings.local_file_patterns)
        cache_key: tuple[str, tuple[str, ...]] | None = None
        if pass_cache is not None:
            try:
                cache_key = self._pass_cache_key(album_directory, patterns)
            except (ArtworkProcessingError, OSError):
                _record_degradation("existing external artwork could not be enumerated")
                return ()
            entry = pass_cache.entries.get(cache_key)
            if entry is not None:
                descriptors: list[ExistingArtworkDescriptor] = []
                for candidate in entry.candidates:
                    image = entry.inspections.get(candidate.candidate_id)
                    if image is None:
                        continue
                    for image_type in candidate.image_types:
                        descriptors.append(
                            self._existing_descriptor(candidate, image_type, image)
                        )
                return tuple(descriptors)
        try:
            candidates = await asyncio.to_thread(
                self._local_candidates,
                album_directory,
                patterns,
            )
        except (ArtworkProcessingError, OSError):
            _record_degradation("existing external artwork could not be enumerated")
            return ()
        inspections: dict[str, "InspectedArtwork | None"] = {}
        inspected: list[ExistingArtworkDescriptor] = []
        for candidate in candidates:
            try:
                content = await asyncio.to_thread(
                    self._read_local_artwork,
                    candidate,
                    self._processor.maximum_input_bytes,
                )
                image = await self._processor.inspect(candidate, content)
            except (ArtworkProcessingError, OSError):
                _record_degradation("existing external artwork could not be inspected")
                inspections[candidate.candidate_id] = None
                continue
            inspections[candidate.candidate_id] = image
            for image_type in candidate.image_types:
                inspected.append(
                    self._existing_descriptor(candidate, image_type, image)
                )
        if pass_cache is not None and cache_key is not None:
            pass_cache.entries[cache_key] = _LocalArtworkPassEntry(
                candidates=candidates, inspections=inspections
            )
        return tuple(inspected)

    @staticmethod
    def _existing_descriptor(
        candidate: ArtworkCandidate,
        image_type,
        image: "InspectedArtwork",
    ) -> ExistingArtworkDescriptor:
        return ExistingArtworkDescriptor(
            image_type=image_type,
            mime_type=image.mime_type,
            width=image.width,
            height=image.height,
            byte_size=image.byte_size,
            sha256=image.sha256,
        )

    async def project(
        self,
        *,
        settings: ArtworkManagementSettings,
        release_mbid: str | None,
        release_group_mbid: str | None,
        album_directory: Path | None,
        existing_embedded: Sequence[ExistingArtworkDescriptor],
        existing_external: Sequence[ExistingArtworkDescriptor],
        embedded_fallback: Sequence[InspectedArtwork] = (),
        selected_cache: dict[ArtworkImageType, InspectedArtwork] | None = None,
        priority: RequestPriority,
        pass_cache: _LocalArtworkPassCache | None = None,
    ) -> ArtworkProjection:
        if not settings.embedded_enabled and not settings.external_enabled:
            return ArtworkProjection(preserved_existing=True)

        desired_types = tuple(settings.image_types)
        selected = selected_cache if selected_cache is not None else {}
        decisions: list[ArtworkDecision] = []
        deferred: list[ArtworkSource] = []

        for provider in settings.providers:
            missing = [value for value in desired_types if value not in selected]
            if not missing:
                break
            if provider == "embedded":
                if embedded_fallback or existing_embedded:
                    for image_type in missing:
                        if any(
                            value.image_type == image_type
                            for value in existing_embedded
                        ):
                            decisions.append(
                                ArtworkDecision(
                                    output_kind="embedded",
                                    image_type=image_type,
                                    action="preserve",
                                    reason="existing embedded artwork is the fallback",
                                )
                            )
                continue
            if provider == "audiodb":
                self._defer(provider, deferred, "AudioDB artwork is unavailable")
                continue
            local_entry: _LocalArtworkPassEntry | None = None
            if provider == "local_files" and pass_cache is not None:
                local_entry = self._scoped_pass_entry(
                    pass_cache, album_directory, tuple(settings.local_file_patterns)
                )
            try:
                candidates = await self._candidates(
                    provider=provider,
                    settings=settings,
                    release_mbid=release_mbid,
                    release_group_mbid=release_group_mbid,
                    album_directory=album_directory,
                    priority=priority,
                    pass_cache=pass_cache,
                )
            except (ArtworkProcessingError, ExternalServiceError, OSError, ValueError):
                self._defer(provider, deferred, "artwork provider failed")
                continue
            await self._select_candidates(
                candidates=candidates,
                settings=settings,
                selected=selected,
                priority=priority,
                decisions=decisions,
                deferred=deferred,
                local_entry=local_entry,
            )

        embedded_outputs: list[ArtworkOutput] = []
        external_outputs: list[ArtworkOutput] = []
        if settings.embedded_enabled:
            embedded_outputs.extend(
                await self._build_outputs(
                    output_kind="embedded",
                    image_types=self._output_types(
                        desired_types, front_only=settings.embedded_front_only
                    ),
                    selected=selected,
                    existing=existing_embedded,
                    maximum_size=settings.embedded_maximum_size,
                    output_format=settings.embedded_format,
                    settings=settings,
                    decisions=decisions,
                )
            )
        if settings.external_enabled:
            external_outputs.extend(
                await self._build_outputs(
                    output_kind="external",
                    image_types=self._output_types(
                        desired_types, front_only=settings.external_front_only
                    ),
                    selected=selected,
                    existing=existing_external,
                    maximum_size=settings.external_maximum_size,
                    output_format=settings.external_format,
                    settings=settings,
                    decisions=decisions,
                )
            )
        return ArtworkProjection(
            embedded=tuple(embedded_outputs),
            external=tuple(external_outputs),
            decisions=tuple(decisions),
            deferred_sources=tuple(deferred),
            preserved_existing=any(
                decision.action == "preserve" for decision in decisions
            ),
        )

    async def _candidates(
        self,
        *,
        provider: ArtworkSource,
        settings: ArtworkManagementSettings,
        release_mbid: str | None,
        release_group_mbid: str | None,
        album_directory: Path | None,
        priority: RequestPriority,
        pass_cache: _LocalArtworkPassCache | None = None,
    ) -> tuple[ArtworkCandidate, ...]:
        if provider == "cover_art_archive_release":
            if not release_mbid:
                return ()
            return await self._repository.list_management_artwork(
                entity_kind="release",
                mbid=release_mbid,
                download_size=settings.download_size,
                priority=priority,
            )
        if provider == "cover_art_archive_release_group":
            if not release_group_mbid:
                return ()
            return await self._repository.list_management_artwork(
                entity_kind="release-group",
                mbid=release_group_mbid,
                download_size=settings.download_size,
                priority=priority,
            )
        if provider == "local_files":
            if album_directory is None:
                return ()
            cache_key: tuple[str, tuple[str, ...]] | None = None
            entry = None
            if pass_cache is not None:
                # Resolution failures here defer the provider exactly like an
                # enumeration failure did before the pass cache existed.
                cache_key = self._pass_cache_key(
                    album_directory, tuple(settings.local_file_patterns)
                )
                entry = pass_cache.entries.get(cache_key)
                if entry is not None:
                    return entry.candidates
            candidates = await asyncio.to_thread(
                self._local_candidates,
                album_directory,
                tuple(settings.local_file_patterns),
            )
            if pass_cache is not None and cache_key is not None:
                pass_cache.entries[cache_key] = _LocalArtworkPassEntry(
                    candidates=candidates, inspections={}
                )
            return candidates
        return ()

    async def _select_candidates(
        self,
        *,
        candidates: Sequence[ArtworkCandidate],
        settings: ArtworkManagementSettings,
        selected: dict[ArtworkImageType, InspectedArtwork],
        priority: RequestPriority,
        decisions: list[ArtworkDecision],
        deferred: list[ArtworkSource],
        local_entry: _LocalArtworkPassEntry | None = None,
    ) -> None:
        configured_types = set(settings.image_types)
        for candidate in candidates:
            candidate_types = tuple(
                value
                for value in candidate.image_types
                if value in configured_types and value not in selected
            )
            if not candidate_types:
                continue
            if settings.approved_only and not candidate.approved:
                for image_type in candidate_types:
                    decisions.append(
                        ArtworkDecision(
                            output_kind="external",
                            image_type=image_type,
                            action="skip",
                            reason="provider image is not approved",
                            candidate_id=candidate.candidate_id,
                        )
                    )
                continue
            reuse_inspection: InspectedArtwork | None | object = _CACHE_MISS
            if local_entry is not None and candidate.source == "local_files":
                stored = local_entry.inspections.get(candidate.candidate_id)
                if stored is not None or candidate.candidate_id in local_entry.inspections:
                    # F-PERF-07: reuse this pass's earlier inspection result,
                    # including an explicit failure marker, instead of
                    # re-reading and re-inspecting the same local file.
                    reuse_inspection = stored
            inspected: InspectedArtwork
            if reuse_inspection is not _CACHE_MISS:
                if isinstance(reuse_inspection, InspectedArtwork):
                    inspected = reuse_inspection
                else:
                    _record_degradation("an artwork candidate could not be validated")
                    continue
            else:
                try:
                    content, declared_mime = await self._load(candidate, priority)
                    inspected = await self._processor.inspect(
                        candidate, content, declared_mime_type=declared_mime
                    )
                except ExternalServiceError:
                    self._defer(
                        candidate.source,
                        deferred,
                        "artwork provider download failed",
                    )
                    continue
                except (ArtworkProcessingError, OSError):
                    if local_entry is not None and candidate.source == "local_files":
                        local_entry.inspections[candidate.candidate_id] = None
                    _record_degradation("an artwork candidate could not be validated")
                    continue
                if local_entry is not None and candidate.source == "local_files":
                    local_entry.inspections[candidate.candidate_id] = inspected
            if (
                inspected.width is not None and inspected.width < settings.minimum_width
            ) or (
                inspected.height is not None
                and inspected.height < settings.minimum_height
            ):
                for image_type in candidate_types:
                    decisions.append(
                        ArtworkDecision(
                            output_kind="external",
                            image_type=image_type,
                            action="skip",
                            reason="provider image is below the minimum dimensions",
                            candidate_id=candidate.candidate_id,
                        )
                    )
                continue
            if inspected.external_only and (
                settings.minimum_width or settings.minimum_height
            ):
                continue
            for image_type in candidate_types:
                selected[image_type] = inspected

    async def _load(
        self, candidate: ArtworkCandidate, priority: RequestPriority
    ) -> tuple[bytes, str | None]:
        if candidate.source in {
            "cover_art_archive_release",
            "cover_art_archive_release_group",
        }:
            return await self._repository.download_management_artwork(
                candidate,
                maximum_bytes=self._processor.maximum_input_bytes,
                priority=priority,
            )
        if candidate.source == "local_files":
            content = await asyncio.to_thread(
                self._read_local_artwork,
                candidate,
                self._processor.maximum_input_bytes,
            )
            return content, None
        raise ArtworkProcessingError("Artwork candidate source cannot provide bytes.")

    async def _build_outputs(
        self,
        *,
        output_kind: ArtworkOutputKind,
        image_types: Sequence[ArtworkImageType],
        selected: dict[ArtworkImageType, InspectedArtwork],
        existing: Sequence[ExistingArtworkDescriptor],
        maximum_size: int,
        output_format: ArtworkProcessingFormat,
        settings: ArtworkManagementSettings,
        decisions: list[ArtworkDecision],
    ) -> tuple[ArtworkOutput, ...]:
        current: dict[ArtworkImageType, ExistingArtworkDescriptor] = {}
        for value in existing:
            previous = current.get(value.image_type)
            if previous is None or self._existing_rank(value) > self._existing_rank(
                previous
            ):
                current[value.image_type] = value
        outputs: list[ArtworkOutput] = []
        for image_type in image_types:
            candidate = selected.get(image_type)
            previous = current.get(image_type)
            if candidate is None:
                decisions.append(
                    ArtworkDecision(
                        output_kind=output_kind,
                        image_type=image_type,
                        action="preserve" if previous is not None else "skip",
                        reason=(
                            "no validated improvement was available"
                            if previous is not None
                            else "no validated artwork was available"
                        ),
                    )
                )
                continue
            if image_type in settings.preserve_existing_types and previous is not None:
                decisions.append(
                    ArtworkDecision(
                        output_kind=output_kind,
                        image_type=image_type,
                        action="preserve",
                        reason="profile preserves this existing image type",
                        candidate_id=candidate.candidate.candidate_id,
                    )
                )
                continue
            if (
                output_kind == "external"
                and previous is not None
                and not settings.overwrite_external_files
            ):
                decisions.append(
                    ArtworkDecision(
                        output_kind=output_kind,
                        image_type=image_type,
                        action="preserve",
                        reason="external artwork overwrite is disabled",
                        candidate_id=candidate.candidate.candidate_id,
                    )
                )
                continue
            try:
                output = await self._processor.process(
                    candidate,
                    output_kind=output_kind,
                    image_type=image_type,
                    maximum_size=maximum_size,
                    output_format=output_format,
                )
            except ArtworkProcessingError:
                _record_degradation("artwork output processing failed")
                decisions.append(
                    ArtworkDecision(
                        output_kind=output_kind,
                        image_type=image_type,
                        action="preserve" if previous is not None else "skip",
                        reason="artwork processing failed",
                        candidate_id=candidate.candidate.candidate_id,
                    )
                )
                continue
            if previous is not None and previous.sha256 == output.sha256:
                decisions.append(
                    ArtworkDecision(
                        output_kind=output_kind,
                        image_type=image_type,
                        action="preserve",
                        reason="selected artwork is already present",
                        candidate_id=candidate.candidate.candidate_id,
                    )
                )
                continue
            if (
                previous is not None
                and settings.never_replace_with_smaller
                and self._is_smaller_or_unknown(output, previous)
            ):
                decisions.append(
                    ArtworkDecision(
                        output_kind=output_kind,
                        image_type=image_type,
                        action="preserve",
                        reason="processed artwork is smaller or cannot prove improvement",
                        candidate_id=candidate.candidate.candidate_id,
                    )
                )
                continue
            outputs.append(output)
            decisions.append(
                ArtworkDecision(
                    output_kind=output_kind,
                    image_type=image_type,
                    action="replace" if previous is not None else "add",
                    reason="selected by configured provider and image-type order",
                    candidate_id=candidate.candidate.candidate_id,
                )
            )
        return tuple(outputs)

    @staticmethod
    def _existing_rank(value: ExistingArtworkDescriptor) -> tuple[int, int, int]:
        if value.width is None or value.height is None:
            return (1, 0, value.byte_size)
        return (0, value.width * value.height, value.byte_size)

    @staticmethod
    def _is_smaller_or_unknown(
        candidate: ArtworkOutput, existing: ExistingArtworkDescriptor
    ) -> bool:
        if (
            candidate.width is None
            or candidate.height is None
            or existing.width is None
            or existing.height is None
        ):
            return True
        return candidate.width < existing.width or candidate.height < existing.height

    @staticmethod
    def _output_types(
        configured: Sequence[ArtworkImageType], *, front_only: bool
    ) -> tuple[ArtworkImageType, ...]:
        if front_only:
            return ("front",) if "front" in configured else ()
        return tuple(configured)

    @staticmethod
    def _defer(
        provider: ArtworkSource,
        deferred: list[ArtworkSource],
        message: str,
    ) -> None:
        if provider not in deferred:
            deferred.append(provider)
        _record_degradation(message)

    @staticmethod
    def _local_candidates(
        album_directory: Path, patterns: tuple[str, ...]
    ) -> tuple[ArtworkCandidate, ...]:
        if album_directory.is_symlink():
            raise ArtworkProcessingError("Local artwork root is not a safe directory.")
        root = album_directory.resolve(strict=True)
        if not root.is_dir():
            raise ArtworkProcessingError("Local artwork root is not a safe directory.")
        matches: list[tuple[str, Path]] = []
        visited = 0
        for current, directories, files in os.walk(root, followlinks=False):
            current_path = Path(current)
            directories[:] = [
                value
                for value in directories
                if not (current_path / value).is_symlink()
            ]
            for name in files:
                visited += 1
                if visited > _MAX_LOCAL_ENTRIES:
                    raise ArtworkProcessingError(
                        "Local artwork search exceeded the safety limit."
                    )
                path = current_path / name
                if path.is_symlink() or not path.is_file():
                    continue
                relative = path.relative_to(root).as_posix()
                if any(
                    fnmatch.fnmatchcase(relative.casefold(), pattern.casefold())
                    for pattern in patterns
                ):
                    matches.append((relative, path))

        candidates: list[ArtworkCandidate] = []
        for relative, path in sorted(matches, key=lambda value: value[0].casefold()):
            image_type = ArtworkProjectionService._local_image_type(path.stem)
            candidates.append(
                ArtworkCandidate(
                    candidate_id=(
                        "local:" + hashlib.sha256(relative.encode()).hexdigest()[:24]
                    ),
                    source="local_files",
                    locator=str(path),
                    image_types=(image_type,),
                    approved=True,
                    primary=image_type == "front",
                    description=relative,
                    boundary_root=str(root),
                )
            )
        return tuple(candidates)

    @staticmethod
    def _read_local_artwork(candidate: ArtworkCandidate, maximum_bytes: int) -> bytes:
        if candidate.boundary_root is None:
            raise ArtworkProcessingError("Local artwork has no safety boundary.")
        root = Path(candidate.boundary_root).resolve(strict=True)
        path = Path(candidate.locator)
        try:
            resolved = path.resolve(strict=True)
            resolved.relative_to(root)
            relative = path.relative_to(root)
        except (OSError, ValueError) as error:
            raise ArtworkProcessingError(
                "Local artwork escaped its album directory."
            ) from error
        cursor = root
        for part in relative.parts:
            cursor /= part
            if cursor.is_symlink():
                raise ArtworkProcessingError("Local artwork cannot follow symlinks.")
        flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
        try:
            descriptor = os.open(path, flags)
        except OSError as error:
            raise ArtworkProcessingError(
                "Local artwork could not be opened safely."
            ) from error
        try:
            metadata = os.fstat(descriptor)
            if not stat.S_ISREG(metadata.st_mode):
                raise ArtworkProcessingError("Local artwork is not a regular file.")
            if metadata.st_size <= 0 or metadata.st_size > maximum_bytes:
                raise ArtworkProcessingError(
                    "Local artwork exceeds the byte safety limit."
                )
            chunks: list[bytes] = []
            remaining = maximum_bytes + 1
            while remaining > 0:
                chunk = os.read(descriptor, min(1024 * 1024, remaining))
                if not chunk:
                    break
                chunks.append(chunk)
                remaining -= len(chunk)
            content = b"".join(chunks)
            if not content or len(content) > maximum_bytes:
                raise ArtworkProcessingError(
                    "Local artwork exceeds the byte safety limit."
                )
            return content
        finally:
            os.close(descriptor)

    @staticmethod
    def _local_image_type(stem: str) -> ArtworkImageType:
        folded = stem.casefold()
        if folded in {"cover", "folder", "front", "albumart"}:
            return "front"
        for image_type in (
            "back",
            "booklet",
            "tray",
            "obi",
            "spine",
            "track",
        ):
            if folded.startswith(image_type):
                return image_type
        if folded.startswith(("disc", "disk", "cd", "medium")):
            return "medium"
        return "other"
