#!/bin/sh
set -eu

if [ ! -f "$BUCKYOS_ROOT/.docker-initialized" ]; then
    cp -a /usr/local/share/buckyos/rootfs/. "$BUCKYOS_ROOT/"
    touch "$BUCKYOS_ROOT/.docker-initialized"
else
    cp -a /usr/local/share/buckyos/rootfs/bin/. "$BUCKYOS_ROOT/bin/"
    cp -a /usr/local/share/buckyos/rootfs/libexec/. "$BUCKYOS_ROOT/libexec/"
    cp /usr/local/share/buckyos/rootfs/etc/boot_gateway.yaml "$BUCKYOS_ROOT/etc/boot_gateway.yaml"
fi

python3 /usr/local/bin/configure-local-access.py

mkdir -p "$BUCKYOS_ROOT/logs"
dockerd --host=unix:///var/run/docker.sock >"$BUCKYOS_ROOT/logs/docker.log" 2>&1 &
docker_pid=$!
attempt=0
until docker info >/dev/null 2>&1; do
    attempt=$((attempt + 1))
    if [ "$attempt" -ge 60 ] || ! kill -0 "$docker_pid" 2>/dev/null; then
        cat "$BUCKYOS_ROOT/logs/docker.log" >&2
        exit 1
    fi
    sleep 1
done

exec "$BUCKYOS_ROOT/bin/node-daemon/node_daemon" --enable_active
