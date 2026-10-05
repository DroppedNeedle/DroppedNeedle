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

### Running tests and checks

```bash
make server-test            # full Rust suite
make server-lint            # clippy, warnings are errors
make server-fmt-check       # rustfmt
make contract-check         # OpenAPI snapshot and generated TypeScript are in sync
make frontend-check         # svelte-check
make frontend-test-server   # vitest, node project
make frontend-test-client   # vitest, browser project
make check                  # all of the above, plus lint and format checks
```

Browser tests use Playwright. Install the browser once with `make frontend-browser-install`.

If you change a route or a request or response type, run `make contract-write` and commit the regenerated `server/openapi/openapi.json` and `frontend/src/lib/api/v3/openapi.d.ts` with your change.

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

## Questions?

Open a thread in [Discord](https://discord.gg/B5suDg7gu2) or start a [GitHub Discussion](https://github.com/DroppedNeedle/DroppedNeedle/discussions).
