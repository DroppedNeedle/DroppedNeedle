"""NewznabClient parse + NewznabIndexer fan-out tests, against the curl-captured
DrunkenSlug (nZEDb) shapes + an audio-search-capable variant. Exercises the real
XML path, the caps-gated query strategy, dedup, partial-failure tolerance, the
rate-limit backoff, and the short-TTL search cache."""

import httpx
import logging
import pytest

from core.exceptions import NewznabApiError, NewznabAuthError, RateLimitedError
from repositories.newznab.newznab_client import NewznabClient
from repositories.newznab.newznab_indexer import (
    NewznabIndexer,
    NewznabIndexerEntry,
    normalize_newznab_query,
)
from tests.mocks import newznab_mock

_BASE = "https://idx.test/api"


def _client(handler, *, indexer_id="ds", name="DrunkenSlug") -> NewznabClient:
    return NewznabClient(
        newznab_mock.client_for(handler), _BASE, "KEY", indexer_id=indexer_id, indexer_name=name
    )


def _entry(handler, *, indexer_id, name, priority=1, enabled=True, categories=None) -> NewznabIndexerEntry:
    return NewznabIndexerEntry(
        _client(handler, indexer_id=indexer_id, name=name),
        indexer_id=indexer_id,
        name=name,
        categories=categories or [3000, 3010, 3040],
        enabled=enabled,
        priority=priority,
    )


# --- client: caps parse ---------------------------------------------------------

@pytest.mark.asyncio
async def test_caps_parse_reads_audio_search_unavailable_and_other_category():
    caps = await _client(newznab_mock.drunkenslug_handler).caps()
    assert caps.supports_text_search is True
    assert caps.text_search_params == ["q"]
    assert caps.supports_audio_search is False  # DrunkenSlug advertises audio-search=no
    assert caps.limit_max == 100
    # nZEDb Audio/Other is 3999, NOT 3050 - read from caps, never hardcoded.
    assert caps.other_audio_category_id() == 3999
    assert 3040 in caps.audio_category_ids()


@pytest.mark.asyncio
async def test_caps_parse_reads_audio_search_available():
    caps = await _client(newznab_mock.audionix_handler).caps()
    assert caps.supports_audio_search is True
    assert set(caps.audio_search_params) == {"q", "artist", "album"}


# --- client: item parse ---------------------------------------------------------

@pytest.mark.asyncio
async def test_search_parse_reads_enclosure_size_grabs_files_date():
    releases, limits = await _client(newznab_mock.drunkenslug_handler).search(
        "Radiohead In Rainbows", [3000, 3010, 3040]
    )
    assert len(releases) == 3
    flac = releases[0]
    assert flac.category_ids == [3040]
    assert flac.size_bytes == 2315726631  # from newznab:attr size
    assert flac.grabs == 205
    assert flac.files == 113
    assert flac.usenet_date is not None
    # NZB url is the self-authenticating <enclosure> (&i=&r= preserved); <link> ignored.
    assert flac.nzb_url == "https://drunkenslug.com/getnzb/93bc.nzb&i=1&r=KEY"
    assert flac.indexer_name == "DrunkenSlug"
    # apilimits surfaced for quota backoff.
    assert limits is not None and limits.api_current == 183


@pytest.mark.asyncio
async def test_search_parse_keeps_obfuscated_title_release():
    releases, _ = await _client(newznab_mock.drunkenslug_handler).search("q", [3040])
    # the scrambled-name release is still returned (category is the quality signal,
    # not the title) - dropping it would break the automatic Usenet fallback.
    assert any("obfuscated" in r.title for r in releases)
    obf = next(r for r in releases if "obfuscated" in r.title)
    assert obf.category_ids == [3040]


_PW_FEED = """<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:newznab="http://www.newznab.com/DTD/2010/feeds/attributes/">
 <channel>
  <item>
   <title>Protected Release</title>
   <guid>g1</guid>
   <enclosure url="https://idx/getnzb/p.nzb" length="500000000" type="application/x-nzb"/>
   <newznab:attr name="category" value="3040"/>
   <newznab:attr name="size" value="500000000"/>
   <newznab:attr name="password" value="2"/>
  </item>
 </channel>
</rss>"""

_EMPTY_FEED = (
    '<?xml version="1.0"?><rss version="2.0" '
    'xmlns:newznab="http://www.newznab.com/DTD/2010/feeds/attributes/">'
    "<channel></channel></rss>"
)


class _Capture:
    """Records every outgoing request, delegating the response to ``handler`` (or a
    fixed body) so a test can assert on the params actually sent."""

    def __init__(self, handler=None, *, body: str | None = None):
        self.handler = handler
        self.body = body
        self.requests: list[httpx.Request] = []

    def __call__(self, request: httpx.Request) -> httpx.Response:
        self.requests.append(request)
        if self.handler is not None:
            return self.handler(request)
        return httpx.Response(200, text=self.body or _EMPTY_FEED,
                              headers={"content-type": "application/xml"})


