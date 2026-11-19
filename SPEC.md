# Room Sensor Node Firmware Spec

Oct 30, 2026

## Overview and goals

Each node is a battery-powered ESP32-S3 that wakes every 60 s to read temperature and humidity,
buffers readings in RTC memory, and sends them over ESP-NOW to a wall-powered gateway Feather. The
gateway is the only device on WiFi; it forwards everything to QuestDB over QWP. All nodes run one
identical firmware image; identity, room and settings live outside the binary. The
[v2 design](docs/v2-design.md) has the reasoning and the
gateway's web page.

Goals:

- Measure temperature and humidity per room, comparable against a reference node next to the Nest
  thermostat.
- Run for months on a 1200 mAh LiPo between charges.
- Never lose or duplicate readings when the gateway or QuestDB is briefly unavailable.
- Update firmware and settings remotely, with no USB access after first flash.
- Report device health (battery, signal, firmware version, errors) alongside the data.

Non-goals for v1: on-device display, Bluetooth, Home Assistant integration, sub-minute sampling.

## Hardware

Everything connects without soldering: the SHT41 plugs into the STEMMA QT port and the LiPo into
the battery connector.

| Part | Adafruit PID | Role | I²C address |
|---|---|---|---|
| ESP32-S3 Feather, 4MB Flash / 2MB PSRAM | 5477 | MCU, ESP-NOW radio, LiPo charger | n/a |
| MAX17048 (on the Feather) | built in | Battery voltage and % | 0x36 |
| SHT41 | 5776 | Temperature and humidity | 0x44 |
| LiPo 3.7 V 1200 mAh | 258 | Power | n/a |
| STEMMA QT cable 100 mm | 4210 | Runs down to the sensor, below the board in the enclosure | n/a |

The enclosure ([enclosure/](enclosure/README.md)) puts the SHT41 below everything that makes heat,
with a vented gap between them; two M2.5 screws hold the Feather and a third the shell.

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

## Operating cycle

The node spends nearly all its time in deep sleep. Every wake (60 s by default; the gateway sets
the interval in each reply) it reads the sensor, sends everything unacked over ESP-NOW, and sleeps
again. A send is a few milliseconds of radio, so there is no separate send interval.

The radio part of a wake is capped at 2 s. Measured battery life is in the README.

## Sensor readings

One SHT41 measurement and one battery read per wake.

- **SHT41:** a small raw I²C driver in the node. Send command 0xFD (high
  precision, heater off, since the heater warms the reading), wait at least 10 ms, read 6 bytes,
  and check both CRC-8 bytes (polynomial 0x31, init 0xFF). Convert with
  T = −45 + 175 · raw / 65535 and RH = −6 + 125 · raw / 65535, clamped to 0–100.
- **Battery (MAX17048):** Voltage from register 0x02 (78.125 µV per bit),
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
after the gateway acks them.

- **Storage:** RTC fast memory, not PSRAM. PSRAM and main SRAM lose their contents in deep sleep,
  so the PSRAM ring from s3-sensors doesn't carry over. The ring is placed with
  `#[esp_hal::ram(unstable(rtc_fast, persistent))]`, which takes types valid in any bit pattern.
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
- **Clearing:** remove samples only when the gateway's reply to that frame says how many it
  stored. The reply echoes the frame's nonce and is tagged as the answer to that frame's tag, so
  an old reply can't be replayed. The gateway acks once a sample is in its own PSRAM buffer, not
  when QuestDB acks it, except while restarting, or on an update's trial before QuestDB has acked
  anything: then it acks nothing and nodes resend. Anything else keeps the buffer; QuestDB
  deduplication absorbs resent rows. Planned restarts and a trial's rollback wait up to 30 s for
  QuestDB first; a crash loses what QuestDB hadn't acked yet, accepted for v1.
- **Lost on power loss:** RTC memory does not survive a battery disconnect or brownout. Accepted
  for v1.

Sample flags:

| Bit | Name | Meaning |
|---|---|---|
| 0 | SENSOR_FAIL | SHT41 read failed; values are NaN |
| 1 | OUT_OF_RANGE | Value failed validation; values are NaN |
| 2 | TIME_UNSYNCED | Taken before the first time sync; timestamp converted afterwards |
| 3 | CHARGING | Gauge reported a positive charge rate. Charging warms the board, so treat these readings as suspect. A full battery still on USB doesn't show up here |

## Radio

- **ESP-NOW only** on the nodes, 2.4 GHz channels 1–13. The gateway is a WiFi station, so ESP-NOW
  runs on whatever channel its AP picks, and the AP can move.
