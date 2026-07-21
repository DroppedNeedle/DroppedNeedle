from __future__ import annotations

import asyncio


class BackgroundWorkloadGate:
    def __init__(self) -> None:
        self._available = asyncio.Event()
        self._available.set()

    @property
    def scan_active(self) -> bool:
        return not self._available.is_set()

    def set_scan_active(self, active: bool) -> None:
        if active:
            self._available.clear()
        else:
            self._available.set()

    async def wait_until_available(self) -> None:
        await self._available.wait()
