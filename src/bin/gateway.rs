#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]

//! Feather ESP32-S3 gateway, on wall power. A WiFi station that also listens on ESP-NOW: nodes'
//! frames go into a PSRAM ring, which qwp-embedded streams to QuestDB. Each frame is acked to its
//! node once it's in the ring, with the time and the wake interval. See
//! docs/superpowers/specs/2026-11-10-espnow-gateway-design.md.

use core::cell::{Cell, RefCell};
use core::mem::MaybeUninit;
use core::net::{IpAddr, SocketAddr};
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};

use embassy_executor::Spawner;
use embassy_net::{
    Runner, Stack, StackResources,
    dns::DnsQueryType,
    udp::{PacketMetadata, UdpSocket},
};
use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embassy_time::{Duration, Instant, Timer, with_timeout};
use esp_backtrace as _;
use esp_hal::{clock::CpuClock, psram::Psram, rng::Rng, timer::timg::TimerGroup};
use esp_radio::esp_now::{EspNow, EspNowWifiInterface, PeerInfo};
use esp_radio::wifi::{
    AuthenticationMethodConfig, Config, ControllerConfig, Interface, PowerSaveMode, WifiController,
    sta::StationConfig,
};
use log::{error, info, warn};
use qwp_embedded::sender::{self, Buffers, SharedRing, Stats};
use qwp_embedded::{Ring, Symbols};
use sntpc::{NtpContext, NtpTimestampGenerator, get_time};
use sntpc_net_embassy::UdpSocketWrapper;
use thermo_feathers::proto::{self, FRAME_MAX, Frame, Reply};
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

/// Sent to every node in every reply. ponytail: fixed until the web page sets it (milestone 4).
const WAKE_S: u16 = 60;

const BODY_LEN: usize = 32 * 1024;
const TCP_TX_LEN: usize = 32 * 1024;
const SCRATCH_RECS: usize = 256;
/// ~16k records: over a day of 4 nodes. The rest of PSRAM is left for node firmware images.
const RING_BYTES: usize = 1024 * 1024;

/// Unix µs minus `Instant` µs.
static OFFSET_US: critical_section::Mutex<Cell<i64>> = critical_section::Mutex::new(Cell::new(0));
static SYNCED: AtomicBool = AtomicBool::new(false);
static WIFI_CHANNEL: AtomicU32 = AtomicU32::new(0);
static SYMBOLS: Symbols = Symbols::new();
static QWP_STATS: Stats = Stats::new();

type Shared = &'static SharedRing<Rec>;

/// A panic reboots: the gateway is on wall power, and a reboot loses only what QuestDB hadn't acked.
#[unsafe(no_mangle)]
fn custom_halt() -> ! {
    esp_hal::system::software_reset()
}

fn now_us() -> i64 {
    Instant::now().as_micros() as i64 + critical_section::with(|cs| OFFSET_US.borrow(cs).get())
}

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    esp_println::logger::init_logger_from_env();
    info!("reset reason: {:?}", esp_hal::system::reset_reason());
    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 73744);
    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    // PSRAM: QWP body and TCP tx buffer, then the ring.
    let psram = mk_static!(Psram, Psram::new(peripherals.PSRAM, Default::default()));
    let (ptr, size) = psram.raw_parts();
    // SAFETY: PSRAM is mapped for the program's lifetime and only handed out here, once.
    let mem: &'static mut [u8] = unsafe { core::slice::from_raw_parts_mut(ptr, size) };
    let (body, rest) = mem.split_at_mut(BODY_LEN);
    let (tcp_tx, rest) = rest.split_at_mut(TCP_TX_LEN);
    let (ring_mem, _) = rest.split_at_mut(RING_BYTES);
    // SAFETY: MaybeUninit<Rec> has no invalid bit patterns
    let (pre, slots, _) = unsafe { ring_mem.align_to_mut::<MaybeUninit<Rec>>() };
    assert!(pre.is_empty());
    let (scratch, slots) = slots.split_at_mut(SCRATCH_RECS);
    let ring: Shared = mk_static!(
        SharedRing<Rec>,
        BlockingMutex::new(RefCell::new(Ring::new(slots)))
    );
    info!("ring: {} records", ring.lock(|r| r.borrow().capacity()));

    let station = Config::Station(
        StationConfig::default()
            .with_ssid(SSID.try_into().unwrap())
            .with_authentication(AuthenticationMethodConfig::Wpa2Personal(
                PASSWORD.try_into().unwrap(),
            )),
    );
    let mut controller = WifiController::new(
        peripherals.WIFI,
        ControllerConfig::default().with_initial_config(station),
    )
    .unwrap();
    // A dozing station misses ESP-NOW frames
    controller.set_power_saving(PowerSaveMode::None).unwrap();
    let esp = controller.esp_now();
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
    spawner.spawn(
        sender(
            stack,
            ring,
            Buffers {
                body,
                tcp_tx,
                tcp_rx: mk_static!([u8; 2048], [0u8; 2048]),
                scratch,
            },
        )
        .unwrap(),
    );
    spawner.spawn(report(stack, ring).unwrap());

    espnow(esp, ring).await
}

