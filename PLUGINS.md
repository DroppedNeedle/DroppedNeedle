# DroppedNeedle Plugin API

> **Status: STABLE** (`api_version = 1`). Plugins written for `api_version = 0`
> keep working exactly as before (see table). Pin your plugin to the api_version
> it was written for; the host refuses manifests it doesn't speak, naming the
> versions it does.

| `api_version` | State | May declare |
| --- | --- | --- |
| `0` | frozen, loads byte-identically (v0 reserved ids `metadata_provider` / `streaming_source` still log-and-skip) | `scrobbler`, `purchase_links` (+ v0 reserved skip-set) |
| `1` | current | all v0 ids + `download_client`, `indexer`, `subscriber`, `publisher`, `metadata_provider`, `scheduler`, `streaming_source` (+ `[[route]]`, `[schedule]`, `[plugin_ui]` tables) |

Host accepts `SUPPORTED_API_VERSIONS = (0, 1)`; anything else (e.g. `api_version = 2`)
fails load with an error naming both accepted versions.

DroppedNeedle loads plugins from the `plugins/` directory in its data folder,
alongside `config/` and `cache/` - one folder per plugin. Under Docker that is
`/app/plugins`, which is not mounted by default: add the volume, or every plugin
you install disappears when the container is recreated. Nothing is bundled, and
there is no plugin registry: what you install is between you and the plugin's
author.

## Trust model - read this first

A plugin is Python running **in-process with the full privileges of your
DroppedNeedle server**. There is no sandbox. Two rules follow:

1. Only install plugins whose code you have read or whose author you trust.
2. Dropping a folder into the plugins directory runs **no code**. A plugin is
   inert until an admin explicitly enables it in **Settings > Plugins**.

A download-client plugin holds the same power as the download-client settings it
parallels: once enabled, it can enqueue downloads and expose files for import with
your server's privileges. The enable gate is the control — read the code before
you enable it.

A streaming plugin can make the server fetch and serve bytes, with the same
admin-enable gate as download clients (§3.4).

Residual risk, stated honestly: per-call timeouts fire only at await points — a
plugin doing sync/blocking work (file I/O, parsing, crypto) past a timeout still
degrades the whole server until it yields. The host logs per-call durations and
surfaces overruns in plugin health; plugin authors must run blocking work in
`asyncio.to_thread` and never block the event loop.

`secret = true` fields are encrypted at rest, masked on every read and over RPC
(including the panel `get_settings` RPC payload — plaintext never appears in
`config.json` dumps or RPC), and saves use a sentinel (sending the mask back
keeps the stored value; only a changed value rewrites it). Reads return the
`plugin****` mask when a value exists.

Plugins load when the server starts, and reload whenever an admin saves any
plugin in Settings > Plugins. If you edit a plugin's code on disk, save it in
Settings (or restart) before the change takes effect.

## Anatomy of a plugin

```
my-plugin/
├── plugin.toml     # the manifest - validated before any import happens
└── plugin.py       # your entrypoint module
```

### plugin.toml

```toml
[plugin]
name = "my-plugin"            # unique id: lowercase kebab-case in v1
display_name = "My Plugin"
version = "1.0.0"
api_version = 1
entrypoint = "plugin:MyPlugin"   # <module>:<ClassName>
capabilities = ["scrobbler"]
description = "One line about what it does."
author = "you"
homepage = "https://acme-music.test"

[[settings]]                   # optional, repeatable: admin-editable fields
key = "webhook_url"
label = "Webhook URL"
help = "Shown under the field in Settings."
secret = false                 # true = encrypted at rest, masked, sentinel saves
```

### Manifest reference (v1 tables)

