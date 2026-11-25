#!/bin/sh
# Stops the server only when the package goes away, never on an upgrade. The
# .deb passes `remove` (or `upgrade`); the .rpm passes 0 on removal.
[ -d /run/systemd/system ] || exit 0
case "$1" in
    remove | 0)
        systemctl disable --now droppedneedle.service || true
        ;;
esac
exit 0
