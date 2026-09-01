import logging

import pytest

import infrastructure.resilience.retry as retry_module
from infrastructure.resilience.retry import CircuitBreaker, with_retry


@pytest.mark.asyncio
async def test_retry_budget_stops_before_oversized_sleep(monkeypatch, caplog) -> None:
    now = 0.0
    sleeps: list[float] = []
    calls = 0
    breaker = CircuitBreaker(failure_threshold=5, name="budget-test")

    async def fake_sleep(delay: float) -> None:
        nonlocal now
        sleeps.append(delay)
        now += delay

    monkeypatch.setattr(retry_module.time, "monotonic", lambda: now)
    monkeypatch.setattr(retry_module.asyncio, "sleep", fake_sleep)

    @with_retry(
        max_attempts=5,
        base_delay=1.0,
        jitter=False,
        retry_budget_seconds=1.5,
        circuit_breaker=breaker,
        retriable_exceptions=(RuntimeError,),
    )
    async def fail() -> None:
        nonlocal calls
        calls += 1
        raise RuntimeError("boom")

    with caplog.at_level(logging.ERROR), pytest.raises(RuntimeError, match="boom"):
        await fail()

    assert calls == 2
    assert sleeps == [1.0]
    assert breaker.failure_count == 1
    assert "failed after 2 attempts (RuntimeError): boom" in caplog.text


@pytest.mark.asyncio
async def test_retry_budget_stops_before_scheduler_managed_delay(
    monkeypatch, caplog
) -> None:
    monkeypatch.setattr(retry_module.time, "monotonic", lambda: 0.0)
    calls = 0
    breaker = CircuitBreaker(failure_threshold=5, name="managed-budget-test")

    @with_retry(
        max_attempts=5,
        retry_budget_seconds=1.5,
        circuit_breaker=breaker,
        retriable_exceptions=(RuntimeError,),
    )
    async def fail() -> None:
        nonlocal calls
        calls += 1
        error = RuntimeError("managed delay")
        error._retry_delay_managed = True
        error._retry_delay_managed_seconds = 2.0
        raise error

    with (
        caplog.at_level(logging.ERROR),
        pytest.raises(RuntimeError, match="managed delay"),
    ):
        await fail()

    assert calls == 1
    assert breaker.failure_count == 1
    assert "failed after 1 attempt (RuntimeError): managed delay" in caplog.text


@pytest.mark.asyncio
async def test_retry_log_names_exception_with_empty_message(caplog) -> None:
    class BlankError(Exception):
        def __str__(self) -> str:
            return ""

    @with_retry(max_attempts=1, retriable_exceptions=(BlankError,))
    async def fail() -> None:
        raise BlankError()

    with caplog.at_level(logging.ERROR), pytest.raises(BlankError):
        await fail()

    assert "failed after 1 attempt (BlankError):" in caplog.text


@pytest.mark.parametrize("budget", [0, -1, float("inf"), float("nan")])
def test_retry_budget_must_be_positive_and_finite(budget: float) -> None:
    with pytest.raises(ValueError, match="positive finite"):

        @with_retry(retry_budget_seconds=budget)
        async def unused() -> None:
            return None
