//! Everything that doesn't touch hardware, tested on the host (see the README).
#![cfg_attr(not(test), no_std)]

pub use qwp_embedded as qwp;

pub mod proto;
pub mod web;

/// Sample flags (SPEC.md, Buffering and batching).
pub const SENSOR_FAIL: u8 = 1 << 0;
pub const OUT_OF_RANGE: u8 = 1 << 1;
pub const TIME_UNSYNCED: u8 = 1 << 2;
pub const CHARGING: u8 = 1 << 3;

/// SHT41 address and its high-precision, heater-off measure command.
pub const SHT41: u8 = 0x44;
pub const SHT41_MEASURE: u8 = 0xFD;

/// Battery gauges, probed in this order; `battery_chip` reports which answered.
pub const MAX17048: u8 = 0x36;
pub const LC709203F: u8 = 0x0B;

pub const RING_LEN: usize = 256; // ~4 h at 60 s

/// The wake intervals a node accepts from the gateway, and the web page offers.
pub const WAKE_MIN_S: u16 = 10;
pub const WAKE_MAX_S: u16 = 3600;

/// Sensirion CRC-8: polynomial 0x31, init 0xFF.
fn crc8(data: &[u8]) -> u8 {
    data.iter().fold(0xFF, |mut crc, b| {
        crc ^= b;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 {
                crc << 1 ^ 0x31
            } else {
                crc << 1
            };
        }
        crc
    })
}

/// Turns the SHT41's 6-byte answer into (°C, %RH, flags). A bad CRC is a failed read; values
/// outside -20..60 °C are out of range, humidity is clamped to 0..100. Bad values are NaN.
pub fn sht41_parse(b: &[u8; 6]) -> (f32, f32, u8) {
    if crc8(&b[0..2]) != b[2] || crc8(&b[3..5]) != b[5] {
        return (f32::NAN, f32::NAN, SENSOR_FAIL);
    }
    let raw = |hi: u8, lo: u8| u16::from_be_bytes([hi, lo]) as f32 / 65535.0;
    let t = -45.0 + 175.0 * raw(b[0], b[1]);
    let rh = (-6.0 + 125.0 * raw(b[3], b[4])).clamp(0.0, 100.0);
    if !(-20.0..=60.0).contains(&t) {
        return (f32::NAN, f32::NAN, OUT_OF_RANGE);
    }
    (t, rh, 0)
}

/// MAX17048 registers to (volts, %, %/hour).
pub fn max17048(vcell: u16, soc: u16, crate_: u16) -> (f32, f32, f32) {
    (
        vcell as f32 * 78.125e-6,
        soc as f32 / 256.0,
        crate_ as i16 as f32 * 0.208,
    )
}

/// One sample as it sits in RTC memory through deep sleep: 12 bytes. Every bit pattern is valid,
/// so a half-written slot after a reset is garbage, never undefined behaviour.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Sample {
    /// Epoch seconds, or RTC seconds while `flags` has TIME_UNSYNCED.
    pub ts_s: u32,
    pub temp_cc: i16,
    pub rh_cp: u16,
    pub flags: u8,
    _pad: [u8; 3],
}

/// Half away from zero; `f32::round` needs std.
fn round(x: f32) -> f32 {
    x + if x < 0.0 { -0.5 } else { 0.5 }
}

const TEMP_NAN: i16 = i16::MIN;
const RH_NAN: u16 = u16::MAX;

impl Sample {
    pub fn new(ts_s: u32, temp_c: f32, rh_pct: f32, flags: u8) -> Self {
        Self {
            ts_s,
            temp_cc: if temp_c.is_nan() {
                TEMP_NAN
            } else {
                round(temp_c * 100.0) as i16
            },
            rh_cp: if rh_pct.is_nan() {
                RH_NAN
            } else {
                round(rh_pct * 100.0) as u16
            },
            flags,
            _pad: [0; 3],
        }
    }

    pub fn temp_c(&self) -> f32 {
        if self.temp_cc == TEMP_NAN {
            f32::NAN
        } else {
            self.temp_cc as f32 / 100.0
        }
    }

