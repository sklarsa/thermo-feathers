# thermo-feathers v2: ESP-NOW nodes and a headless gateway

Status: design, 2026-11-10. This replaces the WiFi, SNTP, cached-lease, manifest and OTA-over-HTTP
parts of [SPEC.md](../../../SPEC.md). Whatever this document doesn't mention (sensor reading,
validation, sample flags, battery gauge, low-battery behaviour, the QuestDB tables) stays as
SPEC.md has it.

## Why

The battery nodes were sending over WiFi, and that's the wrong transport for them:

- Joining WiFi takes about 1.5–3.5 s at roughly 120 mA per send, which dominates the power budget.
- It drags in DHCP, SNTP, TCP and RTC drift correction. Each of those failed at least once during
  milestone 2, and the deep-sleep send wakes never got a reading through after the first one.

The user wants room sensors that run for months on a battery, can be moved around the house freely,
and show up in QuestDB and somewhere they can glance at. The answer: the nodes talk only ESP-NOW,
straight to one wall-powered **gateway**. The gateway is the only device on WiFi. It forwards to
QuestDB, serves a web page that shows the rooms and holds their names and settings, and pushes
firmware updates to the nodes. It has no display for v1. A small scrolling LED matrix is designed below
as a later phase.

## Goals

- Battery nodes that wake, read, send over ESP-NOW in milliseconds, and deep-sleep again. The rough
  target is 4–5 months on the 1200 mAh LiPo. That's an estimate to check, not a measurement.
- Nodes can be moved anywhere in the house. They keep working as long as they're in ESP-NOW range
  of the gateway, whatever WiFi AP the gateway is on.
- One gateway, on wall power at a fixed spot, built from the old s3-sensors Feather ESP32-S3.
  - It forwards every reading and status row to QuestDB over QWP, with `qwp-embedded`.
  - It serves a small web page that shows every node's latest reading. The page is also where you
    rename nodes, choose °F or °C, set the wake interval and update node firmware.
  - Nothing to buy: v1 uses only hardware the user already has.
- Room for 4 nodes. The user has 3.
- No readings lost while the gateway is down for up to about 4 hours.

## Non-goals

