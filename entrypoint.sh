#!/bin/sh
set -e

# A bind mount over /app shadows the image's application code with the user's
# data directory; fail fast here instead of emitting confusing writability or
# runtime errors against the shadowing mount later. /app is always present in
# the image, so a missing /app means we are outside the container (e.g. tests
# or a hand-run shell) and the check is skipped.
if [ -d /app ] && { [ ! -f /app/main.py ] || [ ! -f /app/.droppedneedle-source-revision ] || [ ! -f /app/maintenance/automatic_upgrade.py ]; }; then
    echo "[init] FATAL: /app does not contain the DroppedNeedle application code."
    echo "[init]   A bind mount over /app hides the application with your data."
    echo "[init]   Mount data subdirectories only: /app/config, /app/cache, /app/plugins, /app/imports."
    echo "[init]   Remove the /app bind mount and restart."
    exit 1
fi

REQUESTED_UMASK=${UMASK:-027}
case "$REQUESTED_UMASK" in
    [0-7][0-7][0-7]|0[0-7][0-7][0-7]) ;;
    *) echo "[init] FATAL: UMASK='$REQUESTED_UMASK' must be three or four octal digits."; exit 1;;
esac
umask "$REQUESTED_UMASK"

PUID=${PUID:-1000}
PGID=${PGID:-1000}

case "$PUID" in ''|*[!0-9]*) echo "[init] FATAL: PUID='$PUID' is not a valid numeric UID."; exit 1;; esac
case "$PGID" in ''|*[!0-9]*) echo "[init] FATAL: PGID='$PGID' is not a valid numeric GID."; exit 1;; esac

# Rewrite the frontend's build-time base-path placeholder (baked by the Dockerfile) to
# the runtime BASE_PATH, so one image serves any reverse-proxy subpath. Empty = root.
apply_base_path() {
    _static="/app/static"
    [ -d "$_static" ] || return 0
    _base=$(printf '%s' "${BASE_PATH:-}" | sed 's#/*$##')
    case "$_base" in ""|/*) ;; *) _base="/$_base" ;; esac
    _files=$(grep -rlF '/__DN_BASE_PATH__' "$_static" 2>/dev/null || true)
    [ -n "$_files" ] || return 0
    printf '%s\n' "$_files" | while IFS= read -r _f; do
        if ! sed -i "s#/__DN_BASE_PATH__#${_base}#g" "$_f" 2>/dev/null; then
            echo "[init] WARNING: could not rewrite '$_f' for BASE_PATH (not writable?); frontend may be broken."
        fi
    done
    echo "[init] Serving frontend under base path '${_base:-/}'."
}
apply_base_path || true

check_writable() {
    _dir="$1"
    _identity="$2"
    _probe="$_dir/.droppedneedle_write_test_$$"
    if [ -n "$_identity" ]; then
        gosu "$_identity" touch "$_probe" 2>/dev/null; _rc=$?
        gosu "$_identity" rm -f "$_probe" 2>/dev/null
    else
        touch "$_probe" 2>/dev/null; _rc=$?
        rm -f "$_probe" 2>/dev/null
    fi
    return "$_rc"
}

if [ "$(id -u)" -ne 0 ]; then
    echo "[init] Running as uid=$(id -u) gid=$(id -g) (non-root); skipping user/group setup."
    for dir in /app/cache /app/config; do
        mkdir -p "$dir" 2>/dev/null || true
        if ! check_writable "$dir"; then
            echo "[init] FATAL: $dir is not writable by uid=$(id -u)."
            echo "[init]   Ensure the host directory is owned by this UID/GID."
            echo "[init]   Run: chown $(id -u):$(id -g) <host-path>"
            exit 1
        fi
    done
    exec "$@"
fi

# Only remap when the baked identity differs from the requested PUID/PGID.
# usermod/groupmod can stall for minutes on some storage backends, and they
# block startup before uvicorn execs, so skip them entirely when not needed.
if [ "$(id -g droppedneedle)" != "$PGID" ]; then
    if ! groupmod -o -g "$PGID" droppedneedle 2>/dev/null; then
        echo "[init] WARNING: Could not set droppedneedle group to GID=$PGID."
    fi
fi
if [ "$(id -u droppedneedle)" != "$PUID" ]; then
    if ! usermod -o -u "$PUID" droppedneedle 2>/dev/null; then
        echo "[init] WARNING: Could not set droppedneedle user to UID=$PUID."
    fi
fi

TARGET_UID=$(id -u droppedneedle)
TARGET_GID=$(id -g droppedneedle)
echo "[init] Runtime user: droppedneedle (uid=$TARGET_UID gid=$TARGET_GID)"

if [ "$TARGET_UID" != "$PUID" ]; then
    echo "[init] WARNING: Requested PUID=$PUID but running as uid=$TARGET_UID (usermod may have failed)."
fi
if [ "$TARGET_GID" != "$PGID" ]; then
    echo "[init] WARNING: Requested PGID=$PGID but running as gid=$TARGET_GID (groupmod may have failed)."
fi

for dir in /app/cache /app/config; do
    mkdir -p "$dir" 2>/dev/null || true

    if check_writable "$dir" "$TARGET_UID:$TARGET_GID"; then
        continue
    fi

    if chown droppedneedle:droppedneedle "$dir" 2>/dev/null; then
        echo "[init] Adjusted ownership of $dir - verifying write access."
    else
        echo "[init] WARNING: Could not chown $dir (mount may not support ownership changes)."
    fi

    if ! check_writable "$dir" "$TARGET_UID:$TARGET_GID"; then
        echo "[init] FATAL: $dir is not writable by uid=$TARGET_UID gid=$TARGET_GID."
        echo "[init]   Common causes: FUSE/shfs (Unraid), NFS root_squash, CIFS/SMB, dropped CAP_CHOWN."
        echo "[init]   Fix: ensure the host directory is writable by uid=$TARGET_UID gid=$TARGET_GID."
        exit 1
    fi
done

exec gosu droppedneedle:droppedneedle "$@"
