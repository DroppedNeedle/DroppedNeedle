#!/usr/bin/env bash
# Every rename, copy, link and delete of a file in a library folder
# goes through the staged publisher in server/library/src/publish/. This
# fails on any such std::fs or tokio::fs call elsewhere in
# server/library/src/, except the lines listed below.
#
# Outside this crate, playlist export (server/kernel/src/jobs/) writes its
# own playlist files into a library folder and is the one other writer
# there.
set -euo pipefail

src="$(cd "$(dirname "${BASH_SOURCE[0]}")/../library/src" && pwd)"

# Allowed lines, as "<file under library/src>|<the line, trimmed>", each
# with its reason. An entry that no longer matches a line fails the gate.
allowed=(
  # The tag save wrapper's own save: a hidden copy beside the file, saved
  # and renamed over it. Only tests and fixtures call it; the publisher
  # saves tags in its journal temp through save_in_place.
  "tags/save/mod.rs|fs::rename(&temp, path).map_err(|source| TagsError::Io {"
  "tags/save/mod.rs|let _ = fs::remove_file(&temp);"
  "tags/save/mod.rs|return match fs::copy(original, &temp) {"
  "tags/save/mod.rs|let _ = fs::remove_file(&temp);"
  # Adding a library folder checks it is writable with an empty probe file
  # of its own, removed at once. It is never a music file.
  "management/settings/activation.rs|let _ = std::fs::remove_file(&probe);"
  # The walk's unit test makes a link loop in a temp folder to prove the
  # walk never follows one.
  "scan/walk.rs|std::os::unix::fs::symlink(&root, &link).expect(\"symlink\");"
  # The placement unit test removes its own scratch settings folder in
  # the system temp folder, never a library file.
  "management/placement.rs|std::fs::remove_dir_all(&dir).expect(\"scratch dir removed\");"
)

names='rename|copy|hard_link|symlink|soft_link|remove_file|remove_dir|remove_dir_all'
calls="\\bfs::($names)\\b"
imports="\\buse\\s+(std|tokio)::fs::(\\{[^}]*\\b)?($names)\\b"
# A whole-module import, an alias or a use list split over lines would
# hide a bare call, so those are refused too.
wide='\buse\s+(std|tokio)::fs(::\*|\s+as\s|::\{[^}]*$)'

hits="$(cd "$src" && grep -rnE --include='*.rs' -e "$calls" -e "$imports" -e "$wide" .)" \
  || [ $? -eq 1 ]
found="$(sed 's#^\./##' <<<"$hits" | grep -v '^publish/' || true)"

declare -A budget=()
for entry in "${allowed[@]}"; do
  budget["$entry"]=$(( ${budget["$entry"]:-0} + 1 ))
done

status=0
while IFS= read -r hit; do
  [ -z "$hit" ] && continue
  file="${hit%%:*}"
  rest="${hit#*:}"
  line="${rest#*:}"
  trimmed="$(sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//' <<<"$line")"
  key="$file|$trimmed"
  if [ "${budget["$key"]:-0}" -gt 0 ]; then
    budget["$key"]=$(( budget["$key"] - 1 ))
  else
    echo "library/src/${hit%%:*}:$(cut -d: -f2 <<<"$hit"): file change outside the publisher: $trimmed" >&2
    status=1
  fi
done <<<"$found"

for key in "${!budget[@]}"; do
  if [ "${budget["$key"]}" -gt 0 ]; then
    echo "allowed line no longer found in library/src/${key%%|*}: ${key#*|}" >&2
    status=1
  fi
done

if [ "$status" -ne 0 ]; then
  echo "Move the file change into server/library/src/publish/, or list it in $0 with its reason." >&2
fi
exit "$status"
