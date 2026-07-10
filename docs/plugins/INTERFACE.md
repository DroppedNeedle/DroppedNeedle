# Plugin Interface Reference

Authoritative reference for `plugins.base` (the plugin contract) and
`plugins.manager` (the host side). Everything a plugin imports comes from
`plugins.base`; the exchange structs are re-exports of the orchestrator's own
protocol types (`repositories/protocols/`), so they are identical to what the
core exchanges with the built-in clients.

```python
from plugins.base import (
    API_VERSION, PLUGIN_SECRET_MASK,
    AcquisitionPlugin,
    SettingsField, SelectOption, TestResult, SearchRequest,
    Candidate, DownloadSearchResult, UsenetRelease,
    EnqueueRequest, DownloadFileRef, TaskHandle,
    DownloadTaskStatus, MountDiagnosis,
    PluginIndexerAdapter, PluginDownloadClientAdapter,
)
```

## Module constants

| Name                  | Type  | Meaning                                                       |
| --------------------- | ----- | ------------------------------------------------------------- |
| `API_VERSION`         | `int` | The plugin-contract version this server implements (currently `1`). |
| `PLUGIN_SECRET_MASK`  | `str` | `"plugin****"` — what a stored non-empty `secret` value reads as in API responses. |

---

## class `AcquisitionPlugin` (ABC)

### Identity (class attributes — override all three)

```python
id: ClassVar[str] = ""            # stable machine id; doubles as the source id ('soulseek', 'usenet', ...)
name: ClassVar[str] = ""          # human-readable display name
version: ClassVar[str] = "0.0.0"  # the plugin's own release version
api_version: ClassVar[int] = API_VERSION  # contract version; mismatch => quarantined
```

### Lifecycle

```python
def settings_schema(self) -> list[SettingsField]            # abstract
def configure(self, settings: dict[str, Any]) -> None       # abstract
async def test_connection(self, settings: dict | None = None) -> TestResult  # abstract
async def shutdown(self) -> None                            # default: no-op
```

* `configure` — called after load with persisted values (secrets decrypted,
  missing keys defaulted from the schema) and again after every settings save.
  Must be cheap, no I/O, and must not raise for bad values.
* `test_connection(None)` tests the saved config; a mapping tests those values
  *without persisting them*.

### Capabilities

```python
def is_configured(self) -> bool                                   # default: True
async def health_check(self) -> ServiceStatus                     # default: derived from test_connection
async def search(self, request: SearchRequest) -> list[Candidate] # abstract
async def enqueue(self, request: EnqueueRequest) -> TaskHandle    # abstract
async def get_status(self, handle: TaskHandle) -> DownloadTaskStatus  # abstract
async def cancel(self, handle: TaskHandle) -> bool                # abstract
async def completed_path(self, handle: TaskHandle) -> list[Path]  # abstract
async def get_file_path(self, handle, remote_filename: str, size: int | None = None) -> Path | None  # default: None
async def diagnose_downloads_mount(self) -> MountDiagnosis        # default: supported=False
```

### Orchestrator integration (rarely overridden)

```python
def get_indexer(self)          # -> IndexerProtocol; default: PluginIndexerAdapter(self)
def get_download_client(self)  # -> DownloadClientProtocol; default: PluginDownloadClientAdapter(self)
```

The download orchestrator consumes plugins through these two protocol views.
The default adapters wrap your `search`/`enqueue`/`get_status`/`cancel`/
`completed_path` methods; the built-in plugins override them to return the
pre-existing client singletons.

### Built-in persistence hooks (third-party plugins never override)

```python
def enabled_override(self) -> bool | None   # None => manager persists the enable flag
def apply_enabled(self, enabled: bool) -> bool   # True => plugin persisted it itself
def settings_values(self) -> dict | None    # None => manager owns settings storage
def apply_settings(self, values: dict) -> bool   # True => plugin persisted them itself
```

---

## Settings types

### `SettingsField`

| Field      | Type                                    | Notes                                          |
| ---------- | --------------------------------------- | ---------------------------------------------- |
| `key`      | `str`                                   | Settings-dict key.                             |
| `type`     | `"str" \| "int" \| "bool" \| "select" \| "secret"` | Drives the rendered control + secret handling. |
| `label`    | `str`                                   | Form label.                                    |
| `help`     | `str`                                   | Helper text under the control.                 |
| `default`  | `str \| int \| float \| bool \| None`   | Used when no value is stored.                  |
| `required` | `bool`                                  | UI hint (marked in the form).                  |
| `options`  | `list[SelectOption]`                    | For `select` only.                             |

