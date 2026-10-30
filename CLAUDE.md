# DroppedNeedle v3 - working contract

Rules for all work on the `v3` branch. `main` holds v2 and is frozen.

DroppedNeedle is a self-hosted, multi-user music request and discovery app:
one Rust server (Axum, tokio, SQLite) that manages a library, acquires music
(slskd, SABnzbd with Newznab, Lidarr import, plugins) and emulates
the Subsonic and Jellyfin APIs, plus the SvelteKit web UI it serves.

Maintainers may keep a gitignored local file that adds to this one.

## Where things are

- `server/` is a Cargo workspace. Each area is a crate in `server/<name>`,
  from the bottom: `kernel`, `providers`, `auth`, `library`, `plugins`,
  `media`, `catalog`, `acquire`, `compat`. Then the server crate in
  `server/src` (router, startup, state, admin and settings routes, the API
  document, the web UI), the migration tool in `server/tool` on top, and
  the mock servers in `server/testkit`. Integration tests are in
  `server/tests/<binary>/`, sharing `server/tests/common`. `frontend/` is
  the SvelteKit SPA, `sdk/python/` and `examples/plugins/` the plugin
  helper and examples.
- User-facing docs: `README.md`, `PLUGINS.md`, `docs/`, `CONTRIBUTING.md`.

## Ground rules

- **Total rewrite.** Breaking changes are fine. No back-compat shims.
- **Never remove working functionality** without the maintainer saying so.
- **Finish what's in scope.** Everything in a change is done, reviewed and
  improved. Outside findings go in a change of their own, never widen this
  one.
- **Tests outrank review.** Executable specs, then golden tests, then static
  analysis, then peer review.
- **Simplicity first.** The least code that solves the problem. A setting
  nothing reads, a route nothing calls and an option nothing honours are
  removed, not implemented, once the maintainer has seen the list.
- **It just works.** The app decides and acts by itself. Asking the user
  (a prompt, a card, a "try again") is a last resort, kept for when the app
  cannot tell and a wrong guess would cost data. What it did by itself is
  visible and can be undone, never silent.
- **One best default, with an override.** Automatic choices that could be
  wrong get a user-reachable override.

In every change:

- Architecture, removing a feature and risky deletions are the maintainer's
  call. Decide the rest by these rules and say what you decided.
- User-facing changes update the user-facing docs in the same branch.
- Report honestly: what was done, what was checked, what's still open.

## Commits

- **House style.** Subject `type(scope): imperative subject`, 72 characters at
  most. Types: feat, fix, perf, refactor, test, docs, chore, style, ci. Body:
  2-6 plain lines on what changed and why. One piece of work per commit
  series, nothing unrelated in a commit.
- **Before every commit and push**, read the diff and message as a stranger
  browsing the public repo would, and fix what you find. The Writing rules
  apply. No leftovers: plans, summaries, reports or notes files,
  narration comments, commented-out code, debug prints.
  Nothing private: review notes, local config, credentials, test output.
- Stage files by name. Never `git add -A` or `git add -f`: they sweep in stray
  and gitignored private files.

## Commands

Run from the repo root. `make help` lists every target.

- `make check` (the local gates), `make server-test`, `make contract-write`,
  `make frontend-check` (svelte-check, the only frontend typecheck),
  `make frontend-test-client` (browser suite), and `make image` and
  `make rebuild`, which build `droppedneedle:local` and recreate the dev
  container.
- Focused run: `cd server && cargo test --test <binary> <module>`. The
  binaries are `library`, `acquire`, `compat`, `catalog`, `media` and
  `server` (everything else); the tool's are
  `cargo test -p droppedneedle-tool --test tool <module>`.
- A crate's unit tests: `cargo test -p droppedneedle-<crate>`;
  `cargo test --lib` runs only the server crate's.
- CI is path-filtered: `v3-ci.yml` runs on code changes and adds
  `cargo audit`, `cargo deny check` and a container boot test,
  `frontend-ci.yml` runs on `frontend/` changes. Green locally never excuses
  red CI.
- Clippy rejects `unwrap`, `expect` and `panic!` outside tests. Eslint rejects
  raw `fetch` and hand-built `/api/v3` strings.
- `make contract-check` and `make contract-write` take the document from the
  test binary; CI checks it against the shipping build.
