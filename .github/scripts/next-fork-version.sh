#!/usr/bin/env bash

set -euo pipefail

# Select the nearest reachable tag that exactly matches a plain stable upstream
# version. Filtering before distance comparison skips every prerelease syntax as
# well as legacy -fork.N and this fork's .postN tags.
base=""
best_distance=""
while IFS= read -r candidate; do
  if [[ ! "$candidate" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    continue
  fi
  distance=$(git rev-list --count "${candidate}..HEAD")
  if [[ -z "$best_distance" || "$distance" -lt "$best_distance" ]]; then
    base=$candidate
    best_distance=$distance
  fi
done < <(git tag --merged HEAD --list 'v*')

if [[ -z "$base" ]]; then
  echo "No reachable plain upstream version tag found" >&2
  exit 1
fi

# A rerun of an already released fork commit must not mint another release.
escaped_base=${base//./\.}
if git tag --points-at HEAD | grep -Eq "^${escaped_base}\.post[0-9]+$"; then
  exit 0
fi

last_n=$(git tag -l "${base}.post*" \
  | sed -nE "s/^${escaped_base}\.post([0-9]+)$/\1/p" \
  | sort -n | tail -1)
next_n=$(( ${last_n:-0} + 1 ))

printf '%s.post%s\n' "$base" "$next_n"