/// Answers every node: Hello to Discover, Reply to Data once its records are in the ring.
async fn espnow(mut esp: EspNow, ring: Shared) -> ! {
    let mut nodes = Nodes::new();
    let fw_unknown = SYMBOLS.intern("?").unwrap();
    let mut buf = [0u8; FRAME_MAX];
    loop {
        let r = esp.receive_async().await;
        let src = r.info.src_address;
        let len = match proto::decode(r.data()) {
            // Not before joining WiFi: until then the radio is scanning, and a node would keep a
            // channel the gateway is about to leave.
            Some(Frame::Discover) if WIFI_CHANNEL.load(Relaxed) != 0 => {
                // the channel this arrived on is the one the gateway is on now
                let channel = r.info.rx_control.channel as u8;
                info!("discover from {}, ch {channel}", id_str(&device_id(&src)));
                proto::encode(&Frame::Hello { channel }, &mut buf)
            }
            Some(Frame::Data {
                nonce,
                status,
                n,
                samples,
            }) => {
                let id = device_id(&src);
                let Some(node) = nodes.slot(src) else {
                    warn!("node table full: {} ignored", id_str(&id));
                    continue;
                };
                let fw_len = status.fw.iter().position(|&b| b == 0).unwrap_or(12);
                let fw = core::str::from_utf8(&status.fw[..fw_len]).unwrap_or("?");
                let Some(dev) = SYMBOLS.intern(id_str(&id)) else {
                    warn!("symbol table full: new node {} ignored", id_str(&id));
                    continue;
                };
                // A full table only costs a new version its name, never the node its data
                let fw = SYMBOLS.intern(fw).unwrap_or_else(|| {
                    warn!("symbol table full: {} fw {fw} logged as ?", id_str(&id));
                    fw_unknown
                });
                (node.dev, node.fw) = (dev as u16, fw as u16);
                // The node's time at wake: the same in every frame and retry of a wake, so
                // QuestDB's dedup keeps one row. A node without time yet gets none.
                let t_us = status.t_us;
                let rssi = r.info.rx_control.rssi;
                let channel = r.info.rx_control.channel as u8;
                let status = (t_us > 0).then_some(Status {
                    t_us,
                    battery_v: status.battery_v,
                    battery_pct: status.battery_pct,
                    charge_rate: status.charge_rate,
                    rssi_dbm: rssi,
                    channel,
                    rtc_drift_ppm: status.rtc_drift_ppm,
                    buffered: status.buffered as u32,
                    dropped_samples: status.dropped_samples,
                    boot_count: status.boot_count,
                    reset_reason: status.reset_reason as u32,
                    battery_chip: status.battery_chip,
                });
                let buffered = status.map_or(0, |s| s.buffered);
                let stored = ring.lock(|q| {
                    store_frame(
                        &mut q.borrow_mut(),
                        node.dev,
                        node.fw,
                        &samples[..n as usize],
                        status,
                    )
                });
                if stored < n {
                    warn!("ring full: stored {stored}/{n} from {}", id_str(&id));
                }
                info!(
                    "{}: {stored}/{n} samples, buffered {buffered}, rssi {rssi}, ch {channel}",
                    id_str(&id)
                );
                let reply = Reply {
                    nonce,
                    stored,
                    epoch_us: if SYNCED.load(Relaxed) { now_us() } else { 0 },
                    wake_s: WAKE_S,
                    flags: 0,
                };
                proto::encode(&Frame::Reply(reply), &mut buf)
            }
            _ => continue, // someone else's ESP-NOW, or ours from a newer version
        };
        if !esp.peer_exists(&src) {
            let _ = esp.add_peer(PeerInfo {
                interface: EspNowWifiInterface::Station,
                peer_address: src,
                lmk: None,
                channel: None,
                encrypt: false,
            });
        }
        if let Err(e) = esp.send_async(&src, &buf[..len]).await {
            warn!("reply to {}: {e:?}", id_str(&device_id(&src)));
        }
    }
}

