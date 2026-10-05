"""HTTP Catalog - the primary reference plugin (files mode).

Searches a user-hosted JSON catalog feed and downloads its files. The feed
shape is ``{albums: [{artist, title, files: [{url, filename, size}]}]}`` and
every host in it is fictional (e.g. https://example-catalog.test/feed.json).

Files mode: ``search_album`` returns one ``PluginSearchResult`` carrying exact
``DownloadFileRef``s; the engine enqueues those files and imports them, and
``payload`` (the catalog album id) round-trips so ``enqueue`` can match the
request back to its catalog entry.
"""

import asyncio
from pathlib import Path

from droppedneedle_plugin import (
    DownloadFileRef,
    DownloadMaterialization,
    DownloadTaskStatus,
    EnqueueRequest,
    IndexerResult,
    MountDiagnosis,
    PluginSearchResult,
    ServiceStatus,
    TaskHandle,
)


class HttpCatalog:
    def __init__(self, context):
        self.ctx = context
        self._jobs: dict[str, dict] = {}

    # -- config helpers --

    def _catalog_url(self) -> str:
        return (self.ctx.settings.get("catalog_url") or "").strip()

    def _downloads_dir(self) -> Path:
        # Without a setting, files land in the plugin's own data folder.
        raw = (self.ctx.settings.get("downloads_dir") or "").strip()
        return Path(raw) if raw else self.ctx.data_dir / "downloads"

    # -- DownloadClientProtocol + IndexerProtocol identity --

    @property
    def client_name(self) -> str:
        return "plugin:http-catalog"  # forced by the adapter anyway

    @property
    def indexer_name(self) -> str:
        return "plugin:http-catalog"

    def is_configured(self) -> bool:
        return bool(self._catalog_url())

    async def health_check(self) -> ServiceStatus:
        url = self._catalog_url()
        if not url:
            return ServiceStatus(status="error", message="catalog_url is not configured")
        try:
            async with asyncio.timeout(10):
                response = await self.ctx.http.head(url, timeout=10.0)
        except Exception as exc:  # noqa: BLE001 - degraded, never fatal
            return ServiceStatus(status="error", message=f"catalog unreachable: {exc}")
        if response.status_code >= 400:
            return ServiceStatus(status="error", message=f"catalog HTTP {response.status_code}")
        return ServiceStatus(status="ok")

    # -- catalog IO --

    async def _load_catalog(self, timeout: float = 30.0) -> list[dict]:
        response = await self.ctx.http.get(self._catalog_url(), timeout=timeout)
        if response.status_code >= 400:
            raise RuntimeError(f"catalog HTTP {response.status_code}")
        data = response.json()
        albums = data.get("albums", []) if isinstance(data, dict) else []
        return [a for a in albums if isinstance(a, dict)]

    @staticmethod
    def _file_entries(album: dict) -> list[tuple[str, str, int]]:
        entries = []
        files = album.get("files", []) or []
        for entry in files:
            if not isinstance(entry, dict):
                continue
            filename = str(entry.get("filename", "")).strip()
            url = str(entry.get("url", "")).strip()
            if not filename or not url:
                continue
            entries.append((filename, url, int(entry.get("size", 0) or 0)))
        return entries

    # -- IndexerProtocol --

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
        try:
            async with asyncio.timeout(timeout):
                albums = await self._load_catalog(timeout)
        except Exception as exc:  # noqa: BLE001 - one bad search is [], never a crash
            self.ctx.logger.warning("catalog search failed: %s", exc)
            return []
        want_artist = (artist_name or "").strip().casefold()
        want_album = (album_title or "").strip().casefold()
        results = []
        for index, album in enumerate(albums):
            artist = str(album.get("artist", ""))
            title = str(album.get("title", ""))
            if want_artist and want_artist not in artist.casefold():
                continue
            if want_album and want_album not in title.casefold():
                continue
            entries = self._file_entries(album)
            if not entries:
                continue
            results.append(
                IndexerResult(
                    source="plugin:http-catalog",  # source key, not the display alias
                    plugin=PluginSearchResult(
                        title=f"{artist} - {title}".strip(" -"),
                        size_bytes=sum(size for _, _, size in entries),
                        score=1.0,
                        files=[DownloadFileRef(username="catalog", filename=name, size=size) for name, _, size in entries],
                        payload=f"album-{index}",
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

    # -- DownloadClientProtocol --

    async def enqueue(self, request: EnqueueRequest) -> TaskHandle:
        task_id = request.task_id
        payload = getattr(request, "payload", "") or ""
        albums = await self._load_catalog()
        urls: dict[str, str] = {}
        for index, album in enumerate(albums):
            if f"album-{index}" == payload:
                urls = {name: url for name, url, _ in self._file_entries(album)}
                break
        job_dir = self._downloads_dir() / task_id
        job_dir.mkdir(parents=True, exist_ok=True)
        staged: list[str] = []
        for ref in request.files:
            url = urls.get(ref.filename)
            if url is None:
                self.ctx.logger.warning("enqueue %s: no catalog URL for %s", task_id, ref.filename)
                continue
            response = await self.ctx.http.get(url, timeout=60.0)
            if response.status_code >= 400:
                raise RuntimeError(f"download HTTP {response.status_code} for {ref.filename}")
            dest = job_dir / Path(ref.filename).name
            dest.write_bytes(response.content)
            staged.append(dest.name)
        job = {
            "task_id": task_id,
            "job_name": f"droppedneedle-{task_id}",
            "dir": str(job_dir),
            "files": staged,
            "total": len(request.files),
            "done": True,
            "aborted": False,
        }
        self._jobs[task_id] = job
        self._jobs[job["job_name"]] = job
        return TaskHandle(
            source="plugin:http-catalog",
            filenames=staged,
            job_name=job["job_name"],
            plugin_token=payload,
        )

    def _find_job(self, handle: TaskHandle) -> dict | None:
        job = self._jobs.get(handle.job_name or "")
        if job is not None:
            return job
        for job in self._jobs.values():
            if job.get("filenames", []) == list(handle.filenames or []):
                return job
        return None

    @staticmethod
    def _task_id_for(handle: TaskHandle, job: dict | None) -> str:
        if job is not None:
            return str(job.get("task_id", ""))
        prefix, _, suffix = (handle.job_name or "").partition("droppedneedle-")
        return suffix if not prefix else handle.job_name

    async def get_status(self, handle: TaskHandle) -> DownloadTaskStatus:
        job = self._find_job(handle)
        task_id = self._task_id_for(handle, job)
        if job is None:
            return DownloadTaskStatus(task_id=task_id, status="failed", error="unknown task")
        if job.get("aborted"):
            return DownloadTaskStatus(task_id=task_id, status="failed", error="aborted")
        total = int(job.get("total", 0))
        done = len(job.get("files", [])) if job.get("done") else 0
        return DownloadTaskStatus(
            task_id=task_id,
            status="completed" if job.get("done") else "downloading",
            files_total=total,
            files_completed=done,
            files_failed=0 if job.get("done") else 0,
            bytes_total=sum((Path(job["dir"]) / name).stat().st_size for name in job.get("files", []) if (Path(job["dir"]) / name).is_file()),
            bytes_downloaded=sum((Path(job["dir"]) / name).stat().st_size for name in job.get("files", []) if (Path(job["dir"]) / name).is_file()),
            progress_percent=100.0 if job.get("done") and total else 0.0,
            succeeded_filenames=list(job.get("files", [])) if job.get("done") else [],
        )

    async def abort(self, handle: TaskHandle) -> bool:
        job = self._find_job(handle)
        if job is None:
            return False
        job["aborted"] = True
        job["done"] = False
        return True

    async def inspect_materialization(self, handle: TaskHandle) -> DownloadMaterialization:
        job = self._find_job(handle)
        if job is None:
            return DownloadMaterialization(state="missing")
        paths = [str(Path(job["dir"]) / name) for name in job.get("files", []) if (Path(job["dir"]) / name).is_file()]
        if job.get("aborted"):
            return DownloadMaterialization(state="failed", workspace_path=job["dir"], file_paths=paths)
        return DownloadMaterialization(
            state="completed" if job.get("done") else "active",
            workspace_path=job["dir"],
            file_paths=paths,
        )

    async def discard_client_artifacts(self, handle: TaskHandle) -> bool:
        job = self._find_job(handle)
        if job is None:
            return False
        self._jobs.pop(job.get("task_id", ""), None)
        self._jobs.pop(job.get("job_name", ""), None)
        return True

    async def list_completed_files(self, handle: TaskHandle) -> list[Path]:
        job = self._find_job(handle)
        if job is None or job.get("aborted"):
            return []
        return [Path(job["dir"]) / name for name in job.get("files", []) if (Path(job["dir"]) / name).is_file()]

    async def get_file_path(
        self,
        handle: TaskHandle,
        remote_filename: str,
        size: int | None = None,
    ) -> Path | None:
        job = self._find_job(handle)
        if job is None:
            return None
        candidate = Path(job["dir"]) / Path(remote_filename).name
        return candidate if candidate.is_file() else None

    async def diagnose_downloads_mount(self) -> MountDiagnosis:
        return MountDiagnosis(supported=False)

    # -- SchedulerCapability --

    async def on_tick(self) -> None:
        url = (self.ctx.settings.get("catalog_url") or "").strip()
        if not url:
            return
        try:
            async with asyncio.timeout(30):
                response = await self.ctx.http.head(url, timeout=30.0)
                if response.status_code >= 400:
                    self.ctx.logger.warning("catalog revalidation: HTTP %s", response.status_code)
        except Exception as exc:  # noqa: BLE001 - one bad tick never kills the loop
            self.ctx.logger.warning("catalog revalidation failed: %s", exc)
