from typing import Any, Protocol

from models.library import LibraryAlbum


class LibraryRepositoryProtocol(Protocol):
    """Contract for the native library.

    Implemented by ``services.native.target_library_repository.TargetLibraryRepository``
    via ``NativeLibraryStore``.
    """

    def is_configured(self) -> bool:
        ...


    async def has_album(self, mbid: str) -> bool:
        ...

    async def get_library_albums(self) -> list[LibraryAlbum]:
        ...

    async def get_library_album_mbids(self) -> set[str]:
        ...

    async def get_library_artist_mbids(self) -> set[str]:
        ...

    async def get_library_files_for_recordings(
        self, recording_mbids: list[str]
    ) -> dict[str, list[Any]]:
        ...
