"""Raw httpx + XML wrapper around one Newznab indexer. No fan-out logic (that's
``NewznabIndexer``); this is the per-indexer HTTP + parse layer.

Newznab is **XML-only** - neither Lidarr nor Prowlarr ever sends ``o=json``, and
real indexers vary, so there is no JSON path. The body is hardened (bare ``&``
escaped, illegal control chars stripped) before parsing so the malformed XML some
indexers emit doesn't sink the feed. ``extended=1`` is always sent. ``<error>``
is checked on BOTH caps and search responses (HTTP may still be 200).

The httpx client is INJECTED (AUD-12). Auth is a ``&apikey=`` query param (not a
header) - the same secret the user pasted; never logged.
"""

import logging
import re
from email.utils import parsedate_to_datetime
from urllib.parse import urljoin
from xml.etree import ElementTree as ET

import httpx

from core.exceptions import NewznabApiError, NewznabAuthError, RateLimitedError
from repositories.protocols.indexer import UsenetRelease

from .newznab_models import (
    NewznabApiLimits,
    NewznabCaps,
    NewznabCategory,
    NewznabSubcategory,
)

logger = logging.getLogger(__name__)

# The newznab extended-attribute namespace; attrs are <newznab:attr name= value=>.
_NS = "http://www.newznab.com/DTD/2010/feeds/attributes/"
_ATTR = f"{{{_NS}}}attr"
_RESPONSE = f"{{{_NS}}}response"
_APILIMITS = f"{{{_NS}}}apilimits"

# A bare `&` or any non-predefined `&name;` (e.g. &nbsp;) is what breaks
# ElementTree, so we escape those before a retry parse.
_BARE_AMP = re.compile(r"&(?!(?:amp|lt|gt|quot|apos|#\d+|#x[0-9a-fA-F]+);)")
# XML 1.0 illegal characters (C0 control chars except tab/newline/CR, plus the
# C1 range and the U+FFFE/U+FFFF noncharacters) that make ElementTree choke.
_ILLEGAL_XML = re.compile(
    "[\x00-\x08\x0b\x0c\x0e-\x1f\x7f-\x84\x86-\x9f\uFFFE\uFFFF]"
)


class NewznabClient:
    def __init__(
        self,
        http: httpx.AsyncClient,
        base_url: str,
        api_key: str,
        *,
        indexer_id: str = "",
        indexer_name: str = "",
    ) -> None:
        self._http = http
        # The user pastes the full API path (e.g. https://idx/api); keep it verbatim.
        self._base_url = base_url.rstrip("/")
        self._api_key = api_key
        self._indexer_id = indexer_id
        self._indexer_name = indexer_name or base_url

    async def caps(self, *, timeout: float = 30.0) -> NewznabCaps:
        root = await self._get({"t": "caps"}, timeout=timeout)
        _check_error(root)
        return _parse_caps(root)

    async def search(
        self,
        query: str,
        categories: list[int],
        *,
        offset: int = 0,
        limit: int = 100,
        timeout: float = 30.0,
    ) -> tuple[list[UsenetRelease], NewznabApiLimits | None]:
        """Free-text ``t=search`` - the always-available path (DrunkenSlug only
        supports this; ``t=music`` returns error 202)."""
        params = {
            "t": "search",
            "q": query,
            "extended": "1",
            "offset": str(offset),
            "limit": str(limit),
        }
        if categories:
            params["cat"] = ",".join(str(c) for c in categories)
        return await self._search_request(params, timeout=timeout)

    async def music_search(
        self,
        artist: str,
        album: str,
        categories: list[int],
        *,
        year: int | None = None,
        offset: int = 0,
        limit: int = 100,
        timeout: float = 30.0,
    ) -> tuple[list[UsenetRelease], NewznabApiLimits | None]:
        """Structured ``t=music`` - used ONLY when caps advertises audio-search with
        artist/album params. The caller falls back to ``search`` on a 202."""
        params = {
            "t": "music",
            "artist": artist,
            "album": album,
            "extended": "1",
            "offset": str(offset),
            "limit": str(limit),
        }
        if year:
            params["year"] = str(year)
        if categories:
            params["cat"] = ",".join(str(c) for c in categories)
        return await self._search_request(params, timeout=timeout)

    async def _search_request(
        self, params: dict[str, str], *, timeout: float
    ) -> tuple[list[UsenetRelease], NewznabApiLimits | None]:
        root = await self._get(params, timeout=timeout)
        _check_error(root)
        request_url = f"{self._base_url}?{params.get('t', '')}"
        releases = _parse_items(root, self._indexer_id, self._indexer_name, request_url)
        return releases, _parse_apilimits(root)

    async def _get(self, params: dict[str, str], *, timeout: float) -> ET.Element:
        merged = {**params, "apikey": self._api_key}
        try:
            response = await self._http.get(
                self._base_url, params=merged, timeout=timeout
            )
        except httpx.HTTPError as exc:
            raise NewznabApiError(f"newznab request failed: {exc}") from exc
        if response.status_code == 429:
            retry_after = _retry_after(response)
            raise RateLimitedError(
                "newznab: rate limited", retry_after_seconds=retry_after
            )
        if response.status_code >= 400:
            raise NewznabApiError(
                f"newznab returned HTTP {response.status_code}",
                details=response.text[:200],
                code=response.status_code,
            )
        return _safe_fromstring(response.content)