- pnpm, never npm. Format only the files you touched.

## Backend (Rust)

**Shape and API**
- Thin handlers call services, which reach outside through trait ports and
  adapters. No layer-skips. Services return typed errors. Only handlers map
  them to HTTP status.
- Areas are given one `Db` and hold no other database handle; `WriteLane` is
  private to `kernel/src/db/`.
- No global singletons. Long-lived things are built once, by one builder that
  production and tests share, and passed in through constructors. Nothing is
  attached after construction. The kernel's shared parts (config, `Db`,
  settings store, crypto, HTTP client factory, ids, clock, event hub,
  supervisor) form one `Platform` (`kernel/src/platform.rs`), opened once by
  `bootstrap::open_platform` and in tests by `TestPlatform` over the same
  constructor. Areas borrow `&Platform` at build time and keep only what
  they use; `source_gates.rs` refuses a stored `Platform`, and
  `wiring_gate.rs` refuses a late-binding slot (a `OnceLock`/`OnceCell`
  port, a `*Slot` type, a setter that takes a port after construction).
- Only things outside the process sit behind a trait: HTTP services, the
  clock, ffmpeg. Stores and services are concrete types. A trait with one
  implementation needs a reason beside it; cutting a dependency between two
  areas is one.
- Wall time comes from the one kernel clock (`kernel/src/clock.rs`, passed
  in), never `SystemTime::now()` in an area; tests use its `ManualClock`.
  Files that still read the system clock are listed with a reason in
  `server/clock-outside-kernel.txt`; `source_gates.rs` fails on any other
  read and on a listed file that no longer reads it.
- Every route is registered through the mount API in
  `server/kernel/src/mount.rs`: documented routes with `routes!` over their
  `#[utoipa::path]`, one path per call, in a posture group (public, user,
  curator, admin, own credential); compat routes through the compat mount
  (`CompatRoutes::at`). The top of `server/src/app.rs` describes the
  pre-routing service and the layer order. `route_gate.rs` and
  `route_walk.rs` enforce it.
- Areas are crates, from the bottom: kernel, providers, auth, library,
  plugins, media, catalog, acquire, compat, then the server crate and the
  tool. A crate uses only crates below it, and Cargo refuses an upward
  import. A new `droppedneedle-*` dependency in a crate's `Cargo.toml` is
  an architecture change and the maintainer's call.
- `/api/v3` is snake_case with no duty to v2 shapes. Types are generated,
  never hand-copied: after a route or type change run `make contract-write`,
  commit both generated files and update the frontend query hooks in the same
  change. A field with a closed set of values is a Rust enum with `ToSchema`
  in the document, never a plain string; operation ids are unique
  (`contract.rs` checks it).
- Third-party shapes are serde structs in the provider's models file, never
  walked as raw JSON. Unknown fields are tolerated. Identity fields are
  required: a missing one fails decoding, never defaults to something that
  looks real. Never guess a shape: probe it, note beside the code what you
  verified against which version, and re-verify before removing a quirk
  handler.

**Errors**
- `None` or empty means absence, never failure.
- Error JSON is `{"error": {code, message, details}}` with SCREAMING_SNAKE
  codes. 5xx bodies are fixed strings plus an error id, never error text,
  hosts or paths.
- Never swallow an error in a loop without a log line.
- No handler's message survives at 5xx. Handlers return `ApiError`
  (`server/kernel/src/error.rs`), and the boundary in
  `server/src/middleware.rs` is the only place a 5xx body is written and
  logged. A 5xx answers the fixed internal error with the request id,
  except the closed list in `Unavailable` (server busy, media server down,
  GitHub refusing, limiting or failing a plugin install), each with a
  fixed sentence. A compat 5xx gets its protocol's failure shape. A message
  a user must read needs an honest 4xx or a new `Unavailable` row.
- Request bodies and query strings are read through `ValidJson` and
  `ValidQuery` from `server/kernel/src/extract.rs`, never an area's own
  copy.
- A paged list answers the kernel's `Page<T>` (`server/kernel/src/page.rs`):
  an opaque cursor read through `PageRequest` (one that does not decode
  is a 400), a limit clamped to the list's maximum, `next_cursor` null on
  the last page, and a `total` only where the list counts rows. The
  library's lists use it; other areas move to it when they are next
  touched.