    pub fn rh_pct(&self) -> f32 {
        if self.rh_cp == RH_NAN {
            f32::NAN
        } else {
            self.rh_cp as f32 / 100.0
        }
    }
}

/// Drop-oldest ring of samples in RTC memory. Indexes are plain integers so it survives a reboot
/// as data; `fix` clamps them if the memory held garbage.
#[repr(C)]
pub struct SampleRing {
    head: u16,
    len: u16,
    buf: [Sample; RING_LEN],
}

impl SampleRing {
    pub fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
    }

    /// Returns whether the oldest sample was dropped to make room.
    pub fn push(&mut self, s: Sample) -> bool {
        let full = self.len() == RING_LEN;
        if full {
            self.head = ((self.head as usize + 1) % RING_LEN) as u16;
            self.len -= 1;
        }
        self.buf[(self.head as usize + self.len()) % RING_LEN] = s;
        self.len += 1;
        full
    }

    pub fn len(&self) -> usize {
        self.len as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn get_mut(&mut self, i: usize) -> &mut Sample {
        &mut self.buf[(self.head as usize + i) % RING_LEN]
    }

    pub fn get(&self, i: usize) -> Sample {
        self.buf[(self.head as usize + i) % RING_LEN]
    }

    /// Removes the `n` oldest: the ones QuestDB acked.
    pub fn pop(&mut self, n: usize) {
        let n = n.min(self.len());
        self.head = ((self.head as usize + n) % RING_LEN) as u16;
        self.len -= n as u16;
    }

    pub fn fix(&mut self) {
        if self.head as usize >= RING_LEN || self.len() > RING_LEN {
            self.clear();
        }
    }

    /// Keeps only the samples with epoch time, in order: after a reset, ones still in RTC seconds
    /// have lost their timebase.
    pub fn retain_sendable(&mut self) {
        let mut n = 0;
        for i in 0..self.len() {
            let s = self.get(i);
            if Clock::sendable(&s) {
                *self.get_mut(n) = s;
                n += 1;
            }
        }
        self.len = n as u16;
    }
}

/// The RTC's time since power-up on one fixed scale. esp-hal converts ticks to `us` with
/// `cal_now`, a calibration it re-measures each boot, so its noise times hours of ticks moves the
/// whole value by seconds between wakes. Rescaling to `cal_ref` (saved at cold boot) undoes that;
/// the RC oscillator's real rate error is then constant, and Clock's ppm learns it.
pub fn steady_us(us: u64, cal_now: u32, cal_ref: u32) -> i64 {
    if cal_now == 0 {
        return us as i64;
    }
    (us as u128 * cal_ref as u128 / cal_now as u128) as i64
}

/// RTC-to-epoch conversion, corrected for the RTC's measured rate (SPEC.md, Time sync).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Clock {
    pub synced: u8,
    pub sync_rtc_us: i64,
    pub sync_epoch_us: i64,
    /// How fast the RTC runs against true time: true elapsed = rtc elapsed × (1 + ppm / 1e6).
    pub ppm: f32,
    /// Where the rate is measured from. Syncs come every wake, too close together for a rate, so
    /// this point stays put until one lands MIN_RATE_SPAN_US later.
    pub rate_rtc_us: i64,
    pub rate_epoch_us: i64,
    /// Syncs refused in a row. REFUSALS of them and the gateway wins: it's the only time source.
    pub refused: u8,
}

const EPOCH_MIN_S: u32 = 1_000_000_000; // 2001
/// Gateway times outside 2020..2100 are ignored by nodes, and node status times by the gateway.
pub const EPOCH_MIN_US: i64 = 1_577_836_800_000_000;
pub const EPOCH_MAX_US: i64 = 4_102_444_800_000_000;
const REFUSALS: u8 = 3;
/// A rate further off than this is a time step, not the RC oscillator (measured: -3000).
const PPM_MAX: f32 = 50_000.0;
const DAY_US: i64 = 86_400_000_000;
/// Shorter gaps than this make a noisy rate estimate; keep the old one.
const MIN_RATE_SPAN_US: i64 = 300_000_000;

