"""F-PERF-08: HTTP client construction is settings-order independent.

The factory caches by an immutable effective-construction key (logical name +
timeout, connect timeout, pool limits, HTTP/2, User-Agent, normalized extra
kwargs), so the first caller can never fix configuration for later callers.
Superseded generations retire through an awaited lifecycle and close exactly
once; synchronous lookups never close anything."""

import asyncio

import httpx
import pytest
from types import SimpleNamespace

from infrastructure.http.client import (
    HttpClientFactory,
    get_coverart_http_client,
    get_http_client,
    get_listenbrainz_http_client,
)


@pytest.fixture(autouse=True)
def _isolated_factory():
    HttpClientFactory.reset_for_tests()
    yield
    HttpClientFactory.reset_for_tests()


def _settings(user_agent: str = "dropneedle/test") -> SimpleNamespace:
    return SimpleNamespace(
        get_user_agent=lambda: user_agent,
        http_timeout=10.0,
        http_connect_timeout=5.0,
        http_max_connections=200,
        http_max_keepalive=100,
    )


@pytest.mark.parametrize("first_timeout,second_timeout", [(5.0, 30.0), (30.0, 5.0)])
def test_same_name_different_timeouts_never_share(
    first_timeout: float, second_timeout: float
):
    constructed: list[dict] = []
    real_init = httpx.AsyncClient.__init__

    def recording_init(self, *args, **kwargs):
        constructed.append(kwargs)
        return real_init(self, *args, **kwargs)

    httpx.AsyncClient.__init__ = recording_init
    try:
        first = HttpClientFactory.get_client(
            name="default",
            timeout=first_timeout,
            connect_timeout=1.0,
            max_connections=10,
            max_keepalive=10,
        )
        second = HttpClientFactory.get_client(
            name="default",
            timeout=second_timeout,
            connect_timeout=2.0,
            max_connections=99,
            max_keepalive=99,
        )
    finally:
        httpx.AsyncClient.__init__ = real_init

    assert first is not second
    assert second.timeout.read == second_timeout
    assert second.timeout.connect == 2.0
    # construction kwargs prove each caller got exactly what it asked for
    limits_first = constructed[0]["limits"]
    limits_second = constructed[1]["limits"]
    assert limits_first.max_connections == 10
    assert limits_second.max_connections == 99
    assert limits_second.max_keepalive_connections == 99
    assert constructed[0]["timeout"].read == first_timeout


def test_equal_effective_parameters_share_one_client():
    kwargs = {"trust_env": False}
    one = HttpClientFactory.get_client(name="pool-a", timeout=7.0, **kwargs)
    two = HttpClientFactory.get_client(name="pool-a", timeout=7.0, **dict(kwargs))
    assert one is two


def test_kwargs_participate_in_the_key():
    with_transport = HttpClientFactory.get_client(
        name="kw", timeout=5.0, trust_env=False
    )
    without = HttpClientFactory.get_client(name="kw", timeout=5.0)
    assert with_transport is not without


def test_unhashable_kwargs_are_encoded_not_dropped():
    base = dict(name="kw2", timeout=5.0)
    full = dict(connect_timeout=1.0, max_connections=5, max_keepalive=5, http2=True)
    one = HttpClientFactory._effective_key(
        user_agent="ua", kwargs={"tags": ["a", "b"]}, **base, **full
    )
    two = HttpClientFactory._effective_key(
        user_agent="ua", kwargs={"tags": ["a", "b"]}, **base, **full
    )
    assert one == two  # equal unhashable contents share one effective key

    reordered = HttpClientFactory._effective_key(
        user_agent="ua", kwargs={"meta": {"b": 1, "a": 2}}, **base, **full
    )
    same_meta = HttpClientFactory._effective_key(
        user_agent="ua", kwargs={"meta": {"a": 2, "b": 1}}, **base, **full
    )
    assert reordered == same_meta  # dict ordering is canonicalized


def test_user_agent_identity_is_part_of_the_key():
    first = HttpClientFactory.get_client(
        name="ua", timeout=5.0, settings=_settings("agent/one")
    )
    second = HttpClientFactory.get_client(
        name="ua", timeout=5.0, settings=_settings("agent/two")
    )
    assert first is not second
    assert first.headers["User-Agent"] == "agent/one"
    assert second.headers["User-Agent"] == "agent/two"


def test_named_clients_keep_distinct_policies():
    settings = _settings()
    default_client = get_http_client(settings)
    listenbrainz_client = get_listenbrainz_http_client(settings)
    coverart_client = get_coverart_http_client(settings)

    assert len({id(default_client), id(listenbrainz_client), id(coverart_client)}) == 3
    # ListenBrainz keeps its dedicated pool + HTTP/1.1 choice (http2 flag is
    # part of the key; the transport records the effective choice).
    assert listenbrainz_client is not default_client
    assert getattr(listenbrainz_client, "_transport", None) is not None
    # Cover-art keeps its short budget regardless of construction order.
    assert coverart_client.timeout.read == 6.0
    assert coverart_client.timeout.connect == 3.0


