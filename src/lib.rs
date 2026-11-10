//! Everything that doesn't touch hardware, tested on the host (see the README).
#![cfg_attr(not(test), no_std)]

pub use qwp_embedded as qwp;

/// Sample flags (SPEC.md, Buffering and batching).
pub const SENSOR_FAIL: u8 = 1 << 0;
pub const OUT_OF_RANGE: u8 = 1 << 1;

/// SHT41 address and its high-precision, heater-off measure command.
pub const SHT41: u8 = 0x44;
pub const SHT41_MEASURE: u8 = 0xFD;

/// Symbol dictionary ID of the device: the sender registers `[device]` on each connection.
const SYM_DEVICE: u64 = 0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Reading {
    pub t_us: i64,
    pub temp_c: f32,
    pub rh_pct: f32,
    pub flags: u8,
}

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
/// outside -20..60 °C or 0..100 %RH are out of range. Either way the values are NaN.
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

/// `room_readings` rows. Column types match the table SPEC.md creates up front.
pub fn encode(e: &mut qwp::Encoder<'_>, recs: &[Reading]) -> Result<(), qwp::Full> {
    use qwp::{FLOAT, LONG, SYMBOL};
    e.table(
        "room_readings",
        recs.len(),
        &[
            ("device", SYMBOL),
            ("temp_c", FLOAT),
            ("rh_pct", FLOAT),
            ("flags", LONG),
        ],
    )?;
    e.symbols(recs.iter().map(|_| SYM_DEVICE))?;
    e.f32s(recs.iter().map(|r| r.temp_c))?;
    e.f32s(recs.iter().map(|r| r.rh_pct))?;
    e.i64s(recs.iter().map(|r| r.flags as i64))?;
    e.timestamps(recs.iter().map(|r| r.t_us))
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn encodes_one_table() {
        let mut buf = [0u8; 256];
        let mut e = qwp::Encoder::with_symbols(&mut buf, 0, &["a1b2c3"]).unwrap();
        let r = Reading {
            t_us: 1_700_000_000_000_000,
            temp_c: 21.0,
            rh_pct: 40.0,
            flags: 0,
        };
        encode(
            &mut e,
            &[
                r,
                Reading {
                    t_us: r.t_us + 60_000_000,
                    ..r
                },
            ],
        )
        .unwrap();
        let len = e.finish();
        assert_eq!(&buf[..4], b"QWP1");
        assert_eq!(u16::from_le_bytes([buf[6], buf[7]]), 1); // one table
        assert!(buf[..len].windows(13).any(|w| w == b"room_readings"));
    }
}
