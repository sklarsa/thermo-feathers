#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]

//! Feather ESP32-S3 room node. Each wake reads the SHT41 into a ring in RTC memory, sends what the
//! gateway hasn't stored yet over ESP-NOW, takes the time and wake interval from its reply, and
//! deep-sleeps. A reply can also ask it to fetch new firmware from the gateway (milestone 5). See
//! docs/superpowers/specs/2026-11-10-espnow-gateway-design.md.

use embassy_time::{Duration, Instant, Timer, with_timeout};
use esp_backtrace as _;
use esp_hal::{
    Blocking,
    clock::CpuClock,
    gpio::{Level, Output, OutputConfig},
    i2c::master::{Config as I2cConfig, I2c},
    rtc_cntl::{
        Rtc, RwdtStage, RwdtStageAction, SocResetReason,
        sleep::{LowPower, RtcSleepConfig},
    },
    time::Rate,
    timer::timg::TimerGroup,
};
use esp_radio::esp_now::{BROADCAST_ADDRESS, EspNow, EspNowWifiInterface, PeerInfo};
use esp_radio::wifi::{ControllerConfig, WifiController};
use esp_storage::FlashStorage;
use log::{error, info, warn};
use thermo_feathers::image;
use thermo_feathers::keys::{self, Creds, Dir};
use thermo_feathers::proto::{self, CHUNK, FRAME_MAX, Frame, MAX_SAMPLES, NodeStatus, UPDATE};
use thermo_feathers::sha256::Sha256;
use thermo_feathers::*;

#[path = "../flash.rs"]
mod flash;

// The project name is how the gateway and the node tell a node image from a gateway one.
esp_bootloader_esp_idf::esp_app_desc!(
    env!("CARGO_PKG_VERSION"),
    image::NODE_PROJECT,
    esp_bootloader_esp_idf::BUILD_TIME,
    esp_bootloader_esp_idf::BUILD_DATE,
    esp_bootloader_esp_idf::ESP_IDF_COMPATIBLE_VERSION,
    esp_bootloader_esp_idf::MMU_PAGE_SIZE,
    0,
    u16::MAX,
    esp_bootloader_esp_idf::SECURE_VERSION
);

const FW: &str = env!("CARGO_PKG_VERSION");

const WAKE_S: u16 = 60; // until the gateway says otherwise
const CRITICAL_WAKE_S: u32 = 600;
const LOW_PCT: f32 = 10.0; // the interval doubles below this
const UPDATE_MIN_PCT: f32 = 40.0; // no firmware update below this unless charging
const UPDATE_CAP: Duration = Duration::from_secs(180);
const CHUNK_WAIT: Duration = Duration::from_millis(100);
/// Wakes to ignore update offers after an update fails, so a broken one can't drain the battery.
const UPDATE_BACKOFF_WAKES: u16 = 60;
const CRITICAL_V: f32 = 3.5; // no radio below this: its current spikes can brown the board out
const FLASH_WINDOW: Duration = Duration::from_secs(30);
const REPLY_WAIT: Duration = Duration::from_millis(50);
const HELLO_WAIT: Duration = Duration::from_millis(30);
const RADIO_CAP: Duration = Duration::from_secs(2);
/// Longest a wake may last before the RTC watchdog resets the node: past the flash window plus a
/// whole update. A hang then costs minutes of battery, not all of it, and counts as a crash.
const WATCHDOG: Duration = Duration::from_secs(300);

/// Bump the low half when State's layout changes. A new MAGIC drops the buffered samples, at the
/// update and again if it rolls back, and the image that rolls back sits out a flash window.
const MAGIC: u32 = 0x7EF0_0007;
/// Crashes in a row, without a reply from the gateway, before a confirmed image gives way to the
/// one before it.
const PANICS_TO_FALL_BACK: u8 = 3;