def test_concurrent_first_access_builds_once():
    constructions = {"n": 0}
    real_init = httpx.AsyncClient.__init__

    def counting_init(self, *args, **kwargs):
        constructions["n"] += 1
        return real_init(self, *args, **kwargs)

    httpx.AsyncClient.__init__ = counting_init
    try:

        async def gather_reads():
            loop = asyncio.get_running_loop()
            return await asyncio.gather(
                *[
                    loop.run_in_executor(
                        None,
                        lambda: HttpClientFactory.get_client(name="race", timeout=4.0),
                    )
                    for _ in range(8)
                ]
            )

        clients = asyncio.run(gather_reads())
    finally:
        httpx.AsyncClient.__init__ = real_init

    assert constructions["n"] == 1
    assert len({id(client) for client in clients}) == 1


def test_retire_name_moves_generations_without_closing_from_lookup():
    old = HttpClientFactory.get_client(name="cycle", timeout=1.0)
    closed_flags = []

    async def fake_aclose():
        closed_flags.append(True)

    object.__setattr__(old, "aclose", fake_aclose)

    moved = HttpClientFactory.retire_name("cycle")
    assert moved == 1
    new = HttpClientFactory.get_client(name="cycle", timeout=9.0)
    assert new is not old
    # A synchronous lookup must not have closed the retired client...
    assert closed_flags == []
    # ...and the awaited lifecycle path closes it exactly once.
    closed_now = asyncio.run(HttpClientFactory.close_retired())
    assert closed_now == 1 and closed_flags == [True]
    assert asyncio.run(HttpClientFactory.close_retired()) == 0  # idempotent drain


def test_close_all_closes_every_generation_exactly_once():
    closed: list[str] = []

    def make(tag: str) -> httpx.AsyncClient:
        client = HttpClientFactory.get_client(name=tag, timeout=1.0)

        async def fake_aclose():
            closed.append(tag)

        object.__setattr__(client, "aclose", fake_aclose)
        return client

    a = make("alpha")
    b = make("beta")
    HttpClientFactory.retire_name("alpha")
    make("gamma")

    asyncio.run(HttpClientFactory.close_all())
    assert sorted(closed) == ["alpha", "beta", "gamma"]

    # second call is safe: nothing left to close
    asyncio.run(HttpClientFactory.close_all())
    assert sorted(closed) == ["alpha", "beta", "gamma"]
    del a, b


def test_generation_cap_keeps_history_bounded():
    for index in range(HttpClientFactory._MAX_GENERATIONS + 6):
        HttpClientFactory.get_client(name=f"bounded-{index}", timeout=1.0)
    assert len(HttpClientFactory._clients) <= HttpClientFactory._MAX_GENERATIONS
    # superseded entries became retired work rather than being closed silently
    assert len(HttpClientFactory._retired) >= 6


def test_settings_save_hook_targets_http_fields_only(monkeypatch):
    from services.settings_service import SettingsService

    service = SettingsService.__new__(SettingsService)
    coverart_calls = {"n": 0}
    retire_calls: list[str] = []
    brainzmash_client_resets: list[object] = []

    async def fake_coverart():
        coverart_calls["n"] += 1

    def fake_retire(name: str) -> int:
        retire_calls.append(name)
        return 1

    async def fake_close_retired() -> int:
        return 3

    def fake_clear_listenbrainz() -> None:
        return None

    monkeypatch.setattr(HttpClientFactory, "retire_name", staticmethod(fake_retire))
    monkeypatch.setattr(
        HttpClientFactory, "close_retired", staticmethod(fake_close_retired)
    )
    monkeypatch.setattr(service, "on_coverart_settings_changed", fake_coverart)
    import core.dependencies as dependencies_module
    import repositories.musicbrainz_base as mb_base

    monkeypatch.setattr(
        dependencies_module,
        "clear_listenbrainz_dependent_caches",
        fake_clear_listenbrainz,
    )
    monkeypatch.setattr(
        mb_base,
        "set_mb_brainzmash_http_client",
        lambda client: brainzmash_client_resets.append(client),
    )
    # Targeted comparison used by the advanced-settings route: only the four
    # HTTP-affecting fields trigger generation retirement.
    previous = SimpleNamespace(
        http_timeout=10.0,
        http_connect_timeout=5.0,
        http_max_connections=200,
        http_max_keepalive=100,
        audiodb_api_key="masked",
    )
    changed = SimpleNamespace(
        http_timeout=25.0,
        http_connect_timeout=5.0,
        http_max_connections=200,
        http_max_keepalive=100,
        audiodb_api_key="masked",
    )
    unrelated = SimpleNamespace(
        http_timeout=10.0,
        http_connect_timeout=5.0,
        http_max_connections=200,
        http_max_keepalive=100,
        audiodb_api_key="rotated",
    )

    def http_fields_differ(a, b) -> bool:
        return any(
            getattr(a, field) != getattr(b, field)
            for field in (
                "http_timeout",
                "http_connect_timeout",
                "http_max_connections",
                "http_max_keepalive",
            )
        )

    assert http_fields_differ(previous, changed) is True
    assert http_fields_differ(previous, unrelated) is False

    asyncio.run(service.on_http_settings_changed())

    assert sorted(retire_calls) == [
        "coverart",
        "default",
        "listenbrainz",
        "musicbrainz-brainzmash",
    ]
    assert coverart_calls["n"] == 1  # provider graphs still rebuilt
    assert brainzmash_client_resets == [None]
