# DroppedNeedle plugins

Plugins add things DroppedNeedle does not do on its own: another place to
download from, a webhook for every play, audio for tracks you do not have,
events for your home automation. They work much like community plugins in Lidarr: someone
publishes a plugin in a GitHub repository, you paste the URL, and DroppedNeedle
installs it.

This page is the reference: how plugins run, what they can do, the manifest,
and the protocol. If you want to write one, start with
[docs/PLUGIN-CREATION.md](docs/PLUGIN-CREATION.md).

## Read this first: plugins are trusted code

**Only install plugins you trust.** A plugin is a program that runs on your
server. It is not sandboxed. Once you enable it, it can read and write files
and reach the network just like DroppedNeedle itself can.

What DroppedNeedle does to keep a plugin in its lane:

- **Nothing runs until you say so.** Installing a plugin, or copying a folder
  into the plugins directory, only stores files. A plugin starts when an admin
  enables it in Settings > Plugins.
- **Installs are pinned.** DroppedNeedle installs an exact commit and records
  it. The install screen shows that commit, the plugin's version, and what it
  asks to do, before anything is written. If the repository changes between
  the preview and the install, the install stops and asks you to look again.
- **Each plugin is its own process.** If it crashes, only that plugin stops;
  DroppedNeedle restarts it after a short wait (1 second, doubling up to 5
  minutes). If it hangs, its calls time out and the rest of the server
  carries on; three timeouts in a row restart it.
- **It gets a clean environment.** The plugin process does not inherit the
  server's environment variables, so no secrets from there. Its working
  directory and `HOME` are its own data folder. DroppedNeedle never tells it
  where the config and database folders are.
- **Memory is capped.** Each plugin process can use at most 1 GiB of address
  space, and it cannot dump core or gain privileges.
- **Calls have time limits** (listed per capability below).

What this does **not** do: it does not stop a plugin that goes looking for
your files. The plugin runs as the same user as the server, so if it knows
where `/app/config` is, it can read it. Treat a plugin like any other program
you would run on your server: read the code, or trust the author.

The protocol does not depend on processes, so a sandboxed mode (WebAssembly)
can be added later without changing plugins.

## Installing a plugin

### From GitHub

In **Settings > Plugins**, paste the repository URL and press **Preview**.

- `https://github.com/owner/repo` installs the latest release. If the
  repository has no releases, it installs the default branch as it is now.
- `https://github.com/owner/repo/releases/tag/v1.2.0` installs that release.
- `https://github.com/owner/repo/tree/some-branch` installs that branch (or
  tag) as it is now.
- `https://github.com/owner/repo/commit/<sha>` installs that commit.

The preview lists the plugin's name, version, author, the exact commit, and
what the plugin asks to do. Press **Install** to write it. The plugin arrives
disabled; enable it, fill in its settings, and save.

To update later, press **Update** on the plugin. What moves depends on how
it was installed:

- From releases (a plain repository URL): to the newest release.
- From a branch (a `/tree/` URL, or the default branch of a repository with
  no releases): to the branch's newest commit, only when you press Update.
- From a tag or a commit: nowhere. Those pins stay until you install a
  different version.

Nothing updates on its own. Updating keeps your settings and the plugin's
data.

### By hand

Copy the plugin folder into the plugins directory and reload Settings >
Plugins:

```bash
cp -r examples/plugins/webhook-scrobbler /path/to/app/plugins/
```

Under Docker the plugins directory is `/app/plugins`. **Mount it as a
volume**, or installed plugins disappear when the container is recreated.

### Removing

**Remove** deletes the plugin's code. Its settings stay in the config and its
data folder stays on disk, so reinstalling picks up where you left off.

## What a plugin needs

The Docker image includes `python3`, so plugins written in Python work out of
the box. If you build the image with `--build-arg PLUGIN_PYTHON=0`, Python is
left out and only plugins that ship their own program (see `command` below)
will run. Outside Docker, `python3` (3.9 or newer; the examples use 3.11) must
be on the server's `PATH`.