/// Everything that survives deep sleep. Persistable: any bit pattern is valid.
#[repr(C)]
struct State {
    magic: u32,
    boot_count: u32,
    dropped_samples: u32,
    reset_reason: u32,
    /// esp-hal's slow-clock calibration at cold boot: the fixed scale for RTC time (steady_us).
    cal_ref: u32,
    next_wake_rtc_us: i64,
    clock: Clock,
    battery_v: f32,
    battery_pct: f32,
    charge_rate: f32,
    battery_chip: u8,
    /// Where the gateway was last heard; channel 0 means not found yet.
    gw_mac: [u8; 6],
    gw_channel: u8,
    nonce: u16,
    /// From the gateway's last reply; WAKE_S until then.
    wake_s: u16,
    sweep_failures: u32,
    wakes_to_sweep: u32,
    /// This image's build ID (image::build_id), worked out every boot.
    fw: [u8; 12],
    /// Wakes left to ignore update offers.
    update_skip: u16,
    /// Panics since the gateway last answered.
    panics: u8,
    ring: SampleRing,
}
// SAFETY: integers, floats and arrays of them; SampleRing::fix repairs bad indexes.
unsafe impl esp_hal::Persistable for State {}

#[esp_hal::ram(unstable(rtc_fast, persistent))]
static mut STATE: State = unsafe { core::mem::zeroed() };

/// Recorded as `reset_reason` after a panic. esp-hal's SocResetReason values stay below this.
const PANIC_REASON: u32 = 255;

/// Set by `custom_halt`, read and cleared at the next boot. A static of its own, not part of
/// State: the panic can come while a `&mut State` is live up the stack, and a second one would
/// alias it.
#[esp_hal::ram(unstable(rtc_fast, persistent))]
static mut PANICKED: u32 = 0;
const PANICKED_MAGIC: u32 = 0x7EF0_DEAD;

/// A panic sleeps the node for a wake interval instead of hanging it awake: the wake after is an
/// ordinary deep-sleep wake, so the RTC ring survives, and a panic on every boot costs one wake a
/// minute rather than a reboot loop. device_status shows it as reset_reason 255.
#[unsafe(no_mangle)]
fn custom_halt() -> ! {
    // SAFETY: a plain word written by value, never borrowed; we're dying, so nothing else will
    // touch it or LPWR again
    unsafe {
        *core::ptr::addr_of_mut!(PANICKED) = PANICKED_MAGIC;
        Rtc::new(esp_hal::peripherals::RTC_TIMER::steal())
            .rwdt
            .disable();
        let mut lp = LowPower::new(esp_hal::peripherals::LPWR::steal());
        lp.set_wakeup_deadline(
            esp_hal::time::Instant::now() + esp_hal::time::Duration::from_secs(WAKE_S as u64),
        );
        lp.sleep_deep(RtcSleepConfig::deep())
    }
}

