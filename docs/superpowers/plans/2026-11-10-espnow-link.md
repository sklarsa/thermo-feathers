# ESP-NOW link (v2 milestones 1–3) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Battery nodes send every wake over ESP-NOW to a headless gateway, which forwards `room_readings`
and `device_status` to QuestDB with no gaps or duplicates across a gateway outage.

**Architecture:** One crate, two binaries (`node`, `gateway`) sharing `src/lib.rs`. The frame format
(`src/proto.rs`) and all ack, clock and node-table logic live in the library and are host-tested. The
node is a sleep/read/exchange/sleep loop with no WiFi association. The gateway is a WiFi station that
also listens on ESP-NOW, buffers records in PSRAM and runs `qwp-embedded`'s sender.

**Tech Stack:** Rust `no_std`, esp-hal 1.2.2, esp-rtos/Embassy, esp-radio 1.0.0-beta.1 (`wifi` +
`esp-now`), embassy-net 0.9.1, qwp-embedded.

**Spec:** `docs/superpowers/specs/2026-11-10-espnow-gateway-design.md` (read it with this plan).
Scope here is the spec's milestones 1–3. Milestones 4 (web page), 5 (node OTA) and 6 (side-by-side)
each get their own plan once this one ships.

## Global Constraints

- Rust `no_std` on esp-hal 1.2, Embassy and esp-radio, the same stack as now. Both firmwares live in this repo, sharing `src/lib.rs`.
- The ESP-NOW frame formats, sample packing and ack bookkeeping go in the shared library and get host tests.
- No soldering, nothing to buy.
- Keep the node's 30 s awake window after power-on or RESET (`FLASH_WINDOW`), so it can be flashed.
- Under 3.5 V (`CRITICAL_V`): no radio, keep buffering.
- Node wake interval default 60 s, doubled below 10 % battery; the gateway's reply overrides the base value.
- Never commit a full MAC address, IP, or credentials to this public repo. Device IDs (last 3 MAC bytes) are fine.
- Commits to this repo and to `sklarsa/qwp-embedded` need the user's go-ahead before pushing. The homelab repo may be pushed freely.
- Build env: `. ~/export-esp.sh`. Gateway creds: `WIFI_SSID`/`WIFI_PASSWORD` from `sops -d --extract '["wifi_ssid"]' ~/source/github/homelab/pi5/secrets.yaml` (and `wifi_password`), `QUESTDB_ADDR=192.168.7.74:9000`.
- Host tests: the two `rustc +stable` commands in `.github/workflows/rust_ci.yml` (`host-tests` job), run from the repo root with qwp-embedded checked out at the pinned rev. Call this **`host-test`** below.
- Duplicate checks in QuestDB use short windows (≤ 2 h); wide `count_distinct` over long ranges has OOM-killed it before.

## Review Focus

1. **A late or stray reply** (wrong nonce, or `stored` larger than what was sent) must not pop samples
   the gateway never stored. Pinned in Task 5 (`reply_wrong_nonce_ignored`, `reply_stored_clamped`).
2. **Gateway not yet SNTP-synced** sends `epoch_us = 0`; the node must not sync its clock to 1970,
   but should still pop what was stored. Pinned in Task 5 (`reply_without_time_still_acks`).
3. **First boot, no time yet:** unsynced samples must not be sent; they're frozen to real time after
   the first reply. Pinned in Task 5 (`sendable_prefix_stops_at_unsynced`).
4. **Foreign or damaged ESP-NOW frames** (other devices, truncated, other version) are ignored, never
   panic. Pinned in Task 4 (`decode_rejects_garbage`).
5. **More nodes than slots, or the symbol table full:** the extra node is logged and ignored, the
   others keep flowing. Pinned in Task 2 (`intern_full_returns_none`) and Task 5 (`nodes_full`).

---

### Task 1: Prove deep-sleep wakes (spec milestone 1)

No code unless the evidence says so. The current `src/bin/main.rs` is the firmware under test.

**Files:** none, unless a bug is found.

- [ ] **Step 1: Flash the node with milestone-2 firmware, battery connected, USB connected**

