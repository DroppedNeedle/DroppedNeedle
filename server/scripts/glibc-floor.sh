#!/usr/bin/env bash
# Fails when a binary needs a newer glibc than the floor the docs promise.
#
#   server/scripts/glibc-floor.sh <floor> <binary>...
#
# Reads every GLIBC_ version each binary asks the dynamic linker for, weak
# symbols included, and lists the symbols above the floor when one is.
set -euo pipefail

if [[ $# -lt 2 ]]; then
    echo "usage: $0 <floor> <binary>..." >&2
    exit 2
fi
floor=$1
shift

failed=0
for binary in "$@"; do
    newest=$(readelf --version-info --wide "$binary" \
        | grep -o 'GLIBC_[0-9][0-9.]*' | sed 's/^GLIBC_//' | sort -uV | tail -n 1)
    if [[ -z "$newest" ]]; then
        echo "$binary: no glibc versions found" >&2
        failed=1
        continue
    fi
    if [[ "$(printf '%s\n' "$floor" "$newest" | sort -V | tail -n 1)" != "$floor" ]]; then
        echo "$binary needs glibc $newest, above the $floor floor, for:" >&2
        objdump -T "$binary" | awk -v floor="$floor" '
            match($0, /GLIBC_[0-9][0-9.]*/) {
                version = substr($0, RSTART + 6, RLENGTH - 6)
                split(version, a, "."); split(floor, b, ".")
                if (a[1] > b[1] || (a[1] == b[1] && a[2] + 0 > b[2] + 0)) print "  " $NF " (" version ")"
            }' >&2
        failed=1
        continue
    fi
    echo "$binary: needs glibc $newest at most, within the $floor floor"
done
exit "$failed"
