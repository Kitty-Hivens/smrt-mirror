#!/usr/bin/env bash
# Build the release binaries locally, scp them to the VPS, restart the unit.
# The hand path, for when CI cannot run it; CI does the same thing.
# Assumes SSH access via ~/.ssh/vps_hivens (override via KEY env).
#
# Both binaries ship together. smrt-pack opens the same registry.db the service
# migrates, so a box running a new service beside an old CLI has a CLI that
# cannot read its own database -- which is exactly the state an emergency deploy
# would leave behind, at the moment somebody is most likely to reach for it.

set -euo pipefail

HOST=${HOST:-root@hivens.dev}
KEY=${KEY:-$HOME/.ssh/vps_hivens}
# Two binaries go now, so the knob is the directory. An existing REMOTE_BIN is
# still honoured -- it named the service binary, and its directory is where both
# belong.
REMOTE_DIR=${REMOTE_DIR:-$(dirname "${REMOTE_BIN:-/usr/local/bin/smrt}")}

cd "$(dirname "$0")/.."

echo "==> cargo build --release"
cargo build --release --bin smrt --bin smrt-pack

for name in smrt smrt-pack; do
    binary="target/release/$name"
    [[ -f "$binary" ]] || { echo "missing $binary after build" >&2; exit 1; }
    size=$(stat -c%s "$binary")
    echo "==> $name built ($((size / 1024 / 1024)) MB)"
    echo "==> scp -> $HOST:$REMOTE_DIR/$name.new"
    scp -i "$KEY" "$binary" "$HOST:$REMOTE_DIR/$name.new"
done

echo "==> swap + restart"
ssh -i "$KEY" "$HOST" "bash -se" <<REMOTE
set -euo pipefail
for name in smrt smrt-pack; do
    mv "$REMOTE_DIR/\$name.new" "$REMOTE_DIR/\$name"
    chmod +x "$REMOTE_DIR/\$name"
done
systemctl restart smrt
sleep 1
systemctl status smrt --no-pager -l | head -15
REMOTE

echo "==> done"
