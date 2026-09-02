"""Realistic Newznab XML mocks, built from the curl-captured DrunkenSlug (nZEDb)
responses + a second indexer variant that DOES advertise audio-search, plus the
error/obfuscation cases. Served via ``httpx.MockTransport`` so the real
``NewznabClient`` XML path is exercised end to end.

Two indexers:
- ``drunkenslug``: audio-search=no -> the t=search path; t=music returns error 202.
- ``audionix``: audio-search=yes (q,artist,album) -> the t=music path works; shares
  one release identity with drunkenslug to exercise cross-indexer dedup.
"""

import httpx

# --- DrunkenSlug (nZEDb): the real captured shapes -------------------------------

_DS_CAPS = """<?xml version="1.0" encoding="UTF-8"?>
<caps>
 <server appversion="0.8.21.0" version="0.1" title="DS" strapline="DrunkenSlug"/>
 <limits max="100" default="100"/>
 <searching>
  <search available="yes" supportedParams="q"/>
  <tv-search available="yes" supportedParams="q,season,ep"/>
  <movie-search available="yes" supportedParams="q,imdbid"/>
  <audio-search available="no" supportedParams=""/>
 </searching>
 <categories>
  <category id="3000" name="Audio">
   <subcat id="3030" name="Audiobook"/>
   <subcat id="3060" name="Foreign"/>
   <subcat id="3040" name="Lossless"/>
   <subcat id="3010" name="MP3"/>
   <subcat id="3999" name="Other"/>
   <subcat id="3020" name="Video"/>
  </category>
  <category id="5000" name="TV">
   <subcat id="5040" name="HD"/>
  </category>
 </categories>
</caps>"""

# A real t=search response: a clean FLAC release (cat 3040), an Audio/Other promo
# (cat 3999), and an obfuscated-title release (cat 3040, scrambled name). NZB url is
# the <enclosure> (self-authenticating &i=&r=). Numbers/attrs as the real indexer.
_DS_SEARCH = """<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:atom="http://www.w3.org/2005/Atom" xmlns:newznab="http://www.newznab.com/DTD/2010/feeds/attributes/">
 <channel>
  <title>DS</title>
  <newznab:response offset="0" total="3"/>
  <newznab:apilimits apiCurrent="183" grabCurrent="46"/>
  <item>
   <title>[003/113] &quot;Radiohead-In_Rainbows-LP-24BIT-FLAC-2007-REETKEVER.part002.rar&quot;</title>
   <guid isPermaLink="true">https://drunkenslug.com/details/93bc59792f2ee7af8486c3d278265b1a9964f2e5</guid>
   <link>https://drunkenslug.com/getnzb/93bc.nzb&amp;i=1&amp;r=KEY</link>
   <pubDate>Thu, 23 Oct 2025 19:21:32 +0100</pubDate>
   <category>Audio &gt; Lossless</category>
   <enclosure url="https://drunkenslug.com/getnzb/93bc.nzb&amp;i=1&amp;r=KEY" length="2315726631" type="application/x-nzb"/>
   <newznab:attr name="category" value="3040"/>
   <newznab:attr name="size" value="2315726631"/>
   <newznab:attr name="files" value="113"/>
   <newznab:attr name="grabs" value="205"/>
   <newznab:attr name="password" value="0"/>
   <newznab:attr name="usenetdate" value="Thu, 23 Oct 2025 19:17:23 +0100"/>
  </item>
  <item>
   <title>[4/9] &quot;Radiohead - In Rainbows (For Overhead Play) [CD-R, US promo].zip.vol01+02.par2&quot;</title>
   <guid isPermaLink="true">https://drunkenslug.com/details/58780764e9e6c77177add7bd2cff5894dfc4d787</guid>
   <pubDate>Thu, 19 Feb 2026 18:57:03 +0000</pubDate>
   <category>Audio &gt; Other</category>
   <enclosure url="https://drunkenslug.com/getnzb/5878.nzb&amp;i=1&amp;r=KEY" length="580998479" type="application/x-nzb"/>
   <newznab:attr name="category" value="3999"/>
   <newznab:attr name="size" value="580998479"/>
   <newznab:attr name="files" value="9"/>
   <newznab:attr name="grabs" value="8"/>
   <newznab:attr name="usenetdate" value="Thu, 19 Feb 2026 18:45:35 +0000"/>
  </item>
  <item>
   <title>aHR0cHM6Ly9 obfuscated release name xZQ.part01.rar</title>
   <guid isPermaLink="true">https://drunkenslug.com/details/cafe0000</guid>
   <enclosure url="https://drunkenslug.com/getnzb/cafe.nzb&amp;i=1&amp;r=KEY" length="402653184" type="application/x-nzb"/>
   <newznab:attr name="category" value="3040"/>
   <newznab:attr name="size" value="402653184"/>
   <newznab:attr name="grabs" value="61"/>
  </item>
 </channel>
</rss>"""

