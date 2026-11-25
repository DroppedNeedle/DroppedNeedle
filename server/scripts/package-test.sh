#!/usr/bin/env bash
# Installs the .deb and the .rpm that packaging/build.sh made on the systems
# they promise to run on, and checks each one serves.
#
#   server/scripts/package-test.sh <package-dir>
#
# In each container: install the package with the system's own package
# manager, check its ffmpeg has the encoders transcoding uses, run
# droppedneedle-tool through its wrapper, start the server as the service
# account with the unit's variables and wait for it to report healthy. Then
# hold the unit's systemd-analyze security score at the threshold below.
# Exits non-zero on the first failure. Needs docker and systemd-analyze.
set -euo pipefail

if [[ $# -ne 1 ]]; then
    echo "usage: $0 <package-dir>" >&2
    exit 2
fi
packages=$(cd "$1" && pwd)

# The unit's exposure score in tenths (1.6 under systemd 257): a change that
# weakens it fails here.
security_threshold=16

shopt -s nullglob
debs=("$packages"/*.deb)
rpms=("$packages"/*.rpm)
shopt -u nullglob
if [[ ${#debs[@]} -ne 1 || ${#rpms[@]} -ne 1 ]]; then
    echo "expected one .deb and one .rpm in $packages" >&2
    exit 1
fi
deb=$(basename "${debs[0]}")
rpm=$(basename "${rpms[0]}")

# Runs in each container once the package is installed.
check='
set -eu
encoders=$(ffmpeg -hide_banner -encoders)
for encoder in aac flac libmp3lame libopus; do
    echo "$encoders" | grep -qw "$encoder" || { echo "ffmpeg has no $encoder encoder" >&2; exit 1; }
done
droppedneedle-tool --help > /dev/null
setpriv --reuid=droppedneedle --regid=droppedneedle --init-groups -- env \
    ROOT_APP_DIR=/var/lib/droppedneedle \
    DROPPEDNEEDLE_STATIC_DIR=/usr/share/droppedneedle/web-ui \
    NO_COLOR=1 \
    /usr/lib/droppedneedle/droppedneedle > /tmp/server.log 2>&1 &
for _ in $(seq 1 60); do
    if /usr/lib/droppedneedle/droppedneedle --health-check > /dev/null 2>&1; then
        echo "healthy"
        exit 0
    fi
    sleep 1
done
cat /tmp/server.log >&2
echo "the server did not report healthy within 60 seconds" >&2
exit 1
'

run() {
    local image=$1 install=$2
    echo "== $image"
    docker run --rm -v "$packages:/packages:ro" "$image" sh -c "$install && $check"
}

quiet_apt='export DEBIAN_FRONTEND=noninteractive; apt-get update -qq && apt-get install -y -qq'
run debian:12 "$quiet_apt /packages/$deb > /dev/null"
run ubuntu:22.04 "$quiet_apt /packages/$deb > /dev/null"
run fedora:latest "dnf install -y -q /packages/$rpm > /dev/null"
# RHEL's ffmpeg-free comes from EPEL, which needs CodeReady Builder.
run almalinux:9 "dnf install -y -q dnf-plugins-core epel-release > /dev/null \
    && dnf config-manager --set-enabled crb && dnf install -y -q /packages/$rpm > /dev/null"

echo "== systemd-analyze security"
units=$(mktemp -d)
trap 'rm -rf "$units"' EXIT
dpkg-deb --fsys-tarfile "$packages/$deb" \
    | tar -xO ./usr/lib/systemd/system/droppedneedle.service > "$units/droppedneedle.service"
systemd-analyze security --offline=true --threshold="$security_threshold" \
    "$units/droppedneedle.service" | tail -n 1
