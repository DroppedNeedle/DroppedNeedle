"""Fail-closed validation for the separately started target-only application."""

from __future__ import annotations

import logging
from collections.abc import Callable
from typing import Any, Literal

from core.exceptions import TargetStartupInvariantError
from infrastructure.persistence.native_library_store import NativeLibraryStore

logger = logging.getLogger(__name__)

TargetStartupValidationPhase = Literal["cutover", "steady_state"]


class TargetStartupValidator:
    def __init__(
        self,
        store: NativeLibraryStore,
        configured_root_ids: Callable[[], set[str]] | None = None,
    ) -> None:
        self._store = store
        self._configured_root_ids = configured_root_ids

    async def validate(self, phase: TargetStartupValidationPhase) -> dict[str, Any]:
        state = await self._store.get_target_startup_state()
        marker = state["marker"]
        migration = state["migration"]
        if marker is None or migration is None:
            raise TargetStartupInvariantError(
                "The completed legacy-catalog migration marker is missing."
            )
        if (
            migration["state"] != "completed"
            or migration["source_revision"] != marker["source_revision"]
            or migration["completed_at"] is None
        ):
            raise TargetStartupInvariantError(
                "The migration run does not match the completed target marker."
            )
        if int(marker["target_catalog_revision"]) > int(state["catalog_revision"]):
            raise TargetStartupInvariantError(
                "The target catalog revision predates its migration marker."
            )
        if phase == "cutover":
            invariants = await self._store.validate_migrated_catalog()
        elif phase == "steady_state":
            invariants = await self._store.validate_catalog_integrity()
        else:
            raise ValueError(f"Unsupported target startup validation phase: {phase}")
        failures = {name: count for name, count in invariants.items() if count != 0}
        if failures:
            logger.error(
                "target_startup.catalog_integrity_failed phase=%s counters=%s",
                phase,
                ",".join(f"{name}={count}" for name, count in sorted(failures.items())),
            )
            raise TargetStartupInvariantError(
                "The target catalog failed startup integrity validation."
            )
        if phase == "cutover" and self._configured_root_ids is not None:
            configured = self._configured_root_ids()
            migrated = await self._store.get_migrated_root_ids()
            if configured != migrated:
                raise TargetStartupInvariantError(
                    "The configured library roots do not match the migrated catalog."
                )
        return {**state, "invariants": invariants}