_DS_MUSIC_ERROR = """<?xml version="1.0" encoding="UTF-8"?>
<error code="202" description="No such function (music)"/>"""

# --- Audionix: an indexer that DOES support audio-search (the t=music path) -------

_AX_CAPS = """<?xml version="1.0" encoding="UTF-8"?>
<caps>
 <server version="1.2" title="Audionix"/>
 <limits max="100" default="100"/>
 <searching>
  <search available="yes" supportedParams="q"/>
  <audio-search available="yes" supportedParams="q,artist,album"/>
 </searching>
 <categories>
  <category id="3000" name="Audio">
   <subcat id="3010" name="MP3"/>
   <subcat id="3040" name="Lossless"/>
   <subcat id="3050" name="Other"/>
  </category>
 </categories>
</caps>"""

# Shares the FLAC release identity (same title+size) with DrunkenSlug to test dedup;
# plus a unique MP3 release.
_AX_MUSIC = """<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:newznab="http://www.newznab.com/DTD/2010/feeds/attributes/">
 <channel>
  <newznab:response offset="0" total="2"/>
  <item>
   <title>[003/113] &quot;Radiohead-In_Rainbows-LP-24BIT-FLAC-2007-REETKEVER.part002.rar&quot;</title>
   <guid>audionix-guid-flac</guid>
   <enclosure url="https://audionix.test/nzb/flac" length="2315726631" type="application/x-nzb"/>
   <newznab:attr name="category" value="3040"/>
   <newznab:attr name="size" value="2315726631"/>
   <newznab:attr name="grabs" value="999"/>
  </item>
  <item>
   <title>Radiohead - In Rainbows (2007) [MP3-320]</title>
   <guid>audionix-guid-mp3</guid>
   <enclosure url="https://audionix.test/nzb/mp3" length="115343360" type="application/x-nzb"/>
   <newznab:attr name="category" value="3010"/>
   <newznab:attr name="size" value="115343360"/>
   <newznab:attr name="grabs" value="120"/>
  </item>
 </channel>
</rss>"""

# A sentinel feed returned by Audionix on the WRONG (t=search) path, so a test can
# prove the structured t=music path was taken when caps advertises audio-search.
_AX_SEARCH_WRONG = """<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:newznab="http://www.newznab.com/DTD/2010/feeds/attributes/">
 <channel>
  <item>
   <title>WRONG-PATH-used-t=search-instead-of-t=music</title>
   <guid>wrong</guid>
   <enclosure url="https://audionix.test/nzb/wrong" length="1" type="application/x-nzb"/>
   <newznab:attr name="size" value="1"/>
  </item>
 </channel>
</rss>"""

_AUTH_ERROR = """<?xml version="1.0" encoding="UTF-8"?>
<error code="100" description="Incorrect user credentials"/>"""

_RATE_LIMIT_ERROR = """<?xml version="1.0" encoding="UTF-8"?>
<error code="500" description="Request limit reached"/>"""


def _xml(body: str, status: int = 200) -> httpx.Response:
    return httpx.Response(status, content=body.encode(), headers={"Content-Type": "application/rss+xml"})


# --- Punctuation-ladder scenario (GH #259) ------------------------------------
# Live-verified NZBGeek behavior: the canonical punctuated free-text query
# returns 0 releases; the punctuation-stripped query returns the release.
# Existing fixtures stay intact: any other query still gets the default feed.
_GATED_Q_MARKER = "honestly"  # Drake "Honestly, Nevermind" stand-in
_NORESULT_MARKER = "xyzzynomatch"  # punctuation-clean query matching nothing
_PUNCTUATION_CHARS = frozenset({",", "'", '"', "?", "!", ";", ":", "&", "‘", "’", "“", "”"})