- No sensors on the gateway. The LSM303AGR comes off, and s3-sensors' accelerometer streaming ends.
- No display in v1. The user chose headless to avoid any purchase. The display is designed in
  [Later: LED matrix](#later-led-matrix), and it plugs into the free STEMMA QT port with no
  rework.
- No WiFi on the nodes at all, not even for firmware updates.
- Gateway firmware updates go over USB for v1. Only the nodes update over the air.
- No authentication on the web page. It's on the home LAN only (my call).
- No ESP-NOW encryption for v1 (my call). The worst a neighbour could do is inject fake readings.

## Anti-goals

These would count as failure even if everything technically worked:

- Losing or duplicating readings during a gateway outage or a resend.
- A node that silently stops reporting after it's moved or after the gateway's AP changes channel.
- Soldering, or buying parts for v1.

## Hardware

**Nodes (3, room for 4):** unchanged from SPEC.md:
- ESP32-S3 Feather, 4 MB flash / 2 MB PSRAM (5477).
- SHT41 on a long STEMMA QT cable.
- 1200 mAh LiPo.
- The MAX17048 gauge on the board.

**Gateway:**
- The s3-sensors Feather ESP32-S3, with the LSM303AGR removed.
- Powered from a USB wall supply.
- Nothing else in v1. Its STEMMA QT port stays free for the later display.

## How it works

### Radio and channels

- ESP-NOW only works on 2.4 GHz channels 1–13. The gateway is a WiFi station on the home network,
  and ESP-NOW has to share its channel, which the AP decides. The AP can move: it went from channel
  1 to 6 on 2026-11-03 with no disconnect.
- Each node keeps the gateway's MAC and its last-known channel in RTC memory.
- Each send goes as unicast to the gateway on that channel, relying on ESP-NOW's MAC-layer ack and
  retry.
- If there's no ack, or no MAC is known yet (first boot), the node **sweeps channels 1–13**:
  - On each channel, it sends a broadcast discovery frame and waits briefly for the gateway's
    answer.
  - The gateway answers discovery frames with its MAC.
  - The node stores the channel and MAC it gets back.
  - A full sweep that finds nothing counts as a failed send: the samples stay buffered, and the
    node tries again next wake.
  - Repeated failed sweeps should back off so they don't drain the battery. The policy is up to the
    builder.
- The gateway has to have joined WiFi before it can answer, because that's what sets its channel.

### Node wake cycle

- Each wake follows the cadence from SPEC.md: every 60 s, from remote config, doubled below 10 %
  battery.
- The node reads the SHT41, pushes a sample into the RTC ring, and sends everything unacked in one
  or more ESP-NOW frames. That happens **on every wake**. There's no longer a separate 10-minute
  send interval, because a send is now cheap.
- Each frame carries the node's sample batch and its status:
  - battery V, %, charge rate
  - boot count
  - dropped samples
  - reset reason
  - firmware version
  - battery chip
- The gateway's reply carries:
  - the highest sample it has stored for that node, as an application-level ack
  - the current epoch time
  - the node's settings: wake interval, and whether to stay awake for an update
- The node pops acked samples, takes the time and settings, and deep-sleeps.
- Under 3.5 V (critical), SPEC.md's rule stays: no radio, keep buffering.

### Timestamps

- They stay **frozen on the node**, as milestone 2 built them:
  - The `Clock` in `src/lib.rs` syncs to the epoch time in every gateway reply instead of SNTP.
  - The rate correction and the TIME_UNSYNCED handling carry over unchanged.
- Since the node syncs every minute, drift barely matters.
- A resent sample keeps its timestamp, so QuestDB's `(ts, device)` deduplication absorbs duplicates.

### Buffering and acks

- **Node:** the 256-sample RTC ring from milestone 2, `SampleRing` in `src/lib.rs`, about 4 hours.
  It drops the oldest when full, and pops only what the gateway's reply acks.
- **Gateway:** it acks a sample once that sample is in its own RAM buffer, not when QuestDB acks it.
  That way node wakes stay short. The buffer sits in PSRAM, and the gateway pops it on QWP acks.
  - Hop by hop is my call.
  - A gateway reboot loses whatever QuestDB hadn't acked yet. That's accepted for v1.
- **Duplicates:** if a reply is lost, the node resends samples the gateway already has. QuestDB
  deduplication absorbs them, so no extra bookkeeping is needed.

### What goes to QuestDB

- `room_readings` stays as SPEC.md has it.
- `device_status` gets one row per node frame, or rarer if the builder thinks that's better. Its
  columns change:
  - It loses `wifi_ms` and `cached_connect`.
  - `rssi_dbm` becomes the RSSI of the node's frame as the gateway received it.
  - It gains `channel`.
  - It keeps everything else, including `rtc_drift_ppm`.
  - The live table at 192.168.7.74 has no rows that matter and can be dropped and re-created. Its
    schema lives in the homelab repo, `server-1/questdb/device_status.sql`.
- `device_locations` gets a row from the gateway on every rename. The name goes in `room`, which is
  the column SPEC.md's per-room queries join on. `nickname` stays empty (my call). The gateway's
  starting names come from the latest `device_locations` row per device, if there is one, so
  existing rows aren't lost (my call). After that, the gateway's flash is the source of truth.
- The gateway writes no table about itself in v1. It logs over USB serial.

### The web page

- This page is the v1 user interface. Make it phone-friendly and readable at a glance: one line or
  card per room, temperature first, in the chosen unit.
- It's served by the gateway on port 80.
- Without a screen, finding the page has to be easy. The user reserves a fixed IP for the gateway in
  the eero app (my call). The gateway also logs its address over USB serial. mDNS
  (`thermo-gateway.local`) is a nice extra if it's easy on embassy-net, but not required.
- For each node it shows:
  - ID (last three MAC bytes)
  - name
  - latest temperature and humidity
  - battery, flagged under 10 %
  - last seen, flagged as stale after about 3 missed wakes (my call)
  - firmware version
- It also shows the gateway's own state: WiFi channel, and whether QuestDB is connected.
- It has:
  - a text field to rename each node, any name
  - one site-wide °F/°C setting
  - the nodes' wake interval (default 60 s), sent to the nodes in every reply
  - a firmware upload form (below)
- The gateway keeps names, the unit and the wake interval in flash, so they survive a reboot.
- The gateway is the source of truth for names. QuestDB's `device_locations` is the history.
- Any small HTTP server on embassy-net will do.

### Firmware updates for nodes

1. On the web page, upload a node `.bin` and pick which nodes to update.
2. The gateway holds the image in PSRAM and sets "stay awake for update" in its next reply to each
   chosen node.
3. The node stays awake and receives the image in acked chunks over ESP-NOW. It writes the chunks
   into its inactive OTA slot (`ota_0` / `ota_1` in `partitions.csv`) and verifies the SHA-256.
4. The node switches its boot slot and reboots.
5. The new image marks itself valid after its first acked send. An image that never gets there
   rolls back.
6. The web page shows each node's progress and its version afterwards.

- **Battery guard:** only on charge, or above SPEC.md's `ota_min_battery_pct` (40 %).
- **First:** before relying on any of this, prove that the bootloader rolls back with an image that
  crashes on boot. SPEC.md lists this as an open question.
- If espflash's default bootloader can't roll back, flash an ESP-IDF bootloader with rollback
  enabled to each node over USB, ideally while the nodes are on the bench for milestone 2. If
  rollback can't be made to work at all, node OTA waits and updates stay on USB for v1. The user is
  fine with USB for a handful of nodes. (My call.)

### Settings

- The node's wake interval and the update flag come from the gateway in every reply.
- The wake interval is set on the web page. The user asked for it to be configurable. One interval
  covers all nodes (my call). The reply format carries it per node, so per-node intervals can come
  later without changing the protocol.
- Nodes store nothing but the last values they received.
- That replaces SPEC.md's manifest, creds and settings partitions on the node. The builder can
  repurpose or drop those partitions.
- The gateway's own WiFi credentials and QuestDB address are compile-time environment variables,
  as in s3-sensors.

## Constraints

- Rust `no_std` on esp-hal 1.2, Embassy and esp-radio, the same stack as now. Both firmwares live
  in this repo, sharing `src/lib.rs`.
- The ESP-NOW frame formats, sample packing and ack bookkeeping go in the shared library and get
  host tests, like the existing ones.
- No soldering, and nothing to buy for v1.
- A deep-sleep node can't be flashed over USB, because USB is off while it sleeps. Keep milestone
  2's 30 s awake window after power-on or RESET.

## The s3-sensors stall

The board is free: the user ended the s3-sensors stall hunt on 2026-11-10, and the sniffer is gone.
The stall is still **unexplained**. The board stopped getting data out, its ring filled, and a reset
fixed it instantly. A loss test pointed at packets failing to leave the board, not at a
`qwp-embedded` bug. The hourly reconnects were a separate, now-fixed ack bug in `qwp-embedded`
06aed5b. The gateway sends roughly 1,000× less data than s3-sensors did, but it's the same board
and the same crate, so treat a stall as a known risk:
- The gateway's PSRAM buffer rides out a stall.
- Its serial log and `qwp-embedded`'s `Stats` should make one visible.
- If the gateway stalls, debug it then. Don't block milestones on it.

## Known problem to fix first

Milestone 2 never confirmed that the board wakes from deep sleep:
- The first cold-boot send reached QuestDB at 01:27 UTC on 2026-11-10.
- The next send, 10 wakes later, never arrived.

The node can't work on any transport until that's understood. Start by getting a serial log of a
timer wake. The 30 s window gives a chance to attach.

## Milestones

1. **Deep-sleep wakes proven** on the current node firmware, ESP-NOW or not.
2. **ESP-NOW link:**
   - Nodes send each wake. The gateway receives, replies with time, and forwards `room_readings`
     to QuestDB.
   - Channel discovery works, including after the gateway moves channel.
3. **Buffer and acks end to end:**
   - Unplug the gateway for an hour, plug it back in: no gaps and no duplicates.
   - `device_status` rows arrive with the new columns.
4. **Web page:** the room view, names, units, wake interval, `device_locations` rows.
5. **Node OTA over ESP-NOW,** after the rollback check.
6. **Side-by-side check,** as in SPEC.md.
7. **Later, when the user buys the part:** the LED matrix (below).

## Left to the builder

- The ESP-NOW frame layouts, as long as they're versioned and host-tested.
- How the channel sweep backs off.
- HTTP server crate and the page's look.
- How often `device_status` rows are written.
- Where on the gateway's flash the names and unit live.
- Repo layout for the two binaries.

## Later: LED matrix

This isn't part of v1. Decided 2026-11-10, replacing an earlier OLED-and-buttons plan: no
soldering, one part.

**Part:** [Adafruit IS31FL3741 13×9 RGB LED matrix](https://www.adafruit.com/product/5201) (5201,
$14.95), on a STEMMA QT cable from the gateway's port (I2C0, SDA GPIO3, SCL GPIO4, GPIO7 high to
power the port). I²C address 0x30. It runs on 3.3 V; greens and blues are dimmer than at 5 V.

**Layouts**, chosen on the web page, mocked up live in `.superpowers/brainstorm/` on 2026-11-10:
- **D (default): tiny letters, still.** Room initial and temperature in a 3×5 font (`O76`), a
  humidity bar along the bottom two rows (length = % RH), 3 s per room.
- **A: full line, scrolling.** `Office 76° 41%` in a 5×7 font, about 5 s per room at 20 columns/s.
- **B: name and temperature, scrolling.** `Office 76°`.
- **C: big still digits.** 5×7 temperature, 3 s per room; the bottom row shows which room.

In every layout the temperature is tinted blue (cool) through white (about 72 °F) to amber (warm),
and a node stale after about 3 missed wakes shows in red: blinking dashes in the still layouts,
`Bedroom no data 12m` in the scrolling ones. The unit is the web page's °F/°C.

**Web page settings**, kept in flash with the names: layout, scroll speed, brightness, and off or a
night schedule. The page may also draw a live copy of the matrix.

**Firmware:** a small no_std driver for the IS31FL3741 over esp-hal's async I²C (Adafruit's
libraries are Arduino and Python only), a 5×7 font, and a task on the gateway that redraws from its
node table.