impl Clock {
    pub fn epoch_us(&self, rtc_us: i64) -> i64 {
        let dt = rtc_us - self.sync_rtc_us;
        self.sync_epoch_us + dt + (dt as f64 * self.ppm as f64 / 1e6) as i64
    }

    /// Takes the gateway's time: `epoch_us` was true at `rtc_us`. Once synced, refuses one more
    /// than a day from the estimate, unless REFUSALS in a row say so: then it starts over from it.
    /// Returns the step from the estimate, in µs.
    pub fn sync(&mut self, rtc_us: i64, epoch_us: i64) -> Result<i64, i64> {
        if self.synced == 0 {
            *self = Self {
                synced: 1,
                sync_rtc_us: rtc_us,
                sync_epoch_us: epoch_us,
                ppm: 0.0,
                rate_rtc_us: rtc_us,
                rate_epoch_us: epoch_us,
                refused: 0,
            };
            return Ok(0);
        }
        let step = epoch_us - self.epoch_us(rtc_us);
        if step.abs() > DAY_US {
            self.refused += 1;
            if self.refused < REFUSALS {
                return Err(step);
            }
            self.synced = 0;
            self.sync(rtc_us, epoch_us)?;
            return Ok(step);
        }
        self.refused = 0;
        let rtc_span = rtc_us - self.rate_rtc_us;
        if rtc_span >= MIN_RATE_SPAN_US {
            let true_span = epoch_us - self.rate_epoch_us;
            self.ppm = ((true_span - rtc_span) as f64 * 1e6 / rtc_span as f64) as f32;
            self.ppm = self.ppm.clamp(-PPM_MAX, PPM_MAX);
            self.rate_rtc_us = rtc_us;
            self.rate_epoch_us = epoch_us;
        }
        self.sync_rtc_us = rtc_us;
        self.sync_epoch_us = epoch_us;
        Ok(step)
    }

    /// Converts samples taken before the first sync, once: their `ts_s` was RTC seconds. They keep
    /// TIME_UNSYNCED, since their time is an estimate; an epoch-sized `ts_s` marks them converted
    /// (RTC seconds would take 30 years to get there).
    pub fn freeze_unsynced(&self, ring: &mut SampleRing) {
        for i in 0..ring.len() {
            let s = ring.get_mut(i);
            if s.flags & TIME_UNSYNCED != 0 && s.ts_s < EPOCH_MIN_S {
                s.ts_s = (self.epoch_us(s.ts_s as i64 * 1_000_000) / 1_000_000) as u32;
            }
        }
    }

    /// Whether a sample's timestamp is epoch time yet, so it can be sent.
    pub fn sendable(s: &Sample) -> bool {
        s.ts_s >= EPOCH_MIN_S
    }

    /// The timestamp a new sample freezes with, and its TIME_UNSYNCED flag.
    pub fn stamp(&self, rtc_us: i64) -> (u32, u8) {
        if self.synced == 0 {
            ((rtc_us / 1_000_000) as u32, TIME_UNSYNCED)
        } else {
            ((self.epoch_us(rtc_us) / 1_000_000) as u32, 0)
        }
    }
}

/// Wakes until the next send after `failures` failed sends in a row: 1×, 2×, 4×… the send
/// interval, capped at an hour of wakes.
pub fn wakes_to_next_send(send_every: u32, failures: u32, wake_s: u32) -> u32 {
    let cap = (3600 / wake_s.max(1)).max(1);
    (send_every << failures.min(16)).min(cap.max(send_every))
}

/// Wakes to skip before the next channel sweep after `failures` failed sweeps in a row: sweeps
/// come 1, 2, 4… wakes apart, at most an hour apart.
pub fn sweep_skips(failures: u32, wake_s: u32) -> u32 {
    wakes_to_next_send(1, failures.saturating_sub(1), wake_s) - 1
}

/// Everything QuestDB gets: samples for `room_readings`, statuses for `device_status`, renames for
/// `device_locations`. `dev`, `fw` and `room` are symbol ids, interned by the gateway.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Rec {
    Reading { dev: u16, s: Sample },
    Status { dev: u16, fw: u16, s: Status },
    Location { dev: u16, room: u16, t_us: i64 },
}

