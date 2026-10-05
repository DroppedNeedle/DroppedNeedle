"""Stream Toy - streaming_source only, plugin-dir fixture (no network).

Auth boundary demo: the plugin receives ``(recording_mbid, user_id)`` only -
no credentials, no tokens. The router authenticates first and enforces path
roots, range-passthrough, and concurrency leases around whatever ref returns.
"""

from droppedneedle_plugin import PluginStreamRef


class StreamToy:
    KNOWN_MBID = "00000000-0000-4000-8000-000000000000"  # fixed test fixture, not a real recording

    def __init__(self, context):
        self.ctx = context

    async def resolve_stream(self, recording_mbid, user_id):
        if recording_mbid != self.KNOWN_MBID:
            return None  # not mine: router falls through to local files
        return PluginStreamRef(path="fixtures/test-tone.flac")
