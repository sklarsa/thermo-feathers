# thermo-feathers

`no_std` Rust firmware for battery-powered room temperature and humidity nodes: an Adafruit
ESP32-S3 Feather with an SHT41 on STEMMA QT wakes every minute, buffers readings in RTC memory
through deep sleep, and sends a batch to [QuestDB](https://questdb.com) over QWP every 10 minutes
with [qwp-embedded](https://github.com/sklarsa/qwp-embedded). Every node runs the same image;
credentials, identity and settings live outside it.

[SPEC.md](SPEC.md) is the design. **Status: scaffold.** It builds and boots at 80 MHz; milestone 1
(plugged-in SHT41 logger) is next.

## Bill of materials

Per node. Everything is from Adafruit and plugs together, no soldering.

| Qty | Part | Adafruit PID | Notes |
|---|---|---|---|
| 1 | [ESP32-S3 Feather, 4 MB flash / 2 MB PSRAM](https://www.adafruit.com/product/5477) | 5477 | Has the MAX17048 battery gauge on board |
| 1 | [SHT41 temperature and humidity sensor, STEMMA QT](https://www.adafruit.com/product/5776) | 5776 | |
| 1 | STEMMA QT cable, 50 mm | | Inside the enclosure: the SHT41 sits in its own vented pocket below the board |
| 1 | [LiPo 3.7 V 1200 mAh](https://www.adafruit.com/product/258) | 258 | Battery operation, from milestone 2 |
| 1 | USB-C cable and 5 V USB power supply | | Flashing, charging, and plugged-in running |

The enclosure is in [enclosure/](enclosure/main.scad): a core that holds everything, plus a plain
snap-on cover, in PLA with no supports. The core is still a placeholder without board holders.
Pins and power rules are in [SPEC.md](SPEC.md#hardware).

## Build and flash

Same toolchain as s3-sensors: `espup install --targets esp32s3`, source `~/export-esp.sh`, then

```sh
cargo run --release   # espflash flashes with partitions.csv and opens the monitor
```

## Host tests

The logic in `src/lib.rs` runs on the host against a checkout of qwp-embedded next to this repo:

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

- Enclosure: board holders and test prints for the core. It matters for the side-by-side check,
  since self-heating inside the enclosure is the main error.

## License

MIT