- Each node keeps the gateway's MAC and channel in RTC memory and sends unicast frames, relying on
  ESP-NOW's MAC-layer ack plus the gateway's reply.
- **Discovery:** with no gateway known, or when a send gets no reply, the node sweeps channels
  1–13, broadcasting a Discover frame on each. The gateway answers with Hello carrying its own
  channel, and the node keeps that channel and the sender's MAC.
- **Backoff:** failed sweeps are spaced 1, 2, 4… wakes apart, capped at an hour. Samples stay
  buffered meanwhile.
- The gateway answers Discover only after joining WiFi, since that's what sets its channel.
- Frame formats are in `src/proto.rs`, versioned and host-tested.

## Time sync and timestamps

Readings are timestamped on the device from the RTC clock, corrected with the clock rate measured
between syncs, and frozen as epoch seconds so resends are identical.

- **Clock source:** the Feather has no 32 kHz crystal (checked in Adafruit's schematic), so
  deep-sleep time comes from the S3's internal ~136 kHz RC oscillator. RC oscillators are less
  accurate than crystals and drift with temperature. Syncing every wake makes that barely matter.
- **Sync:** every gateway reply carries the gateway's epoch time (0 until the gateway has synced
  over SNTP), and the node syncs to it.
- **Rate tracking:** compare RTC elapsed time with the gateway's elapsed time between syncs at
  least 5 minutes apart to get the RTC's actual rate. Keep it in RTC memory and use it to convert later RTC
  readings to epoch time. Report it as `rtc_drift_ppm` in `device_status`.
