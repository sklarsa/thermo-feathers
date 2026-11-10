//! What nodes and the gateway say to each other over ESP-NOW. Every frame starts
//! `[MAGIC, VERSION, kind]`; fields follow little-endian, one by one.

use crate::{Clock, Sample, SampleRing};

pub const MAGIC: u8 = b'T';
pub const VERSION: u8 = 1;
/// ESP-NOW v1's payload limit.
pub const FRAME_MAX: usize = 250;
pub const MAX_SAMPLES: usize = 16;

const DISCOVER: u8 = 1;
const HELLO: u8 = 2;
const DATA: u8 = 3;
const REPLY: u8 = 4;

const HEADER: usize = 3;
const STATUS_LEN: usize = 48;
const SAMPLE_LEN: usize = 9;
const DATA_FIXED: usize = HEADER + 2 + STATUS_LEN + 1;
const REPLY_LEN: usize = HEADER + 2 + 1 + 8 + 2 + 1;

/// Everything a node reports about itself, once per frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct NodeStatus {
    /// The node's epoch time at wake, 0 if it has none yet.
    pub t_us: i64,
    pub battery_v: f32,
    pub battery_pct: f32,
    pub charge_rate: f32,
    pub rtc_drift_ppm: i32,
    /// Ring length when the frame was built.
    pub buffered: u16,
    pub dropped_samples: u32,
    pub boot_count: u32,
    pub reset_reason: u8,
    pub battery_chip: u8,
    /// Firmware version, NUL-padded.
    pub fw: [u8; 12],
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Reply {
    /// The `Data` frame's nonce, echoed.
    pub nonce: u16,
    /// How many of that frame's samples the gateway has stored.
    pub stored: u8,
    /// The gateway's epoch time; 0 until it has synced.
    pub epoch_us: i64,
    pub wake_s: u16,
    /// Bit 0: stay awake for a firmware update (milestone 5). Nothing sets it yet.
    pub flags: u8,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Frame {
    /// Node → broadcast: "where's the gateway?"
    Discover,
    /// Gateway → node. The gateway's MAC is the frame's source address.
    Hello,
    Data {
        nonce: u16,
        status: NodeStatus,
        n: u8,
        samples: [Sample; MAX_SAMPLES],
    },
    Reply(Reply),
}

struct W<'a> {
    b: &'a mut [u8; FRAME_MAX],
    n: usize,
}

impl W<'_> {
    fn put(&mut self, x: &[u8]) {
        self.b[self.n..self.n + x.len()].copy_from_slice(x);
        self.n += x.len();
    }
}

struct R<'a> {
    b: &'a [u8],
}

impl R<'_> {
    fn take<const N: usize>(&mut self) -> [u8; N] {
        let (h, t) = self.b.split_at(N);
        self.b = t;
        h.try_into().unwrap()
    }
    fn u8(&mut self) -> u8 {
        self.take::<1>()[0]
    }
    fn u16(&mut self) -> u16 {
        u16::from_le_bytes(self.take())
    }
    fn u32(&mut self) -> u32 {
        u32::from_le_bytes(self.take())
    }
    fn i64(&mut self) -> i64 {
        i64::from_le_bytes(self.take())
    }
    fn f32(&mut self) -> f32 {
        f32::from_le_bytes(self.take())
    }
}

/// Writes `f` into `out`; returns its length.
pub fn encode(f: &Frame, out: &mut [u8; FRAME_MAX]) -> usize {
    let mut w = W { b: out, n: 0 };
    let kind = match f {
        Frame::Discover => DISCOVER,
        Frame::Hello => HELLO,
        Frame::Data { .. } => DATA,
        Frame::Reply(_) => REPLY,
    };
    w.put(&[MAGIC, VERSION, kind]);
    match f {
        Frame::Discover | Frame::Hello => {}
        Frame::Data {
            nonce,
            status: s,
            n,
            samples,
        } => {
            w.put(&nonce.to_le_bytes());
            w.put(&s.t_us.to_le_bytes());
            w.put(&s.battery_v.to_le_bytes());
            w.put(&s.battery_pct.to_le_bytes());
            w.put(&s.charge_rate.to_le_bytes());
            w.put(&s.rtc_drift_ppm.to_le_bytes());
            w.put(&s.buffered.to_le_bytes());
            w.put(&s.dropped_samples.to_le_bytes());
            w.put(&s.boot_count.to_le_bytes());
            w.put(&[s.reset_reason, s.battery_chip]);
            w.put(&s.fw);
            let n = (*n as usize).min(MAX_SAMPLES);
            w.put(&[n as u8]);
            for x in &samples[..n] {
                w.put(&x.ts_s.to_le_bytes());
                w.put(&x.temp_cc.to_le_bytes());
                w.put(&x.rh_cp.to_le_bytes());
                w.put(&[x.flags]);
            }
        }
        Frame::Reply(r) => {
            w.put(&r.nonce.to_le_bytes());
            w.put(&[r.stored]);
            w.put(&r.epoch_us.to_le_bytes());
            w.put(&r.wake_s.to_le_bytes());
            w.put(&[r.flags]);
        }
    }
    w.n
}

