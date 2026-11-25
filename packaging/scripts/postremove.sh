#!/bin/sh
# Runs after the files are gone. The .deb passes `remove` or `purge` (or
# `upgrade`); the .rpm passes 0 on removal. Neither touches the data folder or
# the account. A purge deletes only the settings file, which dpkg does by
# itself because it is a conffile.
case "$1" in
    remove | purge | 0)
        if [ -d /run/systemd/system ]; then
            systemctl daemon-reload || true
        fi
        echo "DroppedNeedle's data stays in /var/lib/droppedneedle, and so does the droppedneedle account. Delete them by hand once you no longer need them."
        ;;
esac
exit 0
