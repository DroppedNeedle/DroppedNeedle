# Migration: slskd and SABnzbd as Plugins

## What changed (this release)

Soulseek (slskd) and Usenet (SABnzbd + Newznab) support became the first two
**built-in acquisition plugins** — `plugins/builtin/soulseek_slskd` and
`plugins/builtin/usenet_sabnzbd`. The refactor is a *wrap, not a rewrite*:

* **The client code did not move.** `repositories/slskd/*`,
  `repositories/sabnzbd/*` and `repositories/newznab/*` are unchanged; the
  plugins delegate to the exact same DI singletons
  (`get_slskd_repository()`, `get_slskd_indexer()`,
  `get_sabnzbd_download_client()`, `get_newznab_indexer()`), resolved lazily
  per call so settings saves still rebuild them.
* **The orchestrator now resolves its per-source providers through the plugin
  registry** (`PluginManager`), keyed by the same source ids as before —
  `soulseek` and `usenet`. Because the built-in plugins hand back the identical
  singleton objects, download behaviour is byte-identical (the test suite
  asserts object identity in `tests/plugins/test_registry_equivalence.py`).
* **Settings stayed where they were.** The plugins proxy the pre-existing
  preference sections (`download_client` for slskd,
  `download_clients.sabnzbd` for SABnzbd), so:
  * the existing admin routes (`/api/v1/download-client/*`,
    `/api/v1/download-clients/*`) keep working unchanged;
  * the new plugin settings API (`/api/v1/plugins/{id}/settings`) reads and
    writes the *same* values — the two surfaces cannot drift;
  * the plugin enable toggles ARE the existing slskd/SABnzbd enable flags.
* **Nothing about your config file changes.** No migration step runs; a
  pre-plugin config is a post-plugin config.

## What operators see today

* A new **Settings → Plugins** page listing Soulseek and Usenet (with a
  *built-in* badge) plus any third-party plugins you drop into
  `{ROOT_APP_DIR}/plugins/`.
* The existing slskd / SABnzbd / Indexers settings pages continue to work and
  stay the richer surface (mount diagnostics, category discovery, etc.). The
  plugin page is an alternative view over the same connection settings.
* Newznab **indexers remain their own admin surface** (`/api/v1/indexers`).
  They are user-supplied sources, not plugin configuration; the Usenet plugin's
  search fans out across whatever indexers you have configured, exactly as
  before.

## What "removal from core" will mean (future release)

The plan of record is to remove the slskd and SABnzbd client code from the
core distribution and ship them as standalone plugins (drop-in or pip-installed
via the `droppedneedle.plugins` entry-point group). When that happens:

* **Your configuration carries over.** The plugins read the same preference
  sections; connection settings, categories, mounts and enable flags survive.
* **Install becomes explicit.** A core-only install will acquire nothing until
  you install at least one acquisition plugin. Operators upgrading across the
  removal release will need to install the plugin(s) matching the sources they
  use — release notes will call this out loudly, and the upgrade path is
  expected to bundle-or-prompt rather than silently strip working sources.
* **The import pipeline stays core.** Identification, quality verification,
  library placement and registration are unaffected — plugins end at "the
  finished files are here".
* **In-flight downloads:** the download manifest persists the client-agnostic
  `TaskHandle`, so a resumed download only needs a plugin that answers the same
  source id (`soulseek`/`usenet`). Upgrading with an empty queue is still the
  recommended, boring path.
* **API stability:** the admin plugin API (`/api/v1/plugins*`) and the
  `plugins.base` contract (`api_version`) are the compatibility surface. The
  legacy per-client routes (`/api/v1/download-client*`) become part of the
  plugins' own surface and will follow the plugins out of core.

## For developers

The pre-plugin protocol seam (`repositories/protocols/download_client.py` and
`indexer.py`) is unchanged and remains the orchestrator's language. The plugin
layer sits on top of it:

```
DownloadOrchestrator
   └── source id ──> PluginManager registry ──> AcquisitionPlugin
                                                    ├── get_indexer()          -> IndexerProtocol
                                                    └── get_download_client()  -> DownloadClientProtocol
```

If you maintain code that imported `get_slskd_repository` and friends
directly: those providers still exist and still return the live singletons,
but new code should resolve per-source clients via
`core.dependencies.get_download_client_for_source(source)` or the manager, so
it keeps working when the built-ins move out of core.
