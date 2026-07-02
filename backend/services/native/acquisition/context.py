"""The single I/O step (ArrRebuild design principle #1).

``build_context`` gathers EVERYTHING the pure specs need into one immutable
``DecisionContext`` BEFORE any spec runs, so every spec is a pure function that
needs no DB, no client, and no mocks. Lidarr does its I/O inside ``IsSatisfiedBy``;
we hoist it all here.

Grows per migration step: step 1 needed only the quarantine/blocklist snapshot;
step 2 adds ``now`` (for the Usenet retention/min-age specs) and ``free_bytes`` (the
free-space spec - supplied with the download client's destination path; ``None``
means "unknown", and the spec then passes). Later steps add library holdings /
worst-tier and indexer caps.
"""

import shutil
import time
from pathlib import Path

import msgspec

from infrastructure.persistence.download_store import DownloadStore


class DecisionContext(msgspec.Struct, frozen=True, kw_only=True):
    """Immutable snapshot of external state for one ranking pass."""

    quarantine_set: frozenset[tuple[str, str]] = msgspec.field(default_factory=frozenset)
    # Unix time captured once when the context was built (specs are pure: they read this,
    # never the clock). 0.0 disables the time-based specs.
    now: float = 0.0
    # Free bytes on the download destination filesystem; None = unknown (free-space spec passes).
    free_bytes: int | None = None
    # The held library tier this run must STRICTLY beat (upgrade-floor spec). Set only
    # for an origin='upgrade' run - the album's worst tier, or the recording's best tier
    # for a per-track upgrade (D12). None = not an upgrade run (the spec passes).
    held_tier: str | None = None


async def build_context(
    store: DownloadStore,
    *,
    now: float | None = None,
    free_path: "Path | str | None" = None,
    held_tier: str | None = None,
) -> DecisionContext:
    """The ONLY I/O step. Snapshots the blocklist/quarantine set, stamps ``now``, and
    (when a destination path is supplied) reads its free bytes. ``free_path`` is the
    download CLIENT's destination dir - not our staging dir - so the scorers leave it
    ``None`` until the source strategy can supply it (step 4); the spec then passes.
    ``held_tier`` arrives pre-resolved by the source strategy (which knows the task's
    origin and whether the floor is album-worst or per-recording), so this stays the
    single I/O step without growing a library handle."""
    quarantined = await store.load_quarantine_set()
    free_bytes: int | None = None
    if free_path is not None:
        try:
            free_bytes = shutil.disk_usage(Path(free_path)).free
        except OSError:
            free_bytes = None
    return DecisionContext(
        quarantine_set=frozenset(quarantined),
        now=now if now is not None else time.time(),
        free_bytes=free_bytes,
        held_tier=held_tier,
    )