### `SelectOption`

| Field   | Type         | Notes                       |
| ------- | ------------ | --------------------------- |
| `value` | `str \| int` | What gets stored.           |
| `label` | `str`        | What the admin sees.        |

### `TestResult`

| Field     | Type          | Notes                                          |
| --------- | ------------- | ---------------------------------------------- |
| `ok`      | `bool`        | Overall verdict.                               |
| `message` | `str`         | Shown verbatim in the admin UI.                |
| `version` | `str \| None` | Backing service version, when known.           |

---

## Search types

### `SearchRequest`

| Field              | Type                      | Notes                                    |
| ------------------ | ------------------------- | ---------------------------------------- |
| `kind`             | `"album" \| "track"`      | Selects which of the fields below apply. |
| `artist_name`      | `str`                     | Always set.                              |
| `album_title`      | `str \| None`             | Album searches; optional context on track searches. |
| `track_title`      | `str \| None`             | Track searches.                          |
| `year`             | `int \| None`             | Album searches.                          |
| `track_count`      | `int \| None`             | Album searches (expected tracklist size).|
| `duration_seconds` | `int \| None`             | Track searches.                          |
| `timeout`          | `float` (default `30.0`)  | Soft budget in seconds — honour it.      |

### `Candidate` (= `IndexerResult`)

A tagged union: exactly one of `soulseek` / `usenet` is set.

| Field      | Type                            | Notes                                     |
| ---------- | ------------------------------- | ----------------------------------------- |
| `source`   | `str`                           | Your plugin `id`.                         |
| `soulseek` | `DownloadSearchResult \| None`  | Per-file, peer-to-peer archetype.         |
| `usenet`   | `UsenetRelease \| None`         | Release/archive archetype.                |

### `DownloadSearchResult` (per-file archetype)

| Field              | Type            | Notes                                  |
| ------------------ | --------------- | -------------------------------------- |
| `username`         | `str`           | Peer/owner of the file.                |
| `filename`         | `str`           | Full remote path of the file.          |
| `parent_directory` | `str`           | Remote folder (release grouping key).  |
| `size`             | `int`           | Bytes.                                 |
| `extension`        | `str`           | e.g. `"flac"`.                         |
| `bitrate`          | `int \| None`   | kbps.                                  |
| `bit_depth`        | `int \| None`   |                                        |
| `sample_rate`      | `int \| None`   | Hz.                                    |
| `duration`         | `float \| None` | Seconds.                               |
| `has_free_slot`    | `bool`          | Peer can start immediately.            |
| `upload_speed`     | `int`           | Peer's advertised speed.               |

### `UsenetRelease` (release archetype)

| Field          | Type            | Notes                                             |
| -------------- | --------------- | ------------------------------------------------- |
| `indexer_id`   | `str`           | Your plugin id (or per-indexer id if you fan out).|
| `indexer_name` | `str`           | Display name.                                     |
| `guid`         | `str`           | Stable identity of the release at the indexer.    |
| `title`        | `str`           | Release title (scored against artist/album).      |
| `nzb_url`      | `str`           | Fetch URL handed back to you in `EnqueueRequest`. |
| `size_bytes`   | `int`           |                                                   |
| `category_ids` | `list[int]`     | Newznab-style categories (3000 = audio).          |
| `grabs`        | `int \| None`   | Optional popularity signal.                       |
| `files`        | `int \| None`   | Optional file count.                              |
| `usenet_date`  | `float \| None` | Unix timestamp of the post.                       |
| `password`     | `int`           | Non-zero = password-protected (rejected pre-grab).|

---

## Acquire/track/locate types

### `EnqueueRequest`

| Field             | Type                     | Notes                                                        |
| ----------------- | ------------------------ | ------------------------------------------------------------ |
| `task_id`         | `str`                    | DroppedNeedle's download-task id.                             |
| `source`          | `str`                    | Your plugin id.                                               |
| `files`           | `list[DownloadFileRef]`  | Per-file sources: the files to fetch.                         |
| `nzb_url`         | `str \| None`            | Release sources: the candidate's fetch URL.                   |
| `job_name`        | `str \| None`            | `droppedneedle-{task_id}` — crash-safe correlation key; honour it. |
| `category`        | `str \| None`            | Client-side category, from plugin settings.                   |
| `priority`        | `int \| None`            |                                                              |
| `post_processing` | `int \| None`            |                                                              |