Run: `. ~/export-esp.sh && WIFI_SSID=... WIFI_PASSWORD=... QUESTDB_ADDR=192.168.7.74:9000 cargo run --release`
Expected: the cold-boot log, then "awake N ms, sleeping ~60000 ms".

- [ ] **Step 2: Capture every wake over serial**

USB re-enumerates on each wake, so `espflash monitor` won't follow it. Run a reopen loop, in the
background, logging to the scratchpad:
`while true; do cat /dev/serial/by-id/usb-Espressif* 2>/dev/null; sleep 0.2; done | while IFS= read -r l; do echo "$(date +%T) $l"; done > wakes.log`
Expected after 15 min: ≥ 10 wake logs about 60 s apart, `boot_count` rising by 1 each,
reset reason `CoreDeepSleep`, and the 10th wake attempting a send.

- [ ] **Step 3: Decide**

- Wakes happen on time → milestone 1 done. Note in the commit message of Task 6 what the 10th-wake
  send did; the WiFi path is deleted there, so don't fix it.
- No wake, wrong interval, or `boot_count` resets → stop. Use superpowers:systematic-debugging on the
  sleep/RTC path (`set_wakeup_deadline`, `State` persistence, `MAGIC`) before any other task. Every
  later task depends on this.

---

### Task 2: qwp-embedded — symbols that can grow while connected

The gateway learns device IDs and firmware versions at runtime; today `Config.symbols` is a fixed
`&[&str]`. Work in `~/source/github/qwp-embedded`.

**Files:**
- Create: `src/symbols.rs`
- Modify: `src/lib.rs` (export), `src/sender.rs` (`Config.symbols`, the message builder near line 290), `README.md`

**Interfaces:**
- Produces:
  - `pub struct Symbols` — append-only, capacity 32 names of ≤ 24 bytes, critical-section mutex inside, usable from a `static`.
  - `pub const fn Symbols::new() -> Self`
  - `pub fn Symbols::intern(&self, s: &str) -> Option<u64>` — existing id if present, else next id; `None` when full or `s` is too long.
  - `pub fn Symbols::len(&self) -> usize`
  - `pub fn Symbols::with<R>(&self, f: impl FnOnce(&[&str]) -> R) -> R` — all names, in id order.
  - `sender::Config.symbols: &'a Symbols` (was `&'a [&'a str]`).

- [ ] **Step 1: Write the failing tests** in `src/symbols.rs`

```rust
#[test]
fn intern_is_stable() {
    let s = Symbols::new();
    assert_eq!(s.intern("a1b2c3"), Some(0));
    assert_eq!(s.intern("0.3.0"), Some(1));
    assert_eq!(s.intern("a1b2c3"), Some(0));
    assert_eq!(s.len(), 2);
    s.with(|n| assert_eq!(n, ["a1b2c3", "0.3.0"]));
}

#[test]
fn intern_full_returns_none() {
    let s = Symbols::new();
    for i in 0..32 { assert!(s.intern(&format!("n{i}")).is_some()); }
    assert_eq!(s.intern("one-more"), None);
    assert_eq!(s.intern("n0"), Some(0));
    assert_eq!(s.intern(&"x".repeat(25)), None);
}
```

- [ ] **Step 2: Run** `cargo test --features sender` — Expected: FAIL, `Symbols` not found.
- [ ] **Step 3: Implement `Symbols`**, then switch the sender: when building each message, read
  `symbols.len()` **after** taking the records from the ring, and send names `symbols_sent..len` as
  the delta. A record can only hold an id that was interned before it was pushed, so it's always
  covered.
- [ ] **Step 4: Add a sender test** that a message built after a new `intern` carries exactly the
  new name in its delta dictionary (reuse the existing `symbol_dictionary` test's harness in
  `src/encoder.rs`).
- [ ] **Step 5: Run** `cargo test --features sender && cargo clippy --features sender -- -D warnings` — Expected: all pass, clean.
- [ ] **Step 6: Commit** `git commit -m "Symbols that can grow on a live connection"`. Ask the user before pushing; Task 3 pins the new rev.

---

### Task 3: Library records carry symbol ids; `device_status` columns change