fn id_str(id: &[u8; 6]) -> &str {
    core::str::from_utf8(id).unwrap_or("??????")
}

#[embassy_executor::task]
async fn connection(mut controller: WifiController<'static>) {
    loop {
        match controller.connect_async().await {
            Ok(_) => {
                if let Ok(ap) = controller.ap_info() {
                    WIFI_CHANNEL.store(ap.channel as u32, Relaxed);
                }
                info!("wifi connected, channel {}", WIFI_CHANNEL.load(Relaxed));
                // The AP can move channel without disconnecting us (it did on 2026-11-03)
                let gone = loop {
                    if let Ok(r) = with_timeout(
                        Duration::from_secs(10),
                        controller.wait_for_disconnect_async(),
                    )
                    .await
                    {
                        break r;
                    }
                    if let Ok(ap) = controller.ap_info()
                        && WIFI_CHANNEL.swap(ap.channel as u32, Relaxed) != ap.channel as u32
                    {
                        info!("wifi channel now {}", ap.channel);
                    }
                };
                WIFI_CHANNEL.store(0, Relaxed);
                warn!("wifi disconnected: {gone:?}");
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

#[embassy_executor::task]
async fn sender(stack: Stack<'static>, ring: Shared, buffers: Buffers<Rec>) {
    let rng = Rng::new();
    let config = sender::Config {
        addr: QUESTDB.parse().expect("QUESTDB_ADDR must be ipv4:port"),
        client_id: concat!("thermo-gateway/", env!("CARGO_PKG_VERSION")),
        symbols: &SYMBOLS,
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
        encode,
    )
    .await
}

/// The address once DHCP binds, then the ring and QWP counters every minute.
#[embassy_executor::task]
async fn report(stack: Stack<'static>, ring: Shared) {
    stack.wait_config_up().await;
    if let Some(c) = stack.config_v4() {
        info!("gateway at {}", c.address.address());
    }
    loop {
        Timer::after_secs(60).await;
        let (len, dropped) = ring.lock(|r| (r.borrow().len(), r.borrow().dropped));
        info!(
            "ring {len} dropped {dropped} | qwp acks {} reconnects {} rejected {} last_error {} | ch {} synced {}",
            QWP_STATS.acks.load(Relaxed),
            QWP_STATS.reconnects.load(Relaxed),
            QWP_STATS.rejected.load(Relaxed),
            QWP_STATS.last_error.load(Relaxed),
            WIFI_CHANNEL.load(Relaxed),
            SYNCED.load(Relaxed)
        );
    }
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

/// Steps the offset on each sync: hourly, every 5 s until the first works.
#[embassy_executor::task]
async fn sntp_task(stack: Stack<'static>) {
    let (mut rx_meta, mut tx_meta) = ([PacketMetadata::EMPTY; 4], [PacketMetadata::EMPTY; 4]);
    let (mut rx, mut tx) = ([0u8; 512], [0u8; 512]);
    stack.wait_config_up().await;
    let mut sock = UdpSocket::new(stack, &mut rx_meta, &mut rx, &mut tx_meta, &mut tx);
    sock.bind(123).unwrap();
    let sock = UdpSocketWrapper::new(sock);
    loop {
        let ok = with_timeout(Duration::from_secs(5), async {
            let addrs = stack.dns_query(NTP_SERVER, DnsQueryType::A).await.ok()?;
            let server = SocketAddr::from((IpAddr::from(*addrs.first()?), 123));
            get_time(server, &sock, NtpContext::new(NtpNow(0)))
                .await
                .ok()
        })
        .await
        .ok()
        .flatten();
        match ok {
            Some(t) => {
                critical_section::with(|cs| {
                    let o = OFFSET_US.borrow(cs);
                    o.set(o.get() + t.offset());
                });
                SYNCED.store(true, Relaxed);
                info!("sntp: step {} µs", t.offset());
            }
            None => warn!("sntp: no answer"),
        }
        Timer::after_secs(if SYNCED.load(Relaxed) { 3600 } else { 5 }).await;
    }
}
