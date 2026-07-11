# DroppedNeedle Acquisition Plugins

DroppedNeedle acquires music through **acquisition plugins**. A plugin owns one
*source*: it searches its network for release candidates, enqueues downloads
with an external client, reports progress, and tells the core where the
finished files landed. Everything after that hand-off — identifying the audio,
matching it to MusicBrainz, moving it into the library, registering it — is the
**core import pipeline** and is deliberately *not* pluggable.

Soulseek (via [slskd](https://github.com/slskd/slskd)) and Usenet (via
SABnzbd + your Newznab indexers) are the first two plugins. They ship built-in
today, but they are ordinary plugins wired through the same registry a
third-party plugin uses — the long-term plan is to move them out of core
entirely and distribute them as standalone plugins (see
[MIGRATION.md](MIGRATION.md)).

## Why plugins?

* **DroppedNeedle ships no sources.** The user supplies every source the app
  can reach. Plugins make that boundary structural: a source is a unit of code
  the *operator* chooses to install and configure, not something the core
  bundles.
* **The acquisition surface is naturally pluggable.** The download orchestrator
  already speaks two small protocols — search (`IndexerProtocol`) and
  acquire/track/locate (`DownloadClientProtocol`) — and never imports a client
  directly. A plugin is simply a named, versioned, self-describing provider of
  those two capabilities plus a settings form.
* **Slimmer core.** When slskd and SABnzbd move out, operators who only use one
  network run none of the other's code.

## What a plugin is

A Python class extending `plugins.base.AcquisitionPlugin`, discovered from one
of three places:

1. **Built-in**: a subpackage of `backend/plugins/builtin/` (ships with the app).
2. **External dir**: a single `.py` module or a package directory dropped into
   `{ROOT_APP_DIR}/plugins/` (for a stock Docker install: `/app/plugins/`).
3. **Entry points**: any installed distribution exposing the
   `droppedneedle.plugins` entry-point group.

The `PluginManager` validates each plugin's declared `api_version` against the
server's `plugins.base.API_VERSION`, and **quarantines** anything that fails —
bad import, wrong api_version, duplicate id, exception during init. A
quarantined plugin shows up in *Settings → Plugins* with its error; the app
never crashes because of a plugin.

## Lifecycle

```
              +--------------------------------------------------------------+
              |                      app startup                             |
              +--------------------------------------------------------------+
                                        |
                                        v
   +--------------------- PluginManager.load() ---------------------------+
   |  discover: builtin pkgs  ->  {ROOT_APP_DIR}/plugins  ->  entry points |
   |  validate: id present? api_version == server? id unique?              |
   |     ok --> instantiate --> configure(stored settings) --> REGISTERED  |
   |     fail -----------------------------------------------> QUARANTINED |
   +-----------------------------------------------------------------------+
                                        |
                                        v
   +-----------------------------------------------------------------------+
   |                          serving requests                             |
   |                                                                       |
   |   orchestrator --- search(request) ----------> list[Candidate]        |
   |   orchestrator --- enqueue(request) ---------> TaskHandle             |
   |   orchestrator --- get_status(handle) --24/7-> DownloadTaskStatus     |
   |   orchestrator --- completed_path(handle) ---> [files on disk]        |
   |                        |                                              |
   |                        v                                              |
   |            core import pipeline (NOT pluggable):                      |
   |            verify -> identify -> move into library -> register        |
   |                                                                       |
   |   orchestrator --- cancel(handle) -----------> stop / clean up        |
   |                                                                       |
   |   admin UI ---- settings_schema() / configure() / test_connection()   |
   |   admin UI ---- enable / disable (persisted)                          |
   +-----------------------------------------------------------------------+
                                        |
                                        v
   +-----------------------------------------------------------------------+
   |                   app shutdown: shutdown() per plugin                  |
   +-----------------------------------------------------------------------+
```

## The exchange types are the orchestrator's own

The request/candidate/progress structs in `plugins.base` are not a new schema —
they are re-exports of the exact structs the download orchestrator already
exchanged with the slskd and SABnzbd clients (`repositories/protocols/`). That
is what makes wrapping the two built-in clients lossless, and it means a
candidate can take one of two archetypal shapes:

* **per-file, peer-to-peer** (`Candidate.soulseek`, a `DownloadSearchResult`):
  a specific file on a specific peer, with audio attributes.
* **release-shaped** (`Candidate.usenet`, a `UsenetRelease`): one downloadable
  archive/release with a title, a fetch URL, and a size.

Pick whichever fits your network. See [INTERFACE.md](INTERFACE.md) for every
field and [AUTHORING.md](AUTHORING.md) for the step-by-step guide.

## Admin API

All admin-gated, under `/api/v1/plugins`:

| Method | Path                          | What                                              |
| ------ | ----------------------------- | ------------------------------------------------- |
| GET    | `/plugins`                    | Registry: id/name/version/builtin/enabled/loaded/error |
| POST   | `/plugins/{id}/enable`        | Enable (persisted)                                |
| POST   | `/plugins/{id}/disable`       | Disable (persisted)                               |
| GET    | `/plugins/{id}/settings`      | Settings schema + current values (secrets masked) |
| PUT    | `/plugins/{id}/settings`      | Save values (masked secret = keep stored)         |
| POST   | `/plugins/{id}/test`          | Connection test (optional body values, unsaved)   |

The *Settings → Plugins* page in the web UI is built on these endpoints and
renders each plugin's form directly from its `settings_schema()`.

## Settings & secrets

Per-plugin settings persist in the app config under the `plugins.{id}`
namespace. Fields declared `secret` in the schema are Fernet-encrypted at rest,
masked (`plugin****`) in every API response, and preserved when the mask is
saved back — the same pattern every core settings section uses. The two
built-in plugins proxy their pre-existing settings sections instead, so the
legacy download-client routes and the plugin API always agree.

## Files in this folder

* [AUTHORING.md](AUTHORING.md) — write, test, package and distribute a plugin.
* [INTERFACE.md](INTERFACE.md) — full reference of the base class and every type.
* [MIGRATION.md](MIGRATION.md) — how slskd/SABnzbd became plugins and what their
  future removal from core means for operators.
* [example/mock_provider.py](example/mock_provider.py) — a complete, runnable
  example plugin (also exercised by the backend test suite).