_GATED_RELEASE_FEED = """<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:newznab="http://www.newznab.com/DTD/2010/feeds/attributes/">
 <channel>
  <newznab:response offset="0" total="1"/>
  <item>
   <title>Drake-Honestly_Nevermind-WEB-FLAC-2022-GROUP</title>
   <guid isPermaLink="true">https://drunkenslug.com/details/d9aa0f7e5c4b4a3b8f6e1d2c5b7a49301</guid>
   <pubDate>Fri, 17 Jun 2022 12:00:00 +0100</pubDate>
   <enclosure url="https://drunkenslug.com/getnzb/d9aa.nzb&amp;i=1&amp;r=KEY" length="734003200" type="application/x-nzb"/>
   <newznab:attr name="category" value="3040"/>
   <newznab:attr name="size" value="734003200"/>
   <newznab:attr name="files" value="42"/>
   <newznab:attr name="grabs" value="87"/>
   <newznab:attr name="password" value="0"/>
   <newznab:attr name="usenetdate" value="Fri, 17 Jun 2022 11:58:00 +0100"/>
  </item>
 </channel>
</rss>"""

_DS_EMPTY = (
    '<?xml version="1.0"?><rss version="2.0" '
    'xmlns:newznab="http://www.newznab.com/DTD/2010/feeds/attributes/">'
    "<channel></channel></rss>"
)

# Every free-text ``q`` received, in order - the assertable wire record for the
# query ladder. Reset per test with ``reset_state`` (mirrors slskd_mock).
received_q: list[str] = []


def reset_state() -> None:
    del received_q[:]


def _record(request: httpx.Request) -> str:
    q = request.url.params.get("q", "")
    received_q.append(q)
    return q


def _free_text_feed(q: str | None, default: str) -> str:
    """The feed for one free-text query: the gated release only on the
    punctuation-free rung, empty on the canonical punctuated rung."""
    low = (q or "").lower()
    if _GATED_Q_MARKER in low:
        if any(ch in (q or "") for ch in _PUNCTUATION_CHARS):
            return _DS_EMPTY
        return _GATED_RELEASE_FEED
    if _NORESULT_MARKER in low:
        return _DS_EMPTY
    return default


def drunkenslug_handler(request: httpx.Request) -> httpx.Response:
    t = request.url.params.get("t")
    if t == "caps":
        return _xml(_DS_CAPS)
    if t == "music":
        return _xml(_DS_MUSIC_ERROR)
    if t == "search":
        return _xml(_free_text_feed(_record(request), _DS_SEARCH))
    return _xml(_DS_MUSIC_ERROR)


def audionix_handler(request: httpx.Request) -> httpx.Response:
    t = request.url.params.get("t")
    if t == "caps":
        return _xml(_AX_CAPS)
    if t == "music":
        return _xml(_AX_MUSIC)
    if t == "search":
        _record(request)
        return _xml(_AX_SEARCH_WRONG)  # sentinel: should NOT be hit when caps=audio-search
    return _xml(_AX_MUSIC)


def audionix_broken_music_handler(request: httpx.Request) -> httpx.Response:
    """Advertises audio-search in caps, but t=music 202s (a real quirk) - the client
    must fall back to t=search, which here returns the valid feed."""
    t = request.url.params.get("t")
    if t == "caps":
        return _xml(_AX_CAPS)
    if t == "music":
        return _xml(_DS_MUSIC_ERROR)  # code 202
    return _xml(_free_text_feed(_record(request), _AX_MUSIC))


def auth_error_handler(request: httpx.Request) -> httpx.Response:
    return _xml(_AUTH_ERROR)


def rate_limit_handler(request: httpx.Request) -> httpx.Response:
    return _xml(_RATE_LIMIT_ERROR)


def client_for(handler) -> httpx.AsyncClient:
    """An httpx.AsyncClient whose transport serves ``handler`` - drop-in for the
    injected client in ``NewznabClient``."""
    return httpx.AsyncClient(transport=httpx.MockTransport(handler))
