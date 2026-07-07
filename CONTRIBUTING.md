# Contributing to DroppedNeedle

Thanks for your interest. Bug reports, feature requests, and pull requests are all welcome.

## Reporting Bugs

Use the [bug report template](https://github.com/HabiRabbu/DroppedNeedle/issues/new?template=bug.yml). Include your DroppedNeedle version, steps to reproduce, and relevant logs from `docker compose logs droppedneedle`. The more detail you give, the faster things get fixed.

## Requesting Features

Use the [feature request template](https://github.com/HabiRabbu/DroppedNeedle/issues/new?template=feature.yml). Check existing issues first to avoid duplicates.

## Development Setup

The backend is Python 3.13 with FastAPI. The frontend is SvelteKit with Svelte 5, Tailwind CSS, and daisyUI.

### Prerequisites

- Python 3.13+
- Node.js 22+
- Docker (for building the full image)

### Running Locally

Backend:

```bash
cd backend
pip install -r requirements-dev.txt
cp env.dev.example .env
uvicorn main:app --reload --port 8688
```

Frontend:

```bash
cd frontend
cp env.development.example .env.development
pnpm install
pnpm run dev
```

### Running Tests

```bash
make backend-test    # backend suite
make frontend-test   # frontend suite
make test            # both
```

Frontend browser tests use Playwright. Install the browser first:

```bash
make frontend-browser-install
```

## Pull Requests

1. Fork the repo and create a branch from `main`.
2. Give your branch a descriptive name: `fix-scrobble-timing`, `feature-playlist-export`, etc.
3. If you're fixing a bug, mention the issue number in the PR description.
4. Make sure tests pass before submitting.
5. Keep changes focused. One PR per fix or feature.

## Code Style

- Backend: strong typing, async/await, no blocking I/O in async contexts.
- Frontend: strict TypeScript, no `any`. Named exports. Async/await only.
- Use existing design tokens (`primary`, `secondary`, etc.) for colours, not hardcoded values.
- Run `pnpm run lint` and `pnpm run check` in the frontend before submitting.

## AI-Assisted Contributions

If you used AI tools (Copilot, ChatGPT, Claude, etc.) to write code in your PR, please mention it. This isn't a problem and won't get your PR rejected, but it helps reviewers calibrate how much scrutiny to apply. A quick note like "Claude helped with the caching logic" is enough.

You're still responsible for understanding and testing the code you submit.

## Questions?

Open a thread in [Discord](https://discord.gg/B5suDg7gu2) or start a [GitHub Discussion](https://github.com/HabiRabbu/DroppedNeedle/discussions).