```toml
[plugin]
name = "acme-music"              # source key becomes plugin:acme-music (the manifest
                                 # `source` field below is a display alias only)
api_version = 1
entrypoint = "plugin:AcmeMusic"
capabilities = ["download_client", "indexer", "subscriber", "publisher", "scheduler"]

[[capability]]                   # per-capability config; every id must also be in `capabilities`
id = "download_client"
source = "acme-music"            # display alias only, ^[a-z0-9][a-z0-9-]{0,31}$
display_name = "Acme Music"

[[capability]]
id = "indexer"
target_source = "plugin:acme-music"   # "usenet" OR a plugin source key (own or another plugin's)

[schedule]                       # required iff `scheduler` is declared
interval_minutes = 60            # int in [5, 1440]
run_on_load = false

[[route]]                        # requires api_version = 1 AND the `publisher` capability
path = "lookup"                  # ^[a-z0-9][a-z0-9/_-]{0,63}$, relative only
method = "GET"                   # GET|POST|DELETE
auth = "user"                    # admin|user, deny-by-default
rate_limit_per_minute = 60       # default 60, max 600

[plugin_ui]                      # requires api_version = 1; entry/pages XOR external_url
entry = "ui/dist/panel.js"       # prebuilt JS inside the plugin dir (no .., absolute, or http(s))
pages = ["panel"]                # v1 single page id allowlist; >1 fails load
# external_url = "https://acme-music.test/setup"   # alternative to entry/pages (https; http loopback-only)
```

Strict rule: unknown keys in any table fail load loudly — a typo'd `soruce`
is a ManifestError, never a silent default. `[[route]]` on a manifest without
`publisher`, or `[plugin_ui]` / `[[route]]` / `[schedule]` on `api_version = 0`, fails load.
`[[settings]]` shape is unchanged (`key`, `label`, `help`, `secret`); `secret = true`
follows the mask-sentinel contract above.

One plugin, one source key: `plugin:<manifest-name>`. A plugin declaring both
`download_client` and `indexer` is one complete source. An indexer-only plugin
with `target_source = "usenet"` feeds the existing SABnzbd pipeline; any other
`target_source` pins the client that enqueues its results, including cross-plugin
pairing (resolved deterministically by plugin-name sort). `display_name`
overrides the card label in Settings and the queue. `source` is a display alias
only and must also match `^[a-z0-9][a-z0-9-]{0,31}$`.

### The entrypoint class

Your class is constructed once as `MyPlugin(context)`. The `context` provides:

- `context.settings` - a live mapping of the admin's saved values for your
  declared settings fields (re-read on every access; saves apply instantly).
- `context.http` - a shared `httpx.AsyncClient` owned by the host (timeouts
  and the app User-Agent are managed for you). **Do not build your own client.**
  Per-request `timeout=` overrides on the shared client cover large transfers.
- `context.logger` - a logger namespaced to your plugin.
- `context.scoring` - opt-in `album_match` / `track_match` helper reusing the
  app's token-matching pipeline (see `indexer`).
- `ctx.publish(kind, payload)` - bound publisher API for plugins declaring the
  `publisher` capability (see `publisher`).

All capability methods are `async`. The host catches exceptions, logs them against your
plugin, and keeps running the flow that called you. A caught exception means
your plugin did nothing, so log generously.

Your type hints and the objects you return come from
`infrastructure.plugins.protocols`: the single documented import surface,
which re-exports the boundary types (`DownloadClientProtocol`,
`EnqueueRequest`, `TaskHandle`, `IndexerProtocol`, `IndexerResult`,
`PluginSearchResult`, and the rest). A plugin runs in-process, so you can
import them directly:

```python
from infrastructure.plugins.protocols import PluginPurchaseLink
```

That import couples your plugin to the host's internals, which is exactly what
`api_version` tracks. When it changes, expect these types to move with it.

## Capabilities

### `scrobbler`

```python
async def on_scrobble(self, event) -> None: ...
```

Called once per accepted play (already deduplicated). `event` has `artist`,
`track`, `album`, `timestamp`, `duration_ms`, `recording_mbid`. Dispatch is
fire-and-forget: take your time, you can't slow the player down.

Reference: [`examples/plugins/webhook-scrobbler`](examples/plugins/webhook-scrobbler).

### `purchase_links`

```python
async def purchase_links(self, artist, album, release_group_mbid) -> list[PluginPurchaseLink]: ...
```

Contribute links to the album page's "Where to buy" section. Return
`PluginPurchaseLink(label=..., url=..., kind='digital'|'physical'|'free')`.
Links are deduplicated by URL and ordered by the app's store-fairness rules -
plugins cannot influence ordering. You have a 10-second budget per album.

### `download_client`

Implements `DownloadClientProtocol` directly (imported from
`infrastructure.plugins.protocols`). The host wraps your instance in an
adapter before handing it to the acquisition engine.

