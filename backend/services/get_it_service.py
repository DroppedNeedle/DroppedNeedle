"""GetItService - the "Where to buy" purchase options behind the album page's
inline section (phase 01, D7/D11/D18/D19).

Link sources, in order of authority:
1. MusicBrainz ownership relationships (``purchase for download``,
   ``purchase for mail-order``, ``download for free``, ``amazon asin``).
   Live-probed 2026-07-10: these live on *releases*, not release groups, so up
   to ``_MAX_RELEASE_LOOKUPS`` releases are consulted (each an MB call under
   the existing limiter + priority queue).
2. The iTunes Search fallback (region-aware) when MB yields no digital link.
3. A Bandcamp search URL as the guaranteed floor - the section never renders
   empty.

Ordering is by artist fairness, never by commission (D19): Bandcamp first at
0%. The affiliate decorator applies DroppedNeedle's baked-in tags only while
the admin's "Support DroppedNeedle" toggle is on; the shipped tag constants are
empty until the owner's programme approvals land, so links stay clean either
way until then.
"""

import logging
from urllib.parse import quote_plus, urlencode, urlparse, urlunparse, parse_qsl

import msgspec

from api.v1.schemas.get_it import (
    ArtistPurchaseOptionsResponse,
    PurchaseLink,
    PurchaseOptionsResponse,
)
from infrastructure.cache.cache_keys import getit_artist_options_key, getit_options_key
from infrastructure.cache.memory_cache import CacheInterface

logger = logging.getLogger(__name__)

# D19: DroppedNeedle's own programme identifiers. Public strings, not secrets,
# shipped as defaults behind the support toggle. An empty value means the
# decorator no-ops for that store and no disclosure renders.
#
# Amazon Associates tracking IDs are PER MARKETPLACE: a US tag on an amazon.co.uk
# link earns nothing and breaches the Associates terms. MusicBrainz's amazon-asin
# rels point at every regional domain, so tags are keyed by marketplace host and
# a link whose host has no tag stays clean. Add rows as programmes are approved.
DN_AMAZON_TAGS: dict[str, str] = {
    "amazon.com": "droppedneedle-20",
    "amazon.co.uk": "droppedneedle-21",
}
DN_APPLE_TOKEN = ""  # Apple Performance Partners at= token
DN_AWIN_PUBLISHER_ID = ""  # Awin publisher id (Qobuz programme)
AWIN_QOBUZ_ADVERTISER_ID = ""  # Awin advertiser id ("mid") for Qobuz

_OWNERSHIP_REL_TYPES = frozenset(
    {"purchase for download", "purchase for mail-order", "download for free", "amazon asin"}
)
# Artist-level rels are storefronts, not one release: an artist's Bandcamp page
# sells their whole catalogue, and "purchase for mail-order" is their merch shop.
# ``bandcamp`` is its own rel type at artist level (it is not at release level).
_ARTIST_STORE_REL_TYPES = frozenset(
    {"bandcamp", "purchase for download", "purchase for mail-order"}
)
_PHYSICAL_REL_TYPES = frozenset({"purchase for mail-order", "amazon asin"})
_FREE_REL_TYPES = frozenset({"download for free"})

# artist-fairness order (D19): Bandcamp first at 0% commission, DRM-free
# specialists next, the giants last, unknown stores after all recognised ones
_STORE_ORDER = [
    "bandcamp",
    "qobuz",
    "beatport",
    "hdtracks",
    "junodownload",
    "7digital",
    "itunes",
    "amazon",
]
_STORE_LABELS = {
    "bandcamp": "Bandcamp",
    "qobuz": "Qobuz",
    "beatport": "Beatport",
    "hdtracks": "HDtracks",
    "junodownload": "Juno Download",
    "7digital": "7digital",
    "itunes": "iTunes / Apple Music",
    "amazon": "Amazon",
}

_CACHE_TTL_SECONDS = 7 * 24 * 3600
_MAX_RELEASE_LOOKUPS = 2


def _store_for(url: str) -> str:
    host = (urlparse(url).netloc or "").lower()
    if host.endswith("bandcamp.com"):
        return "bandcamp"
    if "qobuz.com" in host:
        return "qobuz"
    if "beatport.com" in host:
        return "beatport"
    if "hdtracks.com" in host:
        return "hdtracks"
    if "junodownload.com" in host:
        return "junodownload"
    if "7digital.com" in host:
        return "7digital"
    if host in ("music.apple.com", "itunes.apple.com", "geo.music.apple.com"):
        return "itunes"
    if ".amazon." in f".{host}" or host.startswith("amazon."):
        return "amazon"
    return "other"


def _label_for(store: str, url: str) -> str:
    if store in _STORE_LABELS:
        return _STORE_LABELS[store]
    return urlparse(url).netloc or "Store"


def _order_key(link: PurchaseLink) -> tuple[int, str]:
    try:
        rank = _STORE_ORDER.index(link.store)
    except ValueError:
        rank = len(_STORE_ORDER)
    return (rank, link.label.lower())