/// `None` for anything that isn't exactly one of our frames: other devices share ESP-NOW.
pub fn decode(b: &[u8]) -> Option<Frame> {
    if b.len() < HEADER || b[0] != MAGIC || b[1] != VERSION {
        return None;
    }
    let mut r = R { b: &b[HEADER..] };
    match b[2] {
        DISCOVER if b.len() == HEADER => Some(Frame::Discover),
        HELLO if b.len() == HEADER => Some(Frame::Hello),
        DATA if b.len() >= DATA_FIXED => {
            let nonce = r.u16();
            let status = NodeStatus {
                t_us: r.i64(),
                battery_v: r.f32(),
                battery_pct: r.f32(),
                charge_rate: r.f32(),
                rtc_drift_ppm: r.u32() as i32,
                buffered: r.u16(),
                dropped_samples: r.u32(),
                boot_count: r.u32(),
                reset_reason: r.u8(),
                battery_chip: r.u8(),
                fw: r.take(),
            };
            let n = r.u8();
            if n as usize > MAX_SAMPLES || b.len() != DATA_FIXED + n as usize * SAMPLE_LEN {
                return None;
            }
            let mut samples = [Sample::default(); MAX_SAMPLES];
            for s in &mut samples[..n as usize] {
                s.ts_s = r.u32();
                s.temp_cc = r.u16() as i16;
                s.rh_cp = r.u16();
                s.flags = r.u8();
            }
            Some(Frame::Data {
                nonce,
                status,
                n,
                samples,
            })
        }
        REPLY if b.len() == REPLY_LEN => Some(Frame::Reply(Reply {
            nonce: r.u16(),
            stored: r.u8(),
            epoch_us: r.i64(),
            wake_s: r.u16(),
            flags: r.u8(),
        })),
        _ => None,
    }
}

/// How many samples from the front of the ring can go in the next frame: those with epoch time,
/// up to the first without, at most `max`. Frames carry a prefix so a reply's count pops exactly
/// what was sent.
pub fn sendable_prefix(ring: &SampleRing, max: usize) -> usize {
    (0..ring.len().min(max))
        .take_while(|&i| Clock::sendable(&ring.get(i)))
        .count()
}

/// Applies the gateway's reply to a `Data` frame that carried `sent` samples under `nonce`. A
/// reply to some other frame changes nothing and returns `false`. Time from a gateway that has none
/// yet (0) is ignored, but its ack still counts.
pub fn apply_reply(
    clock: &mut Clock,
    ring: &mut SampleRing,
    nonce: u16,
    sent: u8,
    r: &Reply,
    rtc_us: i64,
) -> bool {
    if r.nonce != nonce {
        return false;
    }
    if r.epoch_us > 0 && clock.sync(rtc_us, r.epoch_us).is_ok() {
        clock.freeze_unsynced(ring);
    }
    ring.pop(r.stored.min(sent) as usize);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_round_trips_full() {
        let mut samples = [Sample::default(); MAX_SAMPLES];
        for (i, s) in samples.iter_mut().enumerate() {
            *s = Sample::new(1_700_000_000 + i as u32, 21.5, 40.0, 0);
        }
        let status = NodeStatus {
            t_us: 1_700_000_000_000_000,
            battery_v: 3.9,
            rtc_drift_ppm: -12,
            boot_count: 7,
            fw: *b"0.3.0\0\0\0\0\0\0\0",
            ..Default::default()
        };
        let f = Frame::Data {
            nonce: 9,
            status,
            n: MAX_SAMPLES as u8,
            samples,
        };
        let mut b = [0; FRAME_MAX];
        let len = encode(&f, &mut b);
        assert!(len <= FRAME_MAX);
        assert_eq!(decode(&b[..len]), Some(f));
    }

    #[test]
    fn reply_and_hello_round_trip() {
        let mut b = [0; FRAME_MAX];
        for f in [
            Frame::Discover,
            Frame::Hello,
            Frame::Reply(Reply {
                nonce: 3,
                stored: 16,
                epoch_us: 1_700_000_000_000_000,
                wake_s: 60,
                flags: 0,
            }),
        ] {
            let len = encode(&f, &mut b);
            assert_eq!(decode(&b[..len]), Some(f));
        }
    }

    #[test]
    fn decode_rejects_garbage() {
        let mut b = [0; FRAME_MAX];
        let r = Reply {
            nonce: 1,
            stored: 1,
            epoch_us: 1,
            wake_s: 60,
            flags: 0,
        };
        let len = encode(&Frame::Reply(r), &mut b);
        assert_eq!(decode(&b[..len - 1]), None); // truncated
        b[1] = VERSION + 1;
        assert_eq!(decode(&b[..len]), None); // other version
        assert_eq!(decode(&[]), None);
        assert_eq!(decode(&[0xAA; 40]), None); // someone else's ESP-NOW
        let mut d = [0; FRAME_MAX];
        let empty = Frame::Data {
            nonce: 0,
            status: NodeStatus::default(),
            n: 0,
            samples: [Sample::default(); MAX_SAMPLES],
        };
        let n = encode(&empty, &mut d);
        d[n - 1] = MAX_SAMPLES as u8 + 1; // claims more samples than fit
        assert_eq!(decode(&d[..n]), None);
        d[n - 1] = 1; // claims a sample that isn't there
        assert_eq!(decode(&d[..n]), None);
    }
}