Python plugins need nothing else installed. DroppedNeedle ships a small helper
module, `droppedneedle_plugin`, and puts it on the plugin's import path.

## Where things live

```
plugins/
  my-plugin/          the plugin's code (plugin.toml at the root)
  .data/my-plugin/    the plugin's own folder: working directory and HOME
  .sdk/python/        the Python helper, written by the server at start
```

Folders starting with `.` or `_` are never treated as plugins.

## The manifest: plugin.toml

Every plugin has a `plugin.toml` at the root of its folder. DroppedNeedle
checks it before running anything; a typo in a key is an error, never
silently ignored.

```toml
[plugin]
name = "acme-music"                 # lowercase letters, digits and dashes, up to 32
display_name = "Acme Music"
version = "1.0.0"
api_version = 1
entrypoint = "plugin:AcmeMusic"     # <python module>:<class>
capabilities = ["download_client", "indexer", "scheduler"]
description = "One line about what it does."
author = "you"
homepage = "https://acme-music.test"
# command = ["./bin/acme-plugin"]   # instead of entrypoint, for non-Python plugins

[[settings]]                        # one per setting the admin fills in
key = "api_key"
label = "API key"
help = "Shown under the field."
secret = true                       # encrypted at rest, masked on screen

[[capability]]                      # extra detail for some capabilities
id = "download_client"
display_name = "Acme Music"

[[capability]]
id = "indexer"
target_source = "plugin:acme-music" # or "usenet", or another plugin's key

[schedule]                          # required with "scheduler"
interval_minutes = 60               # 5 to 1440
run_on_load = false

[[route]]                           # needs the "publisher" capability
path = "lookup"                     # served at /api/v3/plugins/ext/acme-music/lookup
method = "GET"                      # GET, POST or DELETE
auth = "user"                       # "admin" (default) or "user"
rate_limit_per_minute = 60          # 1 to 600

[plugin_ui]                         # optional settings panel
entry = "ui/dist/panel.js"          # a script inside the plugin folder
pages = ["panel"]
# external_url = "https://acme-music.test/setup"   # or a link instead
```

`api_version = 0` is the older, frozen contract: only `scrobbler` and
`purchase_links`, no `[schedule]`, `[[route]]` or `[plugin_ui]`. Manifests
from DroppedNeedle v2 load unchanged.

### Settings

Settings are what the admin types into the plugin's card. The plugin gets the
current values when it starts and again every time they are saved, secrets
decrypted. Secret values are encrypted in the config file and shown as
`plugin****`; saving the mask back keeps the stored value.

### Running something other than Python

Set `command` to the program and its arguments. A first entry starting with
`./` points inside the plugin folder (files under `bin/` and files starting
with `#!` are made executable on install). The program must speak the
protocol below on stdin and stdout, and exit when stdin closes.

## Capabilities

A plugin declares what it does in `capabilities`. With the Python helper, each
capability is one or more methods on the plugin class, with the same names
and arguments as in DroppedNeedle v2. Methods may be `async` or plain
functions. Anything a method raises is logged against the plugin and the
server carries on as if the plugin had said nothing.

| Capability | Methods | Time limit | When a plugin fails |
|---|---|---|---|
| `scrobbler` | `on_scrobble(event)` | 10 s | the play is still recorded |
| `purchase_links` (not shown yet) | `purchase_links(artist, album, release_group_mbid)` | 10 s | no links from it |
| `subscriber` | `on_event(event)` | 5 s | that event is skipped |
| `publisher` | `handle_route(method, subpath, query, body)` for `[[route]]`s; `await ctx.publish(kind, payload)` | 5 s | the route answers 502 |
| `metadata_provider` (not shown yet) | `enrich_artist(...)`, `enrich_album(...)` | 15 s | nothing is filled in |
| `scheduler` | `on_tick()` | the interval | the next tick runs as normal |
| `streaming_source` | `resolve_stream(recording_mbid, user_id)` | 5 s | the next plugin is asked |
| `indexer` | `search_album(...)`, `search_track(...)` | 35 s | no results from it |
| `download_client` | `enqueue`, `get_status`, `inspect_materialization`, `discard_client_artifacts`, `abort` | 120 s enqueue, 30 s others | the download fails over |

