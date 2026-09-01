import asyncio
import logging
import math
import random
import time
from enum import Enum
from functools import wraps
from typing import Awaitable, Callable, TypeVar, ParamSpec, Optional

logger = logging.getLogger(__name__)

P = ParamSpec("P")
T = TypeVar("T")


class CircuitState(Enum):
    CLOSED = "closed"
    OPEN = "open"
    HALF_OPEN = "half_open"


CircuitStateChangeCallback = Callable[
    ["CircuitBreaker", CircuitState, CircuitState, str], None
]

# The per-call "breaker is OPEN" warning is rate-limited so a hot retry loop
# cannot flood the logs while the breaker stays open; the CircuitOpenError
# raise is unaffected. Best-effort under concurrency is acceptable.
OPEN_WARNING_INTERVAL_SECONDS = 30.0


class CircuitBreaker:
    def __init__(
        self,
        failure_threshold: int = 5,
        success_threshold: int = 2,
        timeout: float = 60.0,
        name: str = "default",
        on_state_change: CircuitStateChangeCallback | None = None,
    ):
        self.failure_threshold = failure_threshold
        self.success_threshold = success_threshold
        self.timeout = timeout
        self.name = name
        self._on_state_change = on_state_change
        self._lock = asyncio.Lock()

        self.failure_count = 0
        self.success_count = 0
        self.last_failure_time: float = 0
        self.state = CircuitState.CLOSED
        self._last_open_warning: float = 0.0

    def _notify_state_change(
        self,
        previous_state: CircuitState,
        new_state: CircuitState,
        reason: str,
    ) -> None:
        if previous_state == new_state or self._on_state_change is None:
            return

        try:
            self._on_state_change(self, previous_state, new_state, reason)
        except Exception:
            logger.exception(
                "Circuit breaker '%s' state change callback failed",
                self.name,
            )

    def is_open(self) -> bool:
        if self.state == CircuitState.OPEN:
            if time.time() - self.last_failure_time > self.timeout:
                previous_state = self.state
                self.state = CircuitState.HALF_OPEN
                self.success_count = 0
                self._notify_state_change(previous_state, self.state, "timeout_elapsed")
                return False
            return True
        return False

    def remaining_open_seconds(self) -> float:
        if self.state != CircuitState.OPEN:
            return 0.0
        elapsed = time.time() - self.last_failure_time
        remaining = self.timeout - elapsed
        return max(0.0, remaining)

    async def aremaining_open_seconds(self) -> float:
        async with self._lock:
            return self.remaining_open_seconds()

    def should_log_open_warning(self) -> bool:
        now = time.monotonic()
        if now - self._last_open_warning < OPEN_WARNING_INTERVAL_SECONDS:
            return False
        self._last_open_warning = now
        return True

    def record_success(self):
        if self.state == CircuitState.HALF_OPEN:
            self.success_count += 1
            if self.success_count >= self.success_threshold:
                previous_state = self.state
                self.state = CircuitState.CLOSED
                self.failure_count = 0
                self.success_count = 0
                self._notify_state_change(
                    previous_state, self.state, "success_threshold_reached"
                )
        elif self.state == CircuitState.CLOSED:
            self.failure_count = 0

    def record_failure(self):
        """One breaker outcome per LOGICAL call (QW11 Part 1 contract).

        ``with_retry`` calls this exactly once, after the final attempt of a
        logical call - never once per attempt - so ``failure_threshold``
        means "5 bad calls", not "5 bad attempts". HALF_OPEN is deliberately
        conservative: a single probe failure reopens immediately instead of
        waiting out the full threshold, because the provider just failed on
        its first real request after the timeout window.
        """
        self.last_failure_time = time.time()

        if self.state == CircuitState.HALF_OPEN:
            logger.warning(
                "Circuit breaker '%s' reopening after failure in HALF_OPEN",
                self.name,
            )
            previous_state = self.state
            self.state = CircuitState.OPEN
            self.failure_count = 0
            self.success_count = 0
            self._notify_state_change(previous_state, self.state, "half_open_failure")
        elif self.state == CircuitState.CLOSED:
            self.failure_count += 1
            if self.failure_count >= self.failure_threshold:
                logger.error(
                    "Circuit breaker '%s' opening after %d failures",
                    self.name,
                    self.failure_count,
                )
                previous_state = self.state
                self.state = CircuitState.OPEN
                self._notify_state_change(
                    previous_state, self.state, "failure_threshold_reached"
                )

    def get_state(self) -> dict:
        return {
            "name": self.name,
            "state": self.state.value,
            "failure_count": self.failure_count,
            "success_count": self.success_count,
            "last_failure_time": self.last_failure_time,
        }

    def reset(self):
        previous_state = self.state
        self.state = CircuitState.CLOSED
        self.failure_count = 0
        self.success_count = 0
        self.last_failure_time = 0
        self._last_open_warning = 0.0
        self._notify_state_change(previous_state, self.state, "manual_reset")

    async def arecord_success(self):
        async with self._lock:
            self.record_success()

    async def arecord_failure(self):
        async with self._lock:
            self.record_failure()

    async def atry_transition(self):
        """Acquire the lock and attempt an OPEN -> HALF_OPEN transition if the timeout has elapsed."""
        if self.state != CircuitState.OPEN:
            return
        async with self._lock:
            if (
                self.state == CircuitState.OPEN
                and time.time() - self.last_failure_time > self.timeout
            ):
                previous_state = self.state
                self.state = CircuitState.HALF_OPEN
                self.success_count = 0
                self._notify_state_change(previous_state, self.state, "timeout_elapsed")


