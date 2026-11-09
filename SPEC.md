# Room Sensor Node Firmware Spec

Oct 30, 2026

## Overview and goals

Each node is a battery-powered ESP32-S3 that wakes every 60 s to read temperature and humidity,
buffers readings in RTC memory, and sends a batch to QuestDB over QWP every 10 minutes. All nodes
run one identical firmware image; identity, room and settings live outside the binary.

Goals:

- Measure temperature and humidity per room, comparable against a reference node next to the Nest
  thermostat.
- Run 4+ weeks on a 1200 mAh LiPo between charges.
- Never lose or duplicate readings when Wi-Fi or QuestDB is briefly unavailable.
- Update firmware and settings remotely, with no USB access after first flash.
- Report device health (battery, signal, firmware version, errors) alongside the data.

Non-goals for v1: on-device display, Bluetooth, Home Assistant integration, sub-minute sampling.

## Hardware

Everything connects without soldering: the SHT41 plugs into the STEMMA QT port and the LiPo into
the battery connector.

| Part | Adafruit PID | Role | I²C address |
|---|---|---|---|
| ESP32-S3 Feather, 4MB Flash / 2MB PSRAM | 5477 | MCU, Wi-Fi, LiPo charger | n/a |
| MAX17048 (on the Feather) | built in | Battery voltage and % | 0x36 |
| SHT41 | 5776 | Temperature and humidity | 0x44 |
| LiPo 3.7 V 1200 mAh | 258 | Power | n/a |
| STEMMA QT cable 200–400 mm | 4401 / 5384 / 5385 | Keeps the sensor away from board heat | n/a |

Pin and power rules:

- **STEMMA QT power is switched.** GPIO7 enables a separate 3.3 V regulator that feeds only the
  STEMMA QT port. Drive it HIGH before reading the SHT41, wait 10 ms, and drive it LOW before
  sleeping. I²C0 is on SDA GPIO3 and SCL GPIO4.
- **The battery monitor is always powered.** In Adafruit's published schematic the gauge runs
  directly from the battery, not from the GPIO7 regulator, so it keeps tracking charge through
  deep sleep. That schematic is the earlier LC709203F revision; the MAX17048 revision is expected
  to match, so check that the charge percentage stays sensible across sleep cycles. Read it while
  GPIO7 is on.
- **No USB-power sense pin.** USB 5 V only reaches the charger, the CHG LED and the power-path
  switch; no GPIO can see it. Charging is inferred from the gauge (see the CHARGING flag).
- **NeoPixel:** data on GPIO33, power switched by GPIO21. Keep it off except for the low-battery
  blink.
