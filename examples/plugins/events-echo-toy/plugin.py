"""Events Echo toy - subscriber + publisher + routes in one toy.

They are one surface: events flow in through ``on_event``, and the guarded
``POST note`` route publishes a ``download_note`` for the toy's own task back
out. A ``download_note`` the toy itself published never re-triggers its own
subscriber path (the host's depth-1 + causation-dedup loop guard).
"""

from droppedneedle_plugin import PluginRouteResponse


class EventsEcho:
    OWN_TASK_ID = "toy-owned-task-1"  # fictional task owned by plugin:events-echo-toy

    def __init__(self, context):
        self.ctx = context
        self._seen: int = 0

    async def on_event(self, event) -> None:  # subscriber: log, never raise
        self._seen += 1
        self.ctx.logger.info("saw %s for %s", event.kind, event.payload)

    async def handle_route(self, method, subpath, query: dict, body: object) -> PluginRouteResponse:
        # GET status -> {"seen": n}; POST note -> publish download_note for OWN task
        if method == "GET" and subpath == "status":
            return PluginRouteResponse(status=200, body={"seen": self._seen})
        if method == "POST" and subpath == "note":
            note = (body or {}).get("note", "") if isinstance(body, dict) else ""
            await self.ctx.publish("download_note", {"task_id": self.OWN_TASK_ID, "note": note[:1024]})
            return PluginRouteResponse(status=200, body={"published": True})
        return PluginRouteResponse(status=404, body={"error": {"code": "NOT_FOUND", "message": "Not found"}})