class CircuitOpenError(Exception):
    def __init__(
        self,
        message: str,
        breaker_name: str = "",
        retry_after_seconds: float | None = None,
    ):
        super().__init__(message)
        self.breaker_name = breaker_name
        try:
            value = (
                float(retry_after_seconds) if retry_after_seconds is not None else None
            )
        except (TypeError, ValueError):
            value = None
        if value is None or not math.isfinite(value) or value <= 0:
            self.retry_after_seconds: float | None = None
        else:
            self.retry_after_seconds = value


def _get_retry_after_seconds(exception: Exception) -> Optional[float]:
    retry_after = getattr(exception, "retry_after_seconds", None)
    if retry_after is None:
        return None
    try:
        retry_after_value = float(retry_after)
    except (TypeError, ValueError):
        return None
    if not math.isfinite(retry_after_value) or retry_after_value <= 0:
        return None
    return retry_after_value


def with_retry(
    max_attempts: int = 3,
    base_delay: float = 1.0,
    max_delay: float = 10.0,
    exponential_base: float = 2.0,
    jitter: bool = True,
    circuit_breaker: Optional[CircuitBreaker | Callable[..., CircuitBreaker]] = None,
    retriable_exceptions: tuple = (Exception,),
    non_breaking_exceptions: tuple = (),
    non_retriable_exceptions: tuple = (),
    retry_budget_seconds: float | None = None,
):
    if max_attempts < 1:
        raise ValueError("max_attempts must be >= 1")
    if retry_budget_seconds is not None and (
        not math.isfinite(retry_budget_seconds) or retry_budget_seconds <= 0
    ):
        raise ValueError("retry_budget_seconds must be a positive finite number")

    def decorator(func: Callable[P, Awaitable[T]]) -> Callable[P, Awaitable[T]]:
        # QW11 Part 1: report once per logical call - after the final attempt,
        # not per retry, so a 3-attempt failure chain bumps failure_count once.
        # See ``CircuitBreaker.record_failure`` for HALF_OPEN policy.
        @wraps(func)
        async def wrapper(*args: P.args, **kwargs: P.kwargs) -> T:
            breaker = circuit_breaker
            if breaker is not None and not isinstance(breaker, CircuitBreaker):
                breaker = breaker(*args, **kwargs)
            service_name = breaker.name if breaker else "unknown"
            func_name = func.__name__

            if breaker:
                await breaker.atry_transition()
                retry_after = await breaker.aremaining_open_seconds()
                if breaker.is_open():
                    if breaker.should_log_open_warning():
                        logger.warning(
                            "Circuit breaker '%s' is OPEN",
                            breaker.name,
                            extra={"service_name": service_name, "function": func_name},
                        )
                    if (
                        retry_after is None
                        or not math.isfinite(retry_after)
                        or retry_after <= 0
                    ):
                        retry_after = breaker.timeout
                        if not math.isfinite(retry_after) or retry_after <= 0:
                            retry_after = None
                    raise CircuitOpenError(
                        f"Circuit breaker '{breaker.name}' is OPEN",
                        breaker_name=breaker.name,
                        retry_after_seconds=retry_after,
                    )

            last_exception = None
            attempts_made = 0
            started_at = time.monotonic()
            should_log_failure = False

            for attempt in range(1, max_attempts + 1):
                attempts_made = attempt
                try:
                    result = await func(*args, **kwargs)

                    if breaker:
                        await breaker.arecord_success()

                    return result

                except retriable_exceptions as e:
                    last_exception = e

                    if non_retriable_exceptions and isinstance(
                        e, non_retriable_exceptions
                    ):
                        should_log_failure = not (
                            non_breaking_exceptions
                            and isinstance(e, non_breaking_exceptions)
                        )
                        break

                    if attempt >= max_attempts:
                        should_log_failure = True
                        break

                    if getattr(e, "_retry_delay_managed", False):
                        if retry_budget_seconds is not None:
                            managed_delay = getattr(
                                e, "_retry_delay_managed_seconds", None
                            )
                            if managed_delay is not None:
                                try:
                                    managed_delay = float(managed_delay)
                                except (TypeError, ValueError):
                                    managed_delay = None
                            if managed_delay is not None and (
                                not math.isfinite(managed_delay) or managed_delay < 0
                            ):
                                managed_delay = None
                            if managed_delay is not None:
                                elapsed = time.monotonic() - started_at
                                remaining = retry_budget_seconds - elapsed
                                if remaining <= 0 or managed_delay >= remaining:
                                    should_log_failure = True
                                    break
                        # A shared provider scheduler already admitted and
                        # paced the next attempt; do not sleep twice here.
                        continue
                    retry_after_override = _get_retry_after_seconds(e)
                    if retry_after_override is not None:
                        delay = retry_after_override
                    else:
                        delay = min(
                            base_delay * (exponential_base ** (attempt - 1)),
                            max_delay,
                        )
                        if jitter:
                            delay *= 0.5 + random.random()

                    if retry_budget_seconds is not None:
                        elapsed = time.monotonic() - started_at
                        remaining = retry_budget_seconds - elapsed
                        if remaining <= 0 or delay >= remaining:
                            should_log_failure = True
                            break

                    await asyncio.sleep(delay)

            if last_exception is None:
                raise RuntimeError(f"{func_name} retry loop ended without an exception")

            if should_log_failure:
                logger.error(
                    "%s failed after %d attempt%s (%s): %s",
                    func_name,
                    attempts_made,
                    "" if attempts_made == 1 else "s",
                    type(last_exception).__name__,
                    last_exception,
                )

            if breaker:
                is_non_breaking = (
                    isinstance(last_exception, non_breaking_exceptions)
                    if non_breaking_exceptions
                    else False
                )
                if not is_non_breaking:
                    await breaker.arecord_failure()

            raise last_exception

        return wrapper

    return decorator
