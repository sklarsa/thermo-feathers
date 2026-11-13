# thermo-feathers

`no_std` Rust firmware for battery-powered room temperature and humidity nodes and the gateway
they report to. A node is an Adafruit ESP32-S3 Feather with an SHT41 on STEMMA QT: it wakes every
minute, buffers readings in RTC memory through deep sleep, and sends them over ESP-NOW to a
wall-powered gateway Feather, which forwards them to [QuestDB](https://questdb.com) over QWP with
[qwp-embedded](https://github.com/sklarsa/qwp-embedded). Nodes never join WiFi. Every node runs the
same image.

[SPEC.md](SPEC.md) is the spec; the
[v2 design](docs/superpowers/specs/2026-11-10-espnow-gateway-design.md) explains the move to ESP-NOW.
**Status:** v2 milestones 1–5 done on one node. Milestone 1: 841 deep-sleep wakes in a row
(2026-11-10). Milestone 2: node to gateway to QuestDB over ESP-NOW (2026-11-11). Milestone 3:
gateway off for 60 min, then QuestDB off for 30 min (2026-11-11), with every reading stored
once and none duplicated. Milestone 4: the gateway's web page (2026-11-12). Milestone 5: nodes
update over ESP-NOW and roll back an image that fails (2026-11-12). More nodes come once they're
assembled.

## Bill of materials

Per node. Everything is from Adafruit and plugs together, no soldering.

| Qty | Part | Adafruit PID | Notes |
|---|---|---|---|
| 1 | [ESP32-S3 Feather, 4 MB flash / 2 MB PSRAM](https://www.adafruit.com/product/5477) | 5477 | Has the MAX17048 battery gauge on board |
| 1 | [SHT41 temperature and humidity sensor, STEMMA QT](https://www.adafruit.com/product/5776) | 5776 | |
| 1 | STEMMA QT cable, 200–400 mm ([4401](https://www.adafruit.com/product/4401), [5384](https://www.adafruit.com/product/5384) or [5385](https://www.adafruit.com/product/5385)) | | Long enough to keep the sensor away from the board's heat |
| 1 | [LiPo 3.7 V 1200 mAh](https://www.adafruit.com/product/258) | 258 | Battery operation, from milestone 2 |
| 1 | USB-C cable and 5 V USB power supply | | Flashing, charging, and plugged-in running |

The enclosure isn't designed yet. Pins and power rules are in [SPEC.md](SPEC.md#hardware).

## Build and flash

Same toolchain as s3-sensors: `espup install --targets esp32s3`, source `~/export-esp.sh`, then

```sh
THERMO_MASTER=... scripts/flash-node.sh   # Linux: builds, then flashes the next board on USB
```

`THERMO_MASTER` is 64 hex digits (`openssl rand -hex 32`, once), kept secret and the same for the
gateway. Every ESP-NOW frame is tagged with a key worked out from it and the node's MAC, and the
link is encrypted with ESP-NOW's own CCMP on top: the gateway ignores what it can't verify. The
script writes the node's key into its `creds` partition, so a node needs this one USB flash to
join, and joins by itself once it has it. The gateway's page counts frames that arrive without a
good key, and names the last node that sent one.

A node is only reachable over USB for 30 s after power-on or RESET (deep sleep turns USB off), so
the script waits for one to show up: press RESET whenever it's ready. The gateway, already plugged
in, never shows up anew, so it's never picked. The script also flashes `bootloader/bootloader.bin`,
ESP-IDF's bootloader with app rollback turned on (`bootloader/build.sh` rebuilds it with Docker).
espflash's own bootloader can't roll back, so a node needs this one USB flash before it can be
updated over the air.

After that, update nodes through the gateway:

```sh
scripts/ota.sh <gateway address> [all | id,id]   # build, then hand the image to the gateway
```

or upload the `.bin` from `espflash save-image` on the gateway's Settings page. Each node fetches it
at its next wake (if charging, or above 40 % battery), restarts into it, and keeps it once it hears
from the gateway. If the new image never gets that far, the bootloader goes back to the old one at
the next wake, and the gateway's page says the update rolled back. A confirmed image that then
crashes 3 wakes in a row, before reaching the gateway, goes back to the one before it too. A node
reports its build as `<version>-<6 hex>`, from the image's own SHA-256, so every build is told
apart.

The gateway is any ESP32-S3 Feather with PSRAM, on a USB wall supply, with nothing attached. It's
the only device on WiFi, so it's the only build that needs credentials:

```sh
WIFI_SSID=... WIFI_PASSWORD=... QUESTDB_ADDR=192.168.1.10:9000 THERMO_MASTER=... \
    cargo run --release --bin gateway
```

`cargo run` flashes the rollback bootloader too. After that first USB flash, the gateway updates
over the network, with the same variables set:

```sh
scripts/ota.sh <gateway address> gateway
```

It writes the image to its other slot and restarts into it once QuestDB has everything it holds.
The new image is kept once it reaches WiFi, the time server and QuestDB; otherwise it restarts after
10 minutes and the bootloader goes back to the old one. Like the nodes, a confirmed gateway image
that crashes 3 times in a row goes back to the one before it.

It logs its IP address and WiFi channel over USB serial once it's connected. Reserve that address
in your router so it stays put.

Its web page, at that address, shows each room's latest reading, battery and last-seen time. Its
Settings page renames nodes (each rename is also a `device_locations` row), switches °F/°C, sets
the nodes' wake interval, and can override the build's QuestDB address and time server; the
gateway switches to a new QuestDB address right away, taking along what the old one hadn't stored. Those settings live in the
gateway's flash. Node and gateway firmware can be uploaded there too.

## Host tests

The logic in `src/lib.rs` and its modules runs on the host against a checkout of qwp-embedded next to this repo:

```sh
rustc +stable --edition 2024 --crate-type rlib --crate-name qwp_embedded ../qwp-embedded/src/lib.rs -o /tmp/libqwp_embedded.rlib
rustc +stable --edition 2024 --test src/lib.rs --extern qwp_embedded=/tmp/libqwp_embedded.rlib -o /tmp/thermo-tests && /tmp/thermo-tests
```

## Departures from the spec

- QWP comes from the qwp-embedded crate instead of a copy of `s3-sensors/src/qwp.rs`. The crate
  already encodes SYMBOL columns and the delta symbol dictionary (`Encoder::with_symbols`), so
  the spec's SYMBOL work is done.

## Open questions

Beyond the ones in [SPEC.md](SPEC.md#open-questions):

- Enclosure: not designed yet. Maybe something more whimsical than esp-light-show's house. It
  matters for the side-by-side check, since self-heating inside the enclosure is the main error.

## License

MIT
