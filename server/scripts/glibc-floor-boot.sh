#!/usr/bin/env bash
# Starts the release binaries on the oldest systems they promise to run on,
# glibc 2.28 and 2.31, and waits for the server to report healthy.
#
#   server/scripts/glibc-floor-boot.sh <server-dir> <web-ui-static-dir>
#
# LD_BIND_NOW makes a missing glibc symbol stop the start, not the first call
# that needs it. Needs docker.
set -euo pipefail

if [[ $# -ne 2 ]]; then
    echo "usage: $0 <server-dir> <web-ui-static-dir>" >&2
    exit 2
fi
server=$(cd "$1" && pwd)
static=$(cd "$2" && pwd)

boot='
set -eu
/opt/dn/droppedneedle-tool --help > /dev/null
mkdir -p /tmp/dn
chown 65534:65534 /tmp/dn
setpriv --reuid=65534 --regid=65534 --clear-groups -- env \
    ROOT_APP_DIR=/tmp/dn DROPPEDNEEDLE_STATIC_DIR=/opt/web NO_COLOR=1 \
    /opt/dn/droppedneedle > /tmp/server.log 2>&1 &
for _ in $(seq 1 60); do
    if /opt/dn/droppedneedle --health-check > /dev/null 2>&1; then
        echo "healthy"
        exit 0
    fi
    sleep 1
done
cat /tmp/server.log >&2
echo "the server did not report healthy within 60 seconds" >&2
exit 1
'

for image in rockylinux:8 ubuntu:20.04; do
    echo "== $image"
    docker run --rm -e LD_BIND_NOW=1 \
        -v "$server:/opt/dn:ro" -v "$static:/opt/web:ro" "$image" sh -c "$boot"
done
