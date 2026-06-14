from __future__ import annotations

from infrastructure.memory import get_rss_bytes, trim_malloc


def test_get_rss_bytes_is_positive_on_linux():
    rss = get_rss_bytes()
    assert rss is None or rss > 0
    if rss is not None:
        assert rss > 1_000_000


def test_trim_malloc_returns_bool_without_raising():
    result = trim_malloc()
    assert isinstance(result, bool)


def test_trim_malloc_is_safe_to_repeat():
    first = trim_malloc()
    second = trim_malloc()
    assert isinstance(first, bool)
    assert isinstance(second, bool)