def _amazon_marketplace(url: str) -> str:
    """The Associates marketplace host for an Amazon URL ('amazon.co.uk'), with
    any www./smile. prefix stripped. Regional domains are distinct programmes."""
    host = (urlparse(url).netloc or "").lower()
    for prefix in ("www.", "smile."):
        if host.startswith(prefix):
            host = host[len(prefix) :]
    return host


def _append_param(url: str, key: str, value: str) -> str:
    parts = urlparse(url)
    query = dict(parse_qsl(parts.query, keep_blank_values=True))
    query[key] = value
    return urlunparse(parts._replace(query=urlencode(query)))


class GetItService:
    def __init__(
        self,
        mb_repo,  # noqa: ANN001
        itunes_repo,  # noqa: ANN001
        preferences_service,  # noqa: ANN001
        cache: CacheInterface,
        plugin_host=None,  # noqa: ANN001 - PluginHost, optional (01b purchase_links capability)
    ) -> None:
        self._mb = mb_repo
        self._itunes = itunes_repo
        self._prefs = preferences_service
        self._cache = cache
        self._plugins = plugin_host

    def _plugins_token(self) -> str:
        if self._plugins is None:
            return ""
        return ",".join(sorted(p.manifest.name for p in self._plugins.purchase_providers()))

    async def get_purchase_options(self, release_group_mbid: str) -> PurchaseOptionsResponse:
        settings = self._prefs.get_get_it_settings()
        decorated = settings.support_droppedneedle and self._any_tag_configured()
        # the plugins token keys the cache so enabling/disabling a purchase-link
        # plugin misses to a fresh entry instead of serving week-old options
        cache_key = (
            getit_options_key(release_group_mbid, settings.store_region, decorated)
            + f":{self._plugins_token()}"
        )
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return msgspec.convert(cached, PurchaseOptionsResponse)

        response = await self._build(release_group_mbid, settings.store_region, decorated)
        await self._cache.set(
            cache_key, msgspec.to_builtins(response), ttl_seconds=_CACHE_TTL_SECONDS
        )
        return response

    async def get_artist_purchase_options(
        self, artist_mbid: str, artist_name: str
    ) -> ArtistPurchaseOptionsResponse:
        """The artist's own storefronts (Bandcamp page, merch shop). No iTunes
        fallback: that call is album-shaped and would only ever guess."""
        settings = self._prefs.get_get_it_settings()
        decorated = settings.support_droppedneedle and self._any_tag_configured()
        cache_key = getit_artist_options_key(artist_mbid, decorated)
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return msgspec.convert(cached, ArtistPurchaseOptionsResponse)

        links: dict[str, PurchaseLink] = {}
        try:
            artist = await self._mb.get_artist_relations(artist_mbid)
        except Exception as exc:  # noqa: BLE001 - a dead lookup degrades to search-only
            logger.warning("Artist relations fetch failed for %s: %s", artist_mbid, exc)
            artist = None
        if artist:
            self._collect_rel_links(
                artist.get("relations") or [], links, allowed=_ARTIST_STORE_REL_TYPES
            )

        ordered = sorted(links.values(), key=_order_key)
        any_decorated = False
        if decorated:
            after = [self._decorate(link) for link in ordered]
            any_decorated = any(a.url != b.url for a, b in zip(after, ordered))
            ordered = after

        name = (artist.get("name") if artist else None) or artist_name
        response = ArtistPurchaseOptionsResponse(
            links=ordered,
            bandcamp_search_url=(
                f"https://bandcamp.com/search?q={quote_plus(name.strip())}&item_type=b"
                if name.strip()
                else ""
            ),
            disclosure=any_decorated,
        )
        await self._cache.set(
            cache_key, msgspec.to_builtins(response), ttl_seconds=_CACHE_TTL_SECONDS
        )
        return response

    def _any_tag_configured(self) -> bool:
        return bool(
            any(DN_AMAZON_TAGS.values())
            or DN_APPLE_TOKEN
            or (DN_AWIN_PUBLISHER_ID and AWIN_QOBUZ_ADVERTISER_ID)
        )

    async def _build(
        self, release_group_mbid: str, region: str, decorated: bool
    ) -> PurchaseOptionsResponse:
        rg = await self._mb.get_release_group_by_id(
            release_group_mbid, includes=["artist-credits", "releases", "url-rels"]
        )
        if not rg:
            return PurchaseOptionsResponse()

        artist = _extract_artist(rg)
        title = rg.get("title") or ""

        links: dict[str, PurchaseLink] = {}
        self._collect_rel_links(rg.get("relations") or [], links)

        # purchase rels live on releases (live-probed) - consult a bounded few,
        # official releases first, stopping early once a digital link exists
        release_ids = _candidate_release_ids(rg)
        for release_id in release_ids[:_MAX_RELEASE_LOOKUPS]:
            if any(link.kind == "digital" for link in links.values()):
                break
            release = await self._mb.get_release_by_id(release_id, includes=["url-rels"])
            if release:
                self._collect_rel_links(release.get("relations") or [], links)

        digital = [l for l in links.values() if l.kind == "digital"]
        physical = [l for l in links.values() if l.kind == "physical"]
        free = [l for l in links.values() if l.kind == "free"]

        if self._plugins is not None and (artist or title):
            for plugin_link in await self._plugins.gather_purchase_links(
                artist, title, release_group_mbid
            ):
                url = (plugin_link.url or "").strip()
                if not url.startswith("http") or url in links:
                    continue
                kind = plugin_link.kind if plugin_link.kind in ("digital", "physical", "free") else "digital"
                store = _store_for(url)
                link = PurchaseLink(
                    store=store,
                    label=plugin_link.label or _label_for(store, url),
                    url=url,
                    kind=kind,
                )
                links[url] = link
                {"digital": digital, "physical": physical, "free": free}[kind].append(link)

        if not digital and artist and title:
            found = await self._itunes.find_album(artist, title, country=region)
            if found is not None:
                digital.append(
                    PurchaseLink(
                        store="itunes",
                        label=_STORE_LABELS["itunes"],
                        url=found.url,
                        kind="digital",
                    )
                )

        any_decorated = False
        if decorated:
            decorated_digital = [self._decorate(link) for link in digital]
            decorated_physical = [self._decorate(link) for link in physical]
            any_decorated = any(
                after.url != before.url
                for after, before in zip(
                    decorated_digital + decorated_physical, digital + physical
                )
            )
            digital, physical = decorated_digital, decorated_physical

        search_term = quote_plus(f"{artist} {title}".strip())
        return PurchaseOptionsResponse(
            digital=sorted(digital, key=_order_key),
            physical=sorted(physical, key=_order_key),
            free=sorted(free, key=_order_key),
            bandcamp_search_url=f"https://bandcamp.com/search?q={search_term}&item_type=a",
            disclosure=any_decorated,
        )

    def _collect_rel_links(
        self,
        relations: list[dict],
        links: dict[str, PurchaseLink],
        *,
        allowed: frozenset[str] = _OWNERSHIP_REL_TYPES,
    ) -> None:
        for rel in relations:
            rel_type = rel.get("type") or ""
            if rel_type not in allowed:
                continue
            if rel.get("ended"):
                continue
            url = ((rel.get("url") or {}).get("resource") or "").strip()
            if not url or not url.startswith("http") or url in links:
                continue
            if rel_type in _PHYSICAL_REL_TYPES:
                kind = "physical"
            elif rel_type in _FREE_REL_TYPES:
                kind = "free"
            else:
                kind = "digital"
            store = _store_for(url)
            links[url] = PurchaseLink(
                store=store, label=_label_for(store, url), url=url, kind=kind
            )

    def _decorate(self, link: PurchaseLink) -> PurchaseLink:
        """D19 affiliate decoration. Ordering NEVER changes here - only URLs."""
        if link.store == "amazon":
            tag = DN_AMAZON_TAGS.get(_amazon_marketplace(link.url))
            if tag:
                return PurchaseLink(
                    store=link.store,
                    label=link.label,
                    url=_append_param(link.url, "tag", tag),
                    kind=link.kind,
                )
            return link  # a marketplace we have no tag for stays clean
        if link.store == "itunes" and DN_APPLE_TOKEN:
            return PurchaseLink(
                store=link.store,
                label=link.label,
                url=_append_param(link.url, "at", DN_APPLE_TOKEN),
                kind=link.kind,
            )
        if link.store == "qobuz" and DN_AWIN_PUBLISHER_ID and AWIN_QOBUZ_ADVERTISER_ID:
            wrapped = (
                "https://www.awin1.com/cread.php?"
                f"awinmid={AWIN_QOBUZ_ADVERTISER_ID}&awinaffid={DN_AWIN_PUBLISHER_ID}"
                f"&ued={quote_plus(link.url)}"
            )
            return PurchaseLink(store=link.store, label=link.label, url=wrapped, kind=link.kind)
        return link

def _extract_artist(rg: dict) -> str:
    credits = rg.get("artist-credit") or []
    parts: list[str] = []
    for credit in credits:
        if isinstance(credit, dict):
            name = credit.get("name") or (credit.get("artist") or {}).get("name") or ""
            parts.append(name + (credit.get("joinphrase") or ""))
        elif isinstance(credit, str):
            parts.append(credit)
    return "".join(parts).strip()


def _candidate_release_ids(rg: dict) -> list[str]:
    """Release ids ordered official-first (purchase rels usually sit on the
    canonical official release)."""
    official: list[str] = []
    rest: list[str] = []
    for release in rg.get("releases") or []:
        release_id = release.get("id")
        if not release_id:
            continue
        (official if release.get("status") == "Official" else rest).append(release_id)
    return official + rest
