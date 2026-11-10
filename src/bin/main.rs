#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]

//! Feather ESP32-S3 room node, milestone 1 (plugged in): SHT41 every 60 s -> RAM ring -> QWP over
//! WebSocket to QuestDB's `room_readings`. Deep sleep, batching and device_status come next; see
//! SPEC.md.

use core::cell::{Cell, RefCell};
use core::mem::MaybeUninit;
use core::net::{IpAddr, SocketAddr};

use embassy_executor::Spawner;
use embassy_net::{
    Runner, Stack, StackResources,
    dns::DnsQueryType,
    udp::{PacketMetadata, UdpSocket},
};
use embassy_sync::blocking_mutex::{Mutex as BlockingMutex, raw::CriticalSectionRawMutex};
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer};
use esp_backtrace as _;
use esp_hal::{
    Blocking,
    clock::CpuClock,
    gpio::{Level, Output, OutputConfig},
    i2c::master::{Config as I2cConfig, I2c},
    rng::Rng,
    time::Rate,
    timer::timg::TimerGroup,
};
use esp_radio::wifi::{
    AuthenticationMethodConfig, Config, ControllerConfig, Interface, WifiController,
    sta::StationConfig,
};
use log::{error, info, warn};
use qwp_embedded::Ring;
use qwp_embedded::sender::{self, Buffers, SharedRing, Stats};
use sntpc::{NtpContext, NtpTimestampGenerator, get_time};
use sntpc_net_embassy::UdpSocketWrapper;
use thermo_feathers::{OUT_OF_RANGE, Reading, SENSOR_FAIL, SHT41, SHT41_MEASURE, sht41_parse};

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
const SAMPLE_SECS: u64 = 60;
const RING_LEN: usize = 1440; // a day of readings if QuestDB is away

type Shared = &'static SharedRing<Reading>;

static OFFSET_US: critical_section::Mutex<Cell<i64>> = critical_section::Mutex::new(Cell::new(0));
static SYNCED: Signal<CriticalSectionRawMutex, ()> = Signal::new();
static QWP_STATS: Stats = Stats::new();

fn now_us() -> i64 {
    Instant::now().as_micros() as i64 + critical_section::with(|cs| OFFSET_US.borrow(cs).get())
}

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    esp_println::logger::init_logger_from_env();
    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::_80MHz));
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 73744);
    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    // Device ID: the last three MAC bytes, e.g. a1b2c3
    let mac = esp_hal::efuse::base_mac_address();
    let id = mk_static!([u8; 6], [0; 6]);
    for (i, b) in mac.as_bytes()[3..].iter().enumerate() {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        id[2 * i] = HEX[(b >> 4) as usize];
        id[2 * i + 1] = HEX[(b & 0xF) as usize];
    }
    let device: &'static str = core::str::from_utf8(id).unwrap();
    info!(
        "thermo-feathers {} device {device}",
        env!("CARGO_PKG_VERSION")
    );

    let slots = mk_static!(
        [MaybeUninit<Reading>; RING_LEN],
        [MaybeUninit::uninit(); RING_LEN]
    );
    let ring: Shared = mk_static!(
        SharedRing<Reading>,
        BlockingMutex::new(RefCell::new(Ring::new(slots)))
    );

    // WiFi
    let station = Config::Station(
        StationConfig::default()
            .with_ssid(SSID.try_into().unwrap())
            .with_authentication(AuthenticationMethodConfig::Wpa2Personal(
                PASSWORD.try_into().unwrap(),
            )),
    );
    let controller = WifiController::new(
        peripherals.WIFI,
        ControllerConfig::default().with_initial_config(station),
    )
    .unwrap();
    let rng = Rng::new();
    let seed = (rng.random() as u64) << 32 | rng.random() as u64;
    let (stack, runner) = embassy_net::new(
        Interface::station(),
        embassy_net::Config::dhcpv4(Default::default()),
        mk_static!(StackResources<4>, StackResources::<4>::new()),
        seed,
    );
    spawner.spawn(connection(controller).unwrap());
    spawner.spawn(net_task(runner).unwrap());
    spawner.spawn(sntp_task(stack).unwrap());
    let buffers = Buffers {
        body: mk_static!([u8; 4096], [0; 4096]),
        tcp_tx: mk_static!([u8; 4096], [0; 4096]),
        tcp_rx: mk_static!([u8; 1024], [0; 1024]),
        scratch: mk_static!([MaybeUninit<Reading>; 64], [MaybeUninit::uninit(); 64]),
    };
    spawner.spawn(sender_task(stack, ring, buffers, device).unwrap());

    // STEMMA QT power stays on while plugged in; milestone 2 switches it per wake.
    let _stemma = Output::new(peripherals.GPIO7, Level::High, OutputConfig::default());
    Timer::after_millis(10).await;
    let mut i2c = I2c::new(
        peripherals.I2C0,
        I2cConfig::default().with_frequency(Rate::from_khz(100)),
    )
    .unwrap()
    .with_sda(peripherals.GPIO3)
    .with_scl(peripherals.GPIO4);

    SYNCED.wait().await; // no timestamps before the clock is set
    let mut next = Instant::now();
    loop {
        let t_us = now_us();
        let (temp_c, rh_pct, flags) = read_sht41(&mut i2c).await;
        match flags {
            0 => info!("{temp_c:.2} °C {rh_pct:.1} %RH"),
            SENSOR_FAIL => warn!("sht41 read failed"),
            OUT_OF_RANGE => warn!("sht41 out of range"),
            _ => {}
        }
        let (len, dropped) = ring.lock(|r| {
            let mut r = r.borrow_mut();
            r.push(Reading {
                t_us,
                temp_c,
                rh_pct,
                flags,
            });
            (r.len(), r.dropped)
        });
        if len > 1 || dropped > 0 {
            info!(
                "ring {len}/{RING_LEN} dropped={dropped} qwp_reconnects={} last_error={}",
                QWP_STATS
                    .reconnects
                    .load(core::sync::atomic::Ordering::Relaxed),
                QWP_STATS
                    .last_error
                    .load(core::sync::atomic::Ordering::Relaxed)
            );
        }
        next += Duration::from_secs(SAMPLE_SECS);
        Timer::at(next).await;
    }
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