@pytest.mark.asyncio
async def test_search_parse_reads_password_attr():
    releases, _ = await _client(newznab_mock.drunkenslug_handler).search(
        "Radiohead In Rainbows", [3040]
    )
    assert releases[0].password == 0  # the clean FLAC carries password=0


@pytest.mark.asyncio
async def test_password_protected_release_parses_nonzero():
    cap = _Capture(body=_PW_FEED)
    [rel], _ = await _client(cap).search("q", [3040])
    assert rel.password == 2  # so the scorer can reject it


@pytest.mark.asyncio
async def test_music_search_sends_year_when_provided():
    cap = _Capture()
    client = _client(cap, indexer_id="ax", name="AX")
    await client.music_search("Pink Floyd", "Animals", [3000], year=1977)
    params = cap.requests[-1].url.params
    assert params.get("t") == "music"
    assert params.get("year") == "1977"  # the dead `artist == album` guard is gone


@pytest.mark.asyncio
async def test_year_not_sent_when_caps_dont_advertise_it():
    # Audionix advertises audio-search params q,artist,album - NOT year - so year must
    # not be sent (Lidarr/Prowlarr only send advertised params).
    cap = _Capture(newznab_mock.audionix_handler)
    idx = NewznabIndexer([_entry(cap, indexer_id="ax", name="AX")])
    await idx.search_album("Pink Floyd", "Animals", 1977, 5)
    music = [r for r in cap.requests if r.url.params.get("t") == "music"]
    assert music  # audio-search advertised -> t=music path taken
    assert "year" not in music[-1].url.params


# --- client: errors -------------------------------------------------------------

@pytest.mark.asyncio
async def test_music_unknown_function_raises_with_code():
    with pytest.raises(NewznabApiError) as exc:
        await _client(newznab_mock.drunkenslug_handler).music_search("A", "B", [3000])
    assert exc.value.code == 202


@pytest.mark.asyncio
async def test_auth_error_raises_newznab_auth_error():
    with pytest.raises(NewznabAuthError):
        await _client(newznab_mock.auth_error_handler).search("q", [3000])


@pytest.mark.asyncio
async def test_rate_limit_error_raises_rate_limited():
    with pytest.raises(RateLimitedError):
        await _client(newznab_mock.rate_limit_handler).search("q", [3000])


# --- indexer: query strategy ----------------------------------------------------

@pytest.mark.asyncio
async def test_strategy_uses_search_when_no_audio_search():
    idx = NewznabIndexer([_entry(newznab_mock.drunkenslug_handler, indexer_id="ds", name="DS")])
    results = await idx.search_album("Radiohead", "In Rainbows")
    assert len(results) == 3
    assert all(r.source == "usenet" and r.usenet is not None for r in results)


@pytest.mark.asyncio
async def test_strategy_uses_music_when_audio_search_advertised():
    idx = NewznabIndexer([_entry(newznab_mock.audionix_handler, indexer_id="ax", name="AX")])
    results = await idx.search_album("Radiohead", "In Rainbows")
    titles = [r.usenet.title for r in results]
    # the t=search sentinel must NOT appear -> the structured t=music path was taken.
    assert not any("WRONG-PATH" in t for t in titles)
    assert len(results) == 2


@pytest.mark.asyncio
async def test_strategy_falls_back_to_search_on_music_202():
    idx = NewznabIndexer(
        [_entry(newznab_mock.audionix_broken_music_handler, indexer_id="ax", name="AX")]
    )
    results = await idx.search_album("Radiohead", "In Rainbows")
    # caps said audio-search=yes, music 202'd, fell back to t=search -> the valid feed.
    assert len(results) == 2


# --- query ladder: punctuation-normalized retry on zero results (#259) --------

def test_normalize_newznab_query_strips_scene_punctuation():
    assert (
        normalize_newznab_query("Drake Honestly, Nevermind")
        == "Drake Honestly Nevermind"
    )
    assert (
        normalize_newznab_query("Sabrina Carpenter Man's Best Friend")
        == "Sabrina Carpenter Mans Best Friend"
    )
    assert (
        normalize_newznab_query("Who? What! Why; How: A & B")
        == "Who What Why How A B"
    )


def test_normalize_newznab_query_folds_typographic_quotes():
    assert (
        normalize_newznab_query("Sabrina Carpenter Man’s Best Friend")
        == "Sabrina Carpenter Mans Best Friend"
    )
    assert (
        normalize_newznab_query("Drake “Honestly, Nevermind”")
        == "Drake Honestly Nevermind"
    )