#[esp_rtos::main]
async fn main(_spawner: embassy_executor::Spawner) -> ! {
    esp_println::logger::init_logger_from_env();
    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::_80MHz));
    let mut flash = FlashStorage::new(peripherals.FLASH);
    let reset = esp_hal::system::reset_reason();
    let mut rtc = Rtc::new(peripherals.RTC_TIMER);
    // A core reset leaves RTC memory, so State and the ring come through it. Off for deep sleep,
    // which it would otherwise end.
    rtc.rwdt
        .set_stage_action(RwdtStage::Stage0, RwdtStageAction::ResetCore);
    rtc.rwdt.set_timeout(
        RwdtStage::Stage0,
        esp_hal::time::Duration::from_millis(WATCHDOG.as_millis()),
    );
    rtc.rwdt.enable();
    // esp-hal keeps this boot's slow-clock calibration in STORE1
    let cal_now = esp_hal::peripherals::LPWR::regs().store1().read().bits();

    // SAFETY: the only reference, and nothing else runs yet
    let st = unsafe { &mut *core::ptr::addr_of_mut!(STATE) };
    let cold = reset != Some(SocResetReason::CoreDeepSleep) || st.magic != MAGIC;
    let cal_ref = if cold { cal_now } else { st.cal_ref };
    let rtc_us = || {
        steady_us(
            rtc.time_since_power_up().as_micros() as u64,
            cal_now,
            cal_ref,
        )
    };
    let woke_us = rtc_us();
    if cold {
        // A reset that didn't lose power (watchdog, software, USB) leaves RTC memory as it was:
        // keep the samples that have epoch time. Power-on can also mean a brownout, so not then.
        use SocResetReason::*;
        let keep = st.magic == MAGIC
            && reset.is_some_and(|r| {
                !matches!(r, ChipPowerOn | SysBrownOut | CorePwrGlitch | SysClkGlitch)
            });
        // SAFETY: SampleRing is plain data; this copy is moved back below or dropped
        let ring = keep.then(|| unsafe { core::ptr::read(&st.ring) });
        // crashes in a row survive too, so a hang the watchdog ends still counts toward fall_back
        let panics = if keep { st.panics } else { 0 };
        // ponytail: magic + wake cause, no CRC; a garbled header after a brownout is caught by
        // the reset reason, and SampleRing::fix guards the indexes either way
        // SAFETY: State is valid zeroed
        *st = unsafe { core::mem::zeroed() };
        st.panics = panics;
        if let Some(r) = ring {
            st.ring = r;
            st.ring.fix();
            st.ring.retain_sendable();
        }
        st.magic = MAGIC;
        st.cal_ref = cal_ref;
        st.next_wake_rtc_us = woke_us;
        st.battery_v = f32::NAN;
        st.battery_pct = f32::NAN;
        st.charge_rate = f32::NAN;
        st.wake_s = WAKE_S;
        info!("cold boot: {reset:?}, kept {} samples", st.ring.len());
    }
    st.ring.fix();
    // SAFETY: a plain word, read and written by value; custom_halt is the only other user
    let panicked = unsafe { core::ptr::replace(core::ptr::addr_of_mut!(PANICKED), 0) };
    if panicked == PANICKED_MAGIC {
        st.reset_reason = PANIC_REASON;
        st.panics = st.panics.saturating_add(1);
    }
    if matches!(
        reset,
        Some(SocResetReason::CoreRtcWdt | SocResetReason::CpuRtcWdt | SocResetReason::SysRtcWdt)
    ) {
        warn!("the watchdog ended the last wake");
        st.panics = st.panics.saturating_add(1);
    }
    // A confirmed image that keeps crashing, before it can reach the gateway: the bootloader only
    // rolls back unconfirmed ones, so go back by hand.
    if st.panics >= PANICS_TO_FALL_BACK {
        st.panics = 0;
        if flash::fall_back(&mut flash) {
            esp_hal::system::software_reset();
        }
    }
    // MAGIC is RTC memory's only guard: keep what drives sleep and the radio in range
    st.wake_s = st.wake_s.clamp(WAKE_MIN_S, WAKE_MAX_S);
    st.wakes_to_sweep = st
        .wakes_to_sweep
        .min(sweep_skips(u32::MAX, st.wake_s as u32));
    if !(1..=13).contains(&st.gw_channel) {
        st.gw_channel = 0;
    }
    st.boot_count += 1;
    if reset != Some(SocResetReason::CoreDeepSleep) {
        st.reset_reason = reset.map_or(0, |r| r as u32);
    }

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 73744);
    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);
    if st.boot_count == 1 {
        // Once per power-on or RESET: USB is off in deep sleep, so this is the window to flash
        // (tap RESET, then run espflash) or to attach a serial monitor.
        info!("awake {} s for flashing", FLASH_WINDOW.as_secs());
        Timer::after(FLASH_WINDOW).await;
    }
    // After the flash window, so a crash in here can still be flashed over and watched. Every
    // boot: after a rollback, the old image wakes up with the new one's State.
    st.fw = flash::build_id(&mut flash, FW);
    // A new image is on trial until its first reply from the gateway, in this boot: the
    // bootloader abandons it at the next wake otherwise. Only a cold boot can be the first.
    let on_trial = cold && flash::on_trial(&mut flash);
    let creds = flash::creds(&mut flash);
    info!(
        "fw {}{}",
        core::str::from_utf8(&st.fw).unwrap_or("?"),
        if on_trial { ", on trial" } else { "" }
    );

    // Sensors: STEMMA QT power on for the read only.
    let mut stemma = Output::new(peripherals.GPIO7, Level::High, OutputConfig::default());
    Timer::after_millis(10).await;
    let mut i2c = I2c::new(
        peripherals.I2C0,
        I2cConfig::default().with_frequency(Rate::from_khz(100)),
    )
    .unwrap()
    .with_sda(peripherals.GPIO3)
    .with_scl(peripherals.GPIO4);
    let (temp_c, rh_pct, mut flags) = read_sht41(&mut i2c).await;
    read_gauge(&mut i2c, st);
    stemma.set_low();
    drop(i2c);

    let charging = st.charge_rate > 0.0;
    let critical = st.battery_v < CRITICAL_V && !charging;
    if charging {
        flags |= CHARGING;
    }
    let (ts_s, unsynced) = st.clock.stamp(rtc_us());
    if st
        .ring
        .push(Sample::new(ts_s, temp_c, rh_pct, flags | unsynced))
    {
        st.dropped_samples += 1;
    }
    info!(
        "wake {}: {temp_c:.2} °C {rh_pct:.1} %RH flags {flags} | ring {} | {:.3} V {:.1} % {:.1} %/h",
        st.boot_count,
        st.ring.len(),
        st.battery_v,
        st.battery_pct,
        st.charge_rate
    );

    if critical {
        // ponytail: the spec's 50 ms red NeoPixel blink isn't done; needs a WS2812 driver on GPIO33
        warn!("battery critical: no radio until charging");
    } else {
        let controller =
            WifiController::new(peripherals.WIFI, ControllerConfig::default()).unwrap();
        let mut esp = controller.esp_now();
        let flags = match creds {
            Some(key) => {
                let _ = esp.set_pmk(&key.pmk);
                radio(&mut esp, st, &key, &rtc_us, woke_us).await
            }
            None => {
                warn!("no key in creds: flash this node with scripts/flash-node.sh");
                None
            }
        };
        if flags.is_some() {
            st.panics = 0;
        }
        if on_trial && flags.is_some() {
            flash::confirm(&mut flash);
        }
        st.update_skip = st.update_skip.saturating_sub(1);
        if flags.is_some_and(|f| f & UPDATE != 0) {
            let charging = st.charge_rate > 0.0;
            if st.update_skip > 0 {
                info!(
                    "update offered; {} wakes until we try again",
                    st.update_skip
                );
            } else if !charging && (st.battery_pct < UPDATE_MIN_PCT || st.battery_chip != 1) {
                // only the MAX17048 is read: without it there's no charge level to go by (and
                // no gauge at all means never over the air; USB still works)
                info!("update offered; waiting for charge or {UPDATE_MIN_PCT} %");
            } else {
                // before, so a crash mid-update backs off too; a good update restarts anyway
                st.update_skip = UPDATE_BACKOFF_WAKES;
                let e = match creds {
                    Some(key) => update(&mut esp, st, &key, &mut flash).await,
                    None => "no key",
                };
                error!("update failed: {e}");
            }
        }
    }

    // Sleep to a fixed cadence so wake length doesn't drift the samples.
    let base = st.wake_s as u32;
    let wake_s = if critical {
        CRITICAL_WAKE_S
    } else if st.battery_pct < LOW_PCT {
        base * 2
    } else {
        base
    };
    let now = rtc_us();
    st.next_wake_rtc_us += wake_s as i64 * 1_000_000;
    if st.next_wake_rtc_us < now + 100_000 {
        st.next_wake_rtc_us = now + wake_s as i64 * 1_000_000;
    }
    let sleep_us = (st.next_wake_rtc_us - now) as u64;
    info!(
        "awake {} ms, sleeping {} ms",
        (now - woke_us) / 1000,
        sleep_us / 1000
    );
    rtc.rwdt.disable();
    let mut lp = LowPower::new(peripherals.LPWR);
    lp.set_wakeup_deadline(
        esp_hal::time::Instant::now() + esp_hal::time::Duration::from_micros(sleep_us),
    );
    lp.sleep_deep(RtcSleepConfig::deep())
}

