"""Webhook Scrobbler - the minimal reference plugin.

Demonstrates the whole contract in ~30 lines: the entrypoint class takes the
host's PluginContext, reads live settings, and uses the host-provided HTTP
client (never builds its own).
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
