import ipaddress
import re
from typing import Optional
from urllib.parse import urlparse

from core.exceptions import ValidationError as AppValidationError

MBID_PATTERN = re.compile(r'^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$', re.IGNORECASE)
ISRC_PATTERN = re.compile(r'^[A-Z0-9]{12}$')

_ALLOWED_SERVICE_SCHEMES = frozenset({"http", "https"})


def validate_service_url(url: str, label: str = "URL") -> str:
    """Validate that a user-supplied service URL uses http or https only.

    Raises AppValidationError for empty, malformed, or non-HTTP URLs.
    Returns the normalised URL on success.
    """
    if not url or not isinstance(url, str):
        raise AppValidationError(f"{label} must be a non-empty string")

    url = url.strip()
    if not url:
        raise AppValidationError(f"{label} must be a non-empty string")

    try:
        parsed = urlparse(url)
    except Exception:  # noqa: BLE001
        raise AppValidationError(f"Invalid {label}: malformed URL")

    scheme = (parsed.scheme or "").lower()
    if scheme not in _ALLOWED_SERVICE_SCHEMES:
        raise AppValidationError(
            f"Invalid {label}: only http:// and https:// schemes are allowed"
        )

    if not parsed.hostname:
        raise AppValidationError(f"Invalid {label}: missing hostname")

    return url

AUDIODB_ALLOWED_HOSTS: frozenset[str] = frozenset({
    "www.theaudiodb.com",
    "theaudiodb.com",
    "r2.theaudiodb.com",
})


def validate_audiodb_image_url(url: str) -> bool:
    """Validate that a URL points to a known AudioDB CDN host over HTTPS.

    Rejects non-HTTPS schemes, unknown hosts, and private/loopback/link-local targets.
    """
    if not url or not isinstance(url, str):
        return False
    try:
        parsed = urlparse(url)
    except Exception:  # noqa: BLE001
        return False

    if parsed.scheme != "https":
        return False

    hostname = (parsed.hostname or "").lower().strip()
    if not hostname:
        return False

    if hostname not in AUDIODB_ALLOWED_HOSTS:
        return False

    try:
        addr = ipaddress.ip_address(hostname)
        if addr.is_private or addr.is_loopback or addr.is_link_local or addr.is_reserved:
            return False
    except ValueError:
        pass

    return True


SPOTIFY_CDN_SUFFIX = ".scdn.co"


def validate_spotify_cover_url(url: str) -> bool:
    """Validate that a URL points at a Spotify CDN image host over HTTPS.

    Accepts any ``*.scdn.co`` host (i.scdn.co, mosaic.scdn.co, thisis-images.scdn.co,
    ...). Rejects non-HTTPS schemes, empty hosts, and anything outside the Spotify
    CDN suffix - including lookalikes such as ``i.scdn.co.evil.com`` (suffix match
    is on the hostname, not the path).
    """
    if not url or not isinstance(url, str):
        return False
    try:
        parsed = urlparse(url)
    except Exception:  # noqa: BLE001
        return False

    if parsed.scheme != "https":
        return False

    hostname = (parsed.hostname or "").lower().strip()
    if not hostname:
        return False

    return hostname == "scdn.co" or hostname.endswith(SPOTIFY_CDN_SUFFIX)


def is_valid_mbid(mbid: Optional[str]) -> bool:
    if not mbid or not isinstance(mbid, str):
        return False
    
    mbid = mbid.strip()
    
    if mbid.startswith('unknown_'):
        return False
    
    return bool(MBID_PATTERN.match(mbid))


def validate_mbid(mbid: Optional[str], entity_type: str = "entity") -> str:
    if not mbid or not isinstance(mbid, str):
        raise ValueError(f"Invalid {entity_type} MBID: must be a non-empty string")
    
    mbid = mbid.strip()
    
    if not mbid:
        raise ValueError(f"Invalid {entity_type} MBID: empty string")
    
    if mbid.startswith('unknown_'):
        raise ValueError(f"Cannot process unknown {entity_type} MBID: {mbid}")
    
    if not MBID_PATTERN.match(mbid):
        raise ValueError(f"Invalid {entity_type} MBID format: {mbid}")
    
    return mbid


def is_unknown_mbid(mbid: Optional[str]) -> bool:
    return not mbid or not isinstance(mbid, str) or mbid.startswith('unknown_') or not mbid.strip()


def is_valid_isrc(value: Optional[str]) -> bool:
    if not value or not isinstance(value, str):
        return False

    normalized = value.strip().upper()

    return len(normalized) == 12 and bool(ISRC_PATTERN.match(normalized))


def sanitize_optional_string(value: Optional[str]) -> Optional[str]:
    if not value or not isinstance(value, str):
        return None
    
    value = value.strip()
    return value if value else None


def strip_html_tags(text: str | None) -> str:
    """Strip HTML tags from text, converting <br> to newlines.

    Uses stdlib html.parser - no external dependencies needed.
    Returns plain text suitable for display.
    """
    if not text:
        return ""

    from html.parser import HTMLParser
    from html import unescape

    class _TextExtractor(HTMLParser):
        def __init__(self) -> None:
            super().__init__()
            self._parts: list[str] = []

        def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
            if tag in ("br", "br/"):
                self._parts.append("\n")

        def handle_endtag(self, tag: str) -> None:
            if tag == "p":
                self._parts.append("\n\n")

        def handle_data(self, data: str) -> None:
            self._parts.append(data)

        def get_text(self) -> str:
            return "".join(self._parts).strip()

    extractor = _TextExtractor()
    extractor.feed(unescape(text))
    return extractor.get_text()


_LASTFM_SUFFIX_RE = re.compile(
    r"\s*Read more on Last\.fm\.?\s*$",
    re.IGNORECASE,
)


def clean_lastfm_bio(text: str | None) -> str:
    """Strip HTML tags and remove the trailing 'Read more on Last.fm' suffix."""
    cleaned = strip_html_tags(text)
    if not cleaned:
        return ""
    return _LASTFM_SUFFIX_RE.sub("", cleaned).rstrip()