def _safe_fromstring(raw: bytes) -> ET.Element:
    """Parse XML, hardening on failure (Prowlarr ``XmlCleaner``): strip illegal
    control chars + escape bare ampersands/non-predefined entities, then retry."""
    text = raw.decode("utf-8", errors="replace")
    text = _ILLEGAL_XML.sub("", text)
    try:
        return ET.fromstring(text)
    except ET.ParseError:
        hardened = _BARE_AMP.sub("&amp;", text)
        try:
            return ET.fromstring(hardened)
        except ET.ParseError as exc:
            raise NewznabApiError(f"newznab returned unparseable XML: {exc}") from exc


def _local(tag: str) -> str:
    """Strip any namespace from an element tag (``{ns}item`` -> ``item``)."""
    return tag.rsplit("}", 1)[-1]


def _check_error(root: ET.Element) -> None:
    """``<error code= description=>`` - HTTP may be 200. 100-199 / apikey ⇒ auth;
    'Request limit reached' ⇒ rate limit; else generic."""
    if _local(root.tag) != "error":
        return
    code = _to_int(root.get("code")) or 0
    desc = root.get("description", "")
    low = desc.lower()
    if 100 <= code <= 199 or "apikey" in low or "api key" in low:
        raise NewznabAuthError(desc or "newznab auth failure", code=code)
    if "request limit reached" in low or "limit reached" in low:
        raise RateLimitedError(desc or "newznab request limit reached")
    raise NewznabApiError(desc or f"newznab error {code}", code=code)


def _parse_caps(root: ET.Element) -> NewznabCaps:
    server = root.find("server")
    limits = root.find("limits")
    searching = root.find("searching")
    text_el = searching.find("search") if searching is not None else None
    # Read <audio-search> first, fall back to <music-search> (robustness finding):
    # Prowlarr-as-server emits both, real indexers vary.
    audio_el = None
    if searching is not None:
        audio_el = searching.find("audio-search")
        if audio_el is None:
            audio_el = searching.find("music-search")

    categories: list[NewznabCategory] = []
    cats_el = root.find("categories")
    if cats_el is not None:
        for cat in cats_el.findall("category"):
            cid = _to_int(cat.get("id"))
            if cid is None:
                continue
            subcats = [
                NewznabSubcategory(id=sid, name=sub.get("name", ""))
                for sub in cat.findall("subcat")
                if (sid := _to_int(sub.get("id"))) is not None
            ]
            categories.append(
                NewznabCategory(id=cid, name=cat.get("name", ""), subcats=subcats)
            )

    return NewznabCaps(
        server_title=server.get("title") if server is not None else None,
        server_version=server.get("version") if server is not None else None,
        limit_default=_to_int(limits.get("default")) or 100 if limits is not None else 100,
        limit_max=_to_int(limits.get("max")) or 100 if limits is not None else 100,
        supports_text_search=_available(text_el),
        text_search_params=_params(text_el),
        supports_audio_search=_available(audio_el),
        audio_search_params=_params(audio_el),
        categories=categories,
    )


def _available(el: ET.Element | None) -> bool:
    return el is not None and (el.get("available", "").lower() == "yes")