- `client_name` — ignored; the adapter forces `plugin:<manifest-name>` (no spoofing).
- `is_configured()` — `False` on error; gates source enablement and readiness.
- `health_check()` — errors map to `ServiceStatus.error`; best-effort, never fails listings.
- `enqueue(request)` — failures raise engine-understood failures; `request.payload`
  carries the plugin's opaque correlation token (see below). Per-request `timeout=`
  overrides on the shared client for large transfers — never build your own client.
- `get_status(handle)` / `abort(handle)` — `handle.plugin_token` reattaches the
  correlation id stored at enqueue.
- `inspect_materialization` / `discard_client_artifacts` — safe empty shapes on error.
- `list_completed_files(handle)` — `[]` on error (absence, not failure).
- `get_file_path(handle, remote_filename, size=None)` — `None` on error.
- `diagnose_downloads_mount()` — `MountDiagnosis(supported=False)` on error.

Correlation: `IndexerResult.plugin.payload` (opaque `str`) is handed back
verbatim as `EnqueueRequest.payload`; the adapter stores it against `task_id` in memory and
reattaches it as `TaskHandle.plugin_token` for status/abort/file calls.

Files vs folder mode: `PluginSearchResult.files` non-empty = per-file mode
(exact files are enqueued and imported, Soulseek-shaped); empty = folder mode (the client
downloads the release and `list_completed_files` feeds MB-tracklist folder import,
Usenet-shaped minus the NFS settle — plugin clients manage their own visibility).

Error isolation: every adapter method is delegation + `try/except` that logs against the
plugin and converts to the safe shape above. A plugin can fail a download; it cannot crash
the poll loop, the failover loop, or the host.

List this source in the UI via `GET /api/v3/plugins/sources`: each entry carries
`key` (`plugin:<name>`), `plugin`, `display_name`, `has_client`, `has_indexer`,
`target_source`, `configured`, and `health` (closed enum
`ok`/`degraded`/`error`/`unknown`; anything else reads back as `unknown`).

### `indexer`

Implements `IndexerProtocol` directly:

```python
@property
def indexer_name(self) -> str: ...
def is_configured(self) -> bool: ...
async def health_check(self) -> ServiceStatus: ...
async def search_album(self, artist_name, album_title, year=None, track_count=None, *, timeout=30.0) -> list[IndexerResult]: ...
async def search_track(self, artist_name, track_title, album_title=None, duration_seconds=None, *, timeout=30.0) -> list[IndexerResult]: ...
```