Every plugin may also have `is_configured()` and `health_check()`; they feed
the plugin's health in Settings and the source list.

### scrobbler

`on_scrobble(event)` runs once per accepted play, after duplicates, very short
tracks and Navidrome-handled plays are filtered out. `event` has `artist`,
`track`, `album`, `timestamp`, `duration_ms` and `recording_mbid`. It runs in
the background; the player never waits for it.

Example: `examples/plugins/webhook-scrobbler`.

### purchase_links

**Not shown yet.** The server accepts and runs `purchase_links` plugins, but
the album page in this version does not ask them for links yet. The
contract below is what the page will use.

`purchase_links(artist, album, release_group_mbid)` returns a list of
`PluginPurchaseLink(label, url, kind)`, where `kind` is `digital`, `physical`
or `free`. Links must be `http` or `https`. Duplicates (same URL) are dropped,
and the album page orders links by its own rules: a plugin cannot put itself
first. At most 20 links per plugin.

Example: `examples/plugins/metadata-joke-toy`.

### subscriber

`on_event(event)` receives `event.kind` and `event.payload` (a dict you can
also read with dots, like `event.payload.task_id`):

| Kind | Payload |
|---|---|
| `scrobble` | `artist`, `track`, `album`, `timestamp`, `duration_ms`, `recording_mbid` |
| `playback_started` | `artist`, `track`, `album`, `user_id` |
| `download_started`, `download_completed`, `download_failed` | `task_id`, `user_id`, `release_group_mbid`, `source`, `outcome` |
| `request_created`, `request_fulfilled` | `request_id`, `user_id`, `release_group_mbid`, `status` |
| `import_finished` | `release_group_mbid`, `track_count`, `source` |
| `plugin_notice` | `source_plugin`, `title`, `body` |

Each plugin handles one event at a time. An event that arrives while the
plugin is still busy with the last one is skipped and counted in the plugin's
health as a dropped event. Events never wait on plugins.

`scrobble` and `playback_started` fire together, where scrobbling happens, so
very short tracks and Navidrome-handled plays raise neither.

### publisher

Two things come with `publisher`:

**Routes.** Each `[[route]]` is served at
`/api/v3/plugins/ext/<plugin>/<path>` and calls
`handle_route(method, subpath, query, body)`, which returns
`PluginRouteResponse(status, body)`. DroppedNeedle checks the session and
role first (`auth = "admin"` refuses other users with 403), applies the
per-user rate limit (429 with `Retry-After`), and refuses request bodies over
1 MiB (413). Only statuses 200-299, 400 and 404 pass through; 500-599 become
502 and anything else becomes 200, so a plugin cannot redirect people through
your server. Answers over 1 MiB, and any failure, become a plain 502; the
plugin's error text never reaches the browser. An undeclared path or a
disabled plugin is 404.

**Publishing.** `await ctx.publish(kind, payload)` sends one of three hints:

| Kind | Fields | What happens |
|---|---|---|
| `plugin_notice` | `title`, `body` | sent to every subscriber as a `plugin_notice` event |
| `download_note` | `task_id`, `note` (up to 1 KiB) | written to the server log against that download |
| `indexer_invalidate` | `target_source` | written to the server log; the next search asks the plugin again anyway |

The result has `ok`, `status`, `retry_after` and `error`. Limits: 30
publishes a minute per plugin (429 after that), unknown fields are refused
(422), and a plugin that is handling a `plugin_notice` cannot publish again
(409), so two plugins cannot bounce notices between them forever. The same
event never reaches the same plugin twice.

Example: `examples/plugins/events-echo-toy`.

### metadata_provider

