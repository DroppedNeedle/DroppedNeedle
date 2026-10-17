# Contributing to DroppedNeedle

Thanks for your interest. Bug reports, feature requests, and pull requests are all welcome.

## Reporting bugs

Use the [bug report template](https://github.com/DroppedNeedle/DroppedNeedle/issues/new?template=bug.yml). Include your DroppedNeedle version, steps to reproduce, and relevant logs from `docker compose logs droppedneedle`. The more detail you give, the faster things get fixed.

## Requesting features

Use the [feature request template](https://github.com/DroppedNeedle/DroppedNeedle/issues/new?template=feature.yml). Check existing issues first to avoid duplicates.

## Development setup

The server is Rust (Axum, tokio, SQLite through sqlx). The frontend is SvelteKit with Svelte 5, TanStack Query, Tailwind CSS and daisyUI. The toolchain version is pinned in `rust-toolchain.toml`; rustup picks it up automatically.

### Prerequisites

- Rust via rustup
- `cmake` (the Opus decoder builds a bundled C library)
- Node.js 22+ and pnpm
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
make check                  # all of the above (not cargo audit, cargo deny or the container test)
```

Browser tests use Playwright. Install the browser once with `make frontend-browser-install`.

### Browser journeys

The journeys in `frontend/tests/journeys/` walk through the real app in a browser: set up a fresh server, sign in, and so on. They run against the container image, not the dev server:

```bash
make frontend-install frontend-browser-install   # once
make container-e2e
```

This builds a test image, runs the container checks (boot, health, clean shutdown), then starts the image on a Docker network with no route out and runs the journeys against it. It needs Docker and takes a while, mostly for the image build. Each run starts from an empty server, and its containers and network are removed when it ends. A failed journey leaves a trace in `frontend/test-results/journeys/`; open it with `pnpm exec playwright show-trace <path>/trace.zip` from `frontend/`. CI runs the same script in the Container E2E job and keeps the traces of a failed run as the `journey-traces` artifact.

The mock slskd and MusicBrainz the journeys point the app at run as their own program: `cargo run -p droppedneedle-testkit --bin droppedneedle-mocks -- --slskd 0.0.0.0:9001 --musicbrainz 0.0.0.0:9002` from `server/`. MusicBrainz answers under `/ws/2` from recorded responses only, and logs each request it has no recording for.

If you change a route or a request or response type, run `make contract-write` and commit the regenerated `server/openapi/openapi.json` and `frontend/src/lib/api/v3/openapi.d.ts` with your change. Both targets read the document from the integration-test build, so they reuse what `cargo test` already compiled; CI runs `server/openapi/check.sh` against the server binary itself.

## Pull requests

1. Fork the repo and create a branch from the default branch.
2. Give your branch a descriptive name: `fix-scrobble-timing`, `feature-playlist-export`, etc.
3. If you're fixing a bug, mention the issue number in the PR description.
4. Make sure tests pass before submitting.
5. Keep changes focused. One PR per fix or feature.

## Code style

- Server: no `unwrap`, `expect` or `panic!` outside tests (clippy enforces it). Services return typed errors; only handlers map them to HTTP status. Blocking file, SQLite or CPU-heavy work runs off the async workers.
- The server is a single process. Never run two against the same data directory.
- Frontend: strict TypeScript, no `any`. Svelte 5 runes only. Data fetching goes through TanStack Query hooks and the typed `/api/v3` client.
- Use existing design tokens (`primary`, `secondary`, etc.) for colours, not hardcoded values.
- Run `pnpm run lint` and `pnpm run check` in the frontend before submitting.

## AI-assisted contributions

If you used AI tools (Copilot, ChatGPT, Claude, etc.) to write code in your PR, please mention it. This isn't a problem and won't get your PR rejected, but it helps reviewers calibrate how much scrutiny to apply. A quick note like "Claude helped with the caching logic" is enough.

You're still responsible for understanding and testing the code you submit.

## Releases

Maintainers cut releases. A release is a version tag, `v3.1.0` or `v3.1.0-rc.1` for a pre-release, on a commit of the release branch that CI has passed on. Bump `version` under `[workspace.package]` in `server/Cargo.toml` to the same version, without the `v`, before tagging: the workflow rejects a tag that does not match it. Pushing the tag starts the Release workflow, which checks the tag, the branch and CI, then waits for a maintainer to approve the `release` environment before it publishes `droppedneedle/droppedneedle:<tag>`. `:latest` only moves forward: a pre-release never moves it, and neither does a patch to an older line once a higher full version is tagged.

CI skips commits that change no code. If the commit you want to tag has no v3 CI run, start v3 CI by hand on the release branch first.

To rehearse a release without publishing, run the Release workflow by hand with the version you plan to tag and leave `push` unticked. It runs every check, waits for the same approval and builds the image for both architectures without pushing anything.

## Questions?

Open a thread in [Discord](https://discord.gg/B5suDg7gu2) or start a [GitHub Discussion](https://github.com/DroppedNeedle/DroppedNeedle/discussions).