- `server/tests/server/http_gate.rs` holds the one way in for errors and
  extractors; an allowlist entry needs a reason beside it and is the
  maintainer's call.

**Auth**
- `/api/v3` is deny-by-default: every route is mounted in one posture group
  (public, user, curator, admin or its own credential), and the session
  gate covers the user, curator and admin groups. The public and
  own-credential operations are pinned by name in `auth_e2e.rs`, so adding
  one is a reviewed edit there. The API document's `security` states each
  operation's posture, `auth_e2e` derives the role matrix from it, and
  `auth_ownership.rs` holds the two-user table. Roles: `user`, `trusted`,
  `admin`. Trusted and admin auto-approve and skip quotas.
- Browsers use session cookies, devices and scripts bearer tokens. The design
  is settled, at the top of `server/auth/src/session/mod.rs`.
- One principal: the session gate reads the token and its user in one query
  and puts one `droppedneedle_auth::Principal` in the request; nothing else
  puts one there, and nothing re-reads the user row per request. A handler
  says who may call it by taking `User`, `Curator` (admin or trusted) or
  `Admin`; no other extractor, layer or role enum says who is calling. A
  service that needs an admin takes an `&Admin`, which only an admin
  principal from a request can make. A role check written as an `if` is a
  bug unless it depends on the request body or a setting; a check that a
  resource belongs to the caller stays in the service
  (`Principal::is_admin` for owner-or-admin). Auto-approve and quota skips
  read `principal.role()` and are not access checks. Work done for a user
  with no request builds its principal only through
  `Principal::acting_for`, in the service doing the work, from the stored
  row; it never proves a role and never signs a request in.
  `server/tests/server/caller_shapes.rs` enforces this.
- Compat APIs and the wrapped routes have their own credentials outside that
  gate. Never assume `/api` auth covers them.
- A locked-out admin recovers from the host: stop the server, run
  `droppedneedle-tool recovery-code <user>`, start it again. No route is the
  only way back in.

**Database**
- One SQLite file, WAL mode, local filesystem, opened only in
  `server/kernel/src/db/`; outside it only where
  `server/tests/server/source_gates.rs` lists: the boot stamp probe, backup
  checks, the offline tool and the test template. The same gate keeps
  hand-run transactions, pragmas, connection hooks, ATTACH/VACUUM and leaked
  values inside `db/`, and `Lane::Bulk` inside `db/` and the scan, whose
  writes may wait out a WAL hold. A journal write before a file moves is a
  durable write (`Durable`), so a publish step cannot place a file ahead of
  its row. One server process per data directory, always: the writer
  lane only orders writes within one process, so boot takes an exclusive
  lock (`DataLock::server`) and a second server on the folder refuses to
  start.
- One exception: provider answers are cached in `metadata-cache.db` (in
  `CACHE_DIR`, or beside `library.db` when that folder is refused), a
  throwaway file opened in `kernel/src/db/cache/` under a lock of its own,
  with its own writer and readers that only the providers' cache store
  holds. It has no migrations (a file of another schema or release, or one
  that does not read, is deleted and rebuilt at start), is never backed up
  or exported, keeps its rows under 512 MB, and holds nothing that cannot be
  fetched again. A cache key starts with its source and carries every value
  that changes the answer, so no settings save clears it, and a lookup that
  met any failure stores nothing.
- All writes go through the single writer lane, one transaction per atomic
  change. No network calls inside one: writes run one at a time, so a slow one
  stalls the rest.
- A write closure fails with the caller's own error type
  (`E: From<DbError>`); an outcome a caller branches on is a variant of that
  type, never the text of a database error. A chunked write commits chunk by
  chunk: any prefix may be all that lands, so use it only where each chunk is
  a complete change. Its error says how many rows committed, and a caller
  resumes from there.
- SQL text lives in the area's `sql.rs`, declared through `sql!`
  (statements, fragments, and families whose builders take only
  `sql_choice!`, `bool` or `ListLen` values);
  `every_registered_statement_prepares` prepares each against the schema.
  A crate with a `sql.rs` lists it and has a row in that test. An area
  that moves its SQL there deletes its line from
  `server/sql-outside-registry.txt`, and a gate then keeps raw SQL out.
  A `sql!` block whose statements read a v2 database starts with
  `schema = V2;` and is prepared against the v2 fixture.