/// Talk to the gateway where it was last heard; if it doesn't answer, look for it on every
/// channel, backing off 1, 2, 4… wakes (up to an hour) after sweeps that find nothing.
/// The gateway's reply flags, or `None` if it never answered.
async fn radio(
    esp: &mut EspNow,
    st: &mut State,
    key: &Creds,
    rtc_us: &impl Fn() -> i64,
    woke_us: i64,
) -> Option<u8> {
    if st.gw_channel != 0
        && let Some(f) = exchange(esp, st, key, rtc_us, woke_us).await
    {
        st.sweep_failures = 0;
        st.wakes_to_sweep = 0;
        return Some(f);
    }
    if st.wakes_to_sweep > 0 {
        st.wakes_to_sweep -= 1;
        warn!("no gateway; next sweep in {} wakes", st.wakes_to_sweep + 1);
        return None;
    }
    if sweep(esp, st, key).await
        && let Some(f) = exchange(esp, st, key, rtc_us, woke_us).await
    {
        st.sweep_failures = 0;
        st.wakes_to_sweep = 0;
        return Some(f);
    }
    st.sweep_failures += 1;
    st.wakes_to_sweep = sweep_skips(st.sweep_failures, st.wake_s as u32);
    warn!(
        "no gateway on any channel; next sweep in {} wakes",
        st.wakes_to_sweep + 1
    );
    None
}

