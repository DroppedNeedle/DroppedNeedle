"""Local Folder client - folder-mode reference, paired with local-folder-indexer.

``enqueue`` stages bytes by copying them: ``request.payload`` carries the
source path the paired indexer produced (a file or a folder), and the copy
lands in ``downloads_dir/<task_id>/`` where ``list_completed_files`` feeds the
MB-tracklist folder import. Folder mode means ``PluginSearchResult.files``
stays empty - the client discovers the files at enqueue time.
"""

import shutil
from pathlib import Path

from droppedneedle_plugin import (
    DownloadMaterialization,
    DownloadTaskStatus,
    EnqueueRequest,
    MountDiagnosis,
    ServiceStatus,
    TaskHandle,
)


class LocalFolderClient:
    def __init__(self, context):
        self.ctx = context
        self._jobs: dict[str, dict] = {}

    def _source_dir(self) -> str:
        return (self.ctx.settings.get("source_dir") or "").strip()

    def _downloads_dir(self) -> Path:
        # Without a setting, copies land in the plugin's own data folder.
        raw = (self.ctx.settings.get("downloads_dir") or "").strip()
        return Path(raw) if raw else self.ctx.data_dir / "downloads"

    @property
    def client_name(self) -> str:
        return "plugin:local-folder-client"  # forced by the adapter anyway

    def is_configured(self) -> bool:
        return bool(self._source_dir())

    async def health_check(self) -> ServiceStatus:
        source = self._source_dir()
        if not source:
            return ServiceStatus(status="error", message="source_dir is not configured")
        if not Path(source).is_dir():
            return ServiceStatus(status="error", message=f"source_dir not found: {source}")
        return ServiceStatus(status="ok")

    async def enqueue(self, request: EnqueueRequest) -> TaskHandle:
        # request.payload carries the source path (produced by the paired
        # local-folder-indexer targeting plugin:local-folder-client);
        # copy it into downloads_dir keyed by task_id.
        payload = (getattr(request, "payload", "") or "").strip()
        if not payload:
            raise RuntimeError("local-folder-client needs payload=<source path> from its paired indexer")
        src = Path(payload)
        if not src.exists():
            raise RuntimeError(f"source path not found: {payload}")
        targets = [src] if src.is_file() else sorted(p for p in src.rglob("*") if p.is_file())
        job_dir = self._downloads_dir() / request.task_id
        job_dir.mkdir(parents=True, exist_ok=True)
        staged: list[str] = []
        for path in targets:
            dest = job_dir / path.name
            shutil.copyfile(path, dest)
            staged.append(dest.name)
        job = {
            "task_id": request.task_id,
            "job_name": f"droppedneedle-{request.task_id}",
            "dir": str(job_dir),
            "files": staged,
            "total": len(staged),
            "done": True,
            "aborted": False,
        }
        self._jobs[request.task_id] = job
        self._jobs[job["job_name"]] = job
        return TaskHandle(
            source="plugin:local-folder-client",
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
        return DownloadTaskStatus(
            task_id=task_id,
            status="completed" if job.get("done") else "downloading",
            files_total=total,
            files_completed=len(job.get("files", [])) if job.get("done") else 0,
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
