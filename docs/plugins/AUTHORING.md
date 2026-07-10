# Authoring an Acquisition Plugin

This guide takes you from empty file to a distributed plugin. It assumes you
are a competent Python developer; it does **not** assume you know this
codebase. Read [README.md](README.md) first for the scope boundary — your
plugin ends where the finished files sit on disk; the core import pipeline
takes it from there.

## 0. Ground rules

* **You are async code on the app's event loop.** Never block. Use
  `httpx.AsyncClient`, `asyncio.to_thread` for file I/O bursts, etc.
* **Never crash the host.** Raise freely inside your methods — the orchestrator
  treats a raising source as a failed source — but your module import,
  `__init__` and `configure` must be cheap and safe. A plugin that raises on
  load is quarantined (visible in Settings → Plugins with the error) and the
  app keeps running without it.
* **Ship no sources.** DroppedNeedle's legality boundary: the user supplies
  every endpoint. Do not hard-code indexer/tracker URLs or credentials;
  everything reachable must come from your settings schema.
* **Secrets go in `secret` fields.** The manager encrypts them at rest and
  masks them in API responses. Never log them.

## 1. Scaffold

Create `my_provider.py` (a single module is a complete plugin):

```python
from pathlib import Path
from typing import Any

from plugins.base import (
    AcquisitionPlugin, Candidate, DownloadTaskStatus, EnqueueRequest,
    SearchRequest, SettingsField, TaskHandle, TestResult, UsenetRelease,
)


class MyProviderPlugin(AcquisitionPlugin):
    id = "my_provider"          # stable, lowercase, [a-z0-9_]; doubles as the source id
    name = "My Provider"        # what admins see
    version = "1.0.0"           # your release version (semver recommended)
    # api_version defaults to the version of plugins.base you imported at dev
    # time; PIN IT explicitly when distributing (see §7).

    def settings_schema(self): ...
    def configure(self, settings): ...
    async def test_connection(self, settings=None): ...
    async def search(self, request): ...
    async def enqueue(self, request): ...
    async def get_status(self, handle): ...
    async def cancel(self, handle): ...
    async def completed_path(self, handle): ...
```

Drop it into `{ROOT_APP_DIR}/plugins/` and restart. It appears on
*Settings → Plugins*. A package works exactly the same way: a directory with an
`__init__.py` that defines (or re-exports) your plugin class.

The fastest way to see all of this working end-to-end is the shipped example:
[example/mock_provider.py](example/mock_provider.py).

## 2. Settings schema

Describe your configuration as typed field descriptors; the admin UI renders
the form, and the manager persists/encrypts values for you:

```python
def settings_schema(self):
    return [
        SettingsField(key="url", type="str", label="Server URL",
                      help="e.g. http://myclient:1234", required=True, default=""),
        SettingsField(key="api_key", type="secret", label="API key", default=""),
        SettingsField(key="max_results", type="int", label="Max results", default=25),
        SettingsField(key="prefer_lossless", type="bool", label="Prefer lossless", default=True),
        SettingsField(key="mode", type="select", label="Mode", default="fast",
                      options=[SelectOption(value="fast", label="Fast"),
                               SelectOption(value="thorough", label="Thorough")]),
    ]
```

Field types: `str`, `int`, `bool`, `select` (with `options`), `secret`
(rendered as a password input with show/hide; encrypted at rest; masked as
`plugin****` on read — when you receive the mask back you never will: the
manager resolves it to the stored plaintext before calling you).

`configure(settings)` is called once after load (with persisted values, secrets
decrypted, missing keys filled from your declared defaults) and again after
every save. Store the values on `self`; do **not** open connections here.

## 3. `test_connection`

```python
async def test_connection(self, settings=None):
    values = settings or self._current_values
    try:
        version = await self._ping(values["url"], values["api_key"])
    except Exception as exc:
        return TestResult(ok=False, message=f"unreachable: {exc}")
    return TestResult(ok=True, message=f"connected", version=version)
```

`settings` is `None` when the admin tests the *saved* config, and a full values
mapping when they hit **Test** with unsaved form edits — test what you're
given, persist nothing. Return `TestResult`; never raise (a raise is caught and
reported as a generic failure, which is a worse message than yours).

## 4. Implementing the capabilities

### `search(request) -> list[Candidate]`

`SearchRequest.kind` is `"album"` or `"track"`; honour `request.timeout`
(seconds). Return candidates *tagged with your plugin id* in `source`, in one
of the two archetypal shapes:

```python
async def search(self, request):
    rows = await self._api_search(f"{request.artist_name} {request.album_title}")
    return [
        Candidate(source=self.id, usenet=UsenetRelease(
            indexer_id=self.id, indexer_name=self.name,
            guid=row["id"], title=row["title"],
            nzb_url=row["download_url"], size_bytes=row["bytes"],
        ))
        for row in rows
    ]
```

Use the release shape (`usenet=UsenetRelease(...)`) for "one downloadable
archive per result" networks; use the per-file shape
(`soulseek=DownloadSearchResult(...)`) for peer-to-peer, per-file networks.
Return `[]` for "nothing found" — that is a result, not an error.