def test_normalize_newznab_query_idempotent_and_clean_safe():
    assert normalize_newznab_query("Radiohead In Rainbows") == "Radiohead In Rainbows"
    once = normalize_newznab_query("Drake Honestly, Nevermind!")
    assert normalize_newznab_query(once) == once


@pytest.mark.asyncio
async def test_ladder_retries_with_normalized_query_on_zero_results():
    newznab_mock.reset_state()
    idx = NewznabIndexer([_entry(newznab_mock.drunkenslug_handler, indexer_id="ds", name="DS")])
    results = await idx.search_album("Drake", "Honestly, Nevermind")
    assert len(results) == 1
    assert results[0].usenet is not None and "Honestly" in results[0].usenet.title
    # exact wire sequence: canonical first, normalized rung second.
    assert newznab_mock.received_q == [
        "Drake Honestly, Nevermind",
        "Drake Honestly Nevermind",
    ]


@pytest.mark.asyncio
async def test_ladder_logs_the_retry_rung(caplog):
    newznab_mock.reset_state()
    idx = NewznabIndexer([_entry(newznab_mock.drunkenslug_handler, indexer_id="ds", name="DS")])
    with caplog.at_level(logging.INFO):
        await idx.search_album("Drake", "Honestly, Nevermind")
    assert "newznab.query_normalized_retry" in caplog.text


@pytest.mark.asyncio
async def test_track_search_runs_the_ladder():
    newznab_mock.reset_state()
    idx = NewznabIndexer([_entry(newznab_mock.drunkenslug_handler, indexer_id="ds", name="DS")])
    results = await idx.search_track("Drake", "Honestly, Nevermind")
    assert len(results) == 1
    assert newznab_mock.received_q == [
        "Drake Honestly, Nevermind",
        "Drake Honestly Nevermind",
    ]


@pytest.mark.asyncio
async def test_no_second_query_when_canonical_has_results():
    newznab_mock.reset_state()
    idx = NewznabIndexer([_entry(newznab_mock.drunkenslug_handler, indexer_id="ds", name="DS")])
    results = await idx.search_album("Radiohead", "In Rainbows")
    assert len(results) == 3
    assert newznab_mock.received_q == ["Radiohead In Rainbows"]


@pytest.mark.asyncio
async def test_no_retry_when_normalized_equals_canonical():
    newznab_mock.reset_state()
    idx = NewznabIndexer([_entry(newznab_mock.drunkenslug_handler, indexer_id="ds", name="DS")])
    # punctuation-clean yet matching nothing: a single query, no second rung.
    results = await idx.search_album("XyzzyNomatch", "Unknown Album")
    assert results == []
    assert newznab_mock.received_q == ["XyzzyNomatch Unknown Album"]


@pytest.mark.asyncio
async def test_no_retry_when_indexer_rate_limited():
    counter = _Counter(newznab_mock.rate_limit_handler)
    idx = NewznabIndexer(
        [_entry(counter, indexer_id="ds", name="DS")], rate_limit_backoff=600.0
    )
    assert await idx.search_album("Drake", "Honestly, Nevermind") == []
    # the backoff silence is not a genuine empty: the normalized rung never fired.
    assert counter.counts.get("search") == 1


@pytest.mark.asyncio
async def test_no_retry_when_indexer_auth_fails():
    counter = _Counter(newznab_mock.auth_error_handler)
    idx = NewznabIndexer([_entry(counter, indexer_id="bad", name="BAD")])
    assert await idx.search_album("Drake", "Honestly, Nevermind") == []
    assert counter.counts.get("search") == 1


@pytest.mark.asyncio
async def test_no_retry_when_a_peer_indexer_errored():
    newznab_mock.reset_state()
    idx = NewznabIndexer([
        _entry(newznab_mock.auth_error_handler, indexer_id="bad", name="BAD", priority=1),
        _entry(newznab_mock.drunkenslug_handler, indexer_id="ds", name="DS", priority=2),
    ])
    results = await idx.search_album("Drake", "Honestly, Nevermind")
    assert results == []
    # DS answered empty on the canonical rung, but BAD errored: the pool is
    # untrustworthy, so the normalized rung never fires.
    assert newznab_mock.received_q == ["Drake Honestly, Nevermind"]


@pytest.mark.asyncio
async def test_music_params_stay_canonical():
    cap = _Capture(newznab_mock.audionix_handler)
    idx = NewznabIndexer([_entry(cap, indexer_id="ax", name="AX")])
    results = await idx.search_album("Drake", "Honestly, Nevermind")
    assert len(results) == 2
    music = [r for r in cap.requests if r.url.params.get("t") == "music"]
    assert music
    assert music[-1].url.params.get("artist") == "Drake"
    assert music[-1].url.params.get("album") == "Honestly, Nevermind"
    # t=music answered: no free-text rung at all.
    assert not [r for r in cap.requests if r.url.params.get("t") == "search"]


