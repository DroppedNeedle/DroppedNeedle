"""Background workers owned by the target-only application process."""

from __future__ import annotations

import asyncio
import logging
import os
import socket
from collections.abc import Callable

from core.task_registry import TaskRegistry
from services.native.album_identification_service import AlbumIdentificationService
from services.native.identification_queue_service import IdentificationQueueService
from services.native.library_operation_supervisor import LibraryOperationSupervisor
from services.native.background_workload_gate import BackgroundWorkloadGate

logger = logging.getLogger(__name__)
WORKER_INTERVAL_SECONDS = 1.0


def _worker_id(kind: str) -> str:
    return f"{socket.gethostname()}:{os.getpid()}:{kind}"


def _log_worker_error(task: asyncio.Task[None], *, name: str) -> None:
    if task.cancelled():
        return
    error = task.exception()
    if error is not None:
        logger.error(
            "%s stopped unexpectedly",
            name,
            exc_info=(type(error), error, error.__traceback__),
        )


async def run_target_identification_worker(
    queue_getter: Callable[[], IdentificationQueueService],
    service_getter: Callable[[], AlbumIdentificationService],
    *,
    worker_id: str | None = None,
    workload_gate: BackgroundWorkloadGate | None = None,
) -> None:
    owner = worker_id or _worker_id("identification")
    while True:
        try:
            if workload_gate is None or not workload_gate.scan_active:
                queue = queue_getter()
                await queue.recover()
                if await queue.is_paused():
                    await asyncio.sleep(WORKER_INTERVAL_SECONDS)
                    continue
                job = None
                if workload_gate is None or not workload_gate.scan_active:
                    job = await queue.claim(owner)
                if job is not None:
                    await service_getter().run_claimed_job(job, owner)
        except asyncio.CancelledError:
            break
        except Exception:  # noqa: BLE001 - a durable worker must survive one failed item
            logger.exception("Target identification worker iteration failed")
        try:
            await asyncio.sleep(WORKER_INTERVAL_SECONDS)
        except asyncio.CancelledError:
            break


async def run_target_operation_worker(
    supervisor_getter: Callable[[], LibraryOperationSupervisor],
    *,
    worker_id: str | None = None,
) -> None:
    owner = worker_id or _worker_id("operation")
    while True:
        try:
            supervisor = supervisor_getter()
            await supervisor.recover()
            await supervisor.run_once(owner)
        except asyncio.CancelledError:
            break
        except Exception:  # noqa: BLE001 - a durable worker must survive one failed item
            logger.exception("Target operation worker iteration failed")
        try:
            await asyncio.sleep(WORKER_INTERVAL_SECONDS)
        except asyncio.CancelledError:
            break


def start_target_identification_worker(
    queue_getter: Callable[[], IdentificationQueueService],
    service_getter: Callable[[], AlbumIdentificationService],
    workload_gate: BackgroundWorkloadGate | None = None,
) -> asyncio.Task[None]:
    name = "target-library-identification-worker"
    task = asyncio.create_task(
        run_target_identification_worker(
            queue_getter, service_getter, workload_gate=workload_gate
        )
    )
    TaskRegistry.get_instance().register(name, task)
    task.add_done_callback(lambda item: _log_worker_error(item, name=name))
    return task


def start_target_operation_worker(
    supervisor_getter: Callable[[], LibraryOperationSupervisor],
) -> asyncio.Task[None]:
    name = "target-library-operation-worker"
    task = asyncio.create_task(run_target_operation_worker(supervisor_getter))
    TaskRegistry.get_instance().register(name, task)
    task.add_done_callback(lambda item: _log_worker_error(item, name=name))
    return task