/// Sends frames of samples until the ring has nothing sendable left, the gateway stops answering,
/// or RADIO_CAP runs out. Returns the replies' flags, or `None` if the gateway never answered.
/// The first frame may carry no samples: before the first reply nothing has a real timestamp.
async fn exchange(
    esp: &mut EspNow,
    st: &mut State,
    key: &Creds,
    rtc_us: &impl Fn() -> i64,
    woke_us: i64,
) -> Option<u8> {
    if esp.set_channel(st.gw_channel).is_err() {
        return None;
    }
    while esp.receive().is_some() {}
    // encrypted from here on: the gateway made us a peer the same way when it said Hello
    if !esp.peer_exists(&st.gw_mac) {
        let _ = esp.add_peer(PeerInfo {
            interface: EspNowWifiInterface::Station,
            peer_address: st.gw_mac,
            lmk: Some(keys::lmk(&key.node_key)),
            channel: None,
            encrypt: true,
        });
    }
    let t0 = Instant::now();
    let mut answered = None;
    // One status time per wake, fixed once the clock is set: every frame and retry of this wake is
    // the same device_status row, which QuestDB's (ts, device) dedup collapses to the last. Not
    // recomputed after a reply re-syncs the clock, which would move it.
    let mut wake_t_us = (st.clock.synced != 0).then(|| st.clock.epoch_us(woke_us));
    let mut buf = [0u8; FRAME_MAX];
    loop {
        let n = proto::sendable_prefix(&st.ring, MAX_SAMPLES);
        let mut samples = [Sample::default(); MAX_SAMPLES];
        for (i, s) in samples[..n].iter_mut().enumerate() {
            *s = st.ring.get(i);
        }
        st.nonce = st.nonce.wrapping_add(1);
        let status = NodeStatus {
            t_us: wake_t_us.unwrap_or(0),
            battery_v: st.battery_v,
            battery_pct: st.battery_pct,
            charge_rate: st.charge_rate,
            rtc_drift_ppm: st.clock.ppm as i32,
            buffered: st.ring.len() as u16,
            dropped_samples: st.dropped_samples,
            boot_count: st.boot_count,
            reset_reason: st.reset_reason as u8,
            battery_chip: st.battery_chip,
            fw: st.fw,
        };
        let len = seal(
            key,
            &Frame::Data {
                nonce: st.nonce,
                status,
                n: n as u8,
                samples,
            },
            &mut buf,
        );

        let mut reply = None;
        for _ in 0..2 {
            if esp.send_async(&st.gw_mac, &buf[..len]).await.is_err() {
                continue; // no MAC-layer ack
            }
            reply = with_timeout(REPLY_WAIT, async {
                loop {
                    let r = esp.receive_async().await;
                    if r.info.src_address != st.gw_mac {
                        continue;
                    }
                    if let Some(Frame::Reply(rep)) = open(key, r.data())
                        && rep.nonce == st.nonce
                    {
                        return rep;
                    }
                }
            })
            .await
            .ok();
            if reply.is_some() {
                break;
            }
        }
        let Some(rep) = reply else {
            warn!("gateway: no reply on channel {}", st.gw_channel);
            return answered;
        };
        answered = Some(answered.unwrap_or(0) | rep.flags);
        if let Some(Err(step)) = proto::apply_reply(
            &mut st.clock,
            &mut st.ring,
            st.nonce,
            n as u8,
            &rep,
            rtc_us(),
        ) {
            warn!(
                "gateway time {} s from ours, refused; RESET resets the clock",
                step / 1_000_000
            );
        }
        if wake_t_us.is_none() && st.clock.synced != 0 {
            wake_t_us = Some(st.clock.epoch_us(woke_us));
        }
        if rep.wake_s != 0 {
            st.wake_s = rep.wake_s.clamp(WAKE_MIN_S, WAKE_MAX_S);
        }
        info!(
            "gateway stored {}/{n}, ring {}",
            rep.stored.min(n as u8),
            st.ring.len()
        );
        // stored < n: the gateway is full; what's left waits for the next wake
        if rep.stored < n as u8
            || proto::sendable_prefix(&st.ring, 1) == 0
            || t0.elapsed() > RADIO_CAP
        {
            return answered;
        }
    }
}

