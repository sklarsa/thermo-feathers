#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]

//! Feather ESP32-S3 room node. Each wake reads the SHT41 into a ring in RTC memory, sends what the
//! gateway hasn't stored yet over ESP-NOW, takes the time and wake interval from its reply, and
//! deep-sleeps. See docs/superpowers/specs/2026-11-10-espnow-gateway-design.md.

use embassy_time::{Duration, Instant, Timer, with_timeout};
use esp_backtrace as _;
use esp_hal::{
    Blocking,
    clock::CpuClock,
    gpio::{Level, Output, OutputConfig},
    i2c::master::{Config as I2cConfig, I2c},
    rtc_cntl::{
        Rtc, SocResetReason,
        sleep::{LowPower, RtcSleepConfig},
    },
    time::Rate,
    timer::timg::TimerGroup,
};
use esp_radio::esp_now::{BROADCAST_ADDRESS, EspNow, EspNowWifiInterface, PeerInfo};
use esp_radio::wifi::{ControllerConfig, WifiController};
use log::{info, warn};
use thermo_feathers::proto::{self, FRAME_MAX, Frame, MAX_SAMPLES, NodeStatus};
use thermo_feathers::*;

esp_bootloader_esp_idf::esp_app_desc!();

const FW: &str = env!("CARGO_PKG_VERSION");

const WAKE_S: u32 = 60; // until the gateway says otherwise
const CRITICAL_WAKE_S: u32 = 600;
const LOW_PCT: f32 = 10.0; // the interval doubles below this
const CRITICAL_V: f32 = 3.5; // no radio below this: its current spikes can brown the board out
const FLASH_WINDOW: Duration = Duration::from_secs(30);
const REPLY_WAIT: Duration = Duration::from_millis(50);
const HELLO_WAIT: Duration = Duration::from_millis(30);
const RADIO_CAP: Duration = Duration::from_secs(2);

const MAGIC: u32 = 0x7EF0_0003; // bump the low half when State's layout changes

/// Everything that survives deep sleep. Persistable: any bit pattern is valid.
#[repr(C)]
struct State {
    magic: u32,
    boot_count: u32,
    dropped_samples: u32,
    reset_reason: u32,
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
    /// From the gateway's last reply; 0 means WAKE_S.
    wake_s: u16,
    sweep_failures: u32,
    wakes_to_sweep: u32,
    ring: SampleRing,
}
// SAFETY: integers, floats and arrays of them; SampleRing::fix repairs bad indexes.
unsafe impl esp_hal::Persistable for State {}

#[esp_hal::ram(unstable(rtc_fast, persistent))]
static mut STATE: State = unsafe { core::mem::zeroed() };

#[esp_rtos::main]
async fn main(_spawner: embassy_executor::Spawner) -> ! {
    esp_println::logger::init_logger_from_env();
    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::_80MHz));
    let reset = esp_hal::system::reset_reason();
    let rtc = Rtc::new(peripherals.RTC_TIMER);
    let rtc_us = || rtc.time_since_power_up().as_micros() as i64;
    let woke_us = rtc_us();

    // SAFETY: the only reference, and nothing else runs yet
    let st = unsafe { &mut *core::ptr::addr_of_mut!(STATE) };
    if reset != Some(SocResetReason::CoreDeepSleep) || st.magic != MAGIC {
        // ponytail: magic + wake cause, no CRC; a garbled header after a brownout is caught by
        // the reset reason, and SampleRing::fix guards the indexes either way
        // SAFETY: State is valid zeroed
        *st = unsafe { core::mem::zeroed() };
        st.magic = MAGIC;
        st.next_wake_rtc_us = woke_us;
        st.battery_v = f32::NAN;
        st.battery_pct = f32::NAN;
        st.charge_rate = f32::NAN;
        info!("cold boot: {reset:?}");
    }
    st.ring.fix();
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
        radio(&mut esp, st, &rtc_us).await;
    }

    // Sleep to a fixed cadence so wake length doesn't drift the samples.
    let base = if st.wake_s == 0 {
        WAKE_S
    } else {
        st.wake_s as u32
    };
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
    let mut lp = LowPower::new(peripherals.LPWR);
    lp.set_wakeup_deadline(
        esp_hal::time::Instant::now() + esp_hal::time::Duration::from_micros(sleep_us),
    );
    lp.sleep_deep(RtcSleepConfig::deep())
}