`DownloadFileRef`: `username: str`, `filename: str`, `size: int`.

### `TaskHandle`

Persisted verbatim by the core and handed back to every later call — put
whatever you need in it to find the job again.

| Field       | Type        | Notes                                             |
| ----------- | ----------- | ------------------------------------------------- |
| `source`    | `str`       | Your plugin id.                                   |
| `username`  | `str`       | Per-file sources: the peer.                       |
| `filenames` | `list[str]` | Per-file sources: the remote filenames.           |
| `job_name`  | `str`       | Pre-enqueue correlation key (survives crashes).   |
| `nzo_id`    | `str`       | Client-assigned job id, once known.               |

### `DownloadTaskStatus`

| Field                 | Type          | Notes                                                            |
| --------------------- | ------------- | ---------------------------------------------------------------- |
| `task_id`             | `str`         | May be left `""` — the orchestrator correlates by handle.         |
| `status`              | `str`         | `"queued" \| "downloading" \| "completed" \| "failed"`.           |
| `files_total`         | `int`         |                                                                  |
| `files_completed`     | `int`         |                                                                  |
| `files_failed`        | `int`         |                                                                  |
| `bytes_total`         | `int`         |                                                                  |
| `bytes_downloaded`    | `int`         |                                                                  |
| `progress_percent`    | `float`       | 0–100.                                                           |
| `error`               | `str \| None` | Job-level failure message (may be shown to users).               |
| `succeeded_filenames` | `list[str]`   | Finished subset — lets the core import a partially-failed job.    |
| `has_active_transfer` | `bool`        | Actively moving bytes vs parked in a remote queue (stall watchdog). |
| `matched_transfers`   | `int`         | 0 = the enqueue produced no transfer record (fail over fast).     |

### `MountDiagnosis`

| Field                   | Type          | Notes                                              |
| ----------------------- | ------------- | -------------------------------------------------- |
| `supported`             | `bool`        | `False` = the plugin can't introspect (default).   |
| `completed_downloads`   | `int`         | Finished jobs known to the client.                 |
| `mount_has_files`       | `bool`        | The configured mount contains anything at all.     |
| `resolvable_downloads`  | `int`         | Of a sample, how many resolve under the mount.     |
| `sampled_downloads`     | `int`         | Sample size.                                       |
| `client_downloads_dir`  | `str \| None` | The client's own downloads dir, in its namespace.  |

---

## Adapters

`PluginIndexerAdapter(plugin)` and `PluginDownloadClientAdapter(plugin)` are
signature-exact implementations of the core's `IndexerProtocol` and
`DownloadClientProtocol` over a plugin instance (contract-tested in
`backend/tests/plugins/test_plugin_base.py`). You get them for free via
`get_indexer()` / `get_download_client()`.

---

## class `PluginManager` (host side — you don't implement this)

| Member | Purpose |
| ------ | ------- |
| `load()` | Discover + register everything; idempotent; never raises. |
| `list_records() -> list[PluginRecord]` | Registry, built-ins first. |
| `get_record(id) -> PluginRecord \| None` | One record, loaded or quarantined. |
| `get_plugin(id) -> AcquisitionPlugin \| None` | Loaded instance (regardless of the enable flag). |
| `is_enabled(id)` / `set_enabled(id, bool)` | Enable flag, persisted. |
| `get_settings_schema(id)` | The plugin's schema (empty when quarantined). |
| `get_settings_values(id, raw=False)` | Values; `raw=False` masks secrets. |
| `save_settings(id, values)` | Resolve masks, persist (encrypting secrets), re-`configure`. |
| `test(id, values=None)` | Run `test_connection`, exceptions turned into `TestResult(ok=False)`. |
| `shutdown_all()` | Best-effort `shutdown()` for every loaded plugin. |

`PluginRecord`: `id`, `name`, `version`, `source` (`"builtin" | "external" |
"entry_point"`), `plugin` (instance or `None`), `error` (`str | None`), plus
derived `builtin` and `loaded`.

Discovery constants: external dir `{ROOT_APP_DIR}/plugins`; entry-point group
`droppedneedle.plugins`.
