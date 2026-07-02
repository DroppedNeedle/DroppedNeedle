"""Upgrade-only recycle bin (CollectionManagement D4/D19).

A file replaced by a quality upgrade is MOVED here instead of deleted, so a bad
swap is recoverable for ``recycle_retention_days``. Scope is deliberately narrow:
user-initiated album deletes stay hard deletes (``LibraryService._unlink_paths``
is untouched) so freed space is immediately visible to the storage cap.

Each recycled file gets its own ``<timestamp>-<uuid>`` entry directory, so two
files with the same basename (or the same file recycled twice) can never collide.
The default bin lives at ``<first library path>/.recycle`` - dot-prefixed, so the
scanner and filesystem reconcile both skip it.
"""

import logging
import shutil
import uuid
from datetime import datetime, timedelta, timezone
from pathlib import Path

logger = logging.getLogger(__name__)

# Entry directories are "<stamp>-<uuid8>"; the stamp is authoritative for prune
# age (an EXDEV copy+unlink move rewrites mtimes, so mtime alone would lie).
_ENTRY_STAMP_FORMAT = "%Y%m%dT%H%M%S"


def resolve_bin_path(recycle_bin_path: str, library_paths: list[str]) -> Path | None:
    """The effective bin directory: the configured path, else ``.recycle`` under
    the first library path, else ``None`` when no library is configured yet.
    A RELATIVE configured path is ignored (it would resolve against the server's
    CWD and scatter recycled files who-knows-where)."""
    configured = recycle_bin_path.strip()
    if configured:
        path = Path(configured)
        if path.is_absolute():
            return path
        logger.warning(
            "recycle_bin_path %r is not absolute; using the library default", configured
        )
    if library_paths:
        return Path(library_paths[0]) / ".recycle"
    return None


def recycle(path: Path, bin_path: Path) -> Path:
    """Move ``path`` into the bin (never delete); returns the new location.
    ``shutil.move`` falls back to copy+unlink across filesystems (EXDEV)."""
    stamp = datetime.now(timezone.utc).strftime(_ENTRY_STAMP_FORMAT)
    entry_dir = bin_path / f"{stamp}-{uuid.uuid4().hex[:8]}"
    entry_dir.mkdir(parents=True, exist_ok=True)
    destination = entry_dir / path.name
    shutil.move(str(path), str(destination))
    logger.info("recycle_bin.recycled", extra={"path": str(path), "recycled_to": str(destination)})
    return destination


def _entry_age_cutoff_ok(entry: Path, cutoff: datetime) -> bool:
    """True when the entry is older than the cutoff. Age comes from the entry
    directory's name stamp; unparseable names fall back to mtime."""
    stamp = entry.name.split("-", 1)[0]
    try:
        created = datetime.strptime(stamp, _ENTRY_STAMP_FORMAT).replace(tzinfo=timezone.utc)
    except ValueError:
        created = datetime.fromtimestamp(entry.stat().st_mtime, tz=timezone.utc)
    return created < cutoff


def prune(bin_path: Path, retention_days: int) -> int:
    """Delete entries older than the retention window; returns how many entries
    were removed. Never touches anything outside ``bin_path``."""
    if not bin_path.is_dir():
        return 0
    cutoff = datetime.now(timezone.utc) - timedelta(days=retention_days)
    removed = 0
    for entry in bin_path.iterdir():
        try:
            if not _entry_age_cutoff_ok(entry, cutoff):
                continue
            if entry.is_dir():
                shutil.rmtree(entry)
            else:
                entry.unlink()
            removed += 1
        except OSError as exc:
            logger.warning("recycle_bin.prune_failed for %s: %s", entry, exc)
    return removed