/// Broadcasts Discover on channels 1–13 and keeps the first gateway that says Hello, on the
/// channel the Hello names: a gateway on 6 can be overheard on 4 or 5.
async fn sweep(esp: &mut EspNow, st: &mut State, key: &Creds) -> bool {
    let mut buf = [0u8; FRAME_MAX];
    let len = seal(key, &Frame::Discover, &mut buf);
    // The Hello comes unencrypted (we can't have the gateway's peer entry before we know who it
    // is), and an encrypted peer's plaintext may be dropped: forget the old gateway first.
    if st.gw_channel != 0 && esp.peer_exists(&st.gw_mac) {
        let _ = esp.remove_peer(&st.gw_mac);
    }
    for ch in 1..=13u8 {
        if esp.set_channel(ch).is_err() {
            continue;
        }
        while esp.receive().is_some() {} // a late Hello from the last channel isn't an answer here
        if esp
            .send_async(&BROADCAST_ADDRESS, &buf[..len])
            .await
            .is_err()
        {
            continue;
        }
        let hello = with_timeout(HELLO_WAIT, async {
            loop {
                let r = esp.receive_async().await;
                if let Some(Frame::Hello { channel }) = open(key, r.data()) {
                    return (r.info.src_address, channel);
                }
            }
        })
        .await;
        if let Ok((mac, channel)) = hello {
            st.gw_mac = mac;
            st.gw_channel = channel;
            info!("gateway found on channel {channel} (heard on {ch})");
            return true;
        }
    }
    false
}

/// One measurement, with one retry after 20 ms on an I²C error (SPEC.md, Sensor readings).
async fn read_sht41(i2c: &mut I2c<'_, Blocking>) -> (f32, f32, u8) {
    for attempt in 0..2 {
        if attempt > 0 {
            Timer::after_millis(20).await;
        }
        if i2c.write(SHT41, &[SHT41_MEASURE]).is_err() {
            continue;
        }
        Timer::after_millis(10).await;
        let mut b = [0u8; 6];
        if i2c.read(SHT41, &mut b).is_ok() {
            return sht41_parse(&b);
        }
    }
    (f32::NAN, f32::NAN, SENSOR_FAIL)
}

fn read_gauge(i2c: &mut I2c<'_, Blocking>, st: &mut State) {
    let mut reg = |r: u8| {
        let mut b = [0u8; 2];
        i2c.write_read(MAX17048, &[r], &mut b)
            .ok()
            .map(|_| u16::from_be_bytes(b))
    };
    if let (Some(v), Some(soc), Some(rate)) = (reg(0x02), reg(0x04), reg(0x16)) {
        (st.battery_v, st.battery_pct, st.charge_rate) = max17048(v, soc, rate);
        st.battery_chip = 1;
    } else {
        // ponytail: the LC709203F revision is only detected, not read
        st.battery_chip = if i2c.write(LC709203F, &[]).is_ok() {
            2
        } else {
            0
        };
        (st.battery_v, st.battery_pct, st.charge_rate) = (f32::NAN, f32::NAN, f32::NAN);
    }
}

