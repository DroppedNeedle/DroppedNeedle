#!/bin/sh
# Container entrypoint: apply UMASK, run the given command as PUID/PGID.
#
# Started as root (the default), it moves the baked droppedneedle user to
# PUID/PGID, hands the data folders over to it, and drops to it with gosu.
# Started with `docker run --user`, it skips user setup and runs the command
# as that user. Any command works, e.g. `droppedneedle-tool upgrade`, which
# then writes its files as the same user the server runs as. When started as
# root, the server checks and chowns /app/config, /app/cache, /app/plugins and
# /app/imports where the runtime user cannot write them. With --user, it checks
# only /app/config and /app/cache. The tool reports a folder it cannot write to
# itself.
set -e

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

if [ "$#" -eq 0 ]; then
    set -- droppedneedle
fi

is_server() {
    [ "$(basename -- "$1")" = "droppedneedle" ]
}

# Probe write access by creating and removing a file, as the given
# uid:gid when one is passed, else as the current user.
check_writable() {
    _probe="$1/.droppedneedle_write_test_$$"
    if [ -n "$2" ]; then
        gosu "$2" touch "$_probe" 2>/dev/null; _rc=$?
        gosu "$2" rm -f "$_probe" 2>/dev/null
    else
        touch "$_probe" 2>/dev/null; _rc=$?
        rm -f "$_probe" 2>/dev/null
    fi
    return "$_rc"
}

# Makes $1 writable by the runtime user, chowning the folder itself when it is
# not. Returns 1 if it still is not. The chown never reaches inside the folder.
make_writable() {
    if check_writable "$1" "$TARGET_UID:$TARGET_GID"; then
        return 0
    fi
    if chown -h droppedneedle:droppedneedle "$1" 2>/dev/null; then
        echo "[init] Adjusted ownership of $1; checking write access again."
    else
        echo "[init] WARNING: Could not chown $1 (the mount may not support ownership changes)."
    fi
    check_writable "$1" "$TARGET_UID:$TARGET_GID"
}

if [ "$(id -u)" -ne 0 ]; then
    echo "[init] Running as uid=$(id -u) gid=$(id -g) (non-root); skipping user setup."
    if is_server "$1"; then
        for dir in /app/config /app/cache; do
            mkdir -p "$dir" 2>/dev/null || true
            if ! check_writable "$dir"; then
                echo "[init] FATAL: $dir is not writable by uid=$(id -u)."
                echo "[init]   Make the host directory writable by this user:"
                echo "[init]   chown $(id -u):$(id -g) <host-path>"
                exit 1
            fi
        done
    fi
    exec "$@"
fi

# Remap only when the baked ids differ: usermod and groupmod can stall for
# minutes on some storage backends. The baked home is /nonexistent, so
# usermod edits the account files and walks no folder.
if [ "$(id -g droppedneedle)" != "$PGID" ]; then
    groupmod -o -g "$PGID" droppedneedle 2>/dev/null \
        || echo "[init] WARNING: Could not set the droppedneedle group to GID=$PGID."
fi
OLD_UID=$(id -u droppedneedle)
if [ "$OLD_UID" != "$PUID" ]; then
    usermod -o -d /nonexistent -u "$PUID" droppedneedle 2>/dev/null \
        || echo "[init] WARNING: Could not set the droppedneedle user to UID=$PUID."
fi
TARGET_UID=$(id -u droppedneedle)
TARGET_GID=$(id -g droppedneedle)
if [ "$TARGET_UID" != "$PUID" ] || [ "$TARGET_GID" != "$PGID" ]; then
    echo "[init] WARNING: Requested $PUID:$PGID but running as $TARGET_UID:$TARGET_GID."
fi

# The four folders the server writes belong to the runtime user. The uid that
# owns /app/config is the one the server last ran as: when it differs from the
# runtime uid (a remap, or a PUID set back to the baked 1000), the files that
# uid owns in the four folders move to the runtime user. Only that uid's files
# move, so files another account owns in a shared folder (often imports) are
# never touched. A root-owned config is left to the write check below.
# -depth moves each folder itself last, so a stopped hand-over runs again. find
# does not follow links, and chown -h changes a link itself, never its target.
# A folder that refuses the change gets a warning, not a stop; the write check
# for config and cache below still decides whether the start can go on.
PREV_UID=$(stat -c %u /app/config 2>/dev/null || echo "$TARGET_UID")
if [ "$PREV_UID" != "$TARGET_UID" ] && [ "$PREV_UID" != 0 ]; then
    echo "[init] Handing the data folders over from uid=$PREV_UID to uid=$TARGET_UID."
    for dir in /app/config /app/cache /app/plugins /app/imports; do
        [ -d "$dir" ] || continue
        if ! find "$dir" -depth -user "$PREV_UID" -exec chown -h "$TARGET_UID:$TARGET_GID" {} + 2>/dev/null; then
            echo "[init] WARNING: Could not hand $dir over to uid=$TARGET_UID (the mount may not support ownership changes)."
        fi
    done
fi

if is_server "$1"; then
    echo "[init] Runtime user: droppedneedle (uid=$TARGET_UID gid=$TARGET_GID)"
    for dir in /app/config /app/cache; do
        mkdir -p "$dir" 2>/dev/null || true
        if [ ! -d "$dir" ]; then
            echo "[init] FATAL: $dir is not a folder."
            exit 1
        fi
        if ! make_writable "$dir"; then
            echo "[init] FATAL: $dir is not writable by uid=$TARGET_UID gid=$TARGET_GID."
            echo "[init]   Common causes: FUSE/shfs (Unraid), NFS root_squash, CIFS/SMB, dropped CAP_CHOWN."
            echo "[init]   Fix: make the host directory writable by uid=$TARGET_UID gid=$TARGET_GID."
            exit 1
        fi
    done
    # Plugins and imports only warn: a read-only plugins mount starts today, and
    # a stop would take the whole app down for one feature.
    for dir in /app/plugins /app/imports; do
        mkdir -p "$dir" 2>/dev/null || true
        if [ -L "$dir" ] || [ ! -d "$dir" ]; then
            echo "[init] WARNING: $dir is a link or not a folder; it is left as it is."
            continue
        fi
        if ! make_writable "$dir"; then
            echo "[init] WARNING: $dir is not writable by uid=$TARGET_UID gid=$TARGET_GID; writes there will fail."
            echo "[init]   Common causes: a mount that refuses ownership changes (FUSE/shfs, NFS root_squash, CIFS/SMB), dropped CAP_CHOWN."
        fi
    done
fi

exec gosu droppedneedle:droppedneedle "$@"
