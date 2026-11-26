#!/bin/sh
# Runs a cargo command in the image build through sccache when the build
# installed it (SCCACHE=on, see the Dockerfile), and as it is otherwise. The
# cache is the BuildKit cache mount at /sccache. The server is stopped at the
# end, so its last entries are written before the step's layer is taken.
set -e
sccache=/opt/sccache/bin/sccache
[ -x "$sccache" ] || exec "$@"
export RUSTC_WRAPPER="$sccache" SCCACHE_DIR=/sccache SCCACHE_CACHE_SIZE=5G
status=0
"$@" || status=$?
"$sccache" --stop-server
exit "$status"
