#!/bin/sh
# Container entrypoint: apply UMASK, run the given command as PUID/PGID.
#
# Started as root (the default), it moves the baked droppedneedle user to
# PUID/PGID, hands the data folders over to it, and drops to it with gosu.
# Started with `docker run --user`, it skips user setup and runs the command
# as that user. Any command works, e.g. `droppedneedle-tool upgrade`, which
# then writes its files as the same user the server runs as. Only the server
# (`droppedneedle`) has its /app/config and /app/cache checked and chowned
# here; the tool reports a folder it cannot write to itself.
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

# The four folders the server writes belong to the runtime user. A folder owned
# by another uid was left by an earlier start under another PUID (a remap, or a
# PUID set back to the baked 1000), so the files that uid owns in it move to the
# runtime user. A folder the runtime user already owns was handed over before
# and is not walked, so a recreate costs nothing; a root-owned one is left to the
# write check below. -depth moves the folder itself last, so a stopped hand-over
# runs again. find does not follow links, and chown -h changes a link itself,
# never its target. A folder that refuses the change gets a warning, not a stop;
# the write check for config and cache below still decides whether the start
# can go on.
for dir in /app/config /app/cache /app/plugins /app/imports; do
    PREV_UID=$(stat -c %u "$dir" 2>/dev/null) || continue
    [ "$PREV_UID" = "$TARGET_UID" ] && continue
    [ "$PREV_UID" = 0 ] && continue
    echo "[init] Handing $dir over from uid=$PREV_UID to uid=$TARGET_UID."
    if ! find "$dir" -depth -user "$PREV_UID" -exec chown -h "$TARGET_UID:$TARGET_GID" {} + 2>/dev/null; then
        echo "[init] WARNING: Could not hand $dir over to uid=$TARGET_UID (the mount may not support ownership changes)."
    fi
done

if is_server "$1"; then
    echo "[init] Runtime user: droppedneedle (uid=$TARGET_UID gid=$TARGET_GID)"
    for dir in /app/config /app/cache; do
        mkdir -p "$dir" 2>/dev/null || true
        if check_writable "$dir" "$TARGET_UID:$TARGET_GID"; then
            continue
        fi
        if chown -h droppedneedle:droppedneedle "$dir" 2>/dev/null; then
            echo "[init] Adjusted ownership of $dir; checking write access again."
        else
            echo "[init] WARNING: Could not chown $dir (the mount may not support ownership changes)."
        fi
        if ! check_writable "$dir" "$TARGET_UID:$TARGET_GID"; then
            echo "[init] FATAL: $dir is not writable by uid=$TARGET_UID gid=$TARGET_GID."
            echo "[init]   Common causes: FUSE/shfs (Unraid), NFS root_squash, CIFS/SMB, dropped CAP_CHOWN."
            echo "[init]   Fix: make the host directory writable by uid=$TARGET_UID gid=$TARGET_GID."
            exit 1
        fi
    done
fi

exec gosu droppedneedle:droppedneedle "$@"
