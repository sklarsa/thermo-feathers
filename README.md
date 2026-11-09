# thermo-feathers

`no_std` Rust firmware for battery-powered room temperature and humidity nodes: an Adafruit
ESP32-S3 Feather with an SHT41 on STEMMA QT wakes every minute, buffers readings in RTC memory
through deep sleep, and sends a batch to [QuestDB](https://questdb.com) over QWP every 10 minutes
with [qwp-embedded](https://github.com/sklarsa/qwp-embedded). Every node runs the same image;
credentials, identity and settings live outside it.

[SPEC.md](SPEC.md) is the design. **Status: scaffold.** It builds and boots at 80 MHz; milestone 1
(plugged-in SHT41 logger) is next.

## Hardware

| Part | Adafruit PID |
|---|---|
| ESP32-S3 Feather, 4 MB flash / 2 MB PSRAM | 5477 |
| SHT41 | 5776 |
| LiPo 3.7 V 1200 mAh | 258 |
| STEMMA QT cable, 200–400 mm | 4401 / 5384 / 5385 |

No soldering. Pins and power rules are in [SPEC.md](SPEC.md#hardware).

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

- Enclosure: not designed yet. Maybe something more whimsical than esp-light-show's house. It
  matters for the side-by-side check, since self-heating inside the enclosure is the main error.

## License

MIT