### `enqueue(request) -> TaskHandle`

`EnqueueRequest` carries `task_id` plus the fields of the candidate that was
picked (`files` for per-file sources; `nzb_url`/`job_name`/`category` for
release sources). Start the download in your client and return a `TaskHandle`
holding whatever *you* need to find the job again — the core persists the
handle verbatim and hands it back to every later call. `job_name`
(`droppedneedle-{task_id}`) is the crash-safe correlation key: prefer honouring
it so a job can be found even if the app dies before your client returns an id.

### `get_status(handle) -> DownloadTaskStatus`

Polled every couple of seconds while the task runs. Fill in what you know:
`status` (`"queued" | "downloading" | "completed" | "failed"`), byte/file
counters, `progress_percent`, `error`. Two fields materially affect the
watchdogs: `has_active_transfer` (distinguishes an actively-moving transfer
from one parked in a remote queue) and `succeeded_filenames` (lets the core
import the finished subset of a partially-failed job).

### `completed_path(handle) -> list[Path]`

The hand-off. Return the on-disk paths of the finished job's audio files, in a
location the DroppedNeedle process can read (for containerised setups that
means a shared mount — document what your users must mount). The core moves
the files; you should not delete them yourself on success.

### `cancel(handle) -> bool`

Cancel the job if still running AND/OR clean up its transfer records. Called
both for user cancellation and post-import cleanup. Idempotent; `True` on
success.

### Optional overrides

* `get_file_path(handle, remote_filename, size)` — resolve one remote filename
  to its local path, if your client's layout allows it.
* `diagnose_downloads_mount()` — cross-check your client's completed downloads
  against the mount, to catch misconfigured paths proactively.
* `health_check()` — liveness (defaults to deriving from `test_connection`).
* `is_configured()` — "has enough config to try at all" (defaults to `True`).
* `shutdown()` — close clients on app exit.

## 5. Testing locally

* **Unit-test against the ABC directly** — everything is plain Python:

  ```python
  plugin = MyProviderPlugin()
  plugin.configure({"url": "http://x", "api_key": "k"})
  candidates = await plugin.search(SearchRequest(kind="album",
      artist_name="Artist", album_title="Album"))
  ```

* **Load-test through the manager** the way the app does (this is exactly what
  `backend/tests/plugins/test_mock_provider_external.py` does with the example
  plugin — copy that file as your starting point):

  ```python
  from plugins.manager import PluginManager
  manager = PluginManager(preferences=fake_prefs, external_dir=dir_with_your_py)
  assert manager.get_record("my_provider").loaded
  ```

* **Live**: copy your file into `{ROOT_APP_DIR}/plugins/`, restart, open
  *Settings → Plugins*, fill the form, hit **Test**. Discovery/quarantine
  problems are logged under the `plugins.*` log events.

## 6. Packaging & distribution

Two options:

1. **Drop-in** (simplest): distribute the `.py` file (or package folder);
   operators copy it into `{ROOT_APP_DIR}/plugins/`.
2. **pip package with an entry point** (versioned installs, dependencies):

   ```toml
   # pyproject.toml of your distribution
   [project]
   name = "droppedneedle-my-provider"
   version = "1.0.0"

   [project.entry-points."droppedneedle.plugins"]
   my_provider = "droppedneedle_my_provider:MyProviderPlugin"
   ```

   Installed into the app's environment, the manager discovers it via the
   `droppedneedle.plugins` entry-point group — no file copying.

If your module fails to import because of a missing dependency, you're
quarantined with that ImportError; prefer the pip route when you have deps.

## 7. Versioning & `api_version` policy

* `version` is **yours** — release however you like (semver recommended).
* `api_version` is the **contract version** of `plugins.base` you wrote
  against. The server refuses (quarantines) any plugin whose `api_version`
  differs from its own `plugins.base.API_VERSION` — there is no silent
  best-effort compatibility. The policy:
  * The API version bumps **only on breaking changes** to the base class,
    the exchange structs, or the manager's calling conventions.
  * Purely additive changes (new optional fields with defaults, new optional
    hooks) do **not** bump it.
  * When distributing, pin it explicitly (`api_version = 1`) so a future
    server rejects you with a clear message instead of failing mid-download.

## 8. Error-handling rules

| Where                    | Rule                                                              |
| ------------------------ | ----------------------------------------------------------------- |
| module import / `__init__` / `configure` | Must not raise for bad *values* — you'll be quarantined. Validate lazily. |
| `test_connection`        | Never raise; return `TestResult(ok=False, message=...)`.          |
| `search`                 | `[]` means "nothing found". Raise (or let httpx raise) only for real faults — the orchestrator fails over to the next source. |
| `enqueue`                | Raise on failure — the orchestrator counts it as a failed attempt and retries/fails over per its policy. |
| `get_status`             | Prefer returning `status="failed"` + `error` for job-level failures; raise only for transport faults. |
| `cancel` / `shutdown`    | Best-effort; swallow your own cleanup errors.                     |
| messages                 | Error strings you return may be shown to users verbatim: be specific, actionable, and never include secrets. |
