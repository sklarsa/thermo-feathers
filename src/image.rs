//! ESP app images, as espflash writes them: a 24-byte header, segments, a checksum padded to 16
//! bytes, then the image's own SHA-256 of everything before it. The app description sits at the
//! start of the first segment.

/// The project name in a node image's app description; the gateway's is `thermo-gateway`.
pub const NODE_PROJECT: &str = "thermo-node";

const MAGIC: u8 = 0xE9;
const HEADER: u32 = 24;
const SEGMENTS_MAX: u8 = 16;
const DESC: usize = 32;
const DESC_MAGIC: u32 = 0xABCD_5432;
/// Bytes from the start of an image that hold its app description's version and project name.
pub const DESC_END: usize = DESC + 80;

/// Where the image's own SHA-256 starts. `read(offset, buf)` fills `buf` from the image and
/// returns false past its end; `max` is the most bytes the image can have.
pub fn digest_offset(mut read: impl FnMut(u32, &mut [u8]) -> bool, max: u32) -> Option<u32> {
    let mut h = [0u8; HEADER as usize];
    if !read(0, &mut h) || h[0] != MAGIC || h[23] != 1 || h[1] > SEGMENTS_MAX {
        return None;
    }
    let mut pos = HEADER;
    for _ in 0..h[1] {
        let mut s = [0u8; 8];
        if !read(pos, &mut s) {
            return None;
        }
        let len = u32::from_le_bytes([s[4], s[5], s[6], s[7]]);
        pos = pos.checked_add(8)?.checked_add(len)?;
        if pos > max {
            return None;
        }
    }
    let end = (pos & !15) + 16; // the checksum is the padding's last byte
    (end.checked_add(32)? <= max).then_some(end)
}

/// (version, project name) from the first DESC_END bytes of an image.
pub fn app_desc(b: &[u8]) -> Option<(&str, &str)> {
    let b = b.get(..DESC_END)?;
    if u32::from_le_bytes(b[DESC..DESC + 4].try_into().ok()?) != DESC_MAGIC {
        return None;
    }
    Some((
        field(&b[DESC + 16..DESC + 48])?,
        field(&b[DESC + 48..DESC + 80])?,
    ))
}

/// A NUL-padded string field.
fn field(f: &[u8]) -> Option<&str> {
    let n = f.iter().position(|&c| c == 0).unwrap_or(f.len());
    core::str::from_utf8(&f[..n]).ok()
}

/// What a node reports as its firmware: `<version>-<hex>`, the hex from the image's own SHA-256,
/// as much as fits in 12 bytes. Every build differs, even at the same version.
pub fn build_id(version: &str, digest: &[u8; 32]) -> [u8; 12] {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut id = [0u8; 12];
    let v = version.len().min(12);
    id[..v].copy_from_slice(&version.as_bytes()[..v]);
    if v < 11 {
        id[v] = b'-';
        for (i, slot) in id[v + 1..].iter_mut().enumerate() {
            let b = digest[i / 2];
            *slot = HEX[(if i % 2 == 0 { b >> 4 } else { b & 15 }) as usize];
        }
    }
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Header, two segments (the first with an app description), checksum padding, digest.
    fn image() -> Vec<u8> {
        let mut b = vec![0u8; 24];
        b[0] = MAGIC;
        b[1] = 2;
        b[23] = 1;
        let mut seg1 = vec![0u8; 120];
        seg1[..4].copy_from_slice(&DESC_MAGIC.to_le_bytes());
        seg1[16..21].copy_from_slice(b"0.4.0");
        seg1[48..59].copy_from_slice(NODE_PROJECT.as_bytes());
        for seg in [seg1, vec![0xAA; 37]] {
            b.extend_from_slice(&0x3C00_0000u32.to_le_bytes());
            b.extend_from_slice(&(seg.len() as u32).to_le_bytes());
            b.extend_from_slice(&seg);
        }
        b.resize((b.len() & !15) + 16, 0); // padding and checksum
        b.extend_from_slice(&[0x5A; 32]);
        b
    }

    fn reader(b: &[u8]) -> impl FnMut(u32, &mut [u8]) -> bool + '_ {
        |o, buf| match b.get(o as usize..o as usize + buf.len()) {
            Some(s) => {
                buf.copy_from_slice(s);
                true
            }
            None => false,
        }
    }

    #[test]
    fn finds_digest_and_description() {
        let b = image();
        let at = digest_offset(reader(&b), b.len() as u32).unwrap();
        assert_eq!(at as usize, b.len() - 32);
        assert_eq!(app_desc(&b), Some(("0.4.0", NODE_PROJECT)));
    }

    #[test]
    fn refuses_what_isnt_an_image() {
        let b = image();
        assert_eq!(digest_offset(reader(&b), b.len() as u32 - 1), None); // cut short
        let mut x = b.clone();
        x[23] = 0; // no digest appended
        assert_eq!(digest_offset(reader(&x), x.len() as u32), None);
        let mut x = b.clone();
        x[24 + 4..24 + 8].copy_from_slice(&u32::MAX.to_le_bytes()); // a segment past the end
        assert_eq!(digest_offset(reader(&x), x.len() as u32), None);
        assert_eq!(digest_offset(reader(&[0xFF; 64]), 64), None); // erased flash
        let mut x = b;
        x[DESC] ^= 1;
        assert_eq!(app_desc(&x), None);
    }

    #[test]
    fn build_ids() {
        let d = [0xAB; 32];
        assert_eq!(&build_id("0.3.0", &d), b"0.3.0-ababab");
        assert_eq!(&build_id("0.10.0", &d), b"0.10.0-ababa");
        assert_eq!(&build_id("10.20.300-rc1", &d), b"10.20.300-rc"); // no room for the hex
    }
}