@pytest.mark.asyncio
async def test_music_202_fallback_runs_the_ladder():
    newznab_mock.reset_state()
    cap = _Capture(newznab_mock.audionix_broken_music_handler)
    idx = NewznabIndexer([_entry(cap, indexer_id="ax", name="AX")])
    results = await idx.search_album("Drake", "Honestly, Nevermind")
    assert len(results) == 1
    # rung 1 fell back to t=search with the canonical q (empty); the retry rung
    # sent the normalized q as free text instead of re-sending t=music.
    assert newznab_mock.received_q == [
        "Drake Honestly, Nevermind",
        "Drake Honestly Nevermind",
    ]
    music = [r for r in cap.requests if r.url.params.get("t") == "music"]
    assert len(music) == 1


# --- indexer: fan-out + dedup ---------------------------------------------------

@pytest.mark.asyncio
async def test_fan_out_dedups_by_title_size_keeping_higher_priority():
    idx = NewznabIndexer([
        _entry(newznab_mock.drunkenslug_handler, indexer_id="ds", name="DS", priority=1),
        _entry(newznab_mock.audionix_handler, indexer_id="ax", name="AX", priority=2),
    ])
    results = await idx.search_album("Radiohead", "In Rainbows")
    # DS: 3 unique; AX: 2 (one FLAC shares DS's title+size). Deduped -> 3 + 2 - 1 = 4.
    assert len(results) == 4
    # The shared FLAC identity kept DS's copy (priority 1) -> grabs 205, not AX's 999.
    flac = next(r.usenet for r in results if r.usenet.size_bytes == 2315726631)
    assert flac.grabs == 205
    assert flac.indexer_name == "DS"


@pytest.mark.asyncio
async def test_one_indexer_erroring_never_fails_the_fan_out():
    idx = NewznabIndexer([
        _entry(newznab_mock.drunkenslug_handler, indexer_id="ds", name="DS", priority=1),
        _entry(newznab_mock.auth_error_handler, indexer_id="bad", name="BAD", priority=2),
    ])
    results = await idx.search_album("Radiohead", "In Rainbows")
    assert len(results) == 3  # the bad indexer contributes nothing; DS still returns


# --- indexer: rate-limit backoff + search cache ---------------------------------

class _Counter:
    """Wraps a handler, counting requests by ``t`` so we can prove the cache /
    backoff actually suppress repeat indexer hits."""

    def __init__(self, handler):
        self._handler = handler
        self.counts: dict[str, int] = {}

    def __call__(self, request: httpx.Request) -> httpx.Response:
        t = request.url.params.get("t", "")
        self.counts[t] = self.counts.get(t, 0) + 1
        return self._handler(request)


@pytest.mark.asyncio
async def test_search_cache_suppresses_repeat_indexer_hits():
    counter = _Counter(newznab_mock.drunkenslug_handler)
    idx = NewznabIndexer([_entry(counter, indexer_id="ds", name="DS")], search_cache_ttl=300.0)
    await idx.search_album("Radiohead", "In Rainbows")
    await idx.search_album("Radiohead", "In Rainbows")  # identical -> cache hit
    assert counter.counts.get("search") == 1  # the second search did NOT re-hit


@pytest.mark.asyncio
async def test_rate_limited_indexer_is_backed_off_and_skipped():
    counter = _Counter(newznab_mock.rate_limit_handler)
    idx = NewznabIndexer([_entry(counter, indexer_id="ds", name="DS")], rate_limit_backoff=600.0)
    first = await idx.search_album("Radiohead", "In Rainbows")
    second = await idx.search_album("Radiohead", "Different Album")  # different query
    assert first == [] and second == []
    # the first hit rate-limited (1 search call); the backoff then skips the indexer,
    # so the second, different-query search makes NO further indexer call.
    assert counter.counts.get("search") == 1


@pytest.mark.asyncio
async def test_health_check_ok_when_reachable_and_error_when_not():
    ok = await NewznabIndexer([_entry(newznab_mock.drunkenslug_handler, indexer_id="ds", name="DS")]).health_check()
    assert ok.status == "ok"
    none = await NewznabIndexer([]).health_check()
    assert none.status == "error"
    bad = await NewznabIndexer([_entry(newznab_mock.auth_error_handler, indexer_id="b", name="B")]).health_check()
    # auth-error caps still parses to an <error> -> caps() raises -> unreachable.
    assert bad.status == "error"


def test_is_configured_reflects_enabled_entries():
    enabled = NewznabIndexer([_entry(newznab_mock.drunkenslug_handler, indexer_id="ds", name="DS")])
    disabled = NewznabIndexer(
        [_entry(newznab_mock.drunkenslug_handler, indexer_id="ds", name="DS", enabled=False)]
    )
    assert enabled.is_configured() is True
    assert disabled.is_configured() is False