/// A node's ID: its MAC's last three bytes in lowercase hex.
pub fn device_id(mac: &[u8; 6]) -> [u8; 6] {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut id = [0; 6];
    for (i, b) in mac[3..].iter().enumerate() {
        id[2 * i] = HEX[(b >> 4) as usize];
        id[2 * i + 1] = HEX[(b & 0xF) as usize];
    }
    id
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Status {
    pub t_us: i64,
    pub battery_v: f32,
    pub battery_pct: f32,
    pub charge_rate: f32,
    pub rssi_dbm: i32,
    pub channel: u8,
    pub rtc_drift_ppm: i32,
    pub buffered: u32,
    pub dropped_samples: u32,
    pub boot_count: u32,
    pub reset_reason: u32,
    pub battery_chip: u8,
}

/// All three tables. Column types match the tables SPEC.md creates up front.
pub fn encode(e: &mut qwp::Encoder<'_>, recs: &[Rec]) -> Result<(), qwp::Full> {
    use qwp::{FLOAT, LONG, SYMBOL};
    let readings = || {
        recs.iter().filter_map(|r| match r {
            Rec::Reading { dev, s } => Some((*dev as u64, s)),
            _ => None,
        })
    };
    let n = readings().count();
    if n > 0 {
        e.table(
            "room_readings",
            n,
            &[
                ("device", SYMBOL),
                ("temp_c", FLOAT),
                ("rh_pct", FLOAT),
                ("flags", LONG),
            ],
        )?;
        e.symbols(readings().map(|(d, _)| d))?;
        e.f32s(readings().map(|(_, s)| s.temp_c()))?;
        e.f32s(readings().map(|(_, s)| s.rh_pct()))?;
        e.i64s(readings().map(|(_, s)| s.flags as i64))?;
        e.timestamps(readings().map(|(_, s)| s.ts_s as i64 * 1_000_000))?;
    }

    let status = || {
        recs.iter().filter_map(|r| match r {
            Rec::Status { dev, fw, s } => Some((*dev as u64, *fw as u64, s)),
            _ => None,
        })
    };
    let n = status().count();
    if n > 0 {
        e.table(
            "device_status",
            n,
            &[
                ("device", SYMBOL),
                ("fw_version", SYMBOL),
                ("battery_v", FLOAT),
                ("battery_pct", FLOAT),
                ("charge_rate", FLOAT),
                ("rssi_dbm", LONG),
                ("channel", LONG),
                ("rtc_drift_ppm", LONG),
                ("buffered", LONG),
                ("dropped_samples", LONG),
                ("boot_count", LONG),
                ("reset_reason", LONG),
                ("battery_chip", LONG),
            ],
        )?;
        e.symbols(status().map(|(d, _, _)| d))?;
        e.symbols(status().map(|(_, f, _)| f))?;
        e.f32s(status().map(|(_, _, s)| s.battery_v))?;
        e.f32s(status().map(|(_, _, s)| s.battery_pct))?;
        e.f32s(status().map(|(_, _, s)| s.charge_rate))?;
        e.i64s(status().map(|(_, _, s)| s.rssi_dbm as i64))?;
        e.i64s(status().map(|(_, _, s)| s.channel as i64))?;
        e.i64s(status().map(|(_, _, s)| s.rtc_drift_ppm as i64))?;
        e.i64s(status().map(|(_, _, s)| s.buffered as i64))?;
        e.i64s(status().map(|(_, _, s)| s.dropped_samples as i64))?;
        e.i64s(status().map(|(_, _, s)| s.boot_count as i64))?;
        e.i64s(status().map(|(_, _, s)| s.reset_reason as i64))?;
        e.i64s(status().map(|(_, _, s)| s.battery_chip as i64))?;
        e.timestamps(status().map(|(_, _, s)| s.t_us))?;
    }

    let moves = || {
        recs.iter().filter_map(|r| match r {
            Rec::Location { dev, room, t_us } => Some((*dev as u64, *room as u64, *t_us)),
            _ => None,
        })
    };
    let n = moves().count();
    if n > 0 {
        // nickname stays null (design doc)
        e.table(
            "device_locations",
            n,
            &[("device", SYMBOL), ("room", SYMBOL)],
        )?;
        e.symbols(moves().map(|(d, _, _)| d))?;
        e.symbols(moves().map(|(_, r, _)| r))?;
        e.timestamps(moves().map(|(_, _, t)| t))?;
    }
    Ok(())
}

/// Puts a node frame's readings, then its status, into the gateway's ring, stopping at the first
/// that doesn't fit. Returns how many readings went in: the only number the gateway may ack, since
/// the node deletes what it's told was stored.
pub fn store_frame(
    q: &mut qwp::Ring<Rec>,
    dev: u16,
    fw: u16,
    samples: &[Sample],
    status: Option<Status>,
) -> u8 {
    let free = q.capacity() - q.len();
    let n = samples.len().min(free);
    for s in &samples[..n] {
        q.push(Rec::Reading { dev, s: *s });
    }
    if let Some(s) = status
        && free > n
    {
        q.push(Rec::Status { dev, fw, s });
    }
    n as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    /// esp-hal turns the RTC's ticks since power-up into µs with a calibration it re-measures
    /// every boot; a 0.3% change after 3 h moved every wake's time by 30 s.
    #[test]
    fn steady_us_ignores_per_boot_calibration() {
        let cal_ref = 3_500_000; // µs per tick << 19, as saved at cold boot
        let ticks: u128 = 1_500_000_000; // ~3 h of a 136 kHz RC
        let us = |cal: u32| (ticks * cal as u128 >> 19) as u64;
        let base = steady_us(us(cal_ref), cal_ref, cal_ref);
        for cal in [cal_ref - 10_500, cal_ref + 10_500] {
            assert!((steady_us(us(cal), cal, cal_ref) - base).abs() <= 1);
        }
        assert_eq!(steady_us(1234, 0, cal_ref), 1234); // no calibration: µs as given
    }

    #[test]
    fn crc_matches_datasheet() {
        assert_eq!(crc8(&[0xBE, 0xEF]), 0x92); // Sensirion's worked example
    }

    fn frame(t: u16, rh: u16) -> [u8; 6] {
        let (t, rh) = (t.to_be_bytes(), rh.to_be_bytes());
        [t[0], t[1], crc8(&t), rh[0], rh[1], crc8(&rh)]
    }

    #[test]
    fn parses_a_room() {
        // 22.5 °C: raw = (22.5 + 45) / 175 * 65535; 45 %RH: raw = (45 + 6) / 125 * 65535
        let (t, rh, f) = sht41_parse(&frame(25278, 26738));
        assert_eq!(f, 0);
        assert!((t - 22.5).abs() < 0.01, "{t}");
        assert!((rh - 45.0).abs() < 0.01, "{rh}");
    }

    #[test]
    fn bad_crc_is_sensor_fail() {
        let mut b = frame(25278, 26738);
        b[5] ^= 1;
        let (t, rh, f) = sht41_parse(&b);
        assert_eq!(f, SENSOR_FAIL);
        assert!(t.is_nan() && rh.is_nan());
    }

    #[test]
    fn out_of_range_and_clamp() {
        let (t, _, f) = sht41_parse(&frame(0, 26738)); // -45 °C
        assert_eq!(f, OUT_OF_RANGE);
        assert!(t.is_nan());
        let (_, rh, f) = sht41_parse(&frame(25278, 65535)); // 119 %RH clamps
        assert_eq!((rh, f), (100.0, 0));
    }

    #[test]
    fn gauge_units() {
        let (v, pct, rate) = max17048(0xC800, 0x5080, (-5i16) as u16);
        assert!((v - 3.99999).abs() < 1e-3, "{v}"); // 51200 × 78.125 µV = 4.0 V
        assert_eq!(pct, 80.5);
        assert!((rate + 1.04).abs() < 1e-4, "{rate}");
    }

    #[test]
    fn sample_packs_in_12_bytes() {
        assert_eq!(core::mem::size_of::<Sample>(), 12);
        let s = Sample::new(5, 21.234, 40.567, 0);
        assert_eq!((s.temp_c(), s.rh_pct()), (21.23, 40.57));
        let s = Sample::new(5, f32::NAN, f32::NAN, SENSOR_FAIL);
        assert!(s.temp_c().is_nan() && s.rh_pct().is_nan());
        assert_eq!(Sample::new(5, -12.5, 0.0, 0).temp_c(), -12.5);
    }

    fn ring() -> Box<SampleRing> {
        let mut r = Box::new(SampleRing {
            head: 7,
            len: 9999, // garbage, as after a power-on
            buf: [Sample::default(); RING_LEN],
        });
        r.fix();
        r
    }

    #[test]
    fn ring_drops_oldest_and_wraps() {
        let mut r = ring();
        assert!(r.is_empty());
        for i in 0..RING_LEN as u32 {
            assert!(!r.push(Sample::new(i, 0.0, 0.0, 0)));
        }
        assert!(r.push(Sample::new(1000, 0.0, 0.0, 0)));
        assert_eq!(r.len(), RING_LEN);
        assert_eq!(r.get(0).ts_s, 1); // 0 was dropped
        assert_eq!(r.get(RING_LEN - 1).ts_s, 1000);
        r.pop(10);
        assert_eq!((r.len(), r.get(0).ts_s), (RING_LEN - 10, 11));
        r.pop(1000);
        assert!(r.is_empty());
        r.push(Sample::new(42, 0.0, 0.0, 0));
        assert_eq!(r.get(0).ts_s, 42);
    }

    const E0: i64 = 1_800_000_000_000_000;

    #[test]
    fn clock_measures_and_corrects_rate() {
        let mut c = Clock::default();
        assert_eq!(c.stamp(90_000_000), (90, TIME_UNSYNCED));
        c.sync(100_000_000, E0).unwrap();
        // the RTC runs 1% slow: 600 s of true time reads as 594 s
        let rtc = 100_000_000 + 594_000_000;
        let step = c.sync(rtc, E0 + 600_000_000).unwrap();
        assert_eq!(step, 6_000_000);
        assert!((c.ppm - 10101.01).abs() < 1.0, "{}", c.ppm);
        // the next 594 RTC seconds are now predicted as 600 true seconds
        let est = c.epoch_us(rtc + 594_000_000);
        assert!((est - (E0 + 1_200_000_000)).abs() < 1000, "{}", est - E0);
    }

    #[test]
    fn clock_learns_rate_from_frequent_syncs() {
        // a gateway reply every wake: 60 true seconds read as 59.4 RTC seconds
        let mut c = Clock::default();
        for i in 0..12 {
            c.sync(100_000_000 + i * 59_400_000, E0 + i * 60_000_000)
                .unwrap();
        }
        assert!((c.ppm - 10101.01).abs() < 1.0, "{}", c.ppm);
    }

    #[test]
    fn sweep_backoff_is_1_2_4_wakes_apart() {
        let skips: Vec<u32> = (1..=8).map(|f| sweep_skips(f, 60)).collect();
        assert_eq!(skips, [0, 1, 3, 7, 15, 31, 59, 59]); // an hour of wakes at most
        assert_eq!(sweep_skips(1, 3600), 0);
    }

    #[test]
    fn clock_rejects_wild_sntp() {
        let mut c = Clock::default();
        c.sync(0, E0).unwrap();
        let est = c.epoch_us(600_000_000);
        assert!(c.sync(600_000_000, E0 + 3 * DAY_US).is_err());
        assert_eq!(c.epoch_us(600_000_000), est);
        // the gateway insists: the third in a row wins, and the clock starts over from it
        assert!(c.sync(660_000_000, E0 + 3 * DAY_US).is_err());
        assert_eq!(
            c.sync(720_000_000, E0 + 3 * DAY_US),
            Ok(3 * DAY_US - 720_000_000)
        );
        assert_eq!(c.epoch_us(720_000_000), E0 + 3 * DAY_US);
        assert_eq!((c.ppm, c.refused), (0.0, 0));
    }

    #[test]
    fn clock_rate_clamped_across_a_time_step() {
        let mut c = Clock::default();
        c.sync(0, E0).unwrap();
        // the gateway's time jumped an hour over 10 minutes: not something an RC oscillator does
        c.sync(600_000_000, E0 + 600_000_000 + 3_600_000_000)
            .unwrap();
        assert_eq!(c.ppm, PPM_MAX);
    }

    #[test]
    fn retain_sendable_drops_rtc_time_samples() {
        let mut r = ring();
        r.push(Sample::new(5, 20.0, 40.0, TIME_UNSYNCED)); // RTC seconds
        r.push(Sample::new(1_700_000_000, 21.0, 40.0, 0));
        r.push(Sample::new(6, 20.0, 40.0, TIME_UNSYNCED));
        r.push(Sample::new(1_700_000_060, 22.0, 40.0, 0));
        r.retain_sendable();
        assert_eq!(r.len(), 2);
        assert_eq!(
            (r.get(0).ts_s, r.get(1).ts_s),
            (1_700_000_000, 1_700_000_060)
        );
    }

    #[test]
    fn unsynced_samples_freeze_once() {
        let mut r = ring();
        let mut c = Clock::default();
        let (ts, f) = c.stamp(10_000_000);
        r.push(Sample::new(ts, 20.0, 50.0, f));
        c.sync(70_000_000, E0).unwrap();
        c.freeze_unsynced(&mut r);
        let s = r.get(0);
        assert_eq!(s.ts_s as i64, (E0 - 60_000_000) / 1_000_000);
        assert_eq!(s.flags, TIME_UNSYNCED);
        assert!(Clock::sendable(&s));
        c.freeze_unsynced(&mut r); // a second sync never moves it again
        assert_eq!(r.get(0), s);
    }

    #[test]
    fn backoff_doubles_to_an_hour() {
        assert_eq!(wakes_to_next_send(10, 0, 60), 10);
        assert_eq!(wakes_to_next_send(10, 1, 60), 20);
        assert_eq!(wakes_to_next_send(10, 2, 60), 40);
        assert_eq!(wakes_to_next_send(10, 3, 60), 60);
        assert_eq!(wakes_to_next_send(10, 30, 60), 60);
    }

    #[test]
    fn encodes_both_tables() {
        let mut buf = [0u8; 512];
        let mut e =
            qwp::Encoder::with_symbols(&mut buf, 0, &["a1b2c3", "0.3.0", "d4e5f6"]).unwrap();
        let s = Sample::new(1_800_000_000, 21.0, 40.0, 0);
        let st = Status {
            t_us: 1_800_000_000_000_000,
            channel: 6,
            ..Default::default()
        };
        encode(
            &mut e,
            &[
                Rec::Status {
                    dev: 0,
                    fw: 1,
                    s: st,
                },
                Rec::Reading { dev: 0, s },
                Rec::Reading { dev: 2, s },
                Rec::Location {
                    dev: 2,
                    room: 0,
                    t_us: 1,
                },
            ],
        )
        .unwrap();
        let len = e.finish();
        assert_eq!(u16::from_le_bytes([buf[6], buf[7]]), 3);
        let has = |n: &[u8]| buf[..len].windows(n.len()).any(|w| w == n);
        assert!(has(b"room_readings") && has(b"device_status") && has(b"fw_version"));
        assert!(has(b"device_locations") && has(b"room") && !has(b"nickname"));
        assert!(has(b"channel") && !has(b"wifi_ms") && !has(b"cached_connect"));
    }

    #[test]
    fn device_id_hex() {
        assert_eq!(&device_id(&[0, 0, 0, 0x61, 0x03, 0x38]), b"610338");
    }
    fn synced_clock() -> Clock {
        let mut c = Clock::default();
        c.sync(0, 1_700_000_000_000_000).unwrap();
        c
    }

    fn reply(nonce: u16, stored: u8, epoch_us: i64) -> proto::Reply {
        proto::Reply {
            nonce,
            stored,
            epoch_us,
            wake_s: 60,
            flags: 0,
        }
    }

    #[test]
    fn reply_pops_what_was_stored() {
        let (mut c, mut r) = (synced_clock(), ring());
        for i in 0..5 {
            r.push(Sample::new(1_700_000_000 + i, 20.0, 40.0, 0));
        }
        let rep = reply(7, 3, 1_700_000_060_000_000);
        assert!(proto::apply_reply(&mut c, &mut r, 7, 5, &rep, 60_000_000).is_some());
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn reply_wrong_nonce_ignored() {
        let (mut c, mut r) = (synced_clock(), ring());
        r.push(Sample::new(1_700_000_000, 20.0, 40.0, 0));
        let before = c;
        let rep = reply(6, 1, 1_800_000_000_000_000);
        assert!(proto::apply_reply(&mut c, &mut r, 7, 1, &rep, 1).is_none());
        assert_eq!((r.len(), c), (1, before));
    }

    #[test]
    fn reply_with_wild_time_reports_it_and_still_acks() {
        let (mut c, mut r) = (synced_clock(), ring());
        r.push(Sample::new(1_700_000_000, 20.0, 40.0, 0));
        let rep = reply(1, 1, 1_700_000_000_000_000 + 3 * DAY_US);
        assert!(matches!(
            proto::apply_reply(&mut c, &mut r, 1, 1, &rep, 60_000_000),
            Some(Err(_))
        ));
        assert_eq!(r.len(), 0);
    }

    #[test]
    fn reply_stored_clamped() {
        let (mut c, mut r) = (synced_clock(), ring());
        for i in 0..4 {
            r.push(Sample::new(1_700_000_000 + i, 20.0, 40.0, 0));
        }
        proto::apply_reply(&mut c, &mut r, 1, 2, &reply(1, 200, 0), 1);
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn reply_without_time_still_acks() {
        let (mut c, mut r) = (Clock::default(), ring());
        r.push(Sample::new(1_700_000_000, 20.0, 40.0, 0));
        proto::apply_reply(&mut c, &mut r, 1, 1, &reply(1, 1, 0), 5);
        assert_eq!((r.len(), c.synced), (0, 0));
        // nor does a time no gateway could have: it would overflow epoch_us later
        let rep = reply(2, 0, i64::MAX);
        assert_eq!(
            proto::apply_reply(&mut c, &mut r, 2, 0, &rep, 5),
            Some(Ok(0))
        );
        assert_eq!(c.synced, 0);
    }

    #[test]
    fn sendable_prefix_stops_at_unsynced() {
        let mut r = ring();
        r.push(Sample::new(1_700_000_000, 20.0, 40.0, 0));
        r.push(Sample::new(42, 20.0, 40.0, TIME_UNSYNCED)); // taken before any sync, at RTC 42 s
        r.push(Sample::new(1_700_000_120, 20.0, 40.0, 0));
        assert_eq!(proto::sendable_prefix(&r, 16), 1);
        assert_eq!(proto::sendable_prefix(&r, 0), 0);
        let mut c = Clock::default();
        let rep = reply(1, 0, 1_700_000_200_000_000);
        proto::apply_reply(&mut c, &mut r, 1, 0, &rep, 100_000_000);
        assert_eq!(proto::sendable_prefix(&r, 16), 3); // frozen to real time by the first reply
        assert_eq!(r.get(1).ts_s, 1_700_000_142);
    }

    fn qring(cap: usize) -> qwp::Ring<Rec> {
        let buf = Box::leak(vec![core::mem::MaybeUninit::uninit(); cap].into_boxed_slice());
        qwp::Ring::new(buf)
    }

    #[test]
    fn store_frame_acks_only_what_fits() {
        let mut q = qring(3);
        let s = [Sample::new(1_700_000_000, 20.0, 40.0, 0); 5];
        let st = Status::default();
        assert_eq!(store_frame(&mut q, 1, 2, &s, Some(st)), 3); // no room left for the status
        assert_eq!(q.len(), 3);
        assert_eq!(store_frame(&mut q, 1, 2, &s, Some(st)), 0);
        q.pop(3);
        assert_eq!(store_frame(&mut q, 1, 2, &s[..1], Some(st)), 1);
        assert_eq!(q.len(), 2); // reading and status
        assert_eq!(
            q.get(1),
            Some(Rec::Status {
                dev: 1,
                fw: 2,
                s: st
            })
        );
    }
}
