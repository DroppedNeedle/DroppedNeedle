#!/bin/sh
# Runs after the files are in place. The .deb passes `configure` and the
# version it replaces (empty on a first install); the .rpm passes 1 on an
# install and 2 on an upgrade.
set -e

account=droppedneedle
data=/var/lib/droppedneedle

if ! id -u "$account" > /dev/null 2>&1; then
    if command -v systemd-sysusers > /dev/null 2>&1; then
        systemd-sysusers /usr/lib/sysusers.d/droppedneedle.conf
    else
        useradd --system --user-group --home-dir "$data" --no-create-home \
            --shell /usr/sbin/nologin --comment "DroppedNeedle music server" "$account"
    fi
fi

# The service makes the folder itself, but droppedneedle-tool may run first.
if [ ! -d "$data" ]; then
    install -d -m 0750 -o "$account" -g "$account" "$data"
fi

# Nothing below runs where systemd is not the init, such as in a container.
[ -d /run/systemd/system ] || exit 0
systemctl daemon-reload || true

# The .deb starts the server on a first install and restarts a running one on
# an upgrade. The .rpm starts nothing on install, as Fedora and RHEL expect,
# and restarts a running server in posttrans, once the old files are gone.
case "$1" in
    configure)
        if [ -z "${2:-}" ]; then
            systemctl enable --now droppedneedle.service || true
        else
            systemctl try-restart droppedneedle.service || true
        fi
        ;;
esac
exit 0
