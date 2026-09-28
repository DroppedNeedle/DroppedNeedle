# Capture 05 — Publisher failure scenarios

## What it pins

The v2 Library Management sealed-preview / apply / undo / journal contract:

- **Sealed-preview token:** live-run `_sealed_preview_token()` — determinism,
  sensitivity to both `job_id` and `idempotency_key`, token sha256 + length.
- **Stable JSON:** live-run `_stable_json()` — key-order-insensitive
  canonicalization with a canonical sample.
- **Undo token:** live-run `undo._token()` — deterministic and distinct from
  the preview token for the same inputs.
- **Gate vocabulary:** upper-case constants exported by the publisher module
  (journal/validation gate names).
- **Scenario catalog:** every `test_*` name + docstring first line from the
  preview (30), publisher (69), undo (6), recovery (33), mutation-boundary
  (3), worker (20), and target-application-lifecycle (2) suites — the
  sealed-preview-stale / apply-reject / undo / journal-recovery checklist v3
  must re-satisfy.
- **Exception inventory:** per-module defined exception classes + every
  `raise X` name referenced in publisher / preview / undo / recovery
  (the failure alphabet).

No bundle was published during capture: publisher flows require a live library
DB plus real music files under management, so end-to-end apply/undo/journal
runs are cataloged (test scenarios + failure alphabet), not executed — see
top-level README.

## Method

Live-run pure functions + static pins: `/tmp/vcap/cap05_pub.py`
(`cd backend && .venv/bin/python /tmp/vcap/cap05_pub.py`). Read-only.

## Consumed by (v3)

- **Stage 3 (library management):** token/canonicalization parity — v3 must
  mint identical tokens for identical inputs (cross-version idempotency).
- **Stage 4 (safety/recovery):** scenario catalog + exception inventory become
  the v3 publisher failure-suite test plan (stale-preview rejects, journal
  recovery, undo correctness).