- Migrations are numbered and forward-only. Never edit or renumber a shipped
  one. Each new one ships an upgrade test. They land on `v3` in number
  order: the runner skips any migration below the database's version, so a
  later number landing first loses the earlier one on every database that
  boots in between.
- Back up with the SQLite backup API, never a copy of the live file, which can
  miss what is still in the WAL.
- New tables follow the schema conventions: a point in time is integer
  milliseconds in a column ending `_at`; a table is `<prefix>_<plural noun>`
  with its owning area's prefix; every table is `STRICT`; a foreign key
  cascades for owned data, sets null for "who did this" and restricts for
  what must outlive its parent, and is indexed (`every_foreign_key_is_indexed`
  checks it); a state column is backed by a Rust enum with a CHECK listing
  its values.
- Every table has one owning area, listed in `server/table-owners.txt` and
  checked by a test. Only the owner writes and migrates it.
- A read model derived from other tables (a summary table or a column) is
  kept by triggers in the migration that creates it, never by writer code.
  A rule it recomputes lives in one refresh that the triggers and the
  backfill share. A test compares it with the value worked out from the
  source rows after each kind of write. Trigger guards compare OLD and NEW
  with `IS NOT` and fire only when a value the rule reads changed; a guard
  may also skip a refresh that can only add a reason the kept row already
  has. The
  tables it follows are never written with REPLACE, which removes rows
  without firing their delete triggers.

**Background work and outbound HTTP**
- Loops are cancellable, back off with jitter and re-read settings each
  iteration. A spawned task nobody awaits logs its own failure.
- Every loop runs on the one loop supervisor (`kernel/src/jobs/supervisor.rs`)
  and appears in the admin jobs view. A spawn outside it is listed, with
  its reason, in `server/tests/server/source_gates.rs`.
- A worker that hears a busy database puts its job back and tries again
  later. Busy never spends an attempt or marks work failed.
- Durable operations are SQLite state machines with idempotency keys and
  startup recovery. A restart neither forgets work nor repeats destructive
  work.
- A queue or outbox is a job kind on the kernel's jobs table
  (`kernel/src/jobs/queue/`), claimed and recovered by its one module. A new
  queue never writes its own lease code. A job runs at least once: its
  handler is safe to repeat.
- HTTP clients come only from the factory in `server/kernel/src/http_client.rs`.
- One way out to the internet. Every call to a service the user does not
  run (metadata providers, Spotify, plex.tv, HIBP, OIDC discovery, GitHub,
  Newznab indexers, anything new) goes through its client in
  `server/providers/`, whose executor owns pacing, retries and
  `Retry-After`, with a row in `rates.rs`. No area builds its own request
  to the internet. Services the user runs keep one client each, built from
  the factory, with no rate limiter: media servers through
  `ConnectionResolver` (below), and slskd, SABnzbd, Lidarr and Prowlarr
  through their client in acquire. The caller always states its priority
  (a person waiting is user priority, background work is background);
  there is no default.
- Keep 401/403, 404 and 400 distinct. Raw transport errors never reach a
  response.
- A client for a user's media server (Jellyfin, Navidrome, Plex) is built
  only by `ConnectionResolver::client` in `media/src/remotes/`, over the one
  resolver bootstrap builds, and talks through `RemoteHttp`. API reads a
  person starts share a small per-source bound; images, streams and
  background work stay outside it.
- LAN services (slskd, SABnzbd) get no rate limiter. Public APIs follow the
  one rate table in `server/providers/src/rates.rs`, which the limiters are
  built from; each row carries the date and source it was verified against.
  `server/tests/server/providers_gate.rs` refuses a rate written anywhere
  else, an HTTP client in a provider file and a priority nobody chose.
  Change a limit only after re-verifying it against the provider's docs.
- Addresses of outside services that are fixed in code live in
  `bootstrap::Outbound`. `Outbound::live()` is the only production source:
  never read one from the environment or from settings.
