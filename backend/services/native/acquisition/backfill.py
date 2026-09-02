"""One-shot startup backfill: snapshot pre-existing acquisition rows.

Runs once per database (marker row ``acquisition_snapshot_backfill``), writing
the migration-time global policy as an origin=``legacy_migration``
AcquisitionQualitySnapshot onto every nonterminal native download task, every
search job that still lacks one (searching rows plus parked-review jobs linked
from those tasks), and every Free Music task. Later settings saves can never
mutate these stored blobs - restart-with-current-policy is the explicit
refresh. Reruns are no-ops: the marker short-circuits AND every UPDATE skips
rows already carrying a snapshot, so a crash mid-run converges on the next
startup instead of double-writing.
"""

import logging

from infrastructure.persistence.download_store import DownloadStore
from infrastructure.persistence.free_music_store import FreeMusicStore
from services.native.acquisition import quality

logger = logging.getLogger(__name__)

_NATIVE_ACTIVE_STATUSES = ("queued", "downloading", "processing")


def _snapshot_blob(snapshot) -> str:
    return quality.encode_snapshot(snapshot)


async def run_acquisition_snapshot_backfill(
    download_store: DownloadStore,
    free_music_store: FreeMusicStore,
    policy_provider,
) -> dict:
    """Marker-gated sweep; returns counters for startup logging."""
    if await download_store.acquisition_backfill_completed():
        return {"skipped": True}
    snapshot = quality.migration_snapshot(policy_provider())
    blob = _snapshot_blob(snapshot)

    tasks = await download_store.list_tasks_missing_snapshot(
        list(_NATIVE_ACTIVE_STATUSES)
    )
    task_updates = [
        {
            "id": task.id,
            "quality_snapshot_json": blob,
            "quality_snapshot_hash": snapshot.snapshot_hash,
            "quality_snapshot_summary": snapshot.summary,
        }
        for task in tasks
    ]
    native_count = (
        await download_store.backfill_task_quality_fields(task_updates)
        if task_updates
        else 0
    )

    # Search jobs: parked-review jobs linked from the snapshotted tasks plus any
    # still-searching rows. The task-linked set is authoritative for review.
    job_ids: list[str] = []
    jobs_missing = await download_store.list_search_jobs_missing_snapshot()
    by_id = {job.id: job for job in jobs_missing}
    for task in tasks:
        if task.search_job_id and task.search_job_id in by_id:
            job_ids.append(task.search_job_id)
    seen: set[str] = set()
    job_updates: list[dict] = []
    for job_id in job_ids + [j.id for j in jobs_missing]:
        if job_id in seen:
            continue
        seen.add(job_id)
        job_updates.append(
            {
                "id": job_id,
                "quality_snapshot_json": blob,
                "quality_snapshot_hash": snapshot.snapshot_hash,
                "quality_snapshot_summary": snapshot.summary,
            }
        )
    job_count = (
        await download_store.backfill_search_job_quality_snapshots(job_updates)
        if job_updates
        else 0
    )

    fm_tasks = await free_music_store.list_tasks_missing_snapshot()
    fm_updates = [
        {
            "id": task.id,
            "quality_snapshot_json": blob,
            "quality_snapshot_hash": snapshot.snapshot_hash,
            "quality_snapshot_summary": snapshot.summary,
        }
        for task in fm_tasks
    ]
    fm_count = (
        await free_music_store.backfill_free_music_quality_fields(fm_updates)
        if fm_updates
        else 0
    )
    await download_store.mark_acquisition_backfill(
        native_tasks=native_count, search_jobs=job_count + fm_count
    )
    logger.info(
        "acquisition.snapshot_backfill",
        extra={
            "native_tasks": native_count,
            "search_jobs": job_count,
            "free_music_tasks": fm_count,
            "snapshot_hash": snapshot.snapshot_hash[:12],
        },
    )
    return {
        "skipped": False,
        "native_tasks": native_count,
        "search_jobs": job_count,
        "free_music_tasks": fm_count,
    }