/// Talk to the gateway where it was last heard; if it doesn't answer, look for it on every
/// channel, backing off 1, 2, 4… wakes (up to an hour) after sweeps that find nothing.
async fn radio(esp: &mut EspNow, st: &mut State, rtc_us: &impl Fn() -> i64) {
    if st.gw_channel != 0 && exchange(esp, st, rtc_us).await {
        st.sweep_failures = 0;
        st.wakes_to_sweep = 0;
        return;
    }
    if st.wakes_to_sweep > 0 {
        st.wakes_to_sweep -= 1;
        warn!("no gateway; next sweep in {} wakes", st.wakes_to_sweep);
        return;
    }
    if sweep(esp, st).await && exchange(esp, st, rtc_us).await {
        st.sweep_failures = 0;
        st.wakes_to_sweep = 0;
        return;
    }
    st.sweep_failures += 1;
    let wake_s = if st.wake_s == 0 {
        WAKE_S
    } else {
        st.wake_s as u32
    };
    st.wakes_to_sweep = wakes_to_next_send(1, st.sweep_failures, wake_s);
    warn!(
        "no gateway on any channel; next sweep in {} wakes",
        st.wakes_to_sweep
    );
}

/// Sends frames of samples until the ring has nothing sendable left, the gateway stops answering,
/// or RADIO_CAP runs out. Returns whether the gateway answered at all. The first frame may carry no
/// samples: before the first reply nothing has a real timestamp.
async fn exchange(esp: &mut EspNow, st: &mut State, rtc_us: &impl Fn() -> i64) -> bool {
    if esp.set_channel(st.gw_channel).is_err() {
        return false;
    }
    while esp.receive().is_some() {}
    if !esp.peer_exists(&st.gw_mac) {
        let _ = esp.add_peer(PeerInfo {
            interface: EspNowWifiInterface::Station,
            peer_address: st.gw_mac,
            lmk: None,
            channel: None,
            encrypt: false,
        });
    }
    let t0 = Instant::now();
    let mut answered = false;
    let mut buf = [0u8; FRAME_MAX];
    loop {
        let n = proto::sendable_prefix(&st.ring, MAX_SAMPLES);
        let mut samples = [Sample::default(); MAX_SAMPLES];
        for (i, s) in samples[..n].iter_mut().enumerate() {
            *s = st.ring.get(i);
        }
        st.nonce = st.nonce.wrapping_add(1);
        let mut fw = [0u8; 12];
        fw[..FW.len().min(12)].copy_from_slice(&FW.as_bytes()[..FW.len().min(12)]);
        let status = NodeStatus {
            t_us: if st.clock.synced == 0 {
                0
            } else {
                st.clock.epoch_us(rtc_us())
            },
            battery_v: st.battery_v,
            battery_pct: st.battery_pct,
            charge_rate: st.charge_rate,
            rtc_drift_ppm: st.clock.ppm as i32,
            buffered: st.ring.len() as u16,
            dropped_samples: st.dropped_samples,
            boot_count: st.boot_count,
            reset_reason: st.reset_reason as u8,
            battery_chip: st.battery_chip,
            fw,
        };
        let len = proto::encode(
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
                    if let Some(Frame::Reply(rep)) = proto::decode(r.data())
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
        answered = true;
        proto::apply_reply(
            &mut st.clock,
            &mut st.ring,
            st.nonce,
            n as u8,
            &rep,
            rtc_us(),
        );
        if rep.wake_s != 0 {
            st.wake_s = rep.wake_s;
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
            return true;
        }
    }
}

/// Broadcasts Discover on channels 1–13 and keeps the first gateway that says Hello, on the
/// channel the Hello names: a gateway on 6 can be overheard on 4 or 5.
async fn sweep(esp: &mut EspNow, st: &mut State) -> bool {
    let mut buf = [0u8; FRAME_MAX];
    let len = proto::encode(&Frame::Discover, &mut buf);
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
                if let Some(Frame::Hello { channel }) = proto::decode(r.data()) {
                    return (r.info.src_address, channel);
                }
            }
        })
        .await;
        if let Ok((mac, channel)) = hello {
            if st.gw_channel != 0 && esp.peer_exists(&st.gw_mac) {
                let _ = esp.remove_peer(&st.gw_mac);
            }
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
