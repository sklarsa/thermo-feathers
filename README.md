# thermo-feathers

`no_std` Rust firmware for battery-powered room temperature and humidity nodes and the gateway
they report to. A node is an Adafruit ESP32-S3 Feather with an SHT41 on STEMMA QT: it wakes every
minute, buffers readings in RTC memory through deep sleep, and sends them over ESP-NOW to a
wall-powered gateway Feather, which forwards them to [QuestDB](https://questdb.com) over QWP with
[qwp-embedded](https://github.com/sklarsa/qwp-embedded). Nodes never join WiFi. Every node runs the
same image.

[SPEC.md](SPEC.md) is the spec; the
[v2 design](docs/superpowers/specs/2026-11-10-espnow-gateway-design.md) explains the move to ESP-NOW.
**Status:** v2 milestone 1 done (deep-sleep wakes proven: 841 in a row on 2026-11-10); the ESP-NOW
link is built and waiting for its hardware test.

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
cargo run --release --bin node   # espflash flashes with partitions.csv and opens the monitor
```

A node is only reachable over USB for 30 s after power-on or RESET (deep sleep turns USB off):
tap RESET, then run the command.

The gateway is any ESP32-S3 Feather with PSRAM, on a USB wall supply, with nothing attached. It's
the only device on WiFi, so it's the only build that needs credentials:

```sh
WIFI_SSID=... WIFI_PASSWORD=... QUESTDB_ADDR=192.168.1.10:9000 cargo run --release --bin gateway
```

It logs its IP address and WiFi channel over USB serial once it's connected. Reserve that address
in your router so it stays put.

## Host tests

The logic in `src/lib.rs` and `src/proto.rs` runs on the host against a checkout of qwp-embedded next to this repo:

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

## Todo

- Flash helper: wait for a node to show up on USB after RESET and flash it right away, so there's
  no racing the 30 s window.

## License

MIT
