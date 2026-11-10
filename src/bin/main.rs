#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]

//! Feather ESP32-S3 room node, milestone 2 (battery cycle). Each wake reads the SHT41 into a ring in
//! RTC memory and deep-sleeps; every tenth wake also connects, syncs the clock and sends the ring
//! and a device_status row to QuestDB over QWP. See SPEC.md.

use core::cell::RefCell;
use core::mem::MaybeUninit;
use core::net::{IpAddr, SocketAddr};
use core::sync::atomic::Ordering::Relaxed;

use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_net::{
    Runner, Stack, StackResources,
    dns::DnsQueryType,
    udp::{PacketMetadata, UdpSocket},
};
use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embassy_time::{Duration, Instant, Timer, with_timeout};
use esp_backtrace as _;
use esp_hal::{
    Blocking,
    clock::CpuClock,
    gpio::{Level, Output, OutputConfig},
    i2c::master::{Config as I2cConfig, I2c},
    rng::Rng,
    rtc_cntl::{
        Rtc, SocResetReason,
        sleep::{LowPower, RtcSleepConfig},
    },
    time::Rate,
    timer::timg::TimerGroup,
};
use esp_radio::wifi::{
    AuthenticationMethodConfig, Config, ControllerConfig, Interface, WifiController,
    sta::StationConfig,
};
use log::{info, warn};
use qwp_embedded::Ring;
use qwp_embedded::sender::{self, Buffers, SharedRing, Stats};
use sntpc::{NtpContext, NtpTimestampGenerator, get_time};
use sntpc_net_embassy::UdpSocketWrapper;
use thermo_feathers::*;

esp_bootloader_esp_idf::esp_app_desc!();

macro_rules! mk_static {
    ($t:ty,$val:expr) => {{
        static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
        #[deny(unused_attributes)]
        let x = STATIC_CELL.uninit().write($val);
        x
    }};
}

const SSID: &str = env!("WIFI_SSID");
const PASSWORD: &str = env!("WIFI_PASSWORD");
const QUESTDB: &str = env!("QUESTDB_ADDR"); // ip:port of QuestDB's HTTP endpoint, e.g. 192.168.1.10:9000
const NTP_SERVER: &str = "pool.ntp.org";
const FW: &str = env!("CARGO_PKG_VERSION");

const WAKE_S: u32 = 60;
const SEND_EVERY: u32 = 10; // wakes per send: 10 minutes
const CRITICAL_WAKE_S: u32 = 600;
const LOW_PCT: f32 = 10.0; // both intervals double below this
const CRITICAL_V: f32 = 3.5; // no WiFi below this: its current spikes can brown the board out
const SEND_CAP: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const FLASH_WINDOW: Duration = Duration::from_secs(30);

const MAGIC: u32 = 0x7EF0_0002; // bump the low half when State's layout changes

/// Everything that survives deep sleep. Persistable: any bit pattern is valid.
#[repr(C)]
struct State {
    magic: u32,
    boot_count: u32,
    dropped_samples: u32,
    failed_sends: u32,
    wakes_to_send: u32,
    reset_reason: u32,
    next_wake_rtc_us: i64,
    clock: Clock,
    battery_v: f32,
    battery_pct: f32,
    charge_rate: f32,
    battery_chip: u8,
    ring: SampleRing,
}
// SAFETY: integers, floats and arrays of them; SampleRing::fix repairs bad indexes.
unsafe impl esp_hal::Persistable for State {}

#[esp_hal::ram(unstable(rtc_fast, persistent))]
static mut STATE: State = unsafe { core::mem::zeroed() };