**Files:**
- Modify: `src/lib.rs` (`Rec`, `Status`, `encode`, `SYM_*` consts removed, tests)
- Modify: `Cargo.toml`, `.github/workflows/rust_ci.yml` (qwp-embedded rev from Task 2)
- Modify (homelab repo): `server-1/questdb/device_status.sql`

**Interfaces:**
- Consumes: `qwp::Symbols` ids (Task 2).
- Produces:
  - `pub enum Rec { Reading { dev: u16, s: Sample }, Status { dev: u16, fw: u16, s: Status } }`
  - `Status` loses `wifi_ms`, `cached_connect`; gains `pub channel: u8`. Field order of the rest unchanged.
  - `pub fn device_id(mac: &[u8; 6]) -> [u8; 6]` — lowercase hex ASCII of the last 3 bytes.
  - `encode(e, recs)` writes `device`/`fw_version` from the ids in each record and a `channel LONG` column after `rssi_dbm`.

- [ ] **Step 1: Update the existing encode test** (`src/lib.rs` near line 502) to push records for
  two devices (`dev: 0` and `dev: 2`, `fw: 1`) with `with_symbols(..., &["a1b2c3", "0.3.0", "d4e5f6"])`
  and assert the encoded bytes contain `channel`, and not `wifi_ms`. Add:

```rust
#[test]
fn device_id_hex() {
    assert_eq!(&device_id(&[0, 0, 0, 0x61, 0x03, 0x38]), b"610338");
}
```

- [ ] **Step 2: Run** `host-test` — Expected: FAIL to compile.
- [ ] **Step 3: Implement** the `Rec`/`Status`/`encode`/`device_id` changes. Bump `version` to `0.3.0`.
- [ ] **Step 4: Run** `host-test` — Expected: PASS.
- [ ] **Step 5: Update** `device_status.sql`: drop `wifi_ms` and `cached_connect`, add
  `channel LONG, -- WiFi/ESP-NOW channel the gateway heard the node on` after `rssi_dbm`, and change
  the comments: `rssi_dbm` is "as received by the gateway", `buffered` is "samples in this frame".
  Re-create the live table (it has no rows that matter, per spec):
  `curl -sG http://192.168.7.74:9000/exec --data-urlencode 'query=DROP TABLE device_status'` then
  apply the file the same way. Expected: `SELECT * FROM device_status` returns the new columns, 0 rows.
