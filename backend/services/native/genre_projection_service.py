"""Project ordered multi-source genre candidates without destructive outage fallbacks."""

from __future__ import annotations

import hashlib
import time
from collections.abc import Callable, Sequence

from api.v1.schemas.library_management import GenreManagementSettings
from core.exceptions import (
    ConfigurationError,
    ExternalServiceError,
    RateLimitedError,
    ServiceDisabledUpstreamError,
)
from infrastructure.degradation import try_get_degradation_context
from infrastructure.resilience.retry import CircuitOpenError
from infrastructure.integration_result import IntegrationResult
from models.library_management_canonical import CanonicalReleaseDocument
from models.library_management_genres import GenreCandidate, GenreProjection
from repositories.protocols.lastfm_management import LastFmGenreRepositoryProtocol
from repositories.protocols.listenbrainz_management import (
    ListenBrainzGenreRepositoryProtocol,
)
from services.native.genre_normalizer import GenreNormalizer, fold_genre


def _revision(values: Sequence[str]) -> str:
    return hashlib.sha256("\x00".join(values).encode()).hexdigest()


class GenreProjectionService:
    def __init__(
        self,
        normalizer: GenreNormalizer,
        *,
        listenbrainz: ListenBrainzGenreRepositoryProtocol | None = None,
        lastfm: LastFmGenreRepositoryProtocol | None = None,
        clock: Callable[[], float] = time.time,
    ) -> None:
        self._normalizer = normalizer
        self._listenbrainz = listenbrainz
        self._lastfm = lastfm
        self._clock = clock

    async def project(
        self,
        *,
        settings: GenreManagementSettings,
        canonical_release: CanonicalReleaseDocument,
        existing_genres: Sequence[str],
    ) -> GenreProjection:
        if not settings.enabled:
            return GenreProjection(
                genres=self._existing_candidates(existing_genres, settings),
                preserved_existing=True,
            )

        by_source: dict[str, tuple[GenreCandidate, ...]] = {
            "musicbrainz": self._musicbrainz_candidates(canonical_release, settings)
        }
        deferred: list[str] = []
        if "listenbrainz" in settings.sources:
            if self._listenbrainz is None:
                deferred.append("listenbrainz")
            else:
                try:
                    values = await self._listenbrainz.get_release_group_genres_batch(
                        [canonical_release.identifiers.release_group_mbid]
                    )
                    by_source["listenbrainz"] = tuple(
                        candidate
                        for candidate in values.get(
                            canonical_release.identifiers.release_group_mbid, ()
                        )
                        if candidate.count is None
                        or candidate.count >= settings.listenbrainz_minimum_count
                        if not settings.listenbrainz_curated_only or candidate.curated
                    )
                except (
                    CircuitOpenError,
                    ConfigurationError,
                    ExternalServiceError,
                    RateLimitedError,
                    ServiceDisabledUpstreamError,
                ):
                    self._record_deferred("listenbrainz")
                    deferred.append("listenbrainz")

        if "lastfm" in settings.sources:
            if self._lastfm is None:
                deferred.append("lastfm")
            else:
                try:
                    artist_name = "".join(
                        credit.display_name + credit.join_phrase
                        for credit in canonical_release.artist_credits
                    )
                    album_values = await self._lastfm.get_album_top_genres(
                        artist_name=artist_name,
                        album_title=canonical_release.title,
                    )
                    artist_values = await self._lastfm.get_artist_top_genres(
                        artist_name=artist_name
                    )
                    by_source["lastfm"] = tuple(
                        value
                        for value in (*album_values, *artist_values)
                        if value.weight is None
                        or value.weight >= settings.lastfm_minimum_weight
                    )
                except (
                    CircuitOpenError,
                    ConfigurationError,
                    ExternalServiceError,
                    RateLimitedError,
                ):
                    self._record_deferred("lastfm")
                    deferred.append("lastfm")

        include_existing = "existing_local" in settings.sources or settings.mode in {
            "merge",
            "fill_missing",
        }
        existing = self._existing_candidates(existing_genres, settings)
        if settings.mode == "fill_missing" and existing:
            return GenreProjection(
                genres=existing[: settings.maximum_count],
                deferred_sources=tuple(deferred),
                preserved_existing=True,
            )
        if include_existing:
            by_source["existing_local"] = existing

        selected: list[GenreCandidate] = []
        seen: set[str] = set()
        for source in settings.sources:
            for candidate in by_source.get(source, ()):
                normalized = self._normalizer.normalize(
                    candidate,
                    settings,
                    require_canonical_vocabulary=(
                        (source == "listenbrainz" and not candidate.curated)
                        or (source == "lastfm" and settings.lastfm_whitelist_only)
                    ),
                )
                if normalized is None or normalized.folded_name in seen:
                    continue
                seen.add(normalized.folded_name)
                selected.append(normalized)
                if len(selected) >= settings.maximum_count:
                    break
            if len(selected) >= settings.maximum_count:
                break

        if settings.mode == "merge" and "existing_local" not in settings.sources:
            for candidate in existing:
                if candidate.folded_name not in seen:
                    seen.add(candidate.folded_name)
                    selected.append(candidate)
                    if len(selected) >= settings.maximum_count:
                        break

        if not selected and existing:
            return GenreProjection(
                genres=existing[: settings.maximum_count],
                deferred_sources=tuple(deferred),
                preserved_existing=True,
            )
        return GenreProjection(genres=tuple(selected), deferred_sources=tuple(deferred))

    def _musicbrainz_candidates(
        self,
        release: CanonicalReleaseDocument,
        settings: GenreManagementSettings,
    ) -> tuple[GenreCandidate, ...]:
        values = [*release.genres]
        for medium in release.media:
            for track in medium.tracks:
                values.extend(track.genres)
        now = self._clock()
        return tuple(
            GenreCandidate(
                display_name=value.display_name,
                folded_name=fold_genre(value.display_name),
                provider="musicbrainz",
                provider_entity=value.provider_entity,
                genre_mbid=value.genre_mbid,
                count=value.count,
                curated=True,
                fetched_at=now,
                source_document_revision=release.identifiers.release_mbid,
            )
            for value in values
            if value.count is None or value.count >= settings.musicbrainz_minimum_count
        )

    def _existing_candidates(
        self, values: Sequence[str], settings: GenreManagementSettings
    ) -> tuple[GenreCandidate, ...]:
        revision = _revision(values)
        result: list[GenreCandidate] = []
        seen: set[str] = set()
        for value in values:
            candidate = GenreCandidate(
                display_name=value,
                folded_name=fold_genre(value),
                provider="existing_local",
                provider_entity="audio_tag",
                source_document_revision=revision,
            )
            normalized = self._normalizer.normalize(
                candidate, settings, require_canonical_vocabulary=False
            )
            if normalized is not None and normalized.folded_name not in seen:
                seen.add(normalized.folded_name)
                result.append(normalized)
        return tuple(result)

    @staticmethod
    def _record_deferred(source: str) -> None:
        context = try_get_degradation_context()
        if context is not None:
            context.record(
                IntegrationResult.error(
                    source=source,
                    msg=f"{source} genre enrichment was deferred",
                )
            )