/// `f`, encoded and tagged with our key.
fn seal(key: &Creds, f: &Frame, buf: &mut [u8; FRAME_MAX]) -> usize {
    let len = proto::encode(f, buf);
    keys::seal(&key.node_key, Dir::ToGateway, buf, len)
}

/// A frame from the gateway, if its tag is right.
fn open(key: &Creds, b: &[u8]) -> Option<Frame> {
    proto::decode(keys::open(&key.node_key, Dir::ToNode, b)?)
}

/// Asks the gateway for the image from `offset`. One try.
async fn fetch(esp: &mut EspNow, gw: &[u8; 6], key: &Creds, offset: u32) -> Option<Frame> {
    let mut buf = [0u8; FRAME_MAX];
    let len = seal(key, &Frame::Fetch { offset }, &mut buf);
    esp.send_async(gw, &buf[..len]).await.ok()?;
    with_timeout(CHUNK_WAIT, async {
        loop {
            let r = esp.receive_async().await;
            if r.info.src_address != *gw {
                continue;
            }
            if let Some(f @ Frame::Chunk { offset: o, .. }) = open(key, r.data())
                && o == offset
            {
                return f;
            }
        }
    })
    .await
    .ok()
}

/// Fetches the gateway's image into the slot we didn't boot from, checks it, points otadata at
/// it as New and restarts into it. Returns only if something went wrong, with what; the node
/// then carries on with the image it has.
async fn update(
    esp: &mut EspNow,
    st: &State,
    key: &Creds,
    flash: &mut FlashStorage<'_>,
) -> &'static str {
    let Some(slot) = flash::spare(flash) else {
        return "can't find the other app slot";
    };
    info!("update: fetching into ota_{}", slot.n);

    const SECTOR: usize = flash::SECTOR as usize;
    let mut sector = [0xFFu8; SECTOR];
    let mut sha = Sha256::new();
    let mut want: Option<([u8; 32], u32)> = None;
    let mut offset = 0u32;
    let mut misses = 0;
    let t0 = Instant::now();
    loop {
        if t0.elapsed() > UPDATE_CAP {
            return "took too long";
        }
        let Some(Frame::Chunk {
            sha: s,
            total,
            n,
            data,
            ..
        }) = fetch(esp, &st.gw_mac, key, offset).await
        else {
            misses += 1;
            if misses > 20 {
                return "gateway stopped answering";
            }
            continue;
        };
        misses = 0;
        match want {
            None if (total as usize) < SECTOR || total > slot.len => return "image size",
            None => want = Some((s, total)),
            Some(w) if w != (s, total) => return "the gateway's image changed",
            _ => {}
        }
        let n = n as usize;
        // every chunk but the last is full, so chunks never straddle a sector
        if n != CHUNK && offset as usize + n != total as usize {
            return "short chunk";
        }
        let at = offset as usize % SECTOR;
        sector[at..at + n].copy_from_slice(&data[..n]);
        sha.update(&data[..n]);
        offset += n as u32;
        if (offset as usize).is_multiple_of(SECTOR) || offset == total {
            let start = (offset - 1) / SECTOR as u32 * SECTOR as u32;
            if flash.write(slot.offset + start, &sector).is_err() {
                return "flash write";
            }
            sector.fill(0xFF);
        }
        if offset == total {
            break;
        }
    }
    let (want_sha, total) = want.unwrap_or_default();
    let got = sha.finalize();
    if got != want_sha {
        return "SHA-256 doesn't match";
    }
    if let Err(e) = flash::verify(flash, slot, image::NODE_PROJECT) {
        return e;
    }
    info!(
        "update: {total} bytes in {} s, SHA-256 ok",
        t0.elapsed().as_secs()
    );

    if let Err(e) = flash::boot_next(flash, slot) {
        return e;
    }
    info!("update: restarting into ota_{}", slot.n);
    Timer::after_millis(50).await; // let the log out
    esp_hal::system::software_reset()
}