- **No 32 kHz crystal:** deep-sleep timekeeping runs on the S3's internal ~136 kHz RC oscillator
  (see [Time sync and timestamps](#time-sync-and-timestamps)).
- **Battery monitor chip varies by board revision.** Probe 0x36 (MAX17048) and fall back to 0x0B
  (LC709203F); record which one was found.
- **Deep sleep** draws about 100 µA from the LiPo with I²C and NeoPixel power off, per Adafruit's
  guide.

## Operating cycle and power

The node spends nearly all its time in deep sleep: it wakes every 60 s to read the sensor, and only
every 10 minutes does it turn on Wi-Fi to send. Both intervals come from remote config.

A read-only wake should finish in well under a second; a send wake should stay within a few
seconds, hard-capped at 30 s. OTA wakes are a separate path with a 3-minute cap (see
[OTA updates](#ota-updates)).

Estimated daily power budget, checked against Espressif and Adafruit figures; confirm with real
measurements in testing. Send wakes dominate, and their cost depends mostly on how fast Wi-Fi
reconnects:

| Activity | Per day | Estimate | Energy per day |
|---|---|---|---|
| Deep sleep | continuous | ~100 µA, whole board | ~2.4 mAh |
| Read-only wakes | 1,440 | ~0.3 s at ~40 mA (boot ~0.2 s, then the reading) | ~4.8 mAh |
| Send wakes, cached IP lease | 144 | ~1.5–2 s at ~120 mA | ~7–10 mAh |
| Send wakes, full DHCP each time | 144 | ~3.5 s at ~120 mA | ~17 mAh |
| **Total, cached lease** | | | **~15–17 mAh** |
| **Total, DHCP fallback** | | | **~24 mAh** |

Not all 1200 mAh is usable: as the cell drops toward 3.4–3.5 V, the board's 3.3 V regulator sags
under Wi-Fi current peaks, so count on about 85 %, roughly 1,000 mAh. That gives about 9 weeks when
the cached-lease reconnect works and about 6 weeks when every send falls back to DHCP. The biggest
levers, in order: get the cached-lease reconnect working (it roughly halves send energy), run the
CPU at 80 MHz instead of 240 MHz (Wi-Fi still works and core current drops by about a third), and
raise `send_interval_s`.

Sources: Adafruit ESP32-S3 Feather guide (deep sleep current), ESP32-S3 modem-sleep current table
(CPU current by frequency), Espressif low-power introduction, WiFiManager fast-connect report on
ESP32-S2 (reconnect times), esp32.com wake-time thread (boot from deep sleep).

## Sensor readings

One SHT41 measurement per wake, battery read once per send cycle.

- **SHT41:** small raw I²C driver, or an embedded-hal 1.0 crate. Send command 0xFD (high
  precision, heater off, since the heater warms the reading), wait at least 10 ms, read 6 bytes,
  and check both CRC-8 bytes (polynomial 0x31, init 0xFF). Convert with
  T = −45 + 175 · raw / 65535 and RH = −6 + 125 · raw / 65535, clamped to 0–100.
- **Battery (MAX17048):** read on send cycles only. Voltage from register 0x02 (78.125 µV per bit),
  state of charge from 0x04 (1/256 % per bit), charge rate from 0x16 (signed, 0.208 %/h per bit).
- **Validation:** reject temperature outside −20 to 60 °C or humidity outside 0 to 100 %. A
  rejected or failed read is stored as NaN with a flag, not dropped, so gaps are visible. QuestDB
  treats NaN floats as null.
- **Retries:** one retry after 20 ms on an I²C error; after that, set the SENSOR_FAIL flag and
  move on.
- **Units:** store °C and %RH. Convert to °F in queries and dashboards.
- **No calibration.** The SHT41's typical accuracy is about ±0.2 °C and ±1.8 %RH, finer than the
  room-to-room differences this project looks for. Differences between nodes will come mostly from
  enclosure self-heating, which the side-by-side check reveals.

## Buffering and batching

Readings live in a ring buffer in RTC fast memory that survives deep sleep and is cleared only
after QuestDB acks the batch.

- **Storage:** RTC fast memory, not PSRAM. PSRAM and main SRAM lose their contents in deep sleep,
  so the PSRAM ring from s3-sensors doesn't carry over. Place the ring with esp-hal's `#[ram(...)]`
  attribute using its RTC-fast persistent option (check the exact syntax for esp-hal 1.2).
- **Size:** 256 samples, each packed into 12 bytes: `u32 ts_s`, `i16 temp_cC` (centi-°C),
  `u16 rh_cP` (centi-%), `u8 flags`, 3 bytes padding. About 3 KB of the 8 KB of RTC fast memory.
- **Timestamps are frozen once.** Normally `ts_s` is epoch seconds (a u32 lasts until 2106),
  computed when the sample is taken from the last sync plus the elapsed RTC time, scaled by the
  measured clock rate. Before the first time sync, `ts_s` holds raw RTC seconds and the sample
  carries TIME_UNSYNCED; it's converted once at the first sync. A timestamp never changes after
  that, so a resent batch is byte-for-byte the same rows and deduplication works.
- **Cold boot detection:** persistent RTC memory is not initialized at power-on. Keep a header with
  a magic number, layout version and CRC; if the wake cause isn't the deep sleep timer or the
  header doesn't check out, reset the ring and counters.
- **Capacity:** 256 samples at 60 s is about 4 hours of outage before data is lost.
- **Overflow:** drop the oldest sample and increment `dropped_samples`. (s3-sensors drops the
  newest because its sender reads the head concurrently; this firmware is single-threaded, so
  keeping the newest data is safe.)
- **Clearing:** remove samples only after the QWP response for that message is STATUS_OK with the
  matching sequence number. Anything else keeps the buffer; QuestDB deduplication absorbs resent
  rows.
- **Lost on power loss:** RTC memory does not survive a battery disconnect or brownout. Accepted
  for v1.

Sample flags:

| Bit | Name | Meaning |
|---|---|---|
| 0 | SENSOR_FAIL | SHT41 read failed; values are NaN |
| 1 | OUT_OF_RANGE | Value failed validation; values are NaN |
| 2 | TIME_UNSYNCED | Taken before the first time sync; timestamp converted afterwards |
| 3 | CHARGING | Gauge reported a positive charge rate. Charging warms the board, so treat these readings as suspect. A full battery still on USB doesn't show up here |

## Wi-Fi and network

Wi-Fi credentials and server addresses live in a small partition written once over USB, not in the
firmware image, so OTA images carry no secrets and every node runs the identical image.

- **Credentials partition (`creds`):** a small blob with Wi-Fi SSID and password, QUESTDB_ADDR,
  MANIFEST_URL and the NTP server (default pool.ntp.org). A host script builds it from environment
  variables, and it's written at first flash with espflash's write-to-offset command. The firmware
  only reads it. Changing Wi-Fi means re-writing it over USB.
- **Hostname:** set `roomnode-<id>` (e.g. `roomnode-a1b2c3`) as the hostname in embassy-net's
  DhcpConfig, so the router lists each sensor by the same ID shown in QuestDB. The router learns it
  on the first full DHCP request.
- **Fixed addresses come from the router, never the firmware.** Reserve an IP for each sensor in
  the eero app (network settings, reservations). No IP addresses are compiled in.
- **Fast reconnect:** after a successful connect, save BSSID, channel and the DHCP lease (IP,
  gateway, mask, DNS) in RTC memory. On later wakes, pass BSSID and channel in the esp-radio
  station config and start embassy-net with `Config::ipv4_static` from the cached lease, skipping
  both the scan and DHCP. Because the router reserves that IP, reusing it is safe.
- **Fallback and refresh:** if a cached connect fails, clear the cache and do a normal scan plus
  DHCP, then cache the new result. Also do one full DHCP connect per day so the router's lease and
  hostname stay current.
- **PHY calibration:** ESP-IDF saves radio calibration data so later wakes skip a full
  calibration. Check whether esp-radio can do the same, and measure the wake cost either way.
- **Timeouts:** 8 s for a connect attempt. On failure, keep buffering and retry next send cycle,
  backing off 1×, 2×, 4× the send interval, capped at 1 hour.
- **Radio off:** Wi-Fi is started only on send cycles and stopped before sleep. 2.4 GHz only.

## Time sync and timestamps

Readings are timestamped on the device from the RTC clock, corrected with the clock rate measured
between syncs, and frozen as epoch seconds so resends are identical.

- **Clock source:** the Feather has no 32 kHz crystal (checked in Adafruit's schematic), so
  deep-sleep time comes from the S3's internal ~136 kHz RC oscillator. RC oscillators are less
  accurate than crystals and drift with temperature. With a sync every 10 minutes and rate
  correction, timestamps should be within seconds, which is plenty for room temperature. Measure
  it rather than assume.
- **Sync:** SNTP on every send cycle, right after Wi-Fi connects. The SNTP query is one small UDP
  exchange, so it adds little to the wake.
- **Rate tracking:** at each sync, compare RTC elapsed time with SNTP elapsed time since the
  previous sync to get the RTC's actual rate. Keep it in RTC memory and use it to convert later RTC
  readings to epoch time. Report it as `rtc_drift_ppm` in `device_status`.
- **Freezing:** a sample's epoch timestamp is computed once and never changes (see
  [Buffering and batching](#buffering-and-batching)).
- **First boot:** connect and sync before the first reading. If that fails, record readings with
  TIME_UNSYNCED and convert their times at the first successful sync.
- **Sanity check:** reject an SNTP result more than 1 day from the RTC estimate unless the RTC was
  never synced; log it.
- **If drift ever matters:** a DS3231 real-time clock (temperature-compensated, about ±2 ppm, sold
  as a STEMMA QT board) on the same I²C chain would fix it. Not needed for v1.

## Data model and QWP sending

Two tables: one row per reading, and one health row per send cycle. Device ID and firmware version
are SYMBOL columns, which needs a small addition to the s3-sensors encoder; everything else is
FLOAT or LONG. Both tables deduplicate on timestamp plus device, so resending a batch is always
safe.

`room_readings`, one row per sample:

| Column | Type | Notes |
|---|---|---|
| ts | TIMESTAMP | Designated timestamp; frozen sample time |
| device | SYMBOL | Device ID, e.g. a1b2c3, the same as in the hostname |
| temp_c | FLOAT | °C; NaN when SENSOR_FAIL or OUT_OF_RANGE |
| rh_pct | FLOAT | %RH; NaN when SENSOR_FAIL or OUT_OF_RANGE |
| flags | LONG | Sample flags bitfield |

`device_status`, one row per send cycle:

| Column | Type | Notes |
|---|---|---|
| ts | TIMESTAMP | Designated timestamp; time of the send |
| device | SYMBOL | Device ID |
| fw_version | SYMBOL | e.g. 1.2.0 |
| battery_v | FLOAT | Volts |
| battery_pct | FLOAT | State of charge |
| charge_rate | FLOAT | %/hour; positive while charging |
| rssi_dbm | LONG | Wi-Fi signal |
| wifi_ms | LONG | Connect time; fast reconnect should be well under 1000 |
| cached_connect | LONG | 1 if the cached BSSID and lease worked, 0 if it fell back to scan plus DHCP |
| rtc_drift_ppm | LONG | RTC rate error measured at this sync |
| buffered | LONG | Samples in this batch |
| dropped_samples | LONG | Cumulative since power-on |
| boot_count | LONG | Cumulative wakes since power-on |
| reset_reason | LONG | esp-hal SocResetReason |
| battery_chip | LONG | 0 none, 1 MAX17048, 2 LC709203F |

Sending:

- Copy `src/qwp.rs` from s3-sensors (encoder, WebSocket framing and response parsing) and its host
  tests. Add a record encoder for these two tables in `lib.rs`, following the s3-sensors
  `qwp_message` pattern.
- **SYMBOL support:** type code 0x09. The delta symbol dictionary at the start of the message
  (which `Encoder::new` currently writes empty) carries delta_start, delta_count, then each entry
  as a varint length plus UTF-8 bytes. Column data is the null flag, then one varint dictionary ID
  per row. IDs belong to the connection and reset when it closes; since each wake opens a fresh
  connection and sends one message, every message carries the whole dictionary: two entries, the
  device ID and firmware version. Add host tests against the spec.
- **Per send cycle:** open TCP, do the WebSocket upgrade to `/write/v4`, send one message with both
  table blocks, wait up to 5 s for the response, then close. One message in flight; no pipelining
  needed at this rate.
- Clear the buffer only on STATUS_OK with the matching sequence; log the status and error text
  otherwise.

## Device identity and remote configuration

A node's ID comes from its factory MAC; its room and nickname live only in QuestDB; its settings
come from a manifest on a local web server.

- **Device ID:** the last 3 bytes of the factory MAC as 6 lowercase hex characters (e.g.
  `a1b2c3`). The same string is the hostname suffix, the device SYMBOL in QuestDB and the key in
  the manifest. The full MAC is logged at boot. Same firmware image on every node.
- **Rooms:** never in firmware. A `device_locations` table records where each device is and since
  when; queries match readings to rooms with ASOF JOIN, so history stays right when a node moves.
- **Manifest:** one JSON file on a local HTTP server, also used for OTA. Fetch it with a plain HTTP
  GET over an embassy-net TCP socket (about as simple as the WebSocket upgrade) and parse it with
  serde-json-core. Fetch on the first send cycle after a cold boot, then every
  `config_check_interval_h`.

```json
{
  "firmware": {
    "version": "1.1.0",
    "url": "http://192.168.1.10:8080/fw/room-node-1.1.0.bin",
    "sha256": "<hex>",
    "size": 1048576
  },
  "defaults": {
    "read_interval_s": 60,
    "send_interval_s": 600,
    "config_check_interval_h": 6,
    "ota_min_battery_pct": 40
  },
  "devices": {
    "a1b2c3": { "send_interval_s": 300 }
  }
}
```

- **Settings precedence:** device entry, then defaults, then compiled-in defaults. Keep the merged
  result in RTC memory, and write it to the settings partition with esp-storage only when it
  actually changes, to avoid flash wear.
- **Validation:** clamp `read_interval_s` to 10–3600 and `send_interval_s` to 60–86400; ignore
  unknown keys and keep the last good settings if the manifest doesn't parse.

## OTA updates

Nodes pull updates: when the manifest lists a newer version, the node downloads it into the
inactive slot, verifies it, reboots, and rolls back automatically if the new image can't prove it
works.

1. On a manifest check, compare `firmware.version` with the running version (semantic versioning).
   If it's newer and the battery guard passes (charging, or `battery_pct` at least
   `ota_min_battery_pct`, default 40), set an `ota_pending` flag in RTC memory and finish the
   normal send wake.
2. The next wake sees the flag and takes the OTA path instead of the normal one: it skips the
   sensor reading (a one-sample gap), raises the awake cap to 3 minutes, and does only the update.
   No multitasking needed; main just picks one of two straight-line paths at the start.
3. Stream the image over HTTP into the inactive OTA partition with esp-storage, hashing as it goes
   (the sha2 crate, or esp-hal's SHA peripheral).
4. Verify size and SHA-256 against the manifest. On a mismatch or download failure: clear the flag,
   keep running the current image, report it in the next status row, and don't retry that version
   until the manifest changes, so a bad image can't cause a loop.
5. Point otadata at the new slot (esp-bootloader-esp-idf has OTA helpers for this) and restart.
6. The new image starts in pending-verify state and marks itself valid only after it reads the
   sensor and gets a STATUS_OK ack from QuestDB. If it resets before that, the bootloader reverts
   to the previous image.

Requirements and notes:

- **Partition table:** see [Flash layout](#flash-layout). OTA writes only the app slots and
  otadata, so credentials and settings survive updates.
- **Rollback needs bootloader support.** It requires a second-stage bootloader built with
  `CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE`. Check whether the bootloader espflash flashes by default
  has it; if not, build one from ESP-IDF and pass it with espflash's bootloader option. Also
  confirm how esp-bootloader-esp-idf exposes marking the running image valid. Prove this out first
  in milestone 4, before relying on remote updates.
- **No secrets in images.** The manifest and image are served over plain HTTP on the LAN, with
  SHA-256 as the integrity check. Because credentials live in the creds partition, an image fetched
  by anyone on the LAN reveals nothing. Signed images and secure boot are out of scope for v1.
- Every status row carries `fw_version`, so a rollout is visible in QuestDB.

## Flash layout

The 4 MB flash holds the bootloader, two app slots for OTA, and three small data partitions:

| Offset | Size | Name | What it holds | Who writes it |
|---|---|---|---|---|
| 0x0000 | 32 KB | bootloader | ESP-IDF second-stage bootloader, with rollback enabled | First USB flash only |
| 0x8000 | 4 KB | partition table | This table | First USB flash only |
| 0x9000 | 4 KB | creds | Wi-Fi SSID and password, QuestDB address, manifest URL, NTP server | First USB flash only; firmware only reads it |
| 0xA000 | 8 KB | settings | Merged manifest settings, as two 4 KB copies | Firmware, only when settings change |
| 0xC000 | 8 KB | otadata | Which app slot to boot, and its verify state | Firmware during OTA, bootloader on rollback |
| 0xE000 | 8 KB | (free) | Padding up to the 64 KB alignment app slots need | n/a |
| 0x10000 | 1,984 KB | ota_0 | App image, slot A | First flash, then OTA |
| 0x200000 | 1,984 KB | ota_1 | App image, slot B | OTA |
| 0x3F0000 | 64 KB | spare | Reserved, e.g. for a buffer spill during long outages | Nothing in v1 |

`partitions.csv` for espflash:

```csv
# Name,     Type, SubType, Offset,   Size
creds,      0x40, 0x00,    0x9000,   0x1000
settings,   0x40, 0x01,    0xA000,   0x2000
otadata,    data, ota,     0xC000,   0x2000
ota_0,      app,  ota_0,   0x10000,  0x1F0000
ota_1,      app,  ota_1,   0x200000, 0x1F0000
spare,      0x40, 0x02,    0x3F0000, 0x10000
```

- Custom type 0x40 for creds, settings and spare, read and written directly with esp-storage. No
  ESP-IDF NVS format.
- `settings` keeps two copies in alternate 4 KB sectors, each with a sequence number and CRC. New
  settings go into the older sector, so a power loss mid-write leaves the other copy valid. Flash
  erases a whole 4 KB sector at a time, hence one sector per copy.
- No factory partition. With otadata erased, the bootloader boots ota_0, where espflash puts the
  first image. Erase otadata on first flash.
- App slots are about 1.9 MB each, well above what a no_std image with Wi-Fi needs; `tools/release`
  checks the image size against the slot anyway.
- No nvs or phy_init partitions, since the bare-metal stack doesn't use them. If esp-radio can save
  radio calibration data, the free gap or spare can hold it.

## Error handling and diagnostics

Every wake ends in deep sleep no matter what fails, and every failure leaves a trace in the next
status row.

| Situation | Behaviour |
|---|---|
| Wake runs too long | Hard cap of 30 s for normal wakes and 3 minutes for OTA wakes (watchdog plus a deadline check); force sleep |
| Sensor missing or failing | Store the sample with SENSOR_FAIL; keep sending status |
| Wi-Fi or QuestDB unreachable | Keep buffering; back off as in [Wi-Fi and network](#wi-fi-and-network) |
| Buffer full | Drop oldest, count in `dropped_samples` |
| Battery under 20 % | Normal operation; the status rows let a dashboard alert you days ahead |
| Battery under 10 % | Double both intervals; skip OTA |
| Battery under 3.5 V (critical) | No Wi-Fi at all, since its current spikes could brown out the board and cause a reset loop. Keep buffering readings every 10 minutes and blink the NeoPixel red for about 50 ms on each wake. Resume normally once the gauge shows charging. The board still draws about 100 µA, so recharge within a few weeks |
| Unexpected reset (panic, brownout, watchdog) | Report `reset_reason` on the next send |
| Manifest unreachable or invalid | Keep last good settings from RTC memory or the settings partition |

Diagnostics:

- Logging through esp-println, at INFO by default via `ESP_LOG`. While charging, log every wake.
- NeoPixel off during normal operation; a debug build may blink it on each send.
- RTC-memory counters (`boot_count`, `failed_sends`, `dropped_samples`) reset only on power loss.

## Project structure and build

A new no_std Rust project on the same stack as s3-sensors: esp-hal 1.2, Embassy, esp-radio and
embassy-net. Logic that doesn't touch hardware lives in `lib.rs` so it can be tested on the host.

```
room-node/
  Cargo.toml
  rust-toolchain.toml
  build.rs
  partitions.csv       otadata, ota_0, ota_1, creds, settings
  .cargo/config.toml   target xtensa-esp32s3-none-elf, espflash runner
  tools/
    mkcreds            builds the creds blob from env vars
    release            builds the image, prints size and SHA-256
  src/
    lib.rs             no_std, host-testable: ring buffer, sample packing,
                       timestamp freezing and rate correction, manifest
                       merge, version compare, QWP record encoding
    qwp.rs             from s3-sensors, plus SYMBOL support
    sensors.rs         SHT41 and battery monitor over async I2C
    net.rs             Wi-Fi, cached reconnect, SNTP, HTTP GET
    ota.rs             download, verify, switch slot, mark valid
    bin/main.rs        one wake: init, pick normal or OTA path, then deep sleep
```

- **Toolchain:** the same as s3-sensors: `espup install --targets esp32s3`, then
  `cargo run --release` with espflash as the runner.
- **One task, one core:** each wake is a short straight-line async main, with no long-running tasks
  to coordinate. Run the CPU at 80 MHz rather than s3-sensors' `CpuClock::max()`, and leave out the
  PSRAM setup.
- **First flash:** flash the app with the partition table, then write the creds blob from
  `tools/mkcreds` to its partition offset. After that, updates go over OTA.
- **Version:** `env!("CARGO_PKG_VERSION")`. `tools/release` builds the image with espflash
  save-image and prints its size and SHA-256 for the manifest.
- **Host tests:** as in s3-sensors, `rustc --edition 2024 --test src/lib.rs`. Cover ring wrap and
  overflow, sample packing, timestamp freezing and rate correction, manifest merge and clamping,
  version comparison, SYMBOL encoding and the QWP record encoding.

## QuestDB setup and queries

Create the tables up front so deduplication is on and columns get the right types from the first
row; otherwise QWP creates them with DOUBLE columns. Replace `<ref_device>` with the reference
node's device ID, e.g. `'a1b2c3'`.

```sql
CREATE TABLE IF NOT EXISTS room_readings (
  ts TIMESTAMP, device SYMBOL, temp_c FLOAT, rh_pct FLOAT, flags LONG
) TIMESTAMP(ts) PARTITION BY DAY WAL
DEDUP UPSERT KEYS(ts, device);

CREATE TABLE IF NOT EXISTS device_status (
  ts TIMESTAMP, device SYMBOL, fw_version SYMBOL,
  battery_v FLOAT, battery_pct FLOAT, charge_rate FLOAT,
  rssi_dbm LONG, wifi_ms LONG, cached_connect LONG, rtc_drift_ppm LONG,
  buffered LONG, dropped_samples LONG, boot_count LONG,
  reset_reason LONG, battery_chip LONG
) TIMESTAMP(ts) PARTITION BY DAY WAL
DEDUP UPSERT KEYS(ts, device);

-- Written by hand whenever a node moves
CREATE TABLE IF NOT EXISTS device_locations (
  ts TIMESTAMP, device SYMBOL, room SYMBOL, nickname SYMBOL
) TIMESTAMP(ts) PARTITION BY YEAR WAL;
```

Hourly temperature per room, in °F:

```sql
SELECT ts, room, avg(temp_c * 9 / 5 + 32) AS temp_f
FROM (
  SELECT r.ts, l.room, r.temp_c
  FROM room_readings r
  ASOF JOIN device_locations l ON (device)
  WHERE r.flags = 0
) timestamp(ts)
SAMPLE BY 1h;
```

Each room's difference from the reference node at the same moment:

```sql
SELECT r.ts, r.device, (r.temp_c - ref.temp_c) * 9 / 5 AS delta_f
FROM room_readings r
ASOF JOIN (
  SELECT ts, temp_c FROM room_readings WHERE device = <ref_device>
) ref
WHERE r.device != <ref_device>;
```

Fleet health, latest row per device:

```sql
SELECT device, fw_version, battery_pct, rssi_dbm, ts
FROM device_status
LATEST ON ts PARTITION BY device;
```

## Milestones, testing and open questions

Build in five steps, each usable on its own; the first is plugged-in and needs none of the power
work.

1. **Plugged-in logger:** read SHT41 every 60 s and send each reading over QWP, reusing s3-sensors'
   encoder plus SYMBOL support. Proves sensor, schema and acks.
2. **Battery cycle:** deep sleep, RTC ring buffer, frozen timestamps with rate correction,
   batching, battery readings, low-battery behaviour and `device_status`.
3. **Identity, credentials and fast reconnect:** MAC-based ID, creds partition, hostname, cached
   BSSID and lease, fallback to DHCP.
4. **Remote config and OTA:** prove bootloader rollback first, then manifest fetch and merge, then
   the OTA wake path.
5. **Side-by-side check:** all nodes beside the Nest for 24 hours, in their enclosures. Differences
   there mostly mean enclosure heating, which is the thing to fix.

Tests:

- [ ] Host tests pass (see [Project structure and build](#project-structure-and-build))
- [ ] Unplug the router for 2 hours: no lost or duplicated rows after it returns
- [ ] Stop QuestDB mid-send, across a time sync: buffer kept, resent rows have identical timestamps
  and deduplicate
- [ ] Measure average current over one full send interval with a USB power meter or the battery
  monitor's charge rate
- [ ] Self-heating check: compare a node awake for 10 minutes with a sleeping one side by side
- [ ] Watch `rtc_drift_ppm` over a day with temperature swings
- [ ] Run a node down to the critical threshold: no Wi-Fi attempts, red blink, normal operation
  after charging
- [ ] OTA with a correct image, a wrong SHA-256, and an image that crashes on boot (must roll back)
- [ ] Remove the battery while running: node recovers cleanly on reconnect

Open questions:

- **RTC memory:** exact `#[ram(...)]` syntax for persistent RTC fast memory in esp-hal 1.2, and how
  much of it esp-hal itself uses.
- **OTA rollback:** does espflash's default bootloader have rollback enabled, and how does
  esp-bootloader-esp-idf mark an image valid?
- **Wake cost on esp-radio:** is there a full PHY calibration on every wake, and can calibration
  data be kept?
- **Battery monitor:** confirm the MAX17048 revision is powered from the battery like the published
  LC709203F schematic, and whether it's readable with GPIO7 off.
- **Reference node:** one of the three Feathers beside the Nest, or the ESPHome kit (which would
  need its own QWP path)?
- Spill the buffer to flash during long outages, given the flash wear trade-off?
- Where the manifest and firmware files are hosted (the QuestDB machine is the default
  assumption).
