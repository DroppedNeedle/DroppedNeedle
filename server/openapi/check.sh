#!/bin/sh
# Contract drift gate: utoipa -> OpenAPI -> openapi-typescript.
#
# Regenerates both artifacts from the Rust source and diffs them against the
# committed files. Any drift fails the run, which is exactly what CI checks.
# Run with --write to refresh the committed files after a contract change.
#
# The TypeScript generator version is pinned here so local runs and CI
# produce byte-identical output. Bump deliberately, with a regen + diff review.
set -eu

OPENAPI_TS_VERSION="7.13.0"

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
SNAPSHOT="$ROOT/server/openapi/openapi.json"
TYPES="$ROOT/frontend/src/lib/api/v3/openapi.d.ts"

WRITE=0
if [ "${1:-}" = "--write" ]; then
  WRITE=1
elif [ "${1:-}" != "" ]; then
  echo "usage: check.sh [--write]" >&2
  exit 2
fi

TMP_DIR=$(mktemp -d)
trap 'rm -rf "$TMP_DIR"' EXIT INT TERM
FRESH_JSON="$TMP_DIR/openapi.json"
FRESH_TS="$TMP_DIR/openapi.d.ts"

cargo run --quiet --manifest-path "$ROOT/server/Cargo.toml" -- --print-openapi > "$FRESH_JSON"
pnpm dlx "openapi-typescript@$OPENAPI_TS_VERSION" "$FRESH_JSON" -o "$FRESH_TS" >/dev/null

if [ "$WRITE" = "1" ]; then
  cp "$FRESH_JSON" "$SNAPSHOT"
  cp "$FRESH_TS" "$TYPES"
  echo "contract refreshed: OpenAPI snapshot + generated TypeScript"
  exit 0
fi

FAILED=0
if ! diff -u "$SNAPSHOT" "$FRESH_JSON"; then
  echo "drifted: $SNAPSHOT does not match --print-openapi output" >&2
  FAILED=1
fi
if ! diff -u "$TYPES" "$FRESH_TS"; then
  echo "drifted: $TYPES does not match a fresh openapi-typescript run" >&2
  FAILED=1
fi
if [ "$FAILED" = "1" ]; then
  echo "run server/openapi/check.sh --write to refresh, then review the diff" >&2
  exit 1
fi
echo "contract clean: OpenAPI snapshot and TypeScript match the code"