**Not shown yet.** The server accepts and runs `metadata_provider` plugins,
but the artist and album pages in this version do not ask them yet. The
contract and merge rules below are what the pages will use.

`enrich_artist(*, artist_name, mbid=None, timeout=...)` and
`enrich_album(*, artist_name, album_title, mbid=None, timeout=...)` return a
`PluginArtistEnrichment` / `PluginAlbumEnrichment` (`biography`, `links`,
`tags`, `image_urls`) or `None` for "not mine". Plugins only fill gaps:

| Field | Already known from MusicBrainz, Wikidata and friends | Missing |
|---|---|---|
| biography | kept | the first plugin biography |
| links, tags | kept, plugin entries added after | plugin entries |
| image | kept | the first plugin image URL |

Example: `examples/plugins/metadata-joke-toy`.

### scheduler

`on_tick()` runs every `interval_minutes` (at least 5). One tick at a time per
plugin; a tick still running when the next is due is cancelled; a failed tick
is logged and the next one runs as normal; missed ticks are not made up. With
`run_on_load = true` the first tick runs right after the plugin starts.

For state that should survive restarts, use `await ctx.state_set(key, value)`
and `await ctx.state_get(key)`: small strings (up to 1 MiB each) that the
server keeps in its database for you (at most 1000 keys and 64 MiB per
plugin). Files in `ctx.data_dir` also survive.

Example: `examples/plugins/http-catalog`.

### streaming_source

`resolve_stream(recording_mbid, user_id)` returns
`PluginStreamRef(path=..., url=..., content_type=..., duration_seconds=...)`
with exactly one of `path` or `url`, or `None` for "not mine". The plugin
gets the signed-in user's id, never their password or token.

Your own library always wins: plugins are only asked when the library does
not have the track. They are asked in name order and the first answer wins.
Before anything is served, DroppedNeedle checks the answer:

- A `path` (relative paths are inside the plugin folder) must resolve, after
  symlinks, to a file inside the plugin folder, its data folder, the folder
  in the plugin's `downloads_dir` setting if it has one, or a library
  folder. It is then served like a library file, with seeking and
  transcoding.
- A `url` must be `http` or `https`, and the host must resolve to public
  internet addresses only: no `localhost`, private networks, link-local,
  or IPv6 addresses outside global unicast (IPv4 hidden inside IPv6 is
  checked as IPv4). The server connects directly, without any proxy, checks
  the address it actually connected to, and follows up to three redirects,
  each checked again. The server must answer within 10 seconds and keep
  sending at least every 30 seconds; a stream stops at 500 MiB. The audio
  is passed through as-is, without seeking or transcoding.

Plugin streams are reachable through Subsonic and Jellyfin clients (when the
library misses) and at `/api/v3/stream/plugin/<recording mbid>`.

Example: `examples/plugins/stream-toy`.

### indexer and download_client

A plugin with `download_client` is a download source with the key
`plugin:<name>`. It shows up in the source list and in the source priority
setting; sources not listed there are tried after the listed ones.

An `indexer` searches for releases. Its results go to the source named in
`target_source`: its own client (the default when it also has
`download_client`), another plugin's client (`plugin:other-name`), or
`usenet`, where results that carry an `nzb_url` join the SABnzbd pipeline
next to your Newznab or Prowlarr results.

Search methods return a list of `IndexerResult(source, plugin=PluginSearchResult(...))`
(or bare `PluginSearchResult`s):

```python
PluginSearchResult(
    title="Artist - Album",
    size_bytes=0,
    score=0.9,            # your confidence, 0 to 1
    quality_tier="",      # lossless, mp3_320, mp3_256, mp3_192, low, or ""
    files=[],             # exact files (files mode), or empty (folder mode)
    payload="",           # anything; handed back to your client at enqueue
    nzb_url="",           # for indexers that feed "usenet"
)
```

You rank your source; DroppedNeedle applies its rules on top, the same as
for every other source: quarantined releases, ignored and required terms, the
size limit and the quality range drop results. A score of 0.70 or more
downloads automatically; 0.50 to 0.70 is kept back for a person to pick;
lower is dropped. When a download fails, the next result is tried.