- Treat every outside server as hostile, the user's own media server and
  download client included. Read a response body only through
  `http_client::read_capped`. Never join a value from a request or from a
  server's answer into an outbound URL as text: validate it and build the URL
  from the parsed base. Relay a server's bytes only under a content type of
  ours from a short allowlist, with `nosniff` and a sandbox policy. A path a
  download client or plugin reports is read only from inside that client's
  folder: a downloaded file is copied or removed only through
  `acquire/src/landing/confine.rs`, which opens it beneath the folder's
  real path one component at a time and never follows a link. That
  opener is the kernel's one no-follow opener (`kernel/src/beneath.rs`),
  which local cover art and the scan's folder images also read through.
- Response security headers are added in one place,
  `server/src/security_headers.rs`: the web UI's content-security policy,
  a "nothing runs here" policy and `nosniff` on every other native
  response, and `Cache-Control: no-store` on any `/api/v3` JSON answer that
  sets no cache rule of its own. A route sets its own policy only to be
  stricter. Compat responses are left alone.

**Config and dependencies**
- Deployment values come from the environment only through `AppConfig::load`,
  except `PATH` and the proxy and certificate variables passed through to
  child processes (plugins, ffmpeg).
- Runtime-editable settings are typed sections stored in SQLite
  (`config_sections`, through `runtime_config`), never new env vars. The
  server reads `config.json` once, at its first start on v3, and never
  again. A start never mints an encryption key over a database that holds
  settings or accounts.
- A service holds a live handle (`Live<S>`) to its settings section, never
  a copied value; `server/tests/server/source_gates.rs` keeps the store
  itself to the composition roots and the settings writers. A setting
  nothing reads is a bug (`settings_readers.rs` checks every stored field
  has a reader). A section's get and put routes come from one table,
  `server/src/settings/registry.rs`; only bespoke routes are hand-written.
- A section that cannot be read never acts as its defaults where that would
  change what is downloaded, approved, kept, deleted, identified, placed,
  streamed or which outside server is called: the work it controls waits
  (a loop skips its pass, a request answers an honest error, a landing
  holds), logs once per revision through `Live::read`, and resumes when the
  section is saved again.
- Secrets are masked by default, saved with mask-means-keep, never logged.
  A decrypted section is a `Plain` value, which cannot be serialized, and a
  secret the store opened refuses to serialize at run time. One mask,
  `SECRET_MASK`, on the server and in the web UI. Each settings section is
  listed once in `runtime_config/registry.rs` and declares its save effects
  there.
- The audio stack is decided: lofty behind its save
  wrapper, symphonia, rusty-chromaprint, ffmpeg for transcoding, no WMA.
  Unrelated tags survive every mutation.
- Prefer a healthy, maintained crate over home-made parser, crypto or retry
  code. New licenses and advisory ignores are reviewed edits to
  `server/deny.toml`, with the reason beside them.

**Library management** (product rules, not up for debate)
- Scanning and identification never write music files.
- Every file mutation (tags, artwork, names, paths, Undo) goes through the
  staged publisher in `server/library/src/publish/`. No shortcuts.
  `server/scripts/library-file-gate.sh` (in `make check` and CI) refuses a
  rename, copy, link or remove in `library/src` outside `publish/`, except
  the lines it lists with their reasons.
- Manual Apply uses the exact sealed preview and rejects stale state.
- A publish is checked against its own rows: each track's root, path,
  availability, stat revision and album, and the album's and track's
  identity (revision and ids), before staging and again inside its commit.
  The catalog revision is a change counter, bumped by every commit that
  changes catalog rows and never compared (a plugin's added file or its
  take-back changes none, so it skips the bump). A write that changes catalog rows bumps it inside its own
  transaction through `catalog_revision::bump_catalog`; event loops wait on
  the lane's commit counter (`Db::commits`), never a timer.
- Automatic work needs an enabled root, the matching trigger, an accepted
  exact MusicBrainz release or a custom edition a curator chose for the
  album where the folder's settings allow it, and a track mapping for every
  affected file.
- Never overwrite an occupied destination automatically, never follow a
  symlink inside a library folder or a bin (a link can lead outside it),
  never treat Undo as a rollback of later external edits. The paths an admin
  sets (a library folder, the recycle bin path) may themselves go through
  links; only what lies below them is checked for links. A library folder
  that is not there takes no file changes (work a restart left unfinished
  there waits until it is back), and neither does one reached through a
  link whose real folder is, holds or sits inside another library folder.
  The one other link followed is a library folder's `.recycle`, and only
  once its target is checked to lie outside every library and download
  folder.