- [ ] **Step 6: Commit** homelab (`server-1: device_status for ESP-NOW nodes`, push) and
  thermo-feathers (`Records carry symbol ids; device_status gains channel`, don't push yet).

`src/bin/main.rs` won't compile after this; Task 6 replaces it. Do Tasks 3–6 before the next push.

---

### Task 4: Frame format

**Files:**
- Create: `src/proto.rs` (`pub mod proto;` in `src/lib.rs`)

**Interfaces:**
- Consumes: `Sample` (`src/lib.rs`).
- Produces:

```rust
pub const MAGIC: u8 = b'T';
pub const VERSION: u8 = 1;
pub const FRAME_MAX: usize = 250;    // ESP_NOW_MAX_DATA_LEN (v1)
pub const MAX_SAMPLES: usize = 16;

pub struct NodeStatus {               // Copy, Default, PartialEq, Debug
    pub t_us: i64,                    // node's epoch time at wake, 0 if unsynced
    pub battery_v: f32, pub battery_pct: f32, pub charge_rate: f32,
    pub rtc_drift_ppm: i32,
    pub buffered: u16,                // ring length when the frame was built
    pub dropped_samples: u32, pub boot_count: u32,
    pub reset_reason: u8, pub battery_chip: u8,
    pub fw: [u8; 12],                 // version string, NUL-padded
}
pub struct Reply { pub nonce: u16, pub stored: u8, pub epoch_us: i64, pub wake_s: u16, pub flags: u8 }
pub enum Frame {
    Discover,                                            // node → broadcast
    Hello,                                               // gateway → node; its MAC is the source address
    Data { nonce: u16, status: NodeStatus, n: u8, samples: [Sample; MAX_SAMPLES] },
    Reply(Reply),
}
pub fn encode(f: &Frame, out: &mut [u8; FRAME_MAX]) -> usize;
pub fn decode(b: &[u8]) -> Option<Frame>;
```

Layout: `[MAGIC, VERSION, kind]` then fields little-endian, field by field (no transmutes). Kinds:
Discover 1, Hello 2, Data 3, Reply 4. `Reply.flags` bit 0 is reserved for "stay awake for update"
(milestone 5); nothing sets it yet.

- [ ] **Step 1: Write the failing tests** in `src/proto.rs`

```rust
#[test]
fn data_round_trips_full() {
    let mut samples = [Sample::default(); MAX_SAMPLES];
    for (i, s) in samples.iter_mut().enumerate() { *s = Sample::new(1_700_000_000 + i as u32, 21.5, 40.0, 0); }
    let status = NodeStatus { t_us: 1_700_000_000_000_000, boot_count: 7, fw: *b"0.3.0\0\0\0\0\0\0\0", ..Default::default() };
    let f = Frame::Data { nonce: 9, status, n: MAX_SAMPLES as u8, samples };
    let mut b = [0; FRAME_MAX];
    let len = encode(&f, &mut b);
    assert!(len <= FRAME_MAX);
    assert_eq!(decode(&b[..len]), Some(f));
}

#[test]
fn reply_and_hello_round_trip() {
    let mut b = [0; FRAME_MAX];
    for f in [Frame::Discover, Frame::Hello,
              Frame::Reply(Reply { nonce: 3, stored: 16, epoch_us: 1_700_000_000_000_000, wake_s: 60, flags: 0 })] {
        let len = encode(&f, &mut b);
        assert_eq!(decode(&b[..len]), Some(f));
    }
}

#[test]
fn decode_rejects_garbage() {
    let mut b = [0; FRAME_MAX];
    let len = encode(&Frame::Reply(Reply { nonce: 1, stored: 1, epoch_us: 1, wake_s: 60, flags: 0 }), &mut b);
    assert_eq!(decode(&b[..len - 1]), None);       // truncated
    b[1] = VERSION + 1;
    assert_eq!(decode(&b[..len]), None);           // other version
    assert_eq!(decode(&[]), None);
    assert_eq!(decode(&[0xAA; 40]), None);         // someone else's ESP-NOW
    let mut d = [0; FRAME_MAX];
    let n = encode(&Frame::Data { nonce: 0, status: NodeStatus::default(), n: 0, samples: [Sample::default(); MAX_SAMPLES] }, &mut d);
    d[n - 1] = MAX_SAMPLES as u8 + 1;              // n says more samples than fit
    assert_eq!(decode(&d[..n]), None);
}
```

(`Sample` and `Frame` need `PartialEq`/`Debug`; add `Default` to `Sample` if missing.)

- [ ] **Step 2: Run** `host-test` — Expected: FAIL, module missing.
- [ ] **Step 3: Implement** `encode`/`decode`. `Data` encodes only `n` samples; `decode` requires the
  exact length for the kind.
- [ ] **Step 4: Run** `host-test` — Expected: PASS.
- [ ] **Step 5: Commit** `ESP-NOW frame format`.

---

### Task 5: Ack handling and the gateway's node table

**Files:**
- Modify: `src/proto.rs` (node side), `src/lib.rs` (`Nodes`)

**Interfaces:**
- Consumes: `Clock`, `SampleRing` (`src/lib.rs`), `Reply`, `MAX_SAMPLES` (Task 4).
- Produces:
  - `pub fn sendable_prefix(ring: &SampleRing, max: usize) -> usize` — how many samples from the
    front are `Clock::sendable`, stopping at the first that isn't, at most `max`.
  - `pub fn apply_reply(clock: &mut Clock, ring: &mut SampleRing, nonce: u16, sent: u8, r: &Reply, rtc_us: i64) -> bool`
    — `false` and no change if `r.nonce != nonce`. Otherwise: if `r.epoch_us > 0`, `clock.sync(rtc_us, r.epoch_us)`
    and `clock.freeze_unsynced(ring)`; then `ring.pop(min(r.stored, sent))`; returns `true`.
  - `pub const MAX_NODES: usize = 8;`
  - `pub struct Node { pub mac: [u8; 6], pub dev: u16, pub fw: u16 }` (more fields come with the web page)
  - `pub struct Nodes` with `pub const fn new() -> Self` and
    `pub fn slot(&mut self, mac: [u8; 6]) -> Option<&mut Node>` — existing node, or a new zeroed one; `None` when all `MAX_NODES` are taken.

- [ ] **Step 1: Write the failing tests** (in `src/lib.rs`'s test module, reusing its `ring()` helper)

```rust
fn synced_clock() -> Clock { let mut c = Clock::default(); c.sync(0, 1_700_000_000_000_000).unwrap(); c }
fn reply(nonce: u16, stored: u8, epoch_us: i64) -> proto::Reply { proto::Reply { nonce, stored, epoch_us, wake_s: 60, flags: 0 } }

#[test]
fn reply_pops_what_was_stored() {
    let (mut c, mut r) = (synced_clock(), ring());
    for i in 0..5 { r.push(Sample::new(1_700_000_000 + i, 20.0, 40.0, 0)); }
    assert!(proto::apply_reply(&mut c, &mut r, 7, 5, &reply(7, 3, 1_700_000_060_000_000), 60_000_000));
    assert_eq!(r.len(), 2);
}

#[test]
fn reply_wrong_nonce_ignored() {
    let (mut c, mut r) = (synced_clock(), ring());
    r.push(Sample::new(1_700_000_000, 20.0, 40.0, 0));
    let before = c;
    assert!(!proto::apply_reply(&mut c, &mut r, 7, 1, &reply(6, 1, 1_800_000_000_000_000), 1));
    assert_eq!((r.len(), c), (1, before));
}

#[test]
fn reply_stored_clamped() {
    let (mut c, mut r) = (synced_clock(), ring());
    for i in 0..4 { r.push(Sample::new(1_700_000_000 + i, 20.0, 40.0, 0)); }
    proto::apply_reply(&mut c, &mut r, 1, 2, &reply(1, 200, 0), 1);
    assert_eq!(r.len(), 2);
}

#[test]
fn reply_without_time_still_acks() {
    let (mut c, mut r) = (Clock::default(), ring());
    r.push(Sample::new(1_700_000_000, 20.0, 40.0, 0));
    proto::apply_reply(&mut c, &mut r, 1, 1, &reply(1, 1, 0), 5);
    assert_eq!((r.len(), c.synced), (0, 0));
}

#[test]
fn sendable_prefix_stops_at_unsynced() {
    let mut r = ring();
    r.push(Sample::new(1_700_000_000, 20.0, 40.0, 0));
    r.push(Sample::new(42, 20.0, 40.0, TIME_UNSYNCED));
    r.push(Sample::new(1_700_000_120, 20.0, 40.0, 0));
    assert_eq!(proto::sendable_prefix(&r, 16), 1);
    let mut c = Clock::default();
    proto::apply_reply(&mut c, &mut r, 1, 0, &reply(1, 0, 1_700_000_200_000_000), 100_000_000);
    assert_eq!(proto::sendable_prefix(&r, 16), 3);   // frozen to real time by the first reply
}

#[test]
fn nodes_full() {
    let mut n = Nodes::new();
    for i in 0..MAX_NODES as u8 { assert!(n.slot([0, 0, 0, 0, 0, i]).is_some()); }
    assert!(n.slot([0, 0, 0, 0, 1, 0]).is_none());
    n.slot([0, 0, 0, 0, 0, 3]).unwrap().dev = 5;
    assert_eq!(n.slot([0, 0, 0, 0, 0, 3]).unwrap().dev, 5);
}
```

Adjust the unsynced-sample construction to whatever `Clock::stamp` produces for an unsynced RTC
time, if `Sample::new(42, …, TIME_UNSYNCED)` isn't how milestone 2 represents it.

- [ ] **Step 2: Run** `host-test` — Expected: FAIL.
- [ ] **Step 3: Implement** the four items above.
- [ ] **Step 4: Run** `host-test` — Expected: PASS.
- [ ] **Step 5: Commit** `Node-side ack handling and gateway node table`.

---

### Task 6: Node firmware over ESP-NOW

**Files:**
- Move: `src/bin/main.rs` → `src/bin/node.rs` (`git mv`), rewrite its send path
- Modify: `Cargo.toml` (`[[bin]] node` + `[[bin]] gateway` placeholder comes in Task 7; add `"esp-now"` to esp-radio features; keep `sntpc*`, the gateway uses them)
- Modify: `README.md` (flash command becomes `cargo run --release --bin node`; node needs no WiFi env vars)

**Interfaces:**
- Consumes: `proto::{encode, decode, Frame, NodeStatus, Reply, MAX_SAMPLES, FRAME_MAX, sendable_prefix, apply_reply}`, `wakes_to_next_send`, `Clock`, `SampleRing`.

**Behaviour, decided here:**
- `State` (bump `MAGIC` to `0x7EF0_0003`): remove `failed_sends`, `wakes_to_send`; add
  `gw_mac: [u8; 6]`, `gw_channel: u8` (0 = unknown), `nonce: u16`, `wake_s: u16` (0 = use 60),
  `sweep_failures: u32`, `wakes_to_sweep: u32`.
- Radio: start the WiFi controller in station mode without connecting, then `controller.esp_now()`.
  Add the gateway as a peer (and `BROADCAST_ADDRESS` for discovery).
- Every wake (above `CRITICAL_V`): read sensor and gauge as now, push the sample, then **exchange**:
  1. Build `Data` with `n = sendable_prefix(ring, MAX_SAMPLES)` (may be 0 — the first frame after a
     cold boot carries only status, to get the time), `nonce = st.nonce.wrapping_add(1)`.
  2. Send unicast to `gw_mac` on `gw_channel`; wait up to 50 ms for a `Reply`; one retry.
  3. On reply: `apply_reply`; take `wake_s`; if more sendable samples remain, go to 1. Stop after
     2 s total awake on the radio.
  4. No reply on the first frame, or `gw_channel == 0`: if `wakes_to_sweep == 0`, **sweep**
     channels 1–13: `set_channel(c)`, broadcast `Discover`, wait 30 ms for `Hello`. On `Hello`,
     store source MAC and `c`, reset `sweep_failures`, go to 1. A full sweep with nothing:
     `sweep_failures += 1`, `wakes_to_sweep = wakes_to_next_send(1, sweep_failures, wake_s)` (1, 2, 4…
     wakes, capped at an hour). Otherwise decrement `wakes_to_sweep`.
- Sleep deadline: the existing fixed-cadence logic, with `wake_s` from state (doubled below `LOW_PCT`).
- Delete: WiFi association, DHCP, SNTP, the qwp sender, `CONNECT_TIMEOUT`, `SEND_EVERY`, `SEND_CAP`.

- [ ] **Step 1: Implement** the above.
- [ ] **Step 2: Build** `cargo build --release --bin node && cargo clippy --release -- -D warnings` — Expected: clean.
- [ ] **Step 3: Flash** one node; no gateway exists yet. Expected serial log per wake: a sweep of 13
  channels finding nothing, then `wakes_to_sweep` 1, 2, 4… on later wakes, samples accumulating in the
  ring, awake time under ~1 s on sweep wakes and well under 100 ms otherwise.
- [ ] **Step 4: Commit** `Node: ESP-NOW instead of WiFi` (include Task 1's finding about the old 10th-wake send).

---

### Task 7: Gateway firmware

**Files:**
- Create: `src/bin/gateway.rs`
- Modify: `Cargo.toml` (`[[bin]] name = "gateway"`), `.github/workflows/rust_ci.yml` (CI already sets the WiFi/QuestDB env vars; nothing else), `README.md` (gateway section: env vars, `cargo run --release --bin gateway`, reserve its IP in eero)

**Interfaces:**
- Consumes: `proto::{decode, encode, Frame, Reply}`, `Nodes`, `Rec`, `Status`, `encode` (QuestDB), `device_id`, `qwp::{Symbols, sender}`.

**Behaviour, decided here:**
- Copy from s3-sensors `src/bin/main.rs`: WiFi station + DHCP + reconnect loop, PSRAM setup and the
  PSRAM-backed `SharedRing<Rec>` (its lines ~210–221; give the ring 1 MB), the `qwp::sender::run`
  task. Copy the SNTP function from the old node `main.rs`. No LSM303AGR, no accel tables.
- Power save off (`PowerSaveMode::None`) so ESP-NOW frames aren't missed while the station dozes.
- Gateway epoch = SNTP epoch + `embassy_time::Instant`; until SNTP succeeds, replies carry `epoch_us = 0`.
  Re-sync SNTP hourly.
- ESP-NOW receive loop (`controller.esp_now()`):
  - `Discover` → add the sender as a peer if needed, reply `Hello` unicast.
  - `Data` → `nodes.slot(src)`; if `None`, log "node table full: <device_id>" and drop. Otherwise
    intern `device_id(src)` and the frame's `fw` into `Symbols` (log and drop if full), push one
    `Rec::Reading` per sample and one `Rec::Status` (`t_us` = node's if non-zero else gateway epoch;
    `rssi_dbm` from `rx_control`; `channel` = current channel; `buffered` = status.buffered), **then**
    reply `Reply { nonce, stored: n, epoch_us, wake_s: 60, flags: 0 }` unicast.
  - Anything that doesn't decode → ignore.
- Log over USB serial: IP address once DHCP binds, channel, each frame (device id, n, rssi),
  and every 60 s the qwp `Stats` and ring length.

- [ ] **Step 1: Implement** the above.
- [ ] **Step 2: Build** `cargo build --release --bin gateway && cargo clippy --release -- -D warnings` — Expected: clean.
- [ ] **Step 3: Commit** `Gateway: ESP-NOW to QuestDB`.

---

### Task 8: Link on hardware (spec milestone 2)

Two USB data cables; identify each board by `/dev/serial/by-id/`.

- [ ] **Step 1:** Remove the LSM303AGR from the s3-sensors board; flash `gateway` with creds.
  Expected log: IP, channel, SNTP synced, QuestDB connected.
- [ ] **Step 2:** RESET the node. Expected within one sweep: node log shows `Hello` on the gateway's
  channel; the first frame carries 0 samples and gets time; the next frames drain the buffer.
- [ ] **Step 3:** Check QuestDB:
  `SELECT device, count(), max(ts) FROM room_readings WHERE ts IN '<today>;1h'`
  Expected: one row per minute for the node's ID, `max(ts)` within the last minute; and
  `SELECT * FROM device_status LATEST ON ts PARTITION BY device` shows `rssi_dbm`, `channel`, `fw_version = 0.3.0`.
- [ ] **Step 4: Gateway restart:** RESET the gateway. Expected: the node misses one or two replies,
  keeps buffering, and drains once the gateway is back, with no sweep needed (same channel).
- [ ] **Step 5: Lost channel:** the node is the side that has to find the gateway again, and the AP
  can't be made to switch channel on demand. Unplug the gateway for 3 minutes so the node fails and
  sweeps, then plug it back. Expected: the node's next due sweep finds it. A real AP channel move
  shows up later as a changed `channel` in `device_status`; check it after the first one happens.
- [ ] **Step 6:** Flash the other two nodes. Expected: three devices in `room_readings`.
- [ ] **Step 7:** Push both repos (with the user's go-ahead for thermo-feathers and qwp-embedded).

---

### Task 9: Outage with no gaps or duplicates (spec milestone 3)

- [ ] **Step 1:** Note the time, unplug the gateway for 60 minutes, plug it back in.
- [ ] **Step 2:** Within 10 minutes, every node has drained. Expected per node, with `<start>` 15
  minutes before the unplug:
  `SELECT device, count() FROM room_readings WHERE ts IN '<start>;90m' SAMPLE BY 1m`
  → exactly one row per device per minute, no missing minutes.
  `SELECT * FROM (SELECT device, ts, count() c FROM room_readings WHERE ts IN '<start>;90m') WHERE c > 1`
  → empty.
- [ ] **Step 3:** Same two queries on QuestDB being down instead: stop QuestDB's container on
  server-1 for 30 minutes (ask the user first; it's shared), start it again. Expected: the gateway's
  ring grows, then drains; same result.
- [ ] **Step 4:** Record both results in the README's status line and commit `Milestones 2 and 3 verified`.