The client then gets `enqueue(request)` with `task_id`, `source`, `files`,
`payload`, `job_name` and `download_type`, and returns a `TaskHandle`. Every
later call (`get_status`, `inspect_materialization`, `discard_client_artifacts`,
`abort`) gets that handle back, with `plugin_token` set to the result's
`payload`. `get_status` returns a `DownloadTaskStatus` (`status` is `queued`,
`downloading`, `completed` or `failed`); `inspect_materialization` returns a
`DownloadMaterialization` with the finished files in `file_paths`.

Files mode (non-empty `files`) means the client fetches exactly those files.
Folder mode (empty `files`) means the client works out the files itself and
reports them when done.

Examples: `examples/plugins/http-catalog` (files mode),
`examples/plugins/local-folder-client` with `local-folder-indexer` (folder
mode, two plugins paired).

### Settings panel

With `[plugin_ui] entry`, the admin's plugin card shows the plugin's own
script in a locked-down frame: no access to the page, its cookies or the
network. It can talk to the page only through `postMessage`, with four
read-only requests: `sources.list`, `search.preview`, `get_settings` (its own,
secrets masked) and `health.get` (its own). The script is served to admins
only, from inside the plugin folder. `external_url` shows a link instead.

## The Python helper

`droppedneedle_plugin` gives the plugin class one argument, the context:

- `ctx.settings`: the saved settings, a dict updated in place when they change.
- `ctx.http`: an async HTTP client with `get`, `post`, `put`, `delete`,
  `head` (taking `params`, `headers`, `json`, `data`, `content`, `timeout`),
  returning a response with `status_code`, `headers`, `content`, `text` and
  `json()`.
- `ctx.logger`: a logger; its lines land in the server log under the
  plugin's name.
- `ctx.publish(kind, payload)`, `ctx.state_get(key)`, `ctx.state_set(key, value)`.
- `ctx.scoring.album_match(artist, album, title)` and `track_match(...)`:
  optional 0-1 word-overlap scores for indexers.
- `ctx.name`, `ctx.plugin_dir`, `ctx.data_dir`.

Do not print to stdout: it carries the protocol. The helper sends `print`
output to the log instead, and anything a plugin writes to stderr is logged
too, one entry per line.

### Porting a v2 plugin

1. Change the imports. Everything v2 plugins imported from
   `infrastructure.plugins.protocols`, `models.common` and
   `repositories.protocols.*` now comes from `droppedneedle_plugin`:

   ```python
   from droppedneedle_plugin import PluginRouteResponse, ServiceStatus, TaskHandle
   ```

2. If you used `ctx.http` like `httpx`, it keeps working for the common calls
   above. Streaming responses and client options are not there.
3. Anything that reached into the server's own Python code no longer exists;
   use `ctx` instead.
4. State you kept in memory is lost when the plugin restarts (it is a separate
   process now). Keep anything important in `ctx.state_set` or in files under
   `ctx.data_dir`.

`plugin.toml` stays as it was.

## The protocol

Plugins and the server talk JSON-RPC 2.0 over the plugin's stdin and stdout,
one JSON message per line (UTF-8, ending in `\n`, at most 8 MiB). Either side
may send requests. stderr is free-form log output. The Python helper does all
of this; you need it only for a plugin in another language.

### Starting and stopping

1. The server starts the plugin with its data folder as the working directory
   and sends `initialize`:

   ```json
   {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
     "protocol_version": 1,
     "host": {"name": "droppedneedle", "version": "3.0.0"},
     "plugin": {"name": "acme-music", "version": "1.0.0", "api_version": 1,
                "entrypoint": "plugin:AcmeMusic", "capabilities": ["indexer"]},
     "settings": {"api_key": "..."},
     "plugin_dir": "/app/plugins/acme-music",
     "data_dir": "/app/plugins/.data/acme-music"}}
   ```