- "Never overwrite" is enforced by the operating system call that places the
  file, not by a check made before it. A library on a filesystem that offers
  no such call still works and shows a warning.
- A managed write keeps the file's permission bits, and its journal row is on
  disk before the file moves.
- A recycle bin copy is deleted only when the identity recorded when it
  landed in the bin (device, inode, size, mtime, ctime) still matches, or its
  bytes match the plan hash. Anything else is kept.
- A scan judges a track gone (walked past) in one place, `walked_past!`
  in `library/src/scan/sqlite_store/`, and never while a publish journal
  row naming it is open or was written after its scope's walk began. A
  trigger on the journal stamps that revision, so no write can skip it.
- A publish records each folder it makes, durably, right after making it and
  before anything is written into it. An unwind, forward or after a crash,
  removes exactly those folders, deepest first and only while empty, and
  never a folder that existed before.
- An empty Vorbis comment (`NAME=`) is not kept: it reads as absent and a
  write drops it. It is the one exception to "unrelated tags survive".
- Publisher phases keep their crash-injection tests. Archive extraction
  rejects traversal and links, and enforces size limits.

**Identity** (product rules, not up for debate)
- Release pins steer the edition hint only, never catalog identity.
- Manual and imported identities survive rescans and moves. Automatic ones
  stay revisable and retract on contradiction.
- Automatic artist reconciliation needs provider proof, never name similarity
  alone. The only exception: proof for the album (an accepted MusicBrainz
  release ID or a stored album-level provider-proof row) may stand in for
  proof on every credit when no provider proof contradicts it and the credits
  are not composite or ambiguous.
- A second exception: when a scan leaves an artist record with no credits,
  the record holds no MusicBrainz link or proof, every credit the scan
  moved off it went by name alone (no id in the files) to one other record,
  the two names differ only in capitals, spacing or sort name, no play of
  it is of a track now credited to someone else, and an admin has not
  marked them distinct, the emptied record merges into that one at scan
  end: plays, favourites and ratings move, its id stays an alias and the
  merge is in the audit. Any other record emptied without provider proof is
  never merged by itself; when a scan recorded the move and it holds a
  favourite, rating or play, it is listed in Duplicate artists.
- Retired IDs stay as aliases.
- Every artist merge goes through `retire_artist` with a `MergeBasis`
  built in `corrections/merge_basis.rs` from rows read in the same
  transaction; the merge history shows each basis.
- Setting or clearing an artist's link by hand never starts a merge; when
  it would, it is refused and the records are merged by an admin.
- MusicBrainz's special-purpose artist ids (Various Artists, unknown and
  the like) are never proof and never key an artist row.
- A release group that every file's tags name, or that a revisable match
  names without a release, is searched first and wins a close call. A
  release outside it is only ever an unconfirmed guess, and stays one
  until a person confirms it or the files' tags name its group. An
  automatic match withdrawn on contradiction keeps an undo.
- A release MusicBrainz will not give in a readable form is never
  evidence: identification waits and retries rather than marking an
  album unmatched, and an unreadable tagged release leaves only an
  unconfirmed guess.
- Album and track identities, match state and their undo rows are written
  only through `library/src/identity/`; a source gate holds it.

**Compat, plugins, upgrade, readiness**
- Compat support means the tested matrix and its golden
  traces, for the apps the README lists only, not "parity". Streaming keeps its
  explicit tests: ranges, HEAD, lengths, auth.
- Which Jellyfin routes are public is the one route table in
  `compat/src/jellyfin/router.rs`; every other row is signed in by its
  layer before the handler runs, and the limiter reads the same table.
- A Jellyfin id is stored in `compat_jellyfin_ids` by compat's fill loop
  and never minted again, a data-key reset included. A request resolves
  ids from that table and never lists the catalog.
- A plugin is trusted, unsandboxed code. Nothing runs until an admin enables
  it. `PLUGINS.md` is the public contract: change the manifest, protocol or
  limits and update it in the same branch.
- v2 exports use stable keys (MusicBrainz IDs), never v2 integer keys, and
  never hold plaintext secrets. Rolling back to v2 restores the app and
  database, not files v3 renamed or retagged.
