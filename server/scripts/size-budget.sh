#!/usr/bin/env bash
# Checks the release files against the ceilings in server/size-budget.txt.
# Prints each entry's size, ceiling and headroom, also to the job summary
# when GITHUB_STEP_SUMMARY is set, and fails when an entry is missing or
# over its ceiling.
#
# Usage: size-budget.sh <release-files dir>
set -euo pipefail

if [[ $# -ne 1 || ! -d "$1" ]]; then
  echo "usage: $0 <release-files dir>" >&2
  exit 2
fi
root="$1"
budget="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/size-budget.txt"

fail() {
  if [[ -n "${GITHUB_ACTIONS:-}" ]]; then
    echo "::error::$1"
  fi
  echo "error: $1" >&2
  failed=1
}

mb() {
  awk -v b="$1" 'BEGIN { printf "%.1f", b / 1000000 }'
}

failed=0
rows=()
while read -r path ceiling _; do
  [[ -z "$path" || "$path" == \#* ]] && continue
  if [[ ! "$ceiling" =~ ^[0-9]+(\.[0-9]+)?$ ]]; then
    fail "server/size-budget.txt: '$path' has no ceiling in MB"
    continue
  fi
  target="$root/$path"
  if [[ -f "$target" ]]; then
    bytes="$(stat -c %s "$target")"
  elif [[ -d "$target" ]]; then
    bytes="$(find "$target" -type f -printf '%s\n' | awk '{ s += $1 } END { print s + 0 }')"
  else
    fail "$path is not in $root"
    continue
  fi
  limit="$(awk -v c="$ceiling" 'BEGIN { printf "%.0f", c * 1000000 }')"
  headroom=$((limit - bytes))
  rows+=("$path|$(mb "$bytes") MB|$ceiling MB|$(mb "$headroom") MB")
  if ((bytes > limit)); then
    fail "$path is $(mb "$bytes") MB, over its $ceiling MB ceiling in server/size-budget.txt"
  fi
done < "$budget"

printf '%-28s %10s %10s %10s\n' path size ceiling headroom
for row in "${rows[@]}"; do
  IFS='|' read -r path size ceiling headroom <<< "$row"
  printf '%-28s %10s %10s %10s\n' "$path" "$size" "$ceiling" "$headroom"
done

if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
  {
    echo "### Size budget"
    echo
    echo "| Path | Size | Ceiling | Headroom |"
    echo "|---|---|---|---|"
    for row in "${rows[@]}"; do
      echo "| ${row//|/ | } |"
    done
  } >> "$GITHUB_STEP_SUMMARY"
fi

exit "$failed"
