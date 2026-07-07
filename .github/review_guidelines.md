# DroppedNeedle AI Review Guidelines

These guidelines drive the automated AI review system. They are loaded at runtime. Update
them whenever project conventions change; no code changes are needed.

---

## Review Principles

### Scope

Only review lines that changed. Do not nitpick surrounding code that the PR did not touch.
If a changed line exposes a pre-existing issue in adjacent unchanged code, mention it in
the summary as a Suggestion. Do not attach it as an inline comment on unchanged lines.

### Uncertainty

When you are unsure whether a finding is valid (incomplete call chain, ambiguous diff,
missing context), prefix the comment with **Possibly:** or **Verify:**. Never assert an
issue you cannot confirm from the diff alone.

### Tone

Focus on issues. Skip generic compliments ("looks good", "nice work"). Brief, specific
praise for a genuinely good architectural choice is fine, but keep it to one sentence.

Do not suggest adding comments to self-evident code. Comments make sense for complex
algorithms, non-obvious workarounds, or public API contracts.

### Hotfixes and small PRs

If the PR is clearly a hotfix or urgent bug patch (single file, minimal change, no new
features), downgrade Suggestions to "consider later" and focus only on Must Fix items.

### Large PRs

When the diff is too large to review in one pass, prioritize:
1. Security-sensitive code (auth, crypto, token handling, streaming)
2. Route and service changes
3. New dependencies
4. Infrastructure and persistence changes

Note in the summary which files were skipped due to length.

---

## Severity Levels

### Must Fix (blocking)

Security vulnerabilities, logic bugs that produce incorrect behaviour, and architectural
violations that would corrupt the layered design. These block merge.

### Should Fix (recommended before merge)

Missing error handling on external I/O, type safety violations, missing OpenAPI metadata,
performance concerns like missing caching on expensive lookups, and omissions that degrade
observability. Address before merging unless there is a documented tradeoff.

### Suggestion (non-blocking)

Naming improvements, minor deduplication, DRY violations, clearer control flow, optional
documentation improvements. Author discretion.

---

## Backend (Python, FastAPI)

### Architecture and layering

Route handlers in `backend/api/v1/routes/` validate input, call the service layer, and
map domain exceptions to HTTP responses. If a route contains business logic, data
transformation, or direct I/O: **Must Fix.**

Services in `backend/services/` orchestrate repositories and other services. They raise
domain exceptions (`DroppedNeedleException` subclasses), never `HTTPException`. Catching
`HTTPException` in a service: **Should Fix.**

Repositories in `backend/repositories/` are the only layer that makes HTTP calls, accesses
files, or talks to databases. A service calling `httpx` or `aiofiles` directly: **Must Fix.**
The `infrastructure/` layer provides low-level utilities (caching, resilience, crypto,
persistence) consumed by repositories.

New exception types must extend `DroppedNeedleException` or an existing subclass
(`ExternalServiceError`, `ResourceNotFoundError`, `ValidationError`, `AuthenticationError`,
`RegistrationError`, etc.). Using bare `Exception` for application-signalling errors:
**Should Fix.** It prevents callers from catching specific error types.

New exceptions must be registered in `backend/main.py` via `app.add_exception_handler()`.
Missing registration: **Should Fix.** Unregistered exceptions fall through to the generic
handler, returning a 500 instead of the intended status code.

### Authentication and authorization (applies after multi-user merge)

New API routes default to protected. Unless the route path is added to `_PUBLIC_PATHS` or
`_PUBLIC_PREFIXES` in `backend/middleware.py`, every new route requires authentication.
Missing auth on a route that accesses user or system data: **Must Fix.**

Use `CurrentUserDep` for authenticated routes, `CurrentAdminDep` for admin-only routes,
and `CurrentTokenDep` when the session token object is needed (e.g. logout-all). These are
FastAPI dependencies injected via `Depends()`. Parsing cookies or Authorization headers
by hand instead of using these dependencies: **Should Fix.**

Making a previously unprotected route protected is a breaking change. If the PR adds
`CurrentUserDep` to a route that was previously public: **Should Fix.** The author must
confirm this is intentional and documented.

Session cookies are `httpOnly`, `SameSite=lax`, with `Secure` auto-detected from the
request scheme or `X-Forwarded-Proto`. Custom cookie settings on auth routes should follow
this pattern.

Passwords must be hashed (bcrypt or argon2) and never stored or logged in plaintext.
Plaintext password handling: **Must Fix.**

Sensitive config values (OIDC client secret, Plex token) must be encrypted at rest using
the `infrastructure/crypto.py` Fernet helpers (`encrypt()` and `decrypt()`). Storing them
in plaintext in the database or config: **Must Fix.**

### Error handling