- Never gate "can acquire" on slskd alone: acquisition health is slskd or
  Usenet or an enabled plugin download source, answered by one readiness
  value in acquire (`readiness.rs`) that requests, search, the download
  queue, the health smoke and Discover all read. A source counts once it
  is turned on, whether or not it answers right now. Release readiness is
  a separate gate per source. A request made
  while no source is ready is held and starts by itself when one is saved.
  It is never refused or lost.
- Every file kept for a person's decision (a held download, an upload
  waiting for review) is an entry in acquire's one held list
  (`acquire/src/held/`), kept 90 days and then removed by its prune. An
  entry a person is acting on is never removed under them, and a held
  file is only judged gone when the filesystem says it is absent, never
  on a read error. A held file the library could not take is retried by
  itself (`Gate::HeldRetry`), and that retry never places a file on an
  album a person moved it away from, never adds a second copy and never
  replaces a library copy: those wait for a person.
- A source outside the download settings is never taken by automatic
  work (picks, failover, the search review, the wanted watcher). A person
  may pick it behind a confirm the server enforces, and only that pinned
  attempt's files skip the quality range (`quality_waived` in the plan
  and `pinned` on the handle); every other landing check still runs.

## Frontend (SvelteKit, Svelte 5, TypeScript)

- SPA only: `ssr = false`, adapter-static, thin load functions.
- Svelte 5 runes only: `$props()`, `$state`, `$derived`, callback props. No
  `export let`, `$:`, `createEventDispatcher` or `any`.
- Data fetching is TanStack Query only, in per-feature modules under
  `src/lib/queries/` with `get<Thing>Query` hooks. Calls go through the `api`
  client with URL builders and forward the abort signal. Types come from the
  generated contract. Query parameters and DELETE bodies are typed from the
  contract; the client has no untyped request methods.
- Query keys come from factory helpers built on `lib/queries/keys.ts`,
  never inline arrays: `[scope, feature, ...parts]`, with the scope
  (`user:<id>`, `signed-out`, `shared` or `admin`) always first, so a
  user's keys never leak to another. A query is saved to the browser only
  when its `meta.persist` says so, and a query whose answer depends on the
  metadata source carries `meta.provider`, so a source switch drops it.
- Change the cache only through `setQueryDataWithPersister` and
  `invalidateQueriesWithPersister`. Mutations invalidate every affected
  prefix, including other features'.
- What a change invalidates is a named effect in `lib/queries/effects.ts`;
  only that module invalidates. Mutations are built with
  `createAppMutation` and declare their `effects` (`() => []` when they
  change nothing cached); other callers apply effects, never hand-listed
  prefixes. Server events reach the
  app through one dispatcher (`lib/queries/events/dispatcher.ts`) keyed by
  the contract's event union, so a missing or extra event name does not
  compile.
- A settings form is a `createSettingsSection` (`src/lib/ui/settingsSection.svelte.ts`):
  draft, dirty, save, test and mask-means-keep in one place, with the
  unsaved-changes guard; a tab never hand-writes that state.