- `indexer_name` — forced to the **target** source key (`usenet` for usenet-targeting
  indexers pooling into the composite, else the owning plugin's `plugin:<name>` key).
- `search_album(...)` / `search_track(...)` — wrapped in `asyncio.timeout(timeout)` +
  `try/except → []`: one broken indexer drops only its group, never the search.
- `target_source`: required for indexer-only plugins; defaults to the plugin's own source
  when it also declares `download_client`. `"usenet"` feeds the existing SABnzbd pipeline
  (pooled + deduped by `usenet_identity`); a plugin key feeds that source's client, including
  cross-plugin pairing (resolved deterministically by plugin-name sort).
- `is_configured` (error → `False`) / `health_check` (error → `ServiceStatus.error`).

```python
class PluginSearchResult(AppStruct):
    title: str
    size_bytes: int = 0
    score: float = 0.0              # plugin-provided confidence 0..1 (clamped by the app)
    quality_tier: str = ""          # optional; "" = unknown
    files: list[DownloadFileRef] = []   # per-file mode; empty = folder mode
    payload: str = ""               # opaque correlation token, handed back at enqueue
```

"You rank your source; the app enforces policy": `final_score = clamp(score, 0, 1)`;
quality range, ignored/required terms, max size, quarantine, and held-tier gates apply
identically to plugin results. `context.scoring` (`album_match` / `track_match`) is an
opt-in helper reusing the app's token-matching pipeline for plugins that want it —
using it is never required.

Timeout contract: the `timeout` kwarg (default `30.0`) is the host's budget, enforced with
`asyncio.timeout`; timeout → `[]` + degradation record.

### `subscriber`

```python
async def on_event(self, event: PluginEvent) -> None: ...
```

- `on_event(event: PluginEvent) -> None` — consume-only; the plugin receives small structs,
  never task rows or engine handles, and cannot mutate engine state.
- Event kinds (closed set) + payload shapes: `scrobble` (reuses `ScrobbleEvent`),
  `download_started` / `download_completed` / `download_failed`
  (`DownloadTaskEvent{task_id, user_id, release_group_mbid, source, outcome}`-shaped),
  `request_created` / `request_fulfilled` (`RequestEvent{...}`-shaped),
  `import_finished` (`ImportEvent{release_group_mbid, track_count, source}`-shaped),
  `playback_started` (`PlaybackEvent{artist, track, album, user_id}`-shaped).
- Dispatch is fire-and-forget with per-plugin isolation (per-plugin task + timeout 5 s);
  at most ONE in-flight notification per plugin — a slow subscriber never delays the
  publishing flow; overruns are skipped (skip-if-pending) and counted in health
  (`dropped_events`). Each kind carries exactly one payload struct, stamped by the
  host publisher path with a top-level `causation_id` so fan-out can dedup on
  `(causation_id, subscriber)`.
- `scrobble`-kind events also reach v0 `scrobbler` plugins (alias path, unchanged).
- `playback_started` gate: fires where scrobble dispatch fires — short-track
  and Navidrome-delegated plays do NOT emit.
- `import_finished` scope: ALL imports — the orchestrator `_finalize` (after
  request sync) AND `DropImportService._after_import` (covers manual drop-imports and
  Free Music completions). Every library addition is visible.

### `publisher`

Bound API only (not a plugin method to implement):

```python
await ctx.publish(kind, payload)
```

- Bound API only: `ctx.publish(kind, payload)` — the host stamps `source_plugin`;
  caller-supplied source is ignored; a disabled plugin's publish is dropped + logged.
  Declaring the capability id admits the plugin to the allowlisted publish kinds.
- Allowlisted kinds v1 (closed set; unknown kind = loud error):
  `indexer_invalidate{target_source}` (hint to rescout; engine decides),
  `download_note{task_id, note}` (annotation only, never status mutation; ONLY tasks owned
  by the plugin's own source key — cross-plugin `task_id` dropped + logged with no
  existence oracle; `note` capped at 1 KiB),
  `plugin_notice{title, body}` (opaque, fanned to subscribers, never mutates).
- Schemas: one AppStruct per kind, `msgspec.convert(strict)` both directions; unknown
  keys rejected.
- Rate limits: 30 publishes/min keyed `(plugin, principal)` (per-principal isolation —
  one user cannot burn another's quota); exceed = drop + warn log + health signal +
  documented `429 {code: "rate_limited"}` with `Retry-After: 60`.
- Loop guards: max publish depth 1 (a publish from inside `on_event` is enqueued once;
  nested publishes from the resulting dispatch are dropped + logged); per-top-level-event
  causation id with bounded dedup; never synchronous re-entry; bounded queue (100,
  drop-oldest + counter).

### Custom routes (`/ext/`)

One router: `GET|POST|DELETE /api/v3/plugins/ext/{plugin_name}/{subpath}`;
`{plugin_name}` matches EXACTLY (case-sensitive, no normalization); mismatch → 404.
Requires `api_version = 1` AND the `publisher` capability in the manifest.

```python
async def handle_route(self, method, subpath, query: dict, body: object) -> PluginRouteResponse: ...
```

- Handler shape: `async def handle_route(method, subpath, query, body) -> PluginRouteResponse{status, body}`;
  `asyncio.timeout(5.0)`; exceptions → generic 5xx envelope (never a traceback).
- Auth: deny-by-default; the route declares the loosest dep and the handler re-checks at
  runtime (`auth: admin` re-verified with admin semantics — a user token gets 403, anon
  gets 401). Disabled plugin or unknown path → 404 (no oracle); validation fail → 422;
  rate exceed → 429 + `Retry-After`; request POST body capped (oversized → 413 without
  invoking plugin); response body capped at 1 MiB serialized (over → 502 + warn log).
- Status clamp: only `200–299` plus explicit `400`/`404` pass through —
  anything else the plugin chooses (301/302/500/…) is mapped to `200` (success with body)
  or `502` (plugin-signalled failure) per a fixed table, never proxied (no open-redirect
  through the API origin, no `Location` leak). In full: `200–299`, `400`, `404` pass;
  `500–599` map to `502`; anything else maps to `200`. Unserialisable bodies are a
  plugin failure (generic 502 + warn log).
- `[[route]]` caps: `path` matches `^[a-z0-9][a-z0-9/_-]{0,63}$` relative-only,
  `method` is `GET`, `POST`, or `DELETE`, `auth` is `admin` (default) or `user`,
  `rate_limit_per_minute` is an int in `[1, 600]` (default 60).
- Layering: plugin code never touches stores; file mutations via routes go through the
  library-management publish path only; routes never synchronously dispatch events.

### `metadata_provider`

```python
async def enrich_album(self, *, artist_name, album_title, mbid=None, timeout=30.0) -> PluginAlbumEnrichment | None: ...
async def enrich_artist(self, *, artist_name, mbid=None, timeout=30.0) -> PluginArtistEnrichment | None: ...
```

- `enrich_album(*, artist_name, album_title, mbid=None, timeout=30.0)` /
  `enrich_artist(*, artist_name, mbid=None, timeout=30.0)` → enrichment struct or `None`;
  all fields defaulted — partial enrichment is normal. Consumed BELOW first-party sources.
- Failure degrades to `None` (recorded in the request `DegradationContext` as
  `plugin:<name>`); timeout → `None` + record, never an error page.
- Per-field merge table (a gap is `None` OR empty `""`/`[]`; anything present
  first-party wins, never overwritten):

  | Field | First-party present → | Gap → |
  | --- | --- | --- |
  | `biography` | keep first-party | plugin `biography` if non-empty |
  | `links` | keep first-party list | union (first-party order, then plugin-only entries) |
  | `tags` | keep first-party list | union, same order rule |
  | `images` | keep first-party | plugin `image_urls[0]` only when first-party has none |

### `scheduler`

```python
async def on_tick(self) -> None: ...
```

- Manifest: `scheduler` + `[schedule]` (`interval_minutes` int in `[5, 1440]`, 5-min floor
  is a DoS-by-config guard; `run_on_load = false` default, first tick after the interval).
- `async def on_tick() -> None` — no args, no return surface; work through `context.http`
  and `context.settings`; publish via `ctx.publish`; library file writes ONLY through
  `plugin_write_library_file()` (relative-only path, app music-library roots only,
  100 MiB/day/plugin quota); plugin-dir state files only (symlink escape = error + log,
  10 MiB cap per file); no SQLite access in v1; no engine handles passed.
- Loop semantics: one loop per plugin, no overlap (an overrun delays itself + one full
  interval, never runs twice); a hung tick is cancelled at the interval; an exception
  logs-and-continues; a missed tick is skipped (no backfill); disable-while-running
  cancels the loop. Rebuilt ONLY on load/save via `sync_ticks` (the sole rebuild choke
  point, called after `load_all` from the plugin save route and startup lifecycle;
  loops are `TaskRegistry`-owned as `plugin-tick:<name>`).
- Shipped docs and examples use fictional hosts only
  (`example-catalog.test`, `acme-music.test`, `hooks.example`) — never a real
  indexer/tracker/Soulseek domain.

### `streaming_source`

```python
class PluginStreamRef(AppStruct):
    path: str = ""           # local file the host may serve/transcode
    url: str = ""            # remote http(s) the host proxies
    content_type: str = ""   # hint; host sniffs when empty
    duration_seconds: float | None = None

async def resolve_stream(self, recording_mbid: str, user_id: str) -> PluginStreamRef | None: ...
```

- Args are plain strings (mbid + already-authed `user.id`) — never the user record, never
  a token. `None` = "not mine" (router falls through to the local-files path).
- Exactly one of `path`/`url` set, else `None` + warn log.
- `path` containment: allowlisted roots are EXACTLY (1) the plugin's own directory,
  (2) its configured `downloads_dir` setting when declared, (3) the app music-library
  roots. Symlinks resolved; `..`/escape = `None` + warn log + health signal.
  `path` results reuse the app's decide/stream policy (direct-play vs transcode).
- `url` egress (SSRF): `http(s)` only, dedicated proxy client (`timeout=10.0`,
  `follow_redirects=False`), max 3 re-validated redirects; loopback / link-local /
  private / `localhost` destinations blocked AFTER DNS resolution (private-intranet
  plugins are a documented v1 non-goal). Timeout → `None` + degradation record.
  `url` MAY carry transcode hints (owner override 2026-09-05); the host proxy
  honours them rather than treating every url as opaque passthrough.
- Auth boundary: routers authenticate FIRST with the existing compat auth;
  `resolve_stream` NEVER runs before auth (order-pinned). Precedence is local-first,
  plugin-fallback — a plugin cannot shadow the library.

### Plugin UI (`[plugin_ui]`)

- `[plugin_ui]`: EITHER `entry` + `pages = ["panel"]` (prebuilt JS inside the plugin dir;
  must exist at load or the plugin loads with `error` set) OR `external_url` (https; http
  loopback-only), never both; neither = settings-fields-only (the common case).
- Serving: `GET /api/v3/plugins/{name}/ui/panel.js`, admin-only; `Content-Type:
  text/javascript`, `X-Content-Type-Options: nosniff`, sandbox-compatible CSP; ETag from
  host generation + mtime (no stale bytes across save); traversal → 404, real failures →
  generic 5xx (never fs details). External-URL pages need no backend surface (plain link).
- Embedding: `<iframe sandbox="allow-scripts">` (no `allow-same-origin`: opaque origin —
  DOM/storage/cookies unreachable by construction) + frame CSP egress lock
  (`connect-src 'none'`: the bundle cannot `fetch()` out); `postMessage` is the ONLY
  bridge, gated on opaque-origin `"null"` + `contentWindow` identity (foreign-window
  messages ignored; unknown methods get an `unknown_method` envelope).
- RPC is a CLOSED allowlist of exactly 4 read-only methods — any other method gets the
  `unknown_method` envelope:
  1. `sources.list` — plugin source roster (mirrors `GET /plugins/sources`: key,
     display_name, configured, health).
  2. `search.preview` — manual-search candidate groups with the same grouping labels as
     the review UI; read-only, no enqueue path.
  3. `get_settings` — the panel's OWN plugin settings only, secrets masked via the
     mask-sentinel contract (plaintext never in `config.json` or RPC).
  4. `health.get` — the panel's OWN plugin health entry (same closed-enum shape as
     `PluginSourceInfo.health`).
  The iframe NEVER gets `queryClient`, raw API access, or tokens.
  No write method exists on this bridge; multi-page (`pages > 1`) fails load in v1.

## Rules of the house

- You are responsible for what your plugin accesses. DroppedNeedle ships no
  sources, endorses no plugins, and maintains none beyond the examples below.
- Respect the upstream services you talk to: their terms, their rate limits.
- A plugin that needs credentials should declare them as `secret` settings
  fields, never hardcode them.

## Installing a plugin

Either way, the plugin lands disabled.

### From GitHub

In Settings > Plugins, paste a public repository URL: `https://github.com/owner/repo`,
or `https://github.com/owner/repo/tree/some-branch` to pin a branch. Without a
branch, DroppedNeedle tries `main` and falls back to `master`. The repository root
must contain `plugin.toml`. This stores code; it does not run it.

### By hand

```bash
cp -r examples/plugins/webhook-scrobbler <data dir>/plugins/
```

Read the code, then enable the plugin in Settings > Plugins. Enabling is what runs
it, with your server's privileges.

Install location is `<root_app_dir>/plugins` — under Docker that is `/app/plugins`,
which is not mounted by default: add the volume, or every plugin you install
disappears when the container is recreated. Either way, the plugin lands disabled:
enabling in Settings > Plugins is what runs it, with your server's privileges.
There is no plugin registry: what you install is between you and the plugin's author.
Removing a plugin deletes its folder; its settings stay in `config.json`, so
reinstalling picks up where you left off.

## Publishing a plugin

Put `plugin.toml` and your entrypoint module at the **root** of a public GitHub
repository. That's the whole contract. Users install it by pasting the URL.

## Examples

- `examples/plugins/http-catalog`: download_client + indexer + scheduler, files
  mode + scheduler tick + panel bundle (read this one first).
- `examples/plugins/local-folder-client` + `examples/plugins/local-folder-indexer`:
  folder-mode client paired with its indexer (cross-plugin pairing demo).
- `examples/plugins/events-echo-toy`: subscriber + publisher + routes.
- `examples/plugins/metadata-joke-toy`: metadata_provider.
- `examples/plugins/stream-toy`: streaming_source.
- `examples/plugins/webhook-scrobbler`: v0 scrobbler (frozen-compat reference).

Step-by-step build walkthrough: [docs/PLUGIN-CREATION.md](docs/PLUGIN-CREATION.md).
