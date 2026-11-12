//! The `otadata` partition, which tells ESP-IDF's bootloader which app slot to boot: two 32-byte
//! entries, a sector apart, each `[seq: u32, label: 20 bytes, state: u32, crc: u32]`. The valid
//! entry with the higher `seq` wins, and `seq` picks slot `(seq - 1) % 2`.
//!
//! Ours instead of esp-bootloader-esp-idf's `Ota`: that reads entries through a misaligned pointer,
//! which is a LoadStoreAlignment exception on Xtensa (it panicked a node on every cold boot).

/// Where the second entry starts.
pub const ENTRY_1: u32 = 0x1000;

/// ESP-IDF's `esp_ota_img_states_t`.
pub const NEW: u32 = 0;
pub const PENDING_VERIFY: u32 = 1;
pub const VALID: u32 = 2;
pub const INVALID: u32 = 3;
pub const ABORTED: u32 = 4;

/// ROM `crc32_le(UINT32_MAX, ..)`, which the bootloader checks `seq` with.
fn crc(seq: u32) -> u32 {
    let mut c = 0u32;
    for b in seq.to_le_bytes() {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                c >> 1 ^ 0xEDB8_8320
            } else {
                c >> 1
            };
        }
    }
    !c
}

/// (seq, state) of an entry, or `None` if it's erased or broken.
pub fn parse(e: &[u8; 32]) -> Option<(u32, u32)> {
    let word = |i: usize| u32::from_le_bytes([e[i], e[i + 1], e[i + 2], e[i + 3]]);
    let (seq, state, c) = (word(0), word(24), word(28));
    (seq != u32::MAX && c == crc(seq)).then_some((seq, state))
}

pub fn entry(seq: u32, state: u32) -> [u8; 32] {
    let mut e = [0xFF; 32];
    e[..4].copy_from_slice(&seq.to_le_bytes());
    e[24..28].copy_from_slice(&state.to_le_bytes());
    e[28..].copy_from_slice(&crc(seq).to_le_bytes());
    e
}

/// The entry the bootloader goes by: (which of the two, seq, state). `None` when neither is
/// valid, as after a USB flash: the bootloader boots slot 0 then.
pub fn current(e0: &[u8; 32], e1: &[u8; 32]) -> Option<(usize, u32, u32)> {
    match (parse(e0), parse(e1)) {
        (Some(a), Some(b)) if b.0 > a.0 => Some((1, b.0, b.1)),
        (Some(a), _) => Some((0, a.0, a.1)),
        (None, Some(b)) => Some((1, b.0, b.1)),
        (None, None) => None,
    }
}

/// The app slot (0 or 1) a seq boots.
pub fn slot_of(seq: u32) -> usize {
    (seq.wrapping_sub(1) % 2) as usize
}

/// What to write, and to which entry, to boot app slot `to` next, on trial (`NEW`). It goes in
/// the entry the bootloader isn't using, so a power cut mid-write leaves the old choice standing.
pub fn switch_to(e0: &[u8; 32], e1: &[u8; 32], to: usize) -> (usize, [u8; 32]) {
    let (which, seq) = match current(e0, e1) {
        Some((w, seq, _)) => (1 - w, seq + if slot_of(seq + 1) == to { 1 } else { 2 }),
        None => (0, to as u32 + 1),
    };
    (which, entry(seq, NEW))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// esp-bootloader-esp-idf's test vectors.
    const COUNT_1_VALID: [u8; 32] = [
        1, 0, 0, 0, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
        255, 255, 255, 255, 2, 0, 0, 0, 154, 152, 67, 71,
    ];
    const COUNT_2_NEW: [u8; 32] = [
        2, 0, 0, 0, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
        255, 255, 255, 255, 0, 0, 0, 0, 116, 55, 246, 85,
    ];
    const ERASED: [u8; 32] = [0xFF; 32];

    #[test]
    fn matches_esp_idf() {
        assert_eq!(entry(1, VALID), COUNT_1_VALID);
        assert_eq!(entry(2, NEW), COUNT_2_NEW);
        assert_eq!(parse(&COUNT_2_NEW), Some((2, NEW)));
        assert_eq!(parse(&ERASED), None);
        let mut bad = COUNT_2_NEW;
        bad[0] = 3; // seq without its CRC
        assert_eq!(parse(&bad), None);
    }

    #[test]
    fn higher_seq_wins() {
        assert_eq!(current(&ERASED, &ERASED), None);
        assert_eq!(current(&COUNT_1_VALID, &ERASED), Some((0, 1, VALID)));
        assert_eq!(current(&COUNT_1_VALID, &COUNT_2_NEW), Some((1, 2, NEW)));
        assert_eq!(current(&ERASED, &COUNT_2_NEW), Some((1, 2, NEW)));
        assert_eq!((slot_of(1), slot_of(2), slot_of(3)), (0, 1, 0));
    }

    #[test]
    fn switching_slots() {
        // after a USB flash (slot 0 running): slot 1 is seq 2, in entry 0
        assert_eq!(switch_to(&ERASED, &ERASED, 1), (0, entry(2, NEW)));
        // running slot 1 (seq 2 in entry 0): back to slot 0 is seq 3, in entry 1
        let e0 = entry(2, VALID);
        assert_eq!(switch_to(&e0, &ERASED, 0), (1, entry(3, NEW)));
        // a rolled-back seq 2 (slot 1) while slot 0 runs: slot 1 again is seq 4
        let e0 = entry(2, ABORTED);
        assert_eq!(switch_to(&e0, &ERASED, 1), (1, entry(4, NEW)));
        let (w, e) = switch_to(&e0, &ERASED, 1);
        let (e0, e1) = if w == 0 { (e, e0) } else { (e0, e) };
        assert_eq!(current(&e0, &e1).map(|c| slot_of(c.1)), Some(1));
    }
}