2. The plugin answers within 15 seconds with the protocol version it speaks
   and the capabilities it implements:

   ```json
   {"jsonrpc": "2.0", "id": 1, "result": {"protocol_version": 1, "capabilities": ["indexer"]}}
   ```

   A different `protocol_version`, or an error answer, stops the plugin until
   it is changed.
3. To stop, the server sends `shutdown` and waits 2 seconds before killing the
   plugin's process group. A plugin must also exit when stdin closes.

### Server to plugin

| Method | Params | Result |
|---|---|---|
| `initialize` | see above | `{protocol_version, capabilities}` |
| `shutdown` | none | anything |
| `plugin.health` | none | `{status: "ok"\|"error", message, configured}` |
| `scrobbler.on_scrobble` | scrobble fields | `null` |
| `purchase_links.get` | `{artist, album, release_group_mbid}` | `[{label, url, kind}]` |
| `subscriber.on_event` | `{kind, payload, causation_id}` | `null` |
| `scheduler.on_tick` | none | `null` |
| `routes.handle` | `{method, subpath, query, body}` | `{status, body}` |
| `metadata.enrich_artist` | `{artist_name, mbid}` | enrichment or `null` |
| `metadata.enrich_album` | `{artist_name, album_title, mbid}` | enrichment or `null` |
| `stream.resolve` | `{recording_mbid, user_id}` | `{path, url, content_type, duration_seconds}` or `null` |
| `indexer.search_album` | `{artist_name, album_title, year, track_count, timeout}` | `[search result]` |
| `indexer.search_track` | `{artist_name, track_title, album_title, duration_seconds, timeout}` | `[search result]` |
| `download_client.enqueue` | `{task_id, source, files, payload, job_name, download_type}` | task handle |
| `download_client.status` | `{handle}` | task status |
| `download_client.inspect` | `{handle}` | `{state, workspace_path, file_paths, mount_healthy}` |
| `download_client.discard` | `{handle}` | `true`/`false` |
| `download_client.abort` | `{handle}` | `true`/`false` |

Notifications (no answer): `settings.update` with `{settings}` when the admin
saves, and `$/cancel` with `{id}` when the server stops waiting for a request.

Answer an unknown method with error code `-32601`; the server treats that as
"not implemented". Use `-32000` for your own failures.

### Plugin to server

| Method | Params | Result |
|---|---|---|
| `host.publish` | `{kind, payload, causation_id?}` | `{ok, status, retry_after, error}` |
| `host.state.get` | `{key}` | `{value}` (`null` when unset) |
| `host.state.set` | `{key, value}` | `{}` |

State keys are 1-64 lowercase letters, digits, `_`, `-` or `/`, starting
with a letter or digit; values are strings up to 1 MiB. One plugin may keep
at most 1000 keys and 64 MiB in total. `host.log` is a notification with `{level, message}`
(`error`, `warning`, `info`, `debug`) for plugins that prefer it to stderr.

## Examples

All in `examples/plugins/`, all using fictional hosts:

- `webhook-scrobbler`: `scrobbler`, the smallest plugin (v0 manifest).
- `metadata-joke-toy`: `metadata_provider` and `purchase_links`.
- `events-echo-toy`: `subscriber`, `publisher` and routes.
- `stream-toy`: `streaming_source` with a file in its own folder.
- `http-catalog`: `download_client`, `indexer` (files mode), `scheduler` and a
  settings panel. Read this one first for sources.
- `local-folder-client` and `local-folder-indexer`: folder mode, one plugin
  feeding another.

They double as the plugin tests: the server's test suite runs each one as a
real process.

## Publishing a plugin

Put `plugin.toml` and your code at the root of a public GitHub repository and
make a release. People install it by pasting the repository URL. Bump
`version` in `plugin.toml` with each release so admins can see what they
have.

Respect the services your plugin talks to: their terms and their rate limits.
Ask for credentials as `secret` settings; never put them in the code.
DroppedNeedle ships no sources and endorses no plugins beyond the examples.
