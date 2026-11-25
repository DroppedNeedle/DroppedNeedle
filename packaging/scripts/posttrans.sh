#!/bin/sh
# The .rpm's last step of an install or upgrade. A server that is not running
# stays stopped, so a first install starts nothing.
[ -d /run/systemd/system ] || exit 0
systemctl try-restart droppedneedle.service || true
exit 0