def _params(el: ET.Element | None) -> list[str]:
    if el is None:
        return []
    raw = el.get("supportedParams", "")
    return [p.strip() for p in raw.split(",") if p.strip()]


def _parse_items(
    root: ET.Element, indexer_id: str, indexer_name: str, request_url: str
) -> list[UsenetRelease]:
    releases: list[UsenetRelease] = []
    for item in root.iter("item"):
        try:
            release = _parse_item(item, indexer_id, indexer_name, request_url)
        except _TorznabFeed:
            # A mixed or malformed feed must not discard valid NZB siblings.
            logger.warning(
                "newznab: skipping torrent item returned by %s", indexer_name
            )
            continue
        except Exception as exc:  # noqa: BLE001 - one bad item must not sink the feed
            logger.warning("newznab: skipping unparseable item from %s: %s", indexer_name, exc)
            continue
        if release is not None:
            releases.append(release)
    return releases


class _TorznabFeed(Exception):
    """Internal signal: an item carried a torrent/magnet enclosure."""


def _parse_item(
    item: ET.Element, indexer_id: str, indexer_name: str, request_url: str
) -> UsenetRelease | None:
    nzb_url: str | None = None
    enclosure_size = 0
    for enc in item.findall("enclosure"):
        etype = (enc.get("type") or "").lower()
        url = enc.get("url") or ""
        if "torrent" in etype or url.startswith("magnet:"):
            raise _TorznabFeed()
        # The NZB enclosure is MIME-enforced; a no-type enclosure is accepted.
        if "x-nzb" in etype or etype == "":
            nzb_url = url
            enclosure_size = _to_int(enc.get("length")) or 0
            if "x-nzb" in etype:
                break
    if not nzb_url:
        link = (item.findtext("link") or "").strip()
        if link.startswith("magnet:") or link.lower().endswith(".torrent"):
            raise _TorznabFeed()
        nzb_url = link or None
    if not nzb_url:
        return None

    attrs = _attr_map(item)
    size = _to_int(_first(attrs.get("size"))) or enclosure_size
    category_ids = [c for v in attrs.get("category", []) if (c := _to_int(v)) is not None]
    usenet_date = _parse_date(_first(attrs.get("usenetdate")) or item.findtext("pubDate"))

    return UsenetRelease(
        indexer_id=indexer_id,
        indexer_name=indexer_name,
        guid=item.findtext("guid") or "",
        title=(item.findtext("title") or "Unknown").strip(),
        nzb_url=urljoin(request_url, nzb_url),
        size_bytes=size,
        category_ids=category_ids,
        grabs=_to_int(_first(attrs.get("grabs"))),
        files=_to_int(_first(attrs.get("files"))),
        usenet_date=usenet_date,
        password=_to_int(_first(attrs.get("password"))) or 0,
    )


def _attr_map(item: ET.Element) -> dict[str, list[str]]:
    """``{name.lower(): [values]}`` from the item's ``<newznab:attr>`` elements
    (``category`` repeats). Attr-name match is case-insensitive."""
    out: dict[str, list[str]] = {}
    for attr in item.findall(_ATTR):
        name = (attr.get("name") or "").lower()
        value = attr.get("value")
        if name and value is not None:
            out.setdefault(name, []).append(value)
    return out


def _parse_apilimits(root: ET.Element) -> NewznabApiLimits | None:
    channel = root.find("channel")
    el = channel.find(_APILIMITS) if channel is not None else root.find(_APILIMITS)
    if el is None:
        return None
    return NewznabApiLimits(
        api_current=_to_int(el.get("apiCurrent")),
        api_max=_to_int(el.get("apiMax")),
        grab_current=_to_int(el.get("grabCurrent")),
        grab_max=_to_int(el.get("grabMax")),
    )


def _first(values: list[str] | None) -> str | None:
    return values[0] if values else None


def _to_int(value: str | None) -> int | None:
    if value is None:
        return None
    try:
        return int(str(value).strip())
    except (TypeError, ValueError):
        return None


def _parse_date(value: str | None) -> float | None:
    if not value:
        return None
    try:
        return parsedate_to_datetime(value.strip()).timestamp()
    except (TypeError, ValueError, OverflowError):
        return None


def _retry_after(response: httpx.Response) -> float | None:
    raw = response.headers.get("Retry-After")
    return _to_int(raw) if raw else None
