"""Local Folder indexer - the pairing partner for local-folder-client.

Scans the configured source folder and returns one ``PluginSearchResult`` per
file in folder mode (``files=[]``): the opaque ``payload`` is the source path,
which the client enqueues verbatim to stage the file. ``target_source`` pins
the client, demonstrating cross-plugin pairing.
"""

from pathlib import Path

from droppedneedle_plugin import IndexerResult, PluginSearchResult, ServiceStatus


class LocalFolderIndexer:
    def __init__(self, context):
        self.ctx = context

    def _source_dir(self) -> str:
        return (self.ctx.settings.get("source_dir") or "").strip()

    @property
    def indexer_name(self) -> str:
        return "plugin:local-folder-indexer"

    def is_configured(self) -> bool:
        return bool(self._source_dir())

    async def health_check(self) -> ServiceStatus:
        source = self._source_dir()
        if not source:
            return ServiceStatus(status="error", message="source_dir is not configured")
        if not Path(source).is_dir():
            return ServiceStatus(status="error", message=f"source_dir not found: {source}")
        return ServiceStatus(status="ok")

    def _score(self, filename: str, artist_name: str, album_title: str) -> float:
        hay = filename.casefold()
        for want in ((artist_name or "").strip().casefold(), (album_title or "").strip().casefold()):
            if want and want in hay:
                return 0.9
        return 0.5

    async def search_album(
        self,
        artist_name: str,
        album_title: str,
        year: int | None = None,
        track_count: int | None = None,
        *,
        timeout: float = 30.0,
    ) -> list[IndexerResult]:
        if not self.is_configured():
            return []
        root = Path(self._source_dir())
        if not root.is_dir():
            return []
        results = []
        for path in sorted(root.rglob("*")):
            if not path.is_file():
                continue
            try:
                size = path.stat().st_size
            except OSError:
                size = 0
            # Folder mode (files=[], payload=<source path>); target_source
            # pins the client that downloads this result.
            results.append(
                IndexerResult(
                    source="plugin:local-folder-client",
                    plugin=PluginSearchResult(
                        title=path.name,
                        size_bytes=size,
                        score=self._score(path.name, artist_name, album_title),
                        files=[],
                        payload=str(path),
                    ),
                )
            )
        return results

    async def search_track(
        self,
        artist_name: str,
        track_title: str,
        album_title: str | None = None,
        duration_seconds: int | None = None,
        *,
        timeout: float = 30.0,
    ) -> list[IndexerResult]:
        return []
