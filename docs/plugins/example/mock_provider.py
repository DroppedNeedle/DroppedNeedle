"""mock_provider - a complete, runnable example acquisition plugin.

Install by copying this single file into ``{ROOT_APP_DIR}/plugins/`` and
restarting DroppedNeedle; it appears on the Settings > Plugins page. It fakes a
source: ``search`` fabricates release candidates, ``enqueue`` "downloads" by
writing small placeholder files into a local directory, ``get_status`` reports
them complete, and ``completed_path`` hands the files to the caller.

It exists to (a) document every extension point with working code and (b) prove
third-party loading end-to-end - the backend test suite copies this exact file
into a temporary external plugins dir and drives the full round-trip against it.

NOTE: candidates fabricated here are release-shaped (the ``usenet`` archetype).
A real plugin returns real results from its network; see docs/plugins/AUTHORING.md.
"""

import asyncio
import tempfile
import time
from pathlib import Path
from typing import Any

from plugins.base import (
    AcquisitionPlugin,
    Candidate,
    DownloadTaskStatus,
    EnqueueRequest,
    SearchRequest,
    SelectOption,
    SettingsField,
    TaskHandle,
    TestResult,
    UsenetRelease,
)


class MockProviderPlugin(AcquisitionPlugin):
    # -- identity ------------------------------------------------------------
    id = "mock_provider"
    name = "Mock Provider"
    version = "1.0.0"
    # api_version defaults to plugins.base.API_VERSION; pin it explicitly when
    # distributing, so an incompatible server quarantines you with a clear error.

    def __init__(self) -> None:
        self._download_dir = Path(tempfile.gettempdir()) / "droppedneedle-mock-provider"
        self._latency = 0.0
        self._label = "Mock"
        self._jobs: dict[str, list[Path]] = {}

    # -- lifecycle -------------------------------------------------------------

    def settings_schema(self) -> list[SettingsField]:
        return [
            SettingsField(
                key="download_dir",
                type="str",
                label="Download directory",
                help="Where fake downloads are written.",
                default=str(Path(tempfile.gettempdir()) / "droppedneedle-mock-provider"),
                required=True,
            ),
            SettingsField(
                key="api_token",
                type="secret",
                label="API token",
                help="Demonstrates a secret field: encrypted at rest, masked in the UI.",
                default="",
            ),
            SettingsField(
                key="latency",
                type="select",
                label="Simulated latency",
                help="Seconds each fake operation sleeps.",
                default=0,
                options=[
                    SelectOption(value=0, label="None"),
                    SelectOption(value=1, label="1 second"),
                    SelectOption(value=3, label="3 seconds"),
                ],
            ),
            SettingsField(
                key="verbose",
                type="bool",
                label="Verbose titles",
                default=False,
            ),
            SettingsField(
                key="result_count",
                type="int",
                label="Results per search",
                default=3,
            ),
        ]

    def configure(self, settings: dict[str, Any]) -> None:
        if settings.get("download_dir"):
            self._download_dir = Path(str(settings["download_dir"]))
        self._latency = float(settings.get("latency") or 0)
        self._result_count = int(settings.get("result_count") or 3)
        self._verbose = bool(settings.get("verbose", False))

    async def test_connection(self, settings: dict[str, Any] | None = None) -> TestResult:
        directory = Path(str((settings or {}).get("download_dir") or self._download_dir))
        try:
            directory.mkdir(parents=True, exist_ok=True)
        except OSError as exc:
            return TestResult(ok=False, message=f"cannot create download dir: {exc}")
        return TestResult(ok=True, message=f"mock provider ready at {directory}", version=self.version)

    # -- capabilities ------------------------------------------------------------

    def is_configured(self) -> bool:
        return True

    async def search(self, request: SearchRequest) -> list[Candidate]:
        await asyncio.sleep(self._latency)
        what = request.album_title if request.kind == "album" else request.track_title
        out: list[Candidate] = []
        for i in range(getattr(self, "_result_count", 3)):
            title = f"{request.artist_name} - {what} [mock {i + 1}]"
            if getattr(self, "_verbose", False):
                title += f" ({time.strftime('%Y-%m-%d')})"
            out.append(
                Candidate(
                    source=self.id,
                    usenet=UsenetRelease(
                        indexer_id=self.id,
                        indexer_name=self.name,
                        guid=f"mock-{abs(hash(title))}",
                        title=title,
                        nzb_url=f"mock://release/{i + 1}",
                        size_bytes=(i + 1) * 25_000_000,
                        category_ids=[3000],
                    ),
                )
            )
        return out

    async def enqueue(self, request: EnqueueRequest) -> TaskHandle:
        await asyncio.sleep(self._latency)
        job_name = request.job_name or f"mock-{request.task_id}"
        job_dir = self._download_dir / job_name
        job_dir.mkdir(parents=True, exist_ok=True)
        files: list[Path] = []
        for i in range(1, 3):
            path = job_dir / f"{i:02d} - mock track.mp3"
            path.write_bytes(b"ID3mock-audio-payload")
            files.append(path)
        self._jobs[job_name] = files
        return TaskHandle(source=self.id, job_name=job_name, nzo_id=job_name)

    async def get_status(self, handle: TaskHandle) -> DownloadTaskStatus:
        files = self._jobs.get(handle.job_name, [])
        size = sum(f.stat().st_size for f in files if f.exists())
        return DownloadTaskStatus(
            task_id="",
            status="completed" if files else "failed",
            files_total=len(files),
            files_completed=len(files),
            bytes_total=size,
            bytes_downloaded=size,
            progress_percent=100.0 if files else 0.0,
            succeeded_filenames=[f.name for f in files],
            error=None if files else "unknown mock job",
        )

    async def cancel(self, handle: TaskHandle) -> bool:
        files = self._jobs.pop(handle.job_name, [])
        for f in files:
            try:
                f.unlink(missing_ok=True)
            except OSError:
                pass
        return True

    async def completed_path(self, handle: TaskHandle) -> list[Path]:
        return list(self._jobs.get(handle.job_name, []))

    async def shutdown(self) -> None:
        self._jobs.clear()
