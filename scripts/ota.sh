#!/bin/sh
# Builds node firmware and hands it to the gateway, which updates the nodes over ESP-NOW at their
# next wake. Watch the gateway's page for progress.
#   scripts/ota.sh <gateway address> [all | id,id]
# The address can also come from THERMO_GATEWAY.
set -e
cd "$(dirname "$0")/.."
gw=${1:-$THERMO_GATEWAY}
[ -n "$gw" ] || { echo "usage: scripts/ota.sh <gateway address> [all | id,id]" >&2; exit 2; }
to=${2:-all}
elf=target/xtensa-esp32s3-none-elf/release/node
cargo build --release --bin node
espflash save-image --chip esp32s3 "$elf" "$elf.bin" >/dev/null
curl -sS --fail-with-body --data-binary @"$elf.bin" "http://$gw/update?to=$to"
echo
