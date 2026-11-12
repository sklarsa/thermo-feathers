#!/bin/sh
# Builds firmware and hands it to the gateway, over the network.
#   scripts/ota.sh <gateway address> [all | id,id]   node firmware, fetched at each node's next wake
#   scripts/ota.sh <gateway address> gateway         the gateway's own; it restarts into it
# The address can also come from THERMO_GATEWAY. Gateway builds need WIFI_SSID, WIFI_PASSWORD and
# QUESTDB_ADDR set, as for a USB flash. Watch the gateway's page for progress.
set -e
cd "$(dirname "$0")/.."
gw=${1:-$THERMO_GATEWAY}
[ -n "$gw" ] || { echo "usage: scripts/ota.sh <gateway address> [all | id,id | gateway]" >&2; exit 2; }
to=${2:-all}
if [ "$to" = gateway ]; then
    bin=gateway url="http://$gw/update/gateway"
else
    bin=node url="http://$gw/update?to=$to"
fi
elf=target/xtensa-esp32s3-none-elf/release/$bin
cargo build --release --bin "$bin"
espflash save-image --chip esp32s3 "$elf" "$elf.bin" >/dev/null
curl -sS --fail-with-body --data-binary @"$elf.bin" "$url"
echo
