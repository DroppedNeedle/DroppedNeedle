# DroppedNeedle Plugin API

> **Status: EXPERIMENTAL** (`api_version = 0`). The surface may change between
> releases until `api_version = 1`. Pin your plugin to the api_version it was
> written for; the host refuses manifests it doesn't speak.

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

Plugin settings are stored unencrypted in `config.json` unless the manifest
marks a field `secret = true` (masked in the UI, still plain in the file).

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
name = "my-plugin"            # unique id: letters, digits, - _
display_name = "My Plugin"
version = "1.0.0"
api_version = 0
entrypoint = "plugin:MyPlugin"   # <module>:<ClassName>
capabilities = ["scrobbler"]
description = "One line about what it does."
author = "you"
homepage = "https://example.com"

[[settings]]                   # optional, repeatable: admin-editable fields
key = "webhook_url"
label = "Webhook URL"
help = "Shown under the field in Settings."
secret = false                 # true = masked in the UI
```

### The entrypoint class

Your class is constructed once as `MyPlugin(context)`. The `context` provides:

- `context.settings` - a live mapping of the admin's saved values for your
  declared settings fields (re-read on every access; saves apply instantly).
- `context.http` - a shared `httpx.AsyncClient` owned by the host (timeouts
  and the app User-Agent are managed for you). **Do not build your own client.**
- `context.logger` - a logger namespaced to your plugin.

All capability methods are `async`. Exceptions are caught, logged against your
plugin, and never break the host flow that called you - but they also mean
your plugin silently did nothing, so log generously.

Your type hints and the objects you return come from
`infrastructure.plugins.protocols` - `SourceItem`, `PluginPurchaseLink`,
`ScrobbleEvent`. A plugin runs in-process, so you can import them directly:

```python
from infrastructure.plugins.protocols import SourceItem
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

### `audio_source`

```python
async def search(self, query: str, limit: int) -> list[SourceItem]: ...
async def fetch(self, item_id: str, dest_dir: Path) -> list[Path]: ...
```

A searchable source of audio the user is entitled to. `search` returns
`SourceItem(id, title, artist, detail)`. `fetch` downloads one item's files
into the host-provided `dest_dir` and returns the written paths - the host
then runs them through the standard import pipeline (identify, tag, organise,
notify), and progress appears in the Import tab. Your plugin never touches the
library directly.

Note that DroppedNeedle's library is MusicBrainz-keyed: fetched items are
auto-imported only when they identify against a MusicBrainz release (by tags
or fingerprint). Anything else waits in "Needs a match" for a manual pick, so
sources whose catalogue rarely exists in MusicBrainz (one-off live tapes,
unreleased material) will lean on that manual step.

Reference: [`examples/plugins/lma-source`](examples/plugins/lma-source) - the
Internet Archive's Live Music Archive, where artists explicitly permit free
taping and trading.

### Reserved capability ids

`metadata_provider` and `streaming_source` validate in a manifest today but
are not activated: the host logs and skips them. They exist so early manifests
stay forward-compatible with the api_versions that wire them up.

## Rules of the house

- You are responsible for what your plugin accesses. DroppedNeedle ships no
  sources, endorses no plugins, and maintains none beyond the examples above.
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

Removing a plugin deletes its folder. Its settings stay in `config.json`, so
reinstalling the same plugin picks up where you left off.

## Publishing a plugin

Put `plugin.toml` and your entrypoint module at the **root** of a public GitHub
repository. That's the whole contract - users install it by pasting the URL.