All external HTTP calls need explicit timeouts. Use `httpx.AsyncClient(timeout=...)` or
pass `timeout=15.0` on the request. No timeout, or `timeout=None`: **Must Fix.**

In repositories, `httpx.HTTPError` must be caught and converted to `ExternalServiceError`.
Service layers should not need to catch `httpx.HTTPError` directly. Letting a raw
`HTTPError` propagate out of the repository layer: **Should Fix.** Swallowing network
errors silently (returning `None` without logging): also **Should Fix.**

External API responses must be validated with msgspec. Raw dict access without schema
validation: **Should Fix.** Catch `(msgspec.ValidationError, msgspec.DecodeError,
TypeError, KeyError)`.

Parallel fetching must use `asyncio.gather(..., return_exceptions=True)`. One failing
source should not crash the entire request. Plain `asyncio.gather` without
`return_exceptions=True` on multi-source fetches: **Should Fix.** `asyncio.TaskGroup`
(Python 3.11+) handles exceptions differently: one task failure cancels the group. If
`TaskGroup` is used, the caller must handle `ExceptionGroup` explicitly.

Circuit breaker state changes must log with structured key=value format:
`logger.warning("circuit_breaker.trip service=X previous_state=Y state=Z reason=W")`.
Silent transitions: **Should Fix.** They make production debugging very difficult.

`is_unknown_mbid()` must guard every endpoint that accepts an MBID path parameter from
the user. Endpoints that receive MBIDs indirectly (from internal lookups) do not need the
guard. Service methods that accept user-provided MBIDs directly should also validate with
`is_unknown_mbid()` or `validate_mbid()`. Missing guard at either layer: **Should Fix.**

### Serialization

All structs use msgspec. Pydantic models, dataclasses, or raw dicts where a struct
belongs: **Must Fix.** Msgspec structs are frozen; use `msgspec.structs.replace(obj,
field=new_value)` for updates.

No mutable default values in struct fields. `artists: list = []` is an error (msgspec
rejects it). Use `artists: list = msgspec.field(default_factory=list)`: **Suggestion.**

`RequestBody` uses `MsgSpecBody()`, not FastAPI's `Body()`.

The default response class is `MsgSpecJSONResponse` (set globally in `main.py`).
Returning a struct from a route serializes correctly. Explicitly using FastAPI's
`JSONResponse`: **Suggestion.** It works but is unnecessary noise.

Every route should declare `response_model` with the matching msgspec struct. Without it,
the OpenAPI schema is incomplete and the serialization path is not validated. Missing
`response_model` on a new route: **Should Fix.**

### Async and blocking I/O

No blocking I/O in async functions. `time.sleep()`, the synchronous `requests` library,
or sync file I/O in an `async def` function: **Must Fix.** Use `asyncio.sleep()`,
`httpx.AsyncClient`, and `aiofiles`.

Broad `except Exception` blocks must log the caught exception. If Ruff rule `BLE001`
flags it, the `# noqa: BLE001` comment must be present. The comment silences a linting
false positive; the block must still be intentional and logged. `except Exception: pass`
without logging: **Must Fix.**

### Logging

Structured format: `logger.warning("module.action key1=value1 key2=value2")`.

Module-level logger: `logger = logging.getLogger(__name__)`.

Use `exc_info=True` when logging exceptions at warning level.

Never log secrets, tokens, or passwords, even at debug level. Token leakage in logs:
**Must Fix.**

### Databases and persistence

SQL injection prevention. All queries must use parameterized statements. String
concatenation or f-strings to build SQL: **Must Fix.**

Schema migrations. If a PR changes the persistence schema (new tables, columns, or data
format in `infrastructure/persistence/`), flag it for explicit human verification.
DroppedNeedle uses file-based persistence (JSON stores, SQLite via AuthStore), not an ORM
with formal migrations.

Crypto initialization. `init_crypto()` must be called before any `encrypt()` or
`decrypt()` operations (wired in `init_app_state` during startup). New code that calls
these without verifying crypto is initialized: **Should Fix.**

### CORS and middleware

Changes to CORS origins or middleware are security-sensitive. Broadening CORS origins
beyond localhost in debug mode, adding middleware that modifies the request/response
lifecycle, or changing middleware ordering in `main.py` should be flagged for careful
review. These changes are not automatically Must Fix, but the reviewer must note the
security implications explicitly.

### Testing

Route tests use isolated `FastAPI()` instances with `dependency_overrides`, not the
production app.

Repository tests use `AsyncMock(spec=httpx.AsyncClient)`. The `spec` ensures only real
methods are mockable.

`autouse` fixtures must reset shared state (circuit breakers, caches, global singletons).

Real HTTP calls in tests: **Must Fix.**

`pytest.mark.asyncio` on all async test functions. Missing the marker: **Should Fix**
unless the project has `asyncio_mode = auto` in pytest config, which makes it redundant.
Only flag if the marker is visibly absent and the test is async.

