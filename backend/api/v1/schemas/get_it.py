"""Wire schemas for the "Where to buy" section (Get it, phase 01)."""

from infrastructure.msgspec_fastapi import AppStruct


class PurchaseLink(AppStruct):
    store: str  # 'bandcamp' | 'qobuz' | 'itunes' | 'amazon' | ... | 'other'
    label: str  # human store name ("Bandcamp", or the bare domain for 'other')
    url: str
    kind: str  # 'digital' | 'physical' | 'free'


class PurchaseOptionsResponse(AppStruct):
    digital: list[PurchaseLink] = []
    physical: list[PurchaseLink] = []
    free: list[PurchaseLink] = []
    bandcamp_search_url: str = ""
    # true only when the support toggle is on AND at least one link actually
    # carries an affiliate tag - drives the disclosure line (D19)
    disclosure: bool = False


class ArtistPurchaseOptionsResponse(AppStruct):
    """Where to buy an ARTIST's music: their own storefront pages, not one
    album. Deliberately thinner than the album shape - no iTunes lookup (that
    is album-specific), no physical/free split."""

    links: list[PurchaseLink] = []
    bandcamp_search_url: str = ""
    disclosure: bool = False
