# thermo-feathers

`no_std` Rust firmware for battery-powered room temperature and humidity nodes and the gateway
they report to. A node is an Adafruit ESP32-S3 Feather with an SHT41 on STEMMA QT: it wakes every
minute, buffers readings in RTC memory through deep sleep, and sends them over ESP-NOW to a
wall-powered gateway Feather, which forwards them to [QuestDB](https://questdb.com) over QWP with
[qwp-embedded](https://github.com/sklarsa/qwp-embedded). Nodes never join WiFi. Every node runs the
same image.

[SPEC.md](SPEC.md) is the spec; the
[v2 design](docs/v2-design.md) explains the move to ESP-NOW.

**Status:** three nodes running. Done:

- 841 deep-sleep wakes in a row (2026-11-10)
- node → gateway → QuestDB over ESP-NOW (2026-11-11)
- no reading lost or duplicated through a 60 min gateway outage and a 30 min QuestDB outage (2026-11-11)
- the gateway's web page (2026-11-12)
- updates over ESP-NOW, with rollback of a failed image (2026-11-12)
- per-node keys, encrypted links, gateway updates kept only once nodes report (2026-11-13)
- ESP-NOW link counters in device_status and gateway_status (2026-11-17)

Battery, measured on one node's 1200 mAh LiPo: a 60 s wake uses about 0.07 % an hour (about two
months a charge), 5 s about 1 % an hour (four days). Nearly all of it is wakes, so the time
between charges grows with the interval. USB charges about 20 % an hour, slowing near full.

## Bill of materials

Per node. Everything is from Adafruit and plugs together, no soldering.

| Qty | Part | Adafruit PID | Notes |
|---|---|---|---|
| 1 | [ESP32-S3 Feather, 4 MB flash / 2 MB PSRAM](https://www.adafruit.com/product/5477) | 5477 | Has the MAX17048 battery gauge on board |
| 1 | [SHT41 temperature and humidity sensor, STEMMA QT](https://www.adafruit.com/product/5776) | 5776 | |
| 1 | STEMMA QT cable, 100 mm ([4210](https://www.adafruit.com/product/4210)) | | The enclosure's run is about 85 mm; longer coils behind the battery |
| 2 | M2.5 × 6 mm pan-head self-tapping screws | | Hold the Feather in the enclosure |
| 1 | [LiPo 3.7 V 1200 mAh](https://www.adafruit.com/product/258) | 258 | |
| 1 | USB-C cable and 5 V USB power supply | | Flashing, charging, and plugged-in running |

The enclosure is in [enclosure/](enclosure/README.md). Pins and power rules are in
[SPEC.md](SPEC.md#hardware).

## Build and flash

Same toolchain as s3-sensors: `espup install --targets esp32s3`, then source `~/export-esp.sh`.

`THERMO_MASTER` is a secret of 64 hex digits (`openssl rand -hex 32`, once), shared by every node
build and the gateway. Each node gets its own key from it and its MAC. Every ESP-NOW frame is
tagged with that key and encrypted with ESP-NOW's CCMP, and the gateway ignores frames it can't
verify. Its page counts them and names the last node that sent one. Replies are tagged as answers
to the frame they answer, so a recorded one can't be replayed.

### A new node

```sh
THERMO_MASTER=... scripts/flash-node.sh   # Linux: builds, then flashes the next board on USB
```

The script writes the node's key into its `creds` partition and flashes `bootloader/bootloader.bin`,
ESP-IDF's bootloader with rollback on (`bootloader/build.sh` rebuilds it with Docker; espflash's
own can't roll back). That USB flash is the only one a node needs: it then joins by itself and
updates over the air.

- A node is on USB only for 30 s after power-on or RESET, since deep sleep turns USB off. The
  script waits for a new board to appear: press RESET whenever it's ready.
- A board new from Adafruit runs its factory firmware, which holds the USB port: hold BOOT, press
  RESET, let go of BOOT, and press RESET again once the script is done.
- The gateway, already plugged in, never appears anew, so the script never picks it.
- A new node looks for the gateway at once, then 1, 2, 4… wakes apart, so it can take a few
  minutes to show up on the gateway's page.

### Updating nodes

```sh
scripts/ota.sh <gateway address> [all | id,id]   # builds, then hands the image to the gateway
```

Or upload the `.bin` from `espflash save-image` on the gateway's Settings page.

- Each node fetches the image at its next wake, if charging or above 40 % battery, restarts into
  it, and keeps it once it hears from the gateway.
- If the new image never gets that far, the bootloader goes back to the old one at the next wake,
  and the gateway's page says the update rolled back. The node then ignores offers for an hour,
  so a bad image can't drain its battery.
- A kept image that later crashes 3 wakes in a row, before reaching the gateway, also goes back.
- A node reports its build as `<version>-<6 hex>`, from the image's SHA-256, so every build is
  told apart.

### The gateway

Any ESP32-S3 Feather with PSRAM, on a USB wall supply, with nothing attached. It's the only device
on WiFi, so it's the only build that needs credentials:

```sh
WIFI_SSID=... WIFI_PASSWORD=... QUESTDB_ADDR=192.168.1.10:9000 THERMO_MASTER=... \
    cargo run --release --bin gateway
```

`cargo run` flashes the rollback bootloader too. The gateway logs its IP address and WiFi channel
over USB serial once connected; reserve that address in your router.

After the first USB flash it updates over the network, with the same variables set:

```sh
scripts/ota.sh <gateway address> gateway
```

- It writes the image to its other slot, waits until QuestDB has everything it holds, notes the
  nodes it heard in the last 3 wake intervals, and restarts into the new image.
- The new image is kept once it reaches WiFi, the time server and QuestDB, and has heard from each
  of those nodes. With no node active, WiFi and the time server are enough.
- Otherwise it restarts after 10 minutes (or 3 wake intervals and a minute, if longer) and the
  bootloader goes back to the old image, whose page names the nodes the update didn't hear. A node
  that's gone for good blocks updates until it's back: upload again once it reports.
- While on trial it refuses another gateway upload, since that would overwrite the image a failed
  trial goes back to. It also doesn't tell nodes their readings are stored until QuestDB has acked
  something, so nodes keep their copies. Going back waits for QuestDB first, as a restart does.
- A kept gateway image that crashes 3 times in a row, each within 10 minutes of starting, goes back
  to the one before it, once that one checks out intact.

### The web page

At the gateway's address: each room's latest reading, battery and last-seen time. Its Settings
page:

- renames nodes (each rename is also a `device_locations` row)
- switches °F/°C
- sets the nodes' wake interval, 5–3600 s; nodes switch at their next report
- overrides the build's QuestDB address and time server. A new QuestDB address takes effect at
  once, taking along what the old one hadn't stored.
- uploads node and gateway firmware

The settings live in the gateway's flash.

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

- Enclosure ([enclosure/](enclosure/README.md)): a first core and shell printed (2026-11-17);
  the fixes from them (post roots, sensor fit, cable slot, battery ribs) aren't printed yet.
  Self-heating inside it is the side-by-side check's main error. Only the plain shell exists.

## License

MIT
