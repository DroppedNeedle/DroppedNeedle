"""Shared library-activity revision contract for the SSE event bus.

The central revision poller (``library_revision_poller``) owns the only poll
loop; this module keeps the cadence constant and the revision-source protocol.
"""

from __future__ import annotations

from typing import Protocol

ACTIVITY_POLL_INTERVAL_SECONDS = 2.0


class ActivityRevisionSource(Protocol):
    async def stream_revisions(self) -> dict[str, int]: ...
