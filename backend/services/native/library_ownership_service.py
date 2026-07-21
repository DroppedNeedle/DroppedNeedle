"""Provider-independent local album ownership projection."""

from __future__ import annotations

import unicodedata

import msgspec

from core.exceptions import ProviderIdentityRequiredError
from infrastructure.persistence.native_library_store import NativeLibraryStore


class AlbumOwnershipCandidate(msgspec.Struct, frozen=True):
    release_group_mbid: str | None
    title: str
    album_artist: str
    year: int | None = None


class AlbumOwnershipProjection(msgspec.Struct, frozen=True):
    owned: bool
    local_album_id: str | None = None


def _fold(value: str) -> str:
    normalized = unicodedata.normalize("NFKD", value.strip())
    without_marks = "".join(
        character for character in normalized if not unicodedata.combining(character)
    ).casefold()
    return " ".join(without_marks.split())


class LibraryOwnershipService:
    def __init__(
        self,
        store: NativeLibraryStore,
        *,
        placeholder_names: set[str] | None = None,
    ) -> None:
        self._store = store
        self._placeholders = {
            _fold(value)
            for value in (
                placeholder_names
                or {"Unknown artist", "Unknown album", "Various Artists"}
            )
        }

    async def provider_album_ids(self) -> set[str]:
        _revision, values = await self._store.target_provider_album_snapshot()
        return {value.casefold() for value in values}

    async def existing_provider_album_ids(self, identifiers: list[str]) -> set[str]:
        normalized = {
            value.strip().casefold() for value in identifiers if value.strip()
        }
        rows = await self._store.target_album_ownership_rows(provider_ids=normalized)
        return {
            str(row["release_group_mbid"]).casefold()
            for row in rows
            if row.get("release_group_mbid")
        }

    async def provider_album_id(self, identifier: str) -> str:
        resolved = await self._store.resolve_target_id("album", identifier)
        if resolved is None:
            return identifier
        provider_id = await self._store.target_album_provider_identity(resolved)
        if provider_id is not None:
            return provider_id
        raise ProviderIdentityRequiredError(
            "This album only has local metadata, so it cannot be sent to MusicBrainz "
            "or a download provider."
        )

    async def provider_track_id(self, identifier: str) -> str:
        row = await self._store.get_target_track(identifier)
        if row is None:
            return identifier
        if row.get("recording_mbid"):
            return str(row["recording_mbid"])
        raise ProviderIdentityRequiredError(
            "This track only has local metadata, so it cannot be sent to MusicBrainz "
            "or a download provider."
        )

    async def optional_provider_artist_id(self, identifier: str | None) -> str | None:
        if identifier is None:
            return None
        rows, _ = await self._store.list_target_artists(
            limit=1, offset=0, artist_ids=[identifier]
        )
        if not rows:
            return identifier
        provider_id = rows[0].get("provider_artist_mbid")
        return str(provider_id) if provider_id else None

    async def provider_artist_id(self, identifier: str) -> str:
        provider_id = await self.optional_provider_artist_id(identifier)
        if provider_id is not None:
            return provider_id
        raise ProviderIdentityRequiredError(
            "This artist only has local metadata, so it cannot be sent to MusicBrainz "
            "or another metadata provider."
        )

    async def provider_artist_owned(self, identifier: str) -> bool:
        return await self._store.target_has_provider_artist(identifier)

    async def project_albums(
        self, candidates: list[AlbumOwnershipCandidate]
    ) -> list[AlbumOwnershipProjection]:
        provider_ids = {
            candidate.release_group_mbid
            for candidate in candidates
            if candidate.release_group_mbid
        }
        folded_keys = {
            (_fold(candidate.title), _fold(candidate.album_artist))
            for candidate in candidates
            if candidate.title and candidate.album_artist
        }
        rows = await self._store.target_album_ownership_rows(
            provider_ids=provider_ids,
            folded_keys=folded_keys,
        )
        by_provider = {
            str(row["release_group_mbid"]).casefold(): str(row["local_album_id"])
            for row in rows
            if row.get("release_group_mbid")
        }
        by_key: dict[tuple[str, str], list[dict]] = {}
        for row in rows:
            if row.get("release_group_mbid"):
                continue
            title = _fold(str(row.get("title") or ""))
            artist = _fold(str(row.get("album_artist_name") or ""))
            if not title or not artist:
                continue
            by_key.setdefault((title, artist), []).append(row)

        projections: list[AlbumOwnershipProjection] = []
        for candidate in candidates:
            provider_id = (candidate.release_group_mbid or "").casefold()
            if provider_id and provider_id in by_provider:
                projections.append(
                    AlbumOwnershipProjection(True, by_provider[provider_id])
                )
                continue
            title = _fold(candidate.title)
            artist = _fold(candidate.album_artist)
            if (
                not title
                or not artist
                or title in self._placeholders
                or artist in self._placeholders
            ):
                projections.append(AlbumOwnershipProjection(False))
                continue
            matches = [
                row
                for row in by_key.get((title, artist), [])
                if candidate.year is None
                or row.get("year") is None
                or abs(int(row["year"]) - candidate.year) <= 1
            ]
            local_ids = {str(row["local_album_id"]) for row in matches}
            projections.append(
                AlbumOwnershipProjection(
                    owned=bool(local_ids),
                    local_album_id=next(iter(local_ids))
                    if len(local_ids) == 1
                    else None,
                )
            )
        return projections

    async def project_album(
        self,
        *,
        release_group_mbid: str | None,
        title: str,
        album_artist: str,
        year: int | None = None,
    ) -> AlbumOwnershipProjection:
        return (
            await self.project_albums(
                [
                    AlbumOwnershipCandidate(
                        release_group_mbid=release_group_mbid,
                        title=title,
                        album_artist=album_artist,
                        year=year,
                    )
                ]
            )
        )[0]
