"""Webhook Scrobbler - the minimal reference plugin.

Demonstrates the whole contract in about 30 lines: the entrypoint class
takes the PluginContext, reads live settings, and posts with ctx.http.
No imports needed: the helper module does the protocol work.
"""


class WebhookScrobbler:
    def __init__(self, context):
        self.ctx = context

    async def on_scrobble(self, event):
        url = (self.ctx.settings.get("webhook_url") or "").strip()
        if not url:
            return  # unconfigured: silently do nothing
        payload = {
            "artist": event.artist,
            "track": event.track,
            "album": event.album,
            "timestamp": event.timestamp,
        }
        response = await self.ctx.http.post(url, json=payload)
        if response.status_code >= 400:
            self.ctx.logger.warning("webhook returned HTTP %s", response.status_code)