Tests that mock the layer under test instead of its dependencies (e.g. mocking a service
method inside that service's own test): **Should Fix.** They test nothing meaningful.
Each test layer should mock the layer below it, not itself.

Test fixtures (request bodies, expected responses) can use msgspec structs or plain dicts.
Both are acceptable. If a test constructs a Pydantic model for fixture data: **Should Fix**
(not consistent with the rest of the codebase).

### Security

Hardcoded API keys, tokens, or credentials of any kind: **Must Fix.** Configuration values
come from `config.json` or environment variables.

Sensitive tokens (Plex, Jellyfin) must never appear in client-side responses. The backend
proxies authenticated streams.

Plex token handling in `backend/repositories/plex.py` and related auth service files:
token leakage into URLs, logs, or client-facing API responses: **Must Fix.**

User-controlled input in shell commands, `eval`, or SQL: **Must Fix.**

HTTP clients should follow redirects only to trusted domains. Blind `follow_redirects=True`
to user-provided URLs: **Should Fix.**

HIBP (Have I Been Pwned) password checking uses the k-anonymity API. Only the first 5
characters of the SHA-1 hash are transmitted. If the PR modifies HIBP logic, verify the
full password hash never leaves the server.

HSTS header settings must not be enabled by default. Enabling HSTS on plain HTTP will
break the site. Changes to HSTS or security headers should be verified against the
guidance at Settings > Security.

---

## Frontend (SvelteKit, TypeScript)

### Data fetching

TanStack Query is required for all server-state fetching. Ad-hoc `fetch()` calls in
components for loading API data: **Should Fix.** One-off mutations like form submits are
fine without TanStack Query.

Each query domain needs a `.svelte.ts` query factory file and a `QueryKeyFactory.ts` key
factory file. Missing key factory: **Suggestion.**

Query keys must include all parameters that affect the response (source, filter,
pagination, sort). Missing parameters cause cache collisions: **Should Fix.**

Use `api.global.get()` for global app state and `api.get()` for page-specific data. The
global client uses `globalThis.fetch` and survives navigation. `api.get()` uses SvelteKit's
navigation-aware `fetch` and aborts on navigation. Using `api.get()` for global state
(current user, settings, auth status) or `api.global.get()` for page-specific data that
should abort on navigation: **Should Fix.**

Pass `{ signal }` from `queryFn` context to the API client for proper request
cancellation. Missing `signal`: **Suggestion.**

Cache TTLs should reference `CACHE_TTL` constants from `$lib/constants`. Hardcoded
numbers: **Suggestion.**

### TypeScript

No `any`. Enforced by ESLint (`no-explicit-any: error`). Usage that bypasses this via
`// @ts-ignore` or `as any` without an adjacent comment explaining why the type system
cannot express the correct type: **Should Fix.**

Component props need explicit interfaces. Use `interface Props { ... }` with
`$props<Props>()` or destructured `let { ... }: Props = $props()`.

Prefer `| undefined` for optional values in component props (Svelte 5 convention).
Explicit `| null` is fine when the API or state genuinely distinguishes between absent
and explicitly nulled values. Do not flag this mechanically.

### Svelte 5 runes

Use runes exclusively: `$state()`, `$derived()`, `$effect()`, `$props()`, `$bindable()`.
Svelte 4 patterns (`export let`, `$:`, `on:click`) in new or modified code: **Should Fix.**
The project has fully migrated to Svelte 5; one legacy pattern in an otherwise clean fix
is not merge-blocking.

If a prop is a reactive object or array passed from a parent, destructuring it without
wrapping in `$derived` or using `$props()` bindable can lose reactivity: **Should Fix**
when the prop is clearly meant to be reactive.

`$effect` cleanup functions must be returned when setting timers, intervals, or event
listeners. Missing cleanup: **Must Fix** (memory leak, stale callbacks).

`$derived` must be side-effect-free. Mutating state inside a `$derived` expression:
**Must Fix.**

### SSR safety

Browser-only APIs (`localStorage`, `sessionStorage`, `window`, `document`, `navigator`)
must be guarded by a `browser` check or `onMount` callback. Access at module level or
outside a browser guard will crash during SSR: **Must Fix.** Checking `typeof window !==
'undefined'` or using `browser` from `$app/environment` (as done in
`$lib/stores/authStore.svelte.ts`) is the correct pattern.

TanStack Query definitions should use `enabled: browser` to skip queries during SSR.

### Styling

Colors must reference `$lib/colors` or CSS custom properties (`var(--brand-lastfm)`).
Hardcoded hex, rgb, or rgba values: **Should Fix.**

Use DaisyUI classes for standard components (cards, badges, buttons, alerts, toasts,
spinners). Custom CSS that duplicates DaisyUI functionality: **Suggestion.**

### Components and UX

Async event handlers (button clicks, form submissions) must catch errors and show
user-facing feedback, typically via a toast. Silent failures on user-initiated actions:
**Should Fix.**

When TanStack Query returns an error state, the component should render a meaningful
error message or fallback UI. Missing error UI for API data: **Should Fix.**

Components that fetch data should handle the loading state (TanStack Query's `isLoading`
or `isPending`). Missing loading indicators on data-driven pages: **Suggestion.**

Interactive elements without visible text need `aria-label`. Missing labels on icon-only
buttons: **Should Fix.**

Named exports are preferred for `.ts` utility modules. For `.svelte` component files,
default exports are the norm. Do not flag default exports from `.svelte` files.

After the multi-user merge, UI components that render based on user role should use
`authStore` (`$lib/stores/authStore.svelte.ts`) for the current user and role.
Hardcoding role checks or duplicating auth logic in components: **Suggestion.**

### Testing

Every test has at least one assertion (enforced by Vitest config).

Component tests use Playwright (client project). Server-project tests cover utilities,
stores, and query logic.

Mock external services rather than making real network calls.

---

## Cross-Cutting

### Dependencies

New production dependencies should be justified. Importing a large library for a single
utility function: **Suggestion.**

Lockfiles must be updated. `package.json` changes without a `pnpm-lock.yaml` update:
**Must Fix** for new production dependencies (reproducibility bug), **Should Fix** for
dev dependency changes. `requirements.txt` additions should pin exact versions (e.g.
`httpx==0.28.1`): **Should Fix.** Dockerfile changes that only bump a Python version or
environment variable do not need lockfile updates.

New Python packages added to `requirements.txt` must also appear in
`requirements-dev.txt` if they are needed at test time. Missing test dependency: **Should
Fix.**

### Docker and infrastructure

Dockerfile, docker-compose.yml, and CI workflow changes need extra caution. These affect
build, deployment, and the security boundary. Flag any change that weakens the container
security model: removing `gosu`, removing `tini`, running as root, removing the `umask`,
removing health checks, or broadening volume mounts.

New volumes or bind mounts should use `:ro` (read-only) unless write access is necessary.
Writable mounts that are not justified: **Suggestion.**

### Documentation

Public API endpoints need OpenAPI metadata. Missing `response_model`, `tags`, or
`description` on new route handlers: **Should Fix.** The OpenAPI schema is the project's
API contract.

Complex logic benefits from docstrings across both frontend and backend: stores, services,
utilities, and non-obvious query logic. **Suggestion,** not required.

README or documentation changes that accompany a feature PR should be internally consistent
with the code changes.

### Performance

N+1 query patterns. A repository method called inside a loop that could be batched:
**Should Fix.**

Missing caching on expensive lookups. Repeated identical external API calls within the
same request lifecycle: **Should Fix.**

Large lists rendered without virtualisation. If a page renders 100+ items and each makes
API calls: **Suggestion.**

---

## What the AI Reviewer Should Ignore

Code formatting. Prettier (frontend) and Ruff (backend) handle this in CI. Do not comment
on whitespace, line length, or import ordering.

Style preferences not covered above: naming style, function length, variable ordering.
These are for humans to decide.

Generic positive feedback. Avoid "looks good", "nice work", "LGTM". Brief, specific praise
for a genuinely good architectural choice is fine. One sentence at most.

Comments on self-evident code. Do not suggest adding explanations for what the code
obviously does. Comments are appropriate for complex algorithms, non-obvious workarounds,
public API contracts, or places where the code's intent differs from its implementation.

Test coverage percentages. Flag missing tests for specific edge cases in new logic. Do not
comment on coverage metrics.

---

## Incremental Reviews

When new commits are pushed to an existing PR, review only the files changed in the new
commits. The summary should note whether the fix addresses prior review comments.

If the new changes modify a shared utility, type definition, or dependency consumed by
previously reviewed files, flag it explicitly: "This change modifies `X`, which is used
by previously reviewed files `A`, `B`, `C`. Verify those consumers still behave correctly."

In a stateless workflow (fresh execution on each push), the AI can infer which files were
previously reviewed from the PR description, prior bot comments in the review history, or
by comparing the diff range. If no prior context is available, focus on the current diff
and note any shared utilities that might have impact beyond the changed files.

---

## Comment Format

Keep inline comments concise and actionable. Follow this shape:

**Good (specific, with reasoning):**

> `@with_retry` is missing on this external API call. If the upstream service is flaky, a
> single transient failure will propagate to the user as an error instead of being retried.
> Add the decorator with `retriable_exceptions` matching the repository pattern.

**Bad (vague, no specificity, no reasoning):**

> This function could use better error handling.
