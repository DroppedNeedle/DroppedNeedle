# Contributing to DroppedNeedle

Thanks for your interest. Bug reports, feature requests, and pull requests are all welcome.

## Reporting bugs

Use the [bug report template](https://github.com/DroppedNeedle/DroppedNeedle/issues/new?template=bug.yml). Include your DroppedNeedle version, steps to reproduce, and relevant logs from `docker compose logs droppedneedle`. The more detail you give, the faster things get fixed.

## Requesting features

Use the [feature request template](https://github.com/DroppedNeedle/DroppedNeedle/issues/new?template=feature.yml). Check existing issues first to avoid duplicates.

## Development setup

The server is Rust (Axum, tokio, SQLite through rusqlite). The frontend is SvelteKit with Svelte 5, TanStack Query, Tailwind CSS and daisyUI. The toolchain version is pinned in `rust-toolchain.toml`; rustup picks it up automatically.

### Prerequisites

- Rust via rustup
- `cmake` (the Opus decoder builds a bundled C library)
- Node.js at the version in `.github/node-version`
- pnpm through corepack: run `corepack enable` once, and `pnpm` uses the version in `frontend/package.json`
- Docker, to build the image

### Running locally

Server, with its data in `./dev-data`:

```bash
make server-run
```

Frontend, in a second shell, talking to that server:

```bash
cp frontend/env.development.example frontend/.env.development
make frontend-install
make frontend-dev
```

### Where the code is

`server/` is a Cargo workspace. Each area is a crate in `server/<name>`, from the bottom: `kernel`, `providers`, `auth`, `library`, `plugins`, `media`, `catalog`, `acquire`, `compat`, then the server crate in `server/src` and the migration tool in `server/tool`. A crate uses only crates below it. Integration tests are split into one binary per area (`library`, `acquire`, `compat`, `catalog`, `media` and `server` for the rest) sharing `server/tests/common`, plus `tool` in `server/tool/tests`. Run one module with `cd server && cargo test --test <binary> <module>` (`cargo test -p droppedneedle-tool --test tool <module>` for the tool), and a crate's unit tests with `cargo test -p droppedneedle-<crate>`.

`frontend/src/lib/` is grouped by feature. A feature (album, artist, library, downloads, settings and so on) lives in `lib/features/<name>/`: its components and their helper modules in `components/`, its query and mutation hooks, key factories and adapters in `queries/`, and its client state in `state/`. Another feature imports only the modules on its public list in `lib/features/public.json`, by their `$lib` path; lint refuses any other path into a feature's folders. When another feature needs one of its modules, add the module to the list. A new feature folder gets an entry there too, which a spec checks along with every listed path. Shared code sits outside the features: the UI kit and the generic page pieces in `lib/ui`, cover and artist images and brand icons in `lib/media`, the app shell and navigation in `lib/shell`, the query client, keys, effects and event dispatcher in `lib/queries`, the API client in `lib/api`, app-wide stores in `lib/stores` and small utilities in `lib/utils`. Routes in `src/routes` stay thin and render their feature's components.

Migrations live in `server/migrations/`, starting from the v3 baseline, and are listed in `server/kernel/src/schema/mod.rs`. A new one takes the next number, is never edited or renumbered once it is on `v3`, and ships its upgrade test. A local development database from before the baseline starts once on a build from before it (which brings it to the last number the baseline replaced), or starts from an empty data folder.

### Running tests and checks

```bash
make server-test            # full Rust suite
make server-lint            # clippy, warnings are errors
make server-fmt-check       # rustfmt
make contract-check         # OpenAPI snapshot and generated TypeScript are in sync
make frontend-check         # svelte-check
make frontend-lint          # eslint
make frontend-format-check  # prettier
make frontend-test-server   # vitest, node project
make frontend-test-client   # vitest, browser project
make library-file-gate      # no file changes in the library outside the publisher
make check                  # all of the above (not cargo audit, cargo deny or the container test)
```

`python3 -I server/scripts/dead-code-scan.py server` fails on a public item nothing references; one that must stay goes in `server/scripts/dead-code-allowlist.txt` with its reason.

The timing budgets are ignored tests, which `.github/workflows/perf.yml` runs in release every Monday and by hand. To run one locally: `cd server && cargo test --release --test server login_p95 -- --ignored --nocapture`.

Browser tests use Playwright. Install the browser once with `make frontend-browser-install`.

A browser spec renders the real component with `renderWithQuery` from `frontend/src/lib/test/render.ts`, which gives each test a fresh TanStack `QueryClient` with no retries and no persister. It answers the component's requests with `fakeApi` from `frontend/src/lib/test/api.ts`: routes are keyed by method and contract template (`'GET /api/v3/albums/{album_id}/basic'`), each answer is typed as that operation's response from the generated contract, so svelte-check fails a fixture that drifts from the server, and `apiError(status, code)` builds an error answer. A request with no route fails the test and names it. Mock the player, `$app/navigation` and things outside the browser, never the query modules, the api client or TanStack.

### Browser journeys

The journeys in `frontend/tests/journeys/` walk through the real app in a browser: set up a fresh server, sign in, and so on. They run against the container image, not the dev server:

```bash
make frontend-install frontend-browser-install   # once
make container-e2e
```

This builds a test image, runs the container checks (boot, health, clean shutdown), then starts the image on a Docker network with no route out and runs the journeys against it. It needs Docker and takes a while, mostly for the image build. Each journey gets an empty server of its own, and the containers and network are removed when the run ends. A failed journey leaves a trace in `frontend/test-results/journeys/`; open it with `pnpm exec playwright show-trace <path>/trace.zip` from `frontend/`. CI runs the same script in the Container E2E job and keeps the traces of a failed run as the `journey-traces` artifact.

The mock slskd and MusicBrainz the journeys point the app at run as their own program: `cargo run -p droppedneedle-testkit --bin droppedneedle-mocks -- --slskd 0.0.0.0:9001 --musicbrainz 0.0.0.0:9002` from `server/`. MusicBrainz answers under `/ws/2` from recorded responses only, and logs each request it has no recording for.

If you change a route or a request or response type, run `make contract-write` and commit the regenerated `server/openapi/openapi.json` and `frontend/src/lib/api/v3/openapi.d.ts` with your change. Both targets read the document from the integration-test build, so they reuse what `cargo test` already compiled; CI runs `server/openapi/check.sh` against the server binary itself.

### SQL statements

SQL lives in a file named `sql.rs` beside the code that runs it, so a test can check every statement against the schema. A `sql.rs` holds one `sql!` block and the functions that build statement families, nothing else:

```rust
droppedneedle_kernel::sql! {
    /// Fixed text other statements are built from.
    fragment FEED_COLUMNS = "f.source, f.source_event_id, f.city";
    /// One user's cities.
    pub(super) CITIES = "SELECT city_name FROM catalog_event_cities WHERE user_id = ?";
    /// A statement joined from constant parts at compile time.
    pub(super) FEED = "SELECT " + FEED_COLUMNS + " FROM catalog_live_event_feed f";
    /// One statement per combination of the builder's choices.
    family FEED_PAGE = feed_page;
}

pub(super) fn feed_page(for_user: bool, newest_first: bool) -> String { ... }
```

- A statement is a `Statement` constant; pass `CITIES.text()` to rusqlite. `+` joins literals, fragments and other `&str` constants at compile time.
- A statement that varies by a closed set of choices (a sort order, which optional filters are present, how many values an `IN` list holds) is a family: a function in `sql.rs` whose parameters are all choice types (`bool`, `ListLen`, an enum or struct declared with `sql_choice!`, or a tuple of those) and which returns the text. Nothing from a request is ever joined into SQL. A family over more than 4,096 combinations is split.
- A table name read from the database goes through `db::statement::quoted_identifier`.
- The export tool's reads of a v2 database sit in a block that starts with `schema = V2;`, and the test prepares them against the v2 fixture database instead.
- Each `sql!` block lists its own declarations. A crate that holds a `sql.rs` lists every one of them, one line each, in `statement_files()` at its crate root (compiled only for tests), and has a row in `REGISTRIES` in `server/tests/server/persist_schema.rs`.

`every_registered_statement_prepares` fails when a `sql.rs` is missing from its crate's list or declares something outside its `sql!` block, then prepares every statement, and every family over every combination, against the migrated schema. It catches a renamed column, a dropped table and a second statement after a `;`. It does not check how many values a call site binds or the query plan. A statement that answers more than one column is read through a `row` declared in its `sql!` block, and the test fails when the row's fields differ from the statement's columns.

`raw_sql_stays_in_sql_files` fails on SQL text anywhere else, and on `PRAGMA` or transaction control outside `server/kernel/src/db/`. Test files are exempt, as is the export's bundle builder, whose statements are made at run time from the columns of the v2 database being exported; the few other literals allowed for good are listed in the test with their reasons.

A column that stores an enum as text declares the enum's spellings with `droppedneedle_kernel::spelled!` and carries a `CHECK (column IN (...))` listing exactly those spellings; `every_state_column_lists_exactly_its_enums_values` in `persist_schema.rs` registers each such column with its enum and fails when the two differ.

## Pull requests

1. Fork the repo and create a branch from `v3`. Until v3 is released, the default branch, `main`, holds v2.
2. Give your branch a descriptive name: `fix-scrobble-timing`, `feature-playlist-export`, etc.
3. If you're fixing a bug, mention the issue number in the PR description.
4. Make sure tests pass before submitting.
5. Keep changes focused. One PR per fix or feature.

Outside pull requests are welcome. Each one is reviewed against the rules in [CLAUDE.md](CLAUDE.md) and checked on the current tip before it is taken. A pull request taken as it is merges with your authorship; one that needs changes is taken as your commit with the fixes folded in or added beside it, and the pull request is closed with a note saying which. New features and changes to product rules are the maintainer's call, so open an issue first for those.

## Code style

- Server: no `unwrap`, `expect` or `panic!` outside tests (clippy enforces it). Services return typed errors; only handlers map them to HTTP status. Blocking file, SQLite or CPU-heavy work runs off the async workers.
- The server is a single process. Never run two against the same data directory.
- Frontend: strict TypeScript, no `any`. Svelte 5 runes only. Data fetching goes through TanStack Query hooks and the typed `/api/v3` client.
- Use existing design tokens (`primary`, `secondary`, etc.) for colours, not hardcoded values.
- The web UI runs under an enforced content-security policy: no inline scripts, no `eval`, and no static `style="..."` in markup (eslint refuses one; bind styles through Svelte instead). Loading a script, frame, audio clip or connection from a new outside host is an edit to the list in `server/src/security_headers.rs`, with the reason beside it.
- Run `pnpm run lint` and `pnpm run check` in the frontend before submitting.

## Using AI

DroppedNeedle is built with a lot of help from AI, and AI-assisted contributions are welcome, including pull requests an AI agent wrote most of.

What we ask is the same as for any pull request:

- A clear description of what changed and why.
- A change you thought through and talked through with your tools, not a first draft pasted in.
- Tests that pass, and code you understand well enough to answer questions about.

If you used AI, say so in the pull request: the tool and model (for example Claude Opus 5.5), and roughly how you worked with it. It doesn't change how the pull request is judged. We ask because we're curious what people use. Commit trailers your tool adds, such as `Co-Authored-By`, are fine to keep.

## Releases

Maintainers cut releases. A release is a version tag, `v3.1.0` or `v3.1.0-rc.1` for a pre-release, on a commit of the release branch that CI has passed on. Bump `version` under `[workspace.package]` in `server/Cargo.toml` to the same version, without the `v`, before tagging: the workflow rejects a tag that does not match it. Pushing the tag starts the Release workflow, which checks the tag, the branch and CI, then waits for a maintainer to approve the `release` environment before it publishes `droppedneedle/droppedneedle:<tag>`. `:latest` only moves forward: a pre-release never moves it, and neither does a patch to an older line once a higher full version is tagged.

CI is path-filtered: a commit that changes no code gets no v3 CI run, and a run on one that changes only some areas skips the other jobs and still passes. The Release workflow takes any passing v3 CI run on the commit, so start a full one by hand.

Only maintainers tag, and nothing but a version tag publishes a release image. To cut one:

1. Bump `version` in `server/Cargo.toml` to the release version and push that commit to the release branch.
2. Start v3 CI by hand on that commit, so every job runs, and wait for it to pass.
3. Check that the latest Frontend CI run on the release branch and the weekly Perf budgets run are green.
4. Run the release dry run described below.
5. Push the tag.
6. Approve the `release` environment when the Release workflow asks.

To rehearse a release without publishing, run the Release workflow by hand with the version you plan to tag and leave `push` unticked. It runs every check, waits for the same approval and builds the image for both architectures without pushing anything.

## Questions?

Open a thread in [Discord](https://discord.gg/B5suDg7gu2) or start a [GitHub Discussion](https://github.com/DroppedNeedle/DroppedNeedle/discussions).