static QWP_STATS: Stats = Stats::new();

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
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

    let charging = st.charge_rate > 0.0;
    let critical = st.battery_v < CRITICAL_V && !charging;
    let send = !critical && st.wakes_to_send == 0;

    // Sensors: STEMMA QT power on for the read only. The gauge is read on sends, and on every
    // critical wake to notice charging.
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
    if send || critical || st.battery_chip == 0 {
        read_gauge(&mut i2c, st);
    }
    stemma.set_low();
    drop(i2c);

    if st.charge_rate > 0.0 {
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

    if send {
        let ok = match select(
            send_cycle(spawner, peripherals.WIFI, st, &rtc_us),
            Timer::after(SEND_CAP),
        )
        .await
        {
            Either::First(ok) => ok,
            Either::Second(_) => {
                warn!("send: over {} s, giving up", SEND_CAP.as_secs());
                false
            }
        };
        if ok {
            st.failed_sends = 0;
        } else {
            st.failed_sends += 1;
        }
        st.wakes_to_send = wakes_to_next_send(SEND_EVERY, st.failed_sends, WAKE_S);
    }
    st.wakes_to_send = st.wakes_to_send.saturating_sub(1);

    // Sleep to a fixed cadence so wake length doesn't drift the samples.
    let wake_s = if critical {
        CRITICAL_WAKE_S
    } else if st.battery_pct < LOW_PCT {
        WAKE_S * 2
    } else {
        WAKE_S
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
    if critical {
        // ponytail: the spec's 50 ms red NeoPixel blink isn't done; needs a WS2812 driver on GPIO33
        warn!("battery critical: no WiFi until charging");
    }
    let mut lp = LowPower::new(peripherals.LPWR);
    lp.set_wakeup_deadline(
        esp_hal::time::Instant::now() + esp_hal::time::Duration::from_micros(sleep_us),
    );
    lp.sleep_deep(RtcSleepConfig::deep())
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

/// Connect, sync, send everything sendable plus a status row, and pop what QuestDB acked.
async fn send_cycle(
    spawner: Spawner,
    wifi: esp_hal::peripherals::WIFI<'static>,
    st: &mut State,
    rtc_us: &impl Fn() -> i64,
) -> bool {
    let t0 = Instant::now();
    let station = Config::Station(
        StationConfig::default()
            .with_ssid(SSID.try_into().unwrap())
            .with_authentication(AuthenticationMethodConfig::Wpa2Personal(
                PASSWORD.try_into().unwrap(),
            )),
    );
    let mut controller = WifiController::new(
        wifi,
        ControllerConfig::default().with_initial_config(station),
    )
    .unwrap();
    let rng = Rng::new();
    let seed = (rng.random() as u64) << 32 | rng.random() as u64;
    // smoltcp re-sends DISCOVER after 10 s, and the first one goes out before WiFi associates:
    // every connect would miss the 8 s cap. 1 s instead.
    let mut dhcp = embassy_net::DhcpConfig::default();
    dhcp.retry_config.discover_timeout /= 10;
    let (stack, runner) = embassy_net::new(
        Interface::station(),
        embassy_net::Config::dhcpv4(dhcp),
        mk_static!(StackResources<4>, StackResources::<4>::new()),
        seed,
    );
    spawner.spawn(net_task(runner).unwrap());
    match with_timeout(CONNECT_TIMEOUT, async {
        controller.connect_async().await.ok()?;
        stack.wait_config_up().await;
        Some(())
    })
    .await
    {
        Ok(Some(())) => {}
        _ => {
            warn!("wifi: no connection within {} s", CONNECT_TIMEOUT.as_secs());
            return false;
        }
    }
    let wifi_ms = t0.elapsed().as_millis() as u32;
    let rssi = controller.rssi().unwrap_or(0);
    info!("wifi up in {wifi_ms} ms, rssi {rssi}");

    match sntp(stack, &st.clock, rtc_us).await {
        Some((r, e)) => match st.clock.sync(r, e) {
            Ok(step) => {
                info!("sntp: step {step} µs, rtc {:.0} ppm", st.clock.ppm);
                st.clock.freeze_unsynced(&mut st.ring);
            }
            Err(step) => warn!("sntp: rejected a {} s step", step / 1_000_000),
        },
        None => warn!("sntp: no answer"),
    }
    if st.clock.synced == 0 {
        return false; // nothing has a real timestamp yet
    }

    // The sender wants a qwp Ring: copy the sendable samples into RAM, oldest first.
    let slots = mk_static!(
        [MaybeUninit<Rec>; RING_LEN],
        [MaybeUninit::uninit(); RING_LEN]
    );
    let ring = mk_static!(
        SharedRing<Rec>,
        BlockingMutex::new(RefCell::new(Ring::new(slots)))
    );
    let mut n = 0;
    while n < st.ring.len() && Clock::sendable(&st.ring.get(n)) {
        let s = st.ring.get(n);
        ring.lock(|r| r.borrow_mut().push(Rec::Reading(s)));
        n += 1;
    }
    let status = Status {
        t_us: st.clock.epoch_us(rtc_us()),
        battery_v: st.battery_v,
        battery_pct: st.battery_pct,
        charge_rate: st.charge_rate,
        rssi_dbm: rssi,
        wifi_ms,
        cached_connect: 0, // milestone 3
        rtc_drift_ppm: st.clock.ppm as i32,
        buffered: n as u32,
        dropped_samples: st.dropped_samples,
        boot_count: st.boot_count,
        reset_reason: st.reset_reason,
        battery_chip: st.battery_chip,
    };
    let mut status = Some(Rec::Status(status));

    let device = device_id();
    let symbols = [device, FW];
    let config = sender::Config {
        addr: QUESTDB.parse().expect("QUESTDB_ADDR must be ipv4:port"),
        client_id: concat!("thermo-feathers/", env!("CARGO_PKG_VERSION")),
        symbols: &symbols,
        batch_interval: Duration::from_millis(0),
        stats: &QWP_STATS,
    };
    let buffers = Buffers {
        body: mk_static!([u8; 8192], [0; 8192]),
        tcp_tx: mk_static!([u8; 8192], [0; 8192]),
        tcp_rx: mk_static!([u8; 1024], [0; 1024]),
        scratch: mk_static!([MaybeUninit<Rec>; 128], [MaybeUninit::uninit(); 128]),
    };
    let done = async {
        // the status goes first, so once its ack is in and the ring is empty, everything is acked
        while !(QWP_STATS.acks.load(Relaxed) >= 1 && ring.lock(|r| r.borrow().is_empty())) {
            Timer::after_millis(20).await;
        }
    };
    let sent = select(
        sender::run(
            stack,
            ring,
            buffers,
            &config,
            || rng.random(),
            || status.take(),
            encode,
        ),
        with_timeout(Duration::from_secs(10), done),
    )
    .await;
    let acked = n - ring.lock(|r| r.borrow().len());
    st.ring.pop(acked);
    let ok = matches!(sent, Either::Second(Ok(())));
    info!(
        "qwp: {acked}/{n} samples acked{}",
        if ok { "" } else { ", rest kept" }
    );
    ok
}

/// The last three MAC bytes, e.g. a1b2c3.
fn device_id() -> &'static str {
    let id = mk_static!([u8; 6], [0; 6]);
    let mac = esp_hal::efuse::base_mac_address();
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for (i, b) in mac.as_bytes()[3..].iter().enumerate() {
        id[2 * i] = HEX[(b >> 4) as usize];
        id[2 * i + 1] = HEX[(b & 0xF) as usize];
    }
    core::str::from_utf8(id).unwrap()
}

#[embassy_executor::task]
async fn net_task(mut runner: Runner<'static, Interface>) {
    runner.run().await
}

#[derive(Clone, Copy)]
struct NtpNow(i64);

impl NtpTimestampGenerator for NtpNow {
    fn init(&mut self) {}
    fn timestamp_sec(&self) -> u64 {
        (self.0 / 1_000_000) as u64
    }
    fn timestamp_subsec_micros(&self) -> u32 {
        (self.0 % 1_000_000) as u32
    }
}

/// One SNTP exchange: `(rtc µs, true epoch µs at that moment)`.
async fn sntp(
    stack: Stack<'static>,
    clock: &Clock,
    rtc_us: &impl Fn() -> i64,
) -> Option<(i64, i64)> {
    let (mut rx_meta, mut tx_meta) = ([PacketMetadata::EMPTY; 2], [PacketMetadata::EMPTY; 2]);
    let (mut rx, mut tx) = ([0u8; 256], [0u8; 256]);
    let mut sock = UdpSocket::new(stack, &mut rx_meta, &mut rx, &mut tx_meta, &mut tx);
    sock.bind(123).ok()?;
    let sock = UdpSocketWrapper::new(sock);
    with_timeout(Duration::from_secs(3), async {
        let addrs = stack.dns_query(NTP_SERVER, DnsQueryType::A).await.ok()?;
        let server = SocketAddr::from((IpAddr::from(*addrs.first()?), 123));
        let r = rtc_us();
        let local = if clock.synced == 0 {
            r
        } else {
            clock.epoch_us(r)
        };
        let t = get_time(server, &sock, NtpContext::new(NtpNow(local)))
            .await
            .ok()?;
        Some((r, local + t.offset()))
    })
    .await
    .ok()
    .flatten()
}
