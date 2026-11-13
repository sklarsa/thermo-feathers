#!/bin/sh
# Builds the node firmware, then flashes the next board to arrive on USB. A sleeping node isn't on
# USB at all, so press its RESET after starting this. A board that was already plugged in (the
# gateway) never arrives, so it's never picked. Linux only: watches /dev/serial/by-id.
# Flashes bootloader/bootloader.bin too: it rolls back an update that fails (see bootloader/).
# Extra arguments go to espflash, e.g. --monitor.
set -e
cd "$(dirname "$0")/.."
cargo build --release --bin node
dir=/dev/serial/by-id
prev=$(ls "$dir" 2>/dev/null || true)
echo "Press RESET on the node."
while :; do
    sleep 0.2
    now=$(ls "$dir" 2>/dev/null || true)
    for d in $now; do
        case "$prev" in
        *"$d"*) ;;
        *) exec espflash flash --chip esp32s3 --partition-table partitions.csv \
            --bootloader bootloader/bootloader.bin \
            --port "$dir/$d" target/xtensa-esp32s3-none-elf/release/node "$@" ;;
        esac
    done
    prev=$now
done