#[embassy_executor::task]
async fn connection(mut controller: WifiController<'static>) {
    loop {
        match controller.connect_async().await {
            Ok(_) => {
                info!("wifi connected");
                match controller.wait_for_disconnect_async().await {
                    Ok(i) => warn!("wifi disconnected: reason {:?} rssi {}", i.reason, i.rssi),
                    Err(e) => warn!("wifi disconnected: {e:?}"),
                }
            }
            Err(e) => error!("wifi connect failed: {e:?}"),
        }
        Timer::after_secs(5).await
    }
}

#[embassy_executor::task]
async fn net_task(mut runner: Runner<'static, Interface>) {
    runner.run().await
}

#[derive(Clone, Copy)]
struct NtpNow(i64);

impl NtpTimestampGenerator for NtpNow {
    fn init(&mut self) {
        self.0 = now_us();
    }
    fn timestamp_sec(&self) -> u64 {
        (self.0 / 1_000_000) as u64
    }
    fn timestamp_subsec_micros(&self) -> u32 {
        (self.0 % 1_000_000) as u32
    }
}

/// Each sync steps the offset; steps are ~ms on a LAN.
#[embassy_executor::task]
async fn sntp_task(stack: Stack<'static>) {
    let (mut rx_meta, mut tx_meta) = ([PacketMetadata::EMPTY; 4], [PacketMetadata::EMPTY; 4]);
    let (mut rx, mut tx) = ([0u8; 512], [0u8; 512]);
    stack.wait_config_up().await;
    let mut sock = UdpSocket::new(stack, &mut rx_meta, &mut rx, &mut tx_meta, &mut tx);
    sock.bind(123).unwrap();
    let sock = UdpSocketWrapper::new(sock);
    loop {
        let synced = match stack.dns_query(NTP_SERVER, DnsQueryType::A).await {
            Ok(addrs) if !addrs.is_empty() => {
                let server = SocketAddr::from((IpAddr::from(addrs[0]), 123));
                match get_time(server, &sock, NtpContext::new(NtpNow(0))).await {
                    Ok(t) => {
                        critical_section::with(|cs| {
                            let o = OFFSET_US.borrow(cs);
                            o.set(o.get() + t.offset());
                        });
                        info!("sntp: offset step {} µs", t.offset());
                        SYNCED.signal(());
                        true
                    }
                    Err(e) => {
                        warn!("sntp: {e:?}");
                        false
                    }
                }
            }
            r => {
                warn!("sntp dns: {r:?}");
                false
            }
        };
        Timer::after_secs(if synced { 3600 } else { 5 }).await;
    }
}

#[embassy_executor::task]
async fn sender_task(
    stack: Stack<'static>,
    ring: Shared,
    buffers: Buffers<Reading>,
    device: &'static str,
) {
    let rng = Rng::new();
    let config = sender::Config {
        addr: QUESTDB.parse().expect("QUESTDB_ADDR must be ipv4:port"),
        client_id: concat!("thermo-feathers/", env!("CARGO_PKG_VERSION")),
        symbols: &[device],
        batch_interval: Duration::from_millis(100),
        stats: &QWP_STATS,
    };
    sender::run(
        stack,
        ring,
        buffers,
        &config,
        || rng.random(),
        || None,
        thermo_feathers::encode,
    )
    .await
}
