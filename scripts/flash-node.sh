#!/bin/sh
# Builds the node firmware, then flashes the next board to arrive on USB. A sleeping node isn't on
# USB at all, so press its RESET after starting this. A board that was already plugged in (the
# gateway) never arrives, so it's never picked. Linux only: watches /dev/serial/by-id.
# Flashes bootloader/bootloader.bin too: it rolls back an update that fails (see bootloader/).
# Writes the node's key into its creds partition, worked out from its MAC and the master secret in
# THERMO_MASTER (64 hex digits, the gateway's too): without it, the gateway ignores the node.
# Extra arguments go to espflash, e.g. --monitor.
set -e
cd "$(dirname "$0")/.."
case "$THERMO_MASTER" in
[0-9a-fA-F]*) [ ${#THERMO_MASTER} -eq 64 ] || { echo "THERMO_MASTER must be 64 hex digits" >&2; exit 2; } ;;
*) echo "set THERMO_MASTER to the master secret (64 hex digits)" >&2; exit 2 ;;
esac
cargo build --release --bin node
creds=$(mktemp)
trap 'rm -f "$creds"' EXIT
dir=/dev/serial/by-id
prev=$(ls "$dir" 2>/dev/null || true)
echo "Press RESET on the node."
while :; do
    sleep 0.2
    now=$(ls "$dir" 2>/dev/null || true)
    for d in $now; do
        case "$prev" in
        *"$d"*) ;;
        *)
            port="$dir/$d"
            mac=$(espflash board-info --chip esp32s3 --port "$port" --after no-reset |
                sed -n 's/.*MAC address: *//p' | tr -d ':\r ')
            [ ${#mac} -eq 12 ] || { echo "couldn't read the node's MAC" >&2; exit 1; }
            # keys::Creds: TFK1, node key, PMK, 4 bytes of their SHA-256
            python3 -I -c '
import hashlib, hmac, sys
m, mac = bytes.fromhex(sys.argv[1]), bytes.fromhex(sys.argv[2])
k = hmac.new(m, b"thermo-node" + mac, "sha256").digest()[:16]
p = hmac.new(m, b"thermo-pmk", "sha256").digest()[:16]
b = b"TFK1" + k + p
sys.stdout.buffer.write(b + hashlib.sha256(b).digest()[:4])
' "$THERMO_MASTER" "$mac" >"$creds"
            espflash write-bin --chip esp32s3 --port "$port" --after no-reset 0x9000 "$creds"
            echo "node $(echo "$mac" | cut -c7-12): key written"
            espflash flash --chip esp32s3 --partition-table partitions.csv \
                --bootloader bootloader/bootloader.bin \
                --port "$port" target/xtensa-esp32s3-none-elf/release/node "$@"
            exit ;;
        esac
    done
    prev=$now
done
