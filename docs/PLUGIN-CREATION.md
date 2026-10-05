# Writing a plugin

This walks through a small plugin from an empty folder to a GitHub release.
Every field, method and limit is in [PLUGINS.md](../PLUGINS.md); this page
shows how the pieces fit.

We will build **catalog-watch**: once an hour it checks that a JSON feed you
host still answers, and it adds a "Where to buy" link for one artist.

## 1. Make the folder

```
catalog-watch/
  plugin.toml
  plugin.py
```

## 2. Write the manifest

`plugin.toml` says who the plugin is and what it does:

```toml
[plugin]
name = "catalog-watch"
display_name = "Catalog Watch"
version = "1.0.0"
api_version = 1
entrypoint = "plugin:CatalogWatch"
capabilities = ["scheduler", "purchase_links"]
description = "Checks a catalog feed every hour and links to its shop."
author = "you"

[schedule]
interval_minutes = 60
run_on_load = true

[[settings]]
key = "catalog_url"
label = "Catalog URL"
help = "A JSON feed you host, e.g. https://example-catalog.test/feed.json"
```

`entrypoint` is `<file without .py>:<class name>`. `capabilities` lists
what the class implements; `scheduler` needs the `[schedule]` table.

## 3. Write the class

`plugin.py`:

```python
from droppedneedle_plugin import PluginPurchaseLink


class CatalogWatch:
    def __init__(self, ctx):
        self.ctx = ctx

    async def on_tick(self):
        url = (self.ctx.settings.get("catalog_url") or "").strip()
        if not url:
            return
        response = await self.ctx.http.head(url, timeout=30.0)
        if response.status_code >= 400:
            self.ctx.logger.warning("catalog answered HTTP %s", response.status_code)
        await self.ctx.state_set("last_status", str(response.status_code))

    async def purchase_links(self, artist, album, release_group_mbid):
        if artist != "Test Artist":
            return []
        return [PluginPurchaseLink(label="Example Shop", url="https://shop.example-catalog.test/")]
```

A few things to know:

- `ctx.settings` always holds the latest saved values. You do not need to
  restart anything when the admin changes them.
- Use `ctx.http` for web requests and `ctx.logger` for messages. Do not
  `print`: the helper redirects it to the log, but the logger is clearer.
- If a method raises, DroppedNeedle logs it with the plugin's name and carries
  on. Catch errors yourself when you want a better message.
- Each plugin runs as its own process. Anything you keep in memory is gone
  after a restart; keep what matters with `ctx.state_set` or in files under
  `ctx.data_dir`.

You do not install `droppedneedle_plugin` yourself; DroppedNeedle provides
it. To get editor completion, copy `sdk/python/droppedneedle_plugin.py` from
the DroppedNeedle repository next to your code (it is one file, standard
library only).

## 4. Try it without the server

The helper speaks the plugin protocol on stdin and stdout, so you can poke it
by hand from the plugin folder:

```bash
export PYTHONPATH=/path/to/DroppedNeedle/sdk/python:.
python3 -m droppedneedle_plugin plugin:CatalogWatch
```

Then paste one line at a time:

```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocol_version":1,"host":{"name":"me","version":"0"},"plugin":{"name":"catalog-watch","version":"1.0.0","api_version":1,"entrypoint":"plugin:CatalogWatch","capabilities":["scheduler","purchase_links"]},"settings":{},"plugin_dir":".","data_dir":"/tmp"}}
{"jsonrpc":"2.0","id":2,"method":"purchase_links.get","params":{"artist":"Test Artist","album":"x","release_group_mbid":""}}
```

Each answer comes back on one line. Press Ctrl-D to quit.

## 5. Try it in DroppedNeedle

Copy the folder into the plugins directory (`/app/plugins` under Docker):

```bash
cp -r catalog-watch /path/to/app/plugins/
```

Open **Settings > Plugins**. The plugin shows up disabled. Enable it, fill in
the catalog URL, and save. The card shows whether it is running; the server
log shows its messages, tagged with `plugin=catalog-watch`.

After you change the code, save the plugin in Settings again (or restart
DroppedNeedle) to start the new version.

## 6. Publish it

Push `plugin.toml` and `plugin.py` to the root of a public GitHub repository
and create a release, for example `v1.0.0`. Anyone can now install it by
pasting `https://github.com/you/catalog-watch` into Settings > Plugins; they
get your latest release, pinned to its commit.

When you change the plugin, bump `version` in `plugin.toml` and make a new
release. Admins press **Update** to move to it.

## Where to go next

- A download source: `examples/plugins/http-catalog` (files mode) or the
  `local-folder-client` and `local-folder-indexer` pair (folder mode).
- Events, routes and publishing: `examples/plugins/events-echo-toy`.
- Streaming tracks you do not have: `examples/plugins/stream-toy`.
- A plugin in another language: set `command` in `plugin.toml` and speak the
  protocol in [PLUGINS.md](../PLUGINS.md#the-protocol).
