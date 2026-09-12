from __future__ import annotations

import asyncio
import logging
from contextlib import asynccontextmanager
from time import monotonic
from typing import AsyncIterator, Awaitable, Callable


logger = logging.getLogger(__name__)

INTERACTIVE_QUIET_PERIOD_SECONDS = 5.0
INTERACTIVE_MAX_DEFERRAL_SECONDS = 120.0


class BackgroundWorkloadGate:
    """Coordinate scan safety and fair yielding to interactive API activity.

    ``scan_active`` remains a strict safety signal for code that must not overlap a
    library scan. Background warmers use ``wait_until_available`` and additionally
    wait for a short interactive quiet window. Continuous traffic cannot starve
    maintenance: one bounded unit is admitted after the maximum deferral, and the
    caller checks the gate again before starting its next unit.
    """

    def __init__(
        self,
        *,
        interactive_quiet_period: float = INTERACTIVE_QUIET_PERIOD_SECONDS,
        interactive_max_deferral: float = INTERACTIVE_MAX_DEFERRAL_SECONDS,
    ) -> None:
        self._available = asyncio.Event()
        self._available.set()
        self._warmer_slot = asyncio.Lock()
        self._state_changed = asyncio.Event()
        self._generation = 0
        self._last_interactive_at: float | None = None
        self._active_interactive_requests = 0
        self._interactive_quiet_period = max(0.0, interactive_quiet_period)
        self._interactive_max_deferral = max(0.0, interactive_max_deferral)
        self.deferred_waits = 0
        self.forced_passes = 0
        self.total_deferred_seconds = 0.0

    @property
    def scan_active(self) -> bool:
        return not self._available.is_set()

    def set_scan_active(self, active: bool) -> None:
        if active:
            self._available.clear()
        else:
            self._available.set()
        self._generation += 1
        self._state_changed.set()

    def begin_interactive_request(self) -> None:
        """Hold background admission while an interactive handler is active."""
        self._active_interactive_requests += 1
        self._touch_interactive_activity()

    def end_interactive_request(self) -> None:
        """Release an interactive hold and start its post-request quiet window."""
        if self._active_interactive_requests <= 0:
            raise RuntimeError("No interactive request is active")
        self._active_interactive_requests -= 1
        self._touch_interactive_activity()

    def _touch_interactive_activity(self) -> None:
        self._last_interactive_at = monotonic()
        self._generation += 1
        self._state_changed.set()

    @asynccontextmanager
    async def warmer_slot(self) -> AsyncIterator[None]:
        """Serialize bounded proactive warmer units across warmer families."""
        async with self._warmer_slot:
            await self.wait_until_available()
            yield

    async def run_warmer_unit[T](self, operation: Callable[[], Awaitable[T]]) -> T:
        await self.wait_until_available()
        async with self.warmer_slot():
            return await operation()

    async def wait_until_available(self) -> None:
        started = monotonic()
        deferred = False
        forced = False

        while True:
            await self._available.wait()
            now = monotonic()
            last_activity = self._last_interactive_at
            quiet_remaining = (
                0.0
                if last_activity is None
                else self._interactive_quiet_period - (now - last_activity)
            )
            max_remaining = self._interactive_max_deferral - (now - started)

            if (
                self._available.is_set()
                and self._active_interactive_requests == 0
                and quiet_remaining <= 0
            ):
                break
            if self._available.is_set() and max_remaining <= 0:
                forced = True
                break

            deferred = True
            generation = self._generation
            self._state_changed.clear()
            if generation != self._generation:
                continue

            timeout = max(
                0.001,
                max_remaining
                if self._active_interactive_requests > 0
                else min(quiet_remaining, max_remaining),
            )
            try:
                await asyncio.wait_for(self._state_changed.wait(), timeout=timeout)
            except asyncio.TimeoutError:
                pass

        if deferred:
            waited = monotonic() - started
            self.deferred_waits += 1
            self.total_deferred_seconds += waited
            if forced:
                self.forced_passes += 1
            logger.info(
                "Background work gate resumed after %.2fs (fairness_pass=%s)",
                waited,
                forced,
            )
