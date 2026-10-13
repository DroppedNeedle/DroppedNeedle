#!/bin/sh
# Contract drift gate: utoipa -> OpenAPI -> openapi-typescript.
#
# Regenerates both artifacts from the Rust source and diffs them against the
# committed files. Any drift fails the run, which is exactly what CI checks.
# Run with --write to refresh the committed files after a contract change.
#
# The document comes from the server binary's --print-openapi, the build
# that ships. With --from-tests it comes from the integration-test binary
# instead, which a working tree has usually built already; the make targets
# use that, and CI keeps the shipping build.
#
# The TypeScript generator version is pinned here so local runs and CI
# produce byte-identical output. Bump deliberately, with a regen + diff review.
set -eu

OPENAPI_TS_VERSION="7.13.0"

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
SNAPSHOT="$ROOT/server/openapi/openapi.json"
TYPES="$ROOT/frontend/src/lib/api/v3/openapi.d.ts"

WRITE=0
FROM_TESTS=0
for arg in "$@"; do
  case "$arg" in
    --write) WRITE=1 ;;
    --from-tests) FROM_TESTS=1 ;;
    *)
      echo "usage: check.sh [--from-tests] [--write]" >&2
      exit 2
      ;;
  esac
done

TMP_DIR=$(mktemp -d)
trap 'rm -rf "$TMP_DIR"' EXIT INT TERM
FRESH_JSON="$TMP_DIR/openapi.json"
FRESH_TS="$TMP_DIR/openapi.d.ts"

if [ "$FROM_TESTS" = "1" ]; then
  if ! DN_OPENAPI_OUT="$FRESH_JSON" cargo test --quiet --manifest-path "$ROOT/server/Cargo.toml" \
    --test it -- --ignored --exact contract::write_openapi_document > "$TMP_DIR/test.log"; then
    cat "$TMP_DIR/test.log" >&2
    exit 1
  fi
else
  cargo run --quiet --manifest-path "$ROOT/server/Cargo.toml" --bin droppedneedle -- --print-openapi > "$FRESH_JSON"
fi
pnpm dlx "openapi-typescript@$OPENAPI_TS_VERSION" "$FRESH_JSON" -o "$FRESH_TS" >/dev/null

if [ "$WRITE" = "1" ]; then
  cp "$FRESH_JSON" "$SNAPSHOT"
  cp "$FRESH_TS" "$TYPES"
  echo "contract refreshed: OpenAPI snapshot + generated TypeScript"
  exit 0
fi

FAILED=0
if ! diff -u "$SNAPSHOT" "$FRESH_JSON"; then
  echo "drifted: $SNAPSHOT does not match the document the code builds" >&2
  FAILED=1
fi
if ! diff -u "$TYPES" "$FRESH_TS"; then
  echo "drifted: $TYPES does not match a fresh openapi-typescript run" >&2
  FAILED=1
fi
if [ "$FAILED" = "1" ]; then
  echo "run make contract-write to refresh, then review the diff" >&2
  exit 1
fi
echo "contract clean: OpenAPI snapshot and TypeScript match the code"
