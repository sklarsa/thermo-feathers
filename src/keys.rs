//! Who may talk to whom. A master secret, kept in sops and built into the gateway, gives every
//! node its own key from its MAC; `scripts/flash-node.sh` writes a node's key into its `creds`
//! partition. Every ESP-NOW frame carries a tag made with the node's key, so the gateway knows a
//! frame comes from one of ours and the node knows a frame comes from our gateway. ESP-NOW's own
//! encryption, on top, uses keys derived from the same.

use crate::sha256::{hmac, sha256};

/// Bytes of tag at the end of every frame.
pub const TAG: usize = 16;

/// Which way a frame goes: part of its tag, so a frame can't be sent back the other way.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dir {
    ToGateway = 0,
    ToNode = 1,
}

/// A node's key: the master's HMAC of its MAC.
pub fn node_key(master: &[u8; 32], mac: &[u8; 6]) -> [u8; 16] {
    cut(hmac(master, &[b"thermo-node", mac]))
}

/// ESP-NOW's primary master key, the same on every board.
pub fn pmk(master: &[u8; 32]) -> [u8; 16] {
    cut(hmac(master, &[b"thermo-pmk"]))
}

/// ESP-NOW's local master key for the link between a node and the gateway.
pub fn lmk(node_key: &[u8; 16]) -> [u8; 16] {
    cut(hmac(node_key, &[b"thermo-lmk"]))
}

fn cut(h: [u8; 32]) -> [u8; 16] {
    let mut k = [0; 16];
    k.copy_from_slice(&h[..16]);
    k
}

fn tag(key: &[u8; 16], dir: Dir, frame: &[u8]) -> [u8; TAG] {
    cut(hmac(key, &[&[dir as u8], frame]))
}

/// Appends the tag to the `len` bytes of frame in `buf`; returns the new length. `buf` needs
/// TAG bytes of room after the frame.
pub fn seal(key: &[u8; 16], dir: Dir, buf: &mut [u8], len: usize) -> usize {
    let t = tag(key, dir, &buf[..len]);
    buf[len..len + TAG].copy_from_slice(&t);
    len + TAG
}

/// The frame without its tag, if the tag is right.
pub fn open<'a>(key: &[u8; 16], dir: Dir, b: &'a [u8]) -> Option<&'a [u8]> {
    let n = b.len().checked_sub(TAG)?;
    let want = tag(key, dir, &b[..n]);
    // every byte, so the time taken doesn't say how much of a forged tag was right
    let diff = want.iter().zip(&b[n..]).fold(0, |d, (a, b)| d | (a ^ b));
    (diff == 0).then_some(&b[..n])
}

/// The `creds` partition: `TFK1`, the node key, the PMK, then 4 bytes of their SHA-256.
pub const CREDS_LEN: usize = 4 + 16 + 16 + 4;
const CREDS_MAGIC: &[u8; 4] = b"TFK1";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Creds {
    pub node_key: [u8; 16],
    pub pmk: [u8; 16],
}

impl Creds {
    pub fn for_node(master: &[u8; 32], mac: &[u8; 6]) -> Self {
        Self {
            node_key: node_key(master, mac),
            pmk: pmk(master),
        }
    }

    pub fn to_bytes(&self) -> [u8; CREDS_LEN] {
        let mut b = [0; CREDS_LEN];
        b[..4].copy_from_slice(CREDS_MAGIC);
        b[4..20].copy_from_slice(&self.node_key);
        b[20..36].copy_from_slice(&self.pmk);
        let check = sha256(&b[..36]);
        b[36..].copy_from_slice(&check[..4]);
        b
    }

    /// `None` for erased flash, or anything else that isn't a whole, intact record.
    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        let b = b.get(..CREDS_LEN)?;
        if &b[..4] != CREDS_MAGIC || sha256(&b[..36])[..4] != b[36..] {
            return None;
        }
        let mut c = Self {
            node_key: [0; 16],
            pmk: [0; 16],
        };
        c.node_key.copy_from_slice(&b[4..20]);
        c.pmk.copy_from_slice(&b[20..36]);
        Some(c)
    }
}

/// The master secret from its 64 hex digits (as built into the gateway).
pub fn parse_master(hex: &str) -> Option<[u8; 32]> {
    let h = hex.trim().as_bytes();
    if h.len() != 64 {
        return None;
    }
    let mut m = [0; 32];
    for (i, b) in m.iter_mut().enumerate() {
        let d = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
        *b = d(h[2 * i])? << 4 | d(h[2 * i + 1])?;
    }
    Some(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MASTER: [u8; 32] = [7; 32];
    const MAC: [u8; 6] = [0xf4, 0x12, 0xfa, 0x61, 0x03, 0x38];

    #[test]
    fn keys_differ_per_node_and_use() {
        let k = node_key(&MASTER, &MAC);
        let mut other = MAC;
        other[5] ^= 1;
        assert_ne!(k, node_key(&MASTER, &other));
        assert_ne!(k, node_key(&[8; 32], &MAC));
        assert_ne!(k, lmk(&k));
        assert_ne!(pmk(&MASTER), k);
    }

    /// The same derivation as scripts/flash-node.sh, which uses Python's hmac module:
    /// python3 -c 'import hmac;print(hmac.new(bytes([7]*32),b"thermo-node"+bytes.fromhex("f412fa610338"),"sha256").hexdigest()[:32])'
    #[test]
    fn matches_flash_node_script() {
        let want = "fab9074b655234dac700f4b716272837";
        let got: String = node_key(&MASTER, &MAC)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(got, want);
        // and the whole creds record it writes
        let want =
            "54464b31fab9074b655234dac700f4b716272837221a4fa8965ce30efb9490e34130bc3ed9e59e92";
        let got: String = Creds::for_node(&MASTER, &MAC)
            .to_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(got, want);
    }

    #[test]
    fn tags() {
        let k = node_key(&MASTER, &MAC);
        let mut buf = [0u8; 64];
        buf[..5].copy_from_slice(b"hello");
        let n = seal(&k, Dir::ToGateway, &mut buf, 5);
        assert_eq!(open(&k, Dir::ToGateway, &buf[..n]), Some(&b"hello"[..]));
        // the other direction, another key, a flipped bit, a cut-off tag: all refused
        assert_eq!(open(&k, Dir::ToNode, &buf[..n]), None);
        assert_eq!(open(&lmk(&k), Dir::ToGateway, &buf[..n]), None);
        let mut bad = buf;
        bad[2] ^= 1;
        assert_eq!(open(&k, Dir::ToGateway, &bad[..n]), None);
        assert_eq!(open(&k, Dir::ToGateway, &buf[..n - 1]), None);
        assert_eq!(open(&k, Dir::ToGateway, &buf[..3]), None);
    }

    #[test]
    fn creds_round_trip() {
        let c = Creds::for_node(&MASTER, &MAC);
        let b = c.to_bytes();
        assert_eq!(Creds::from_bytes(&b), Some(c));
        assert_eq!(Creds::from_bytes(&[0xFF; CREDS_LEN]), None); // erased
        let mut bad = b;
        bad[10] ^= 1;
        assert_eq!(Creds::from_bytes(&bad), None);
    }

    #[test]
    fn master_hex() {
        let h = "07".repeat(32);
        assert_eq!(parse_master(&h), Some(MASTER));
        assert_eq!(parse_master("07"), None);
        assert_eq!(parse_master(&"zz".repeat(32)), None);
    }
}