- **Freezing:** a sample's epoch timestamp is computed once and never changes (see
  [Buffering and batching](#buffering-and-batching)).
- **First boot:** readings taken before the first reply carry TIME_UNSYNCED and are converted at
  the first successful sync.
- **Sanity check:** reject a gateway time more than 1 day from the RTC estimate unless the RTC was
  never synced; log it.
- **If drift ever matters:** a DS3231 real-time clock (temperature-compensated, about ±2 ppm, sold
  as a STEMMA QT board) on the same I²C chain would fix it. Not needed for v1.

## Data model and QWP sending

The gateway writes `room_readings` (one row per reading), `device_status` (one health row per node
wake) and `gateway_status` (its own, once a minute). Device ID and firmware version are SYMBOL
columns; everything else is FLOAT or LONG. The node tables deduplicate on timestamp plus device,
`gateway_status` on timestamp, so resending a batch is always safe.

`room_readings`, one row per sample:

| Column | Type | Notes |
|---|---|---|
| ts | TIMESTAMP | Designated timestamp; frozen sample time |
| device | SYMBOL | Device ID, e.g. a1b2c3 |
| temp_c | FLOAT | °C; NaN when SENSOR_FAIL or OUT_OF_RANGE |
| rh_pct | FLOAT | %RH; NaN when SENSOR_FAIL or OUT_OF_RANGE |
| flags | LONG | Sample flags bitfield |

`device_status`, one row per node wake:

| Column | Type | Notes |
|---|---|---|
| ts | TIMESTAMP | Designated timestamp; the node's time at wake, the same in every frame of that wake |
| device | SYMBOL | Device ID |
| fw_version | SYMBOL | e.g. 1.2.0 |
| battery_v | FLOAT | Volts |
| battery_pct | FLOAT | State of charge |
| charge_rate | FLOAT | %/hour; positive while charging |
| rssi_dbm | LONG | The node's frame as the gateway received it |
| rtc_drift_ppm | LONG | RTC rate error measured at this sync |
| buffered | LONG | Samples in the node's buffer when it sent |
| dropped_samples | LONG | Cumulative since power-on |
| boot_count | LONG | Wakes since the last restart (power-on, update, watchdog or panic) |
| reset_reason | LONG | esp-hal SocResetReason of the last restart other than a deep-sleep wake; 255 = firmware panic |
| battery_chip | LONG | 0 none, 1 MAX17048, 2 LC709203F |
| channel | LONG | Channel the gateway heard the node on |
| sends | LONG | Data frames sent, retries included; cumulative since power-on, like the four below |
| send_fails | LONG | Sends with no ESP-NOW MAC-layer ack |
| no_replies | LONG | Acked sends the gateway didn't reply to within 50 ms |
| sweeps | LONG | Searches of channels 1–13 for the gateway |
| sweep_misses | LONG | Sweeps that didn't find it |

The five link counters ride after a Data frame's samples; the gateway still takes frames without
them (0s), from nodes that predate them.

`gateway_status`, one row a minute while the gateway has time and its ring is under half full:
ts, fw_version, uptime_s, channel, ring_len, ring_dropped, qwp_acks, qwp_reconnects, qwp_rejected,
frames (node frames with a good tag), unsigned (ours without one) and reply_fails. All but the
first four count since the gateway booted.

Sending, on the gateway:

- QWP over WebSocket with [qwp-embedded](https://github.com/sklarsa/qwp-embedded), one persistent
  connection. Device IDs and firmware versions are SYMBOLs, sent with its delta symbol dictionary
  (`Encoder::with_symbols`).
- Rows queue in a 1 MB PSRAM ring and leave it only on QuestDB's ack. The status row is skipped
  until the node has time.

## Device identity and settings

A node's ID comes from its factory MAC; its room and nickname live only in QuestDB; its settings
come from the gateway.

- **Device ID:** the last 3 bytes of the factory MAC as 6 lowercase hex characters (e.g.
  `a1b2c3`). The same string is the device SYMBOL in QuestDB. The full MAC is logged at boot. Same
  firmware image on every node.
- **Rooms:** never in node firmware. The gateway keeps each node's name (its Settings page) and
  writes a `device_locations` row on every rename, recording where each device is and since
  when; queries match readings to rooms with ASOF JOIN, so history stays right when a node moves.
- **Settings:** every gateway reply carries the wake interval (clamped to 5–3600 s) and a flags
  byte (bit 0: stay awake for an update). Nodes store only the last values received. The gateway's
  WiFi credentials are compile-time environment variables; its QuestDB address and time server
  are too, overridable on its Settings page.

## OTA updates

Built in milestone 5 of the v2 design: the gateway holds an uploaded image and sets the update
flag in its replies; the node stays awake, fetches the image chunk by chunk over ESP-NOW into its
inactive slot, verifies the SHA-256, switches slots and reboots.

- **Battery guard:** only while charging, or above 40 %.
- **Backoff:** after a failed update, a node ignores offers for an hour.
- **Rollback:** the new image starts in pending-verify state and marks itself valid only after its
  first acked send. This needs a second-stage bootloader built with
  `CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE` (`bootloader/`), flashed once over USB; an image that
  crashes on boot was shown to roll back.
- Every status row carries `fw_version`, so a rollout is visible in QuestDB.

## Flash layout

The 4 MB flash holds the bootloader, two app slots for OTA, and three small data partitions:

| Offset | Size | Name | What it holds | Who writes it |
|---|---|---|---|---|
| 0x0000 | 32 KB | bootloader | ESP-IDF second-stage bootloader, with rollback enabled | First USB flash only |
| 0x8000 | 4 KB | partition table | This table | First USB flash only |
| 0x9000 | 4 KB | creds | Node: its ESP-NOW key (`src/keys.rs`) | `scripts/flash-node.sh` |
| 0xA000 | 8 KB | settings | Gateway: node names, unit, wake interval, address overrides | The gateway's Settings page |
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

- No factory partition. With otadata erased, the bootloader boots ota_0, where espflash puts the
  first image. Erase otadata on first flash.
- App slots are about 1.9 MB each, well above what a no_std node image needs.
- No nvs or phy_init partitions, since the bare-metal stack doesn't use them. If esp-radio can save
  radio calibration data, the free gap or spare can hold it.

## Error handling and diagnostics

Every wake ends in deep sleep no matter what fails, and every failure leaves a trace in the next
status row.

| Situation | Behaviour |
|---|---|
| Wake runs too long | Radio part capped at 2 s; force sleep |
| Sensor missing or failing | Store the sample with SENSOR_FAIL; keep sending status |
| Gateway unreachable | Keep buffering; sweep with backoff as in [Radio](#radio) |
| QuestDB unreachable | The gateway buffers in PSRAM and keeps acking nodes (not on an update's trial: see [Buffering](#buffering-and-batching)) |
| Buffer full | Drop oldest, count in `dropped_samples` |
| Battery under 20 % | Normal operation; the status rows let a dashboard alert you days ahead |
| Battery under 10 % | Double the wake interval; skip OTA |
| Battery under 3.5 V (critical) | No radio at all, since its current spikes could brown out the board and cause a reset loop. Keep buffering readings every 10 minutes and blink the NeoPixel red for about 50 ms on each wake. Resume normally once the gauge shows charging. The board still draws about 100 µA, so recharge within a few weeks |
| Firmware panic | Deep-sleep 60 s and restart; report `reset_reason` 255 |
| Unexpected reset (brownout, watchdog) | Report `reset_reason` on the next send |

Diagnostics:

- Logging through esp-println, at INFO by default via `ESP_LOG`. While charging, log every wake.
- NeoPixel off during normal operation; a debug build may blink it on each send.
- `dropped_samples` resets only on power loss; `boot_count` on any restart other than a deep
  sleep wake.

## Project structure and build

no_std Rust on esp-hal 1.2, Embassy, esp-radio and embassy-net. Logic that doesn't touch hardware
lives in the library so it can be tested on the host.

```
thermo-feathers/
  partitions.csv       otadata, ota_0, ota_1, plus unused data partitions
  src/
    lib.rs             host-testable: ring buffer, sample packing, clock sync and rate
                       correction, backoff, QWP record encoding
    proto.rs           ESP-NOW frame formats and reply handling
    keys.rs            per-node keys from the master secret, frame tags
    sha256.rs          SHA-256 and HMAC, dependency-free
    image.rs           app image parsing and checks, for OTA
    otadata.rs         which app slot the bootloader boots
    web.rs             the gateway's web page and stored settings
    flash.rs           flash helpers both binaries include
    bin/node.rs        one wake: read, send over ESP-NOW, deep sleep
    bin/gateway.rs     WiFi station + ESP-NOW listener, SNTP, QWP sender
```

- **Node:** each wake is a short straight-line async main at 80 MHz, no PSRAM.
- **Gateway:** long-running Embassy tasks, PSRAM for the QWP ring.
- **Version:** `env!("CARGO_PKG_VERSION")`.
- **Host tests:** see the README.

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
  rssi_dbm LONG, rtc_drift_ppm LONG,
  buffered LONG, dropped_samples LONG, boot_count LONG,
  reset_reason LONG, battery_chip LONG, channel LONG,
  sends LONG, send_fails LONG, no_replies LONG, sweeps LONG, sweep_misses LONG
) TIMESTAMP(ts) PARTITION BY DAY WAL
DEDUP UPSERT KEYS(ts, device);

CREATE TABLE IF NOT EXISTS gateway_status (
  ts TIMESTAMP, fw_version SYMBOL, uptime_s LONG, channel LONG,
  ring_len LONG, ring_dropped LONG, qwp_acks LONG, qwp_reconnects LONG, qwp_rejected LONG,
  frames LONG, unsigned LONG, reply_fails LONG
) TIMESTAMP(ts) PARTITION BY DAY WAL
DEDUP UPSERT KEYS(ts);

-- The gateway writes a row on every rename
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

Milestones are in the [v2 design](docs/v2-design.md#milestones).

Tests:

- [x] Host tests pass (see [Project structure and build](#project-structure-and-build))
- [x] Unplug the gateway for an hour: no lost or duplicated rows after it returns (2026-11-11)
- [x] Stop QuestDB for a while: the gateway buffers, then catches up with no duplicates (2026-11-11)
- [ ] Move the gateway's AP to another channel: nodes find it again
- [x] Measure average current over a few wakes with a USB power meter or the battery monitor's
  charge rate (2026-11-16, from the gauge: see the README)
- [ ] Self-heating check: compare a node in its enclosure with a bare one side by side, asleep
  and after 10 minutes awake
- [ ] Watch `rtc_drift_ppm` over a day with temperature swings
- [ ] Run a node down to the critical threshold: no radio, red blink, normal operation
  after charging
- [ ] OTA with a correct image, a wrong SHA-256, and an image that crashes on boot (must roll back);
  correct and crashing done (2026-11-12)
- [ ] Remove the battery while running: node recovers cleanly on reconnect
- [ ] Force a panic: the node sleeps, restarts, and reports `reset_reason` 255

Open questions:

- ~~RTC memory syntax~~: `#[esp_hal::ram(unstable(rtc_fast, persistent))]`, for types valid in
  any bit pattern.
- ~~OTA rollback~~: espflash's bootloader can't roll back; `bootloader/` builds ESP-IDF's with it
  on, and the firmware marks itself valid by rewriting its otadata entry.
- ~~Reply latency~~: yes, at -30 to -50 dBm.
- **Wake cost on esp-radio:** is there a full PHY calibration on every wake, and can calibration
  data be kept?
- **Battery monitor:** confirm the MAX17048 revision is powered from the battery like the published
  LC709203F schematic, and whether it's readable with GPIO7 off.
- **Reference node:** one of the three Feathers beside the Nest, or the ESPHome kit (which would
  need its own QWP path)?
- Spill the buffer to flash during long outages, given the flash wear trade-off?
- **Enclosure** ([enclosure/](enclosure/README.md)): a first core and shell printed 2026-11-17;
  the fixes from them (post roots, sensor fit, cable slot, battery ribs) aren't printed yet. Does
  the battery stay put under the ribs, and how much warmer does a node read inside it than bare?
  That self-heating is the side-by-side check's main error. Only the plain shell exists.