- Stores hold client and session state only.
- UI: Tailwind v4 with DaisyUI tokens in `src/app.css`, no new hex colors.
  Build on `src/lib/ui` (`Button`, `IconButton`, `Card`, `Input`, `Modal`,
  `CardTitle`, `CardActions`, `ConfirmDialog`, `Field`, `TextField`,
  `NumberField`, `SecretField`, `SelectField`, `TextAreaField`, `ToggleRow`,
  `SaveBar`,
  `AsyncButton`, `QueryBoundary`, `RatingStars`, `PageShell`, `Toaster`,
  `CoverPlaceholder`, `UnsavedChangesGuard`):
  do not hand-write a button, a card, a text input, a modal, a confirm, a
  form row or a query's loading, error and empty branches. Lint refuses a
  raw `btn` or `card` class (or their DaisyUI modifiers) and a text-like
  `<input>` outside `src/lib/ui`. A look the kit lacks is added to the kit
  as a variant, not to one screen.
  Lucide icons, `AlbumImage` and `ArtistImage`, matching `*Skeleton`
  loaders. Missing art shows `CoverPlaceholder` (a colour pair worked out
  from the item's id), never a grey box or a hand-drawn icon. `app.css` holds tokens, fonts, keyframes and `@utility`
  definitions; a component's styles live in its own `<style>` block, or
  in a stylesheet beside the components that share them. One rule in
  `app.css` stops motion under `prefers-reduced-motion`; motion that
  carries meaning opts out with `motion-keep`, and Svelte transitions come
  from `$lib/ui/motion`.
- Navigation destinations live in `lib/components/shell/navItems.ts`; the
  sidebar, the bottom bar and the More sheet all render from it.
- A Discover or Home section is one entry in the registry in
  `catalog/src/reads/discover/sections.rs` (its page, zone, settings
  title, the reads it needs, its dedupe); the page build, the section
  settings and the web UI's lookups all read it, and nothing else lists
  sections.
- The UI runs under an enforced content-security policy: no inline script,
  no `eval`, no `<style>` or static `style="..."` in markup (set styles
  through Svelte's style bindings; eslint refuses a static one), and
  outside hosts only from the list in `server/src/security_headers.rs`.
  Loading a script, frame, audio clip or connection from a new host is a
  reviewed edit to that list, with the reason beside it. The browser
  journeys fail on any violation.
- Text from outside (biographies, album notes, plugin text, release notes)
  renders as text. Markup goes only through `SafeHtml`; eslint rejects a raw
  `{@html}`. A link to an outside address goes through `outsideLink`, which
  passes only http and https.

## Testing (brief-first, minimal, fast)

- A test is a short brief saying exactly how one behavior must work.
- Write one only for what would regress silently and cost something: API
  contracts, auth, security, identity, data loss and other destructive paths,
  compat wire shapes, performance budgets. Not fakes or trivial code.
- A test written to prove a bug is deleted once the fix lands unless it meets
  that bar. The suite as a whole gets smaller and faster over time, not
  larger.
- A few end-to-end tests of real user journeys beat dozens of unit tests.
- Watch a new test fail before the fix lands. Never weaken or delete a failing
  test to get green.
- To watch a test fail on the old code, save the change as a patch, reverse
  it on the source files only, run the test, then apply the patch again and
  check the diff matches. Never `git stash`: a stash is shared by every
  worktree.
- Keep the suite fast. A slow suite is a bug in the suite.
- No real network or waits: the in-repo mock servers for download clients,
  mock servers or scripted fakes for metadata providers, virtual time for
  loops. Stores are never faked: tests run on real SQLite from the template
  database. A journey starts the app through the production builder.
- Audio tests use the committed fixtures, never a tagger's own round-trip.
- Routes for user-owned resources join the ownership table in
  `auth_ownership.rs` (owner, other user, admin); the derived matrix covers
  401 and roles.
- Frontend specs sit beside the code and use `render()` with awaited locator
  assertions, not `querySelector`. A new or rewritten browser spec fakes the
  API with `lib/test/api.ts` (fixtures typed from the contract) and renders
  with a real QueryClient (`renderWithQuery`); it never mocks a query
  module, the API client or TanStack.
- Frontend CI runs only when `frontend/` changes, so a frontend change runs
  every spec that mocks a module it changed, not only the specs it edited.
- Journeys use `TestApp` (`server/tests/common/app.rs`): the production
  builder with every outside address closed. Test databases are copies of the
  migrated template. Wait on something observable, never on time.
- Tests clean up after themselves.
- A test on a paused clock that waits for work on blocking threads (any
  store call) bounds that wait in wall-clock time: the paused clock jumps
  ahead while the blocking work runs.

## Writing

- Everything that isn't code reads like a person wrote it: commits, comments,
  docs, UI copy, error messages.
- Plain and direct. No sales language, AI-sounding phrasing or emojis.
  Hyphens, never em-dashes.
- Comments say what and why, briefly. No thinking out loud, no build-process
  words like "stage" or "wave".
- Branding is **DroppedNeedle**: one word, PascalCase.

## Leave these alone

- GitHub URLs point at the `DroppedNeedle/DroppedNeedle` org repo. Never
  create repos under the old names.
- The two ignored advisories (RUSTSEC-2023-0071 in CI, RUSTSEC-2024-0436 in
  `deny.toml`) are reviewed. Re-check them when sqlx or lofty move.
- Gaps in the migration numbering stay as they are.
