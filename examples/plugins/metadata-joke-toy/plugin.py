"""Metadata Joke toy - metadata_provider and purchase_links, fixed fictional data.

Merge rule demo: the plugin fills gaps only (missing or empty first-party
fields); anything a first-party source already has wins and is never
overwritten. The buy link shows the purchase_links shape: the album page
orders links by its own rules, so a plugin cannot push itself to the top.
"""

from droppedneedle_plugin import PluginArtistEnrichment, PluginPurchaseLink


class MetadataJoke:
    def __init__(self, context):
        self.ctx = context

    async def enrich_artist(self, *, artist_name, mbid=None, timeout=30.0):
        if artist_name != "Test Artist":
            return None  # not mine: merge treats as gap, first-party wins
        return PluginArtistEnrichment(
            biography="Test Artist is a fictional example used by plugin tests.",
            tags=["example", "test-fixture"],
        )

    async def enrich_album(self, *, artist_name, album_title, mbid=None, timeout=30.0):
        return None

    async def purchase_links(self, artist, album, release_group_mbid):
        if artist != "Test Artist":
            return []
        return [
            PluginPurchaseLink(
                label="Example Records",
                url="https://shop.example-catalog.test/test-artist",
                kind="digital",
            )
        ]
