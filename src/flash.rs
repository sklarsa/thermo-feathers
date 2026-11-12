//! Flash helpers both firmwares share: app slots, otadata, and the running image's build ID. Each
//! binary includes this file (`#[path]`); the library stays free of hardware.

use esp_bootloader_esp_idf::partitions::{self, PARTITION_TABLE_MAX_LEN};
use esp_storage::FlashStorage;
use log::{error, info, warn};
use sha2::{Digest, Sha256};
use thermo_feathers::{image, otadata};

pub const SECTOR: u32 = 4096;

/// Partition types and subtypes, as in partitions.csv.
const APP: u8 = 0;
const OTA_0: u8 = 0x10;
const DATA: u8 = 1;
const OTADATA: u8 = 0;

/// An app slot: which one (0 or 1), and where it is.
#[derive(Clone, Copy)]
pub struct Slot {
    pub n: usize,
    pub offset: u32,
    pub len: u32,
}

/// (offset, len), by raw type and subtype: esp-bootloader-esp-idf 0.6's `find_partition` panics
/// (`unreachable!`) on the first custom-type partition it passes, and partitions.csv starts with
/// three (0x40).
fn find(flash: &mut FlashStorage, ty: u8, sub: u8) -> Option<(u32, u32)> {
    let mut buf = [0u8; PARTITION_TABLE_MAX_LEN];
    let pt = partitions::read_partition_table(flash, &mut buf).ok()?;
    pt.iter()
        .find(|p| p.raw_type() == ty && p.raw_subtype() == sub)
        .map(|p| (p.offset(), p.len()))
}

/// The slot we booted from. `booted_partition` goes by the MMU, not partition types.
pub fn booted(flash: &mut FlashStorage) -> Option<Slot> {
    let mut buf = [0u8; PARTITION_TABLE_MAX_LEN];
    let pt = partitions::read_partition_table(flash, &mut buf).ok()?;
    let p = pt.booted_partition().ok()??;
    let n = p.raw_subtype().checked_sub(OTA_0).filter(|&n| n < 2)? as usize;
    Some(Slot {
        n,
        offset: p.offset(),
        len: p.len(),
    })
}

/// The slot we didn't boot from: where an update goes.
pub fn spare(flash: &mut FlashStorage) -> Option<Slot> {
    let n = 1 - booted(flash)?.n;
    let (offset, len) = find(flash, APP, OTA_0 + n as u8)?;
    Some(Slot { n, offset, len })
}

fn reader<'a, 'd>(
    flash: &'a mut FlashStorage<'d>,
    base: u32,
) -> impl FnMut(u32, &mut [u8]) -> bool + use<'a, 'd> {
    move |o, b| flash.read(base + o, b).is_ok()
}

/// The running image's build ID: `version` and the image's own SHA-256.
pub fn build_id(flash: &mut FlashStorage, version: &str) -> [u8; 12] {
    let digest = (|| {
        let s = booted(flash)?;
        let at = image::digest_offset(reader(flash, s.offset), s.len)?;
        let mut d = [0u8; 32];
        flash.read(s.offset + at, &mut d).ok()?;
        Some(d)
    })();
    if digest.is_none() {
        warn!("can't read our own image's digest");
    }
    image::build_id(version, &digest.unwrap_or_default())
}

/// Checks the image in `slot` as it sits in flash: an app image of `project`, whose own SHA-256
/// holds. Returns its length.
pub fn verify(flash: &mut FlashStorage, slot: Slot, project: &str) -> Result<u32, &'static str> {
    let mut head = [0u8; image::DESC_END];
    flash
        .read(slot.offset, &mut head)
        .map_err(|_| "flash read")?;
    if image::app_desc(&head).map(|d| d.1) != Some(project) {
        return Err("not the right kind of image");
    }
    let at =
        image::digest_offset(reader(flash, slot.offset), slot.len).ok_or("not an app image")?;
    let mut sha = Sha256::new();
    let mut buf = [0u8; 1024];
    let mut o = 0;
    while o < at {
        let n = (at - o).min(buf.len() as u32) as usize;
        flash
            .read(slot.offset + o, &mut buf[..n])
            .map_err(|_| "flash read")?;
        sha.update(&buf[..n]);
        o += n as u32;
    }
    let mut want = [0u8; 32];
    flash
        .read(slot.offset + at, &mut want)
        .map_err(|_| "flash read")?;
    let got: [u8; 32] = sha.finalize().into();
    if got != want {
        return Err("the image in flash is damaged");
    }
    Ok(at + 32)
}

/// otadata's address and its two entries.
fn read_otadata(flash: &mut FlashStorage) -> Option<(u32, [u8; 32], [u8; 32])> {
    let (at, _) = find(flash, DATA, OTADATA)?;
    let (mut e0, mut e1) = ([0u8; 32], [0u8; 32]);
    flash.read(at, &mut e0).ok()?;
    flash.read(at + otadata::ENTRY_1, &mut e1).ok()?;
    Some((at, e0, e1))
}

/// Whether this boot is a new image's trial: the bootloader marked it pending, and abandons it
/// at the next boot unless it's confirmed first.
pub fn on_trial(flash: &mut FlashStorage) -> bool {
    read_otadata(flash)
        .and_then(|(_, e0, e1)| otadata::current(&e0, &e1))
        .is_some_and(|c| c.2 == otadata::PENDING_VERIFY)
}

/// Keeps this image: the bootloader won't roll it back.
pub fn confirm(flash: &mut FlashStorage) {
    let done = read_otadata(flash).and_then(|(at, e0, e1)| {
        let (which, seq, _) = otadata::current(&e0, &e1)?;
        let e = otadata::entry(seq, otadata::VALID);
        flash.write(at + which as u32 * otadata::ENTRY_1, &e).ok()
    });
    match done {
        Some(()) => info!("update confirmed: this image stays"),
        None => error!("couldn't mark this image valid; it rolls back at the next boot"),
    }
}

/// Points otadata at `slot`, on trial, for the next boot.
pub fn boot_next(flash: &mut FlashStorage, slot: Slot) -> Result<(), &'static str> {
    let (at, e0, e1) = read_otadata(flash).ok_or("no otadata")?;
    let (which, e) = otadata::switch_to(&e0, &e1, slot.n);
    flash
        .write(at + which as u32 * otadata::ENTRY_1, &e)
        .map_err(|_| "otadata write")
}

/// For a confirmed image that keeps crashing: boot the other slot again if otadata still names it
/// as a good image. Returns whether it did; the caller restarts.
pub fn fall_back(flash: &mut FlashStorage) -> bool {
    let Some(b) = booted(flash) else {
        return false;
    };
    let Some((at, e0, e1)) = read_otadata(flash) else {
        return false;
    };
    let Some((which, e)) = otadata::fall_back(&e0, &e1, b.n) else {
        warn!("crashing, and no earlier image to go back to");
        return false;
    };
    let ok = flash
        .write(at + which as u32 * otadata::ENTRY_1, &e)
        .is_ok();
    if ok {
        error!("crashing: going back to the image in ota_{}", 1 - b.n);
    }
    ok
}
