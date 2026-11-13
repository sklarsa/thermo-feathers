#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]

//! Feather ESP32-S3 gateway, on wall power. A WiFi station that also listens on ESP-NOW: nodes'
//! frames go into a PSRAM ring, which qwp-embedded streams to QuestDB. Each frame is acked to its
//! node once it's in the ring, with the time and the wake interval. Serves the web page on port 80.
//! See docs/superpowers/specs/2026-11-10-espnow-gateway-design.md.

use core::cell::{Cell, RefCell};
use core::mem::MaybeUninit;
use core::net::{IpAddr, SocketAddr};
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};

use embassy_executor::Spawner;
use embassy_net::{
    Runner, Stack, StackResources,
    dns::DnsQueryType,
    tcp::TcpSocket,
    udp::{PacketMetadata, UdpSocket},
};
use embassy_sync::blocking_mutex::{Mutex as BlockingMutex, raw::CriticalSectionRawMutex};
use embassy_time::{Duration, Instant, Timer, with_timeout};
use esp_backtrace as _;
use esp_hal::{
    clock::CpuClock,
    psram::Psram,
    rng::Rng,
    rtc_cntl::{Rtc, RwdtStage, RwdtStageAction, SocResetReason},
    timer::timg::TimerGroup,
};
use esp_radio::esp_now::{EspNow, EspNowWifiInterface, PeerInfo};
use esp_radio::wifi::{
    AuthenticationMethodConfig, Config, ControllerConfig, Interface, PowerSaveMode, WifiController,
    sta::StationConfig,
};
use esp_storage::FlashStorage;
use log::{error, info, warn};
use qwp_embedded::sender::{self, Buffers, SharedRing, Stats};
use qwp_embedded::{Ring, Symbols};
use sha2::{Digest, Sha256};
use sntpc::{NtpContext, NtpTimestampGenerator, get_time};
use sntpc_net_embassy::UdpSocketWrapper;
use thermo_feathers::image;

#[path = "../flash.rs"]
mod flash;
use thermo_feathers::proto::{self, CHUNK, FRAME_MAX, Frame, Reply, UPDATE};
use thermo_feathers::web::{self, Nodes, Rollout, SETTINGS_LEN, Seen, Settings};
use thermo_feathers::*;

extern crate alloc;

// "thermo-gateway", so a node refuses this image if it's uploaded as node firmware.
esp_bootloader_esp_idf::esp_app_desc!(
    env!("CARGO_PKG_VERSION"),
    "thermo-gateway",
    esp_bootloader_esp_idf::BUILD_TIME,
    esp_bootloader_esp_idf::BUILD_DATE,
    esp_bootloader_esp_idf::ESP_IDF_COMPATIBLE_VERSION,
    esp_bootloader_esp_idf::MMU_PAGE_SIZE,
    0,
    u16::MAX,
    esp_bootloader_esp_idf::SECURE_VERSION
);

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
const GATEWAY_PROJECT: &str = "thermo-gateway";
/// A new gateway image that hasn't reached WiFi, the time server and QuestDB by then restarts,
/// and the bootloader goes back to the old one.
const TRIAL: Duration = Duration::from_secs(600);
/// How long a restart waits for the ring to reach QuestDB: nodes deleted what's in it.
const DRAIN: Duration = Duration::from_secs(30);

/// partitions.csv's `settings` partition: names, unit and wake interval.
const SETTINGS_ADDR: u32 = 0xA000;

const BODY_LEN: usize = 32 * 1024;
const TCP_TX_LEN: usize = 32 * 1024;
const SCRATCH_RECS: usize = 256;
/// ~16k records: over a day of 4 nodes. The rest of PSRAM is left for node firmware images.
const RING_BYTES: usize = 1024 * 1024;

/// Unix µs minus `Instant` µs.
static OFFSET_US: critical_section::Mutex<Cell<i64>> = critical_section::Mutex::new(Cell::new(0));
static SYNCED: AtomicBool = AtomicBool::new(false);
static WIFI_CHANNEL: AtomicU32 = AtomicU32::new(0);
/// Device IDs, firmware versions and room names. Append-only until a reboot.
static SYMBOLS: Symbols<128, 24> = Symbols::sized();
/// The web page's settings, and what each node last sent.
static UI: BlockingMutex<CriticalSectionRawMutex, RefCell<(Settings, Nodes)>> =
    BlockingMutex::new(RefCell::new((Settings::DEFAULT, Nodes::EMPTY)));
static QWP_STATS: Stats = Stats::new();
/// Where the sender sends: set at boot, and again when the settings page changes it.
static QWP_ADDR: sender::Addr = sender::Addr::new(core::net::SocketAddrV4::new(
    core::net::Ipv4Addr::UNSPECIFIED,
    0,
));

/// Settings and updates write flash from more than one task.
static FLASH: BlockingMutex<CriticalSectionRawMutex, RefCell<Option<FlashStorage<'static>>>> =
    BlockingMutex::new(RefCell::new(None));
/// This image's build ID.
static GW_FW: BlockingMutex<CriticalSectionRawMutex, Cell<[u8; 12]>> =
    BlockingMutex::new(Cell::new([0; 12]));

fn with_flash<R>(f: impl FnOnce(&mut FlashStorage<'static>) -> R) -> Option<R> {
    FLASH.lock(|fl| fl.borrow_mut().as_mut().map(f))
}

/// Node firmware waiting in PSRAM, and which nodes are to get it. `buf` is out while an upload
/// writes into it, and `rollout` is `None` then.
struct Image {
    buf: Option<&'static mut [u8]>,
    /// SHA-256 of the whole upload, which every chunk names.
    sha: [u8; 32],
    rollout: Option<Rollout>,
}
static IMAGE: BlockingMutex<CriticalSectionRawMutex, RefCell<Image>> =
    BlockingMutex::new(RefCell::new(Image {
        buf: None,
        sha: [0; 32],
        rollout: None,
    }));

type Shared = &'static SharedRing<Rec>;

/// Panics since the gateway was last healthy, through restarts: `[PANIC_MAGIC, count]`.
#[esp_hal::ram(unstable(rtc_fast, persistent))]
static mut PANICS: [u32; 2] = [0; 2];
const PANIC_MAGIC: u32 = 0x7EF0_6A7E;
/// Panics in a row, without becoming healthy, before a confirmed image gives way to the one
/// before it.
const PANICS_TO_FALL_BACK: u32 = 3;

fn panics() -> u32 {
    // SAFETY: two words, only touched from the boot path, custom_halt and `healthy`, one at a time
    let p = unsafe { *core::ptr::addr_of!(PANICS) };
    if p[0] == PANIC_MAGIC { p[1] } else { 0 }
}

fn set_panics(n: u32) {
    // SAFETY: as in `panics`
    unsafe { *core::ptr::addr_of_mut!(PANICS) = [PANIC_MAGIC, n] };
}

/// A panic reboots: the gateway is on wall power, and a reboot loses only what QuestDB hadn't acked.
#[unsafe(no_mangle)]
fn custom_halt() -> ! {
    set_panics(panics().saturating_add(1));
    esp_hal::system::software_reset()
}

fn now_us() -> i64 {
    Instant::now().as_micros() as i64 + critical_section::with(|cs| OFFSET_US.borrow(cs).get())
}

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    esp_println::logger::init_logger_from_env();
    let reset = esp_hal::system::reset_reason();
    info!("reset reason: {reset:?}");
    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    if matches!(
        reset,
        Some(SocResetReason::CoreRtcWdt | SocResetReason::CpuRtcWdt | SocResetReason::SysRtcWdt)
    ) {
        warn!("the watchdog reset us: a hang, counted as a crash");
        set_panics(panics().saturating_add(1));
    }
    // A hang is a reset, fed by a task: if the executor stops running tasks, it stops.
    let mut rtc = Rtc::new(peripherals.RTC_TIMER);
    rtc.rwdt
        .set_stage_action(RwdtStage::Stage0, RwdtStageAction::ResetCore);
    rtc.rwdt.set_timeout(
        RwdtStage::Stage0,
        esp_hal::time::Duration::from_secs(WATCHDOG_S),
    );
    rtc.rwdt.enable();

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 73744);

    let mut flash = FlashStorage::new(peripherals.FLASH);
    let mut b = [0u8; SETTINGS_LEN];
    match flash
        .read(SETTINGS_ADDR, &mut b)
        .ok()
        .and_then(|_| Settings::from_bytes(&b))
    {
        Some(s) => UI.lock(|u| u.borrow_mut().0 = s),
        None => info!("settings: none saved, using defaults"),
    }
    let fw = flash::build_id(&mut flash, env!("CARGO_PKG_VERSION"));
    GW_FW.lock(|f| f.set(fw));
    let trial = flash::on_trial(&mut flash);
    // A confirmed image that keeps crashing: the bootloader only rolls back unconfirmed ones.
    if !trial && panics() >= PANICS_TO_FALL_BACK {
        set_panics(0);
        if flash::fall_back(&mut flash) {
            esp_hal::system::software_reset();
        }
    }
    info!(
        "fw {}{}",
        core::str::from_utf8(&fw).unwrap_or("?"),
        if trial { ", on trial" } else { "" }
    );
    FLASH.lock(|f| *f.borrow_mut() = Some(flash));
    QWP_ADDR.set(questdb_addr(&UI.lock(|u| u.borrow().0)));
    info!("questdb: {}", QWP_ADDR.get());
    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    // PSRAM: QWP body and TCP tx buffer, then the ring.
    let psram = mk_static!(Psram, Psram::new(peripherals.PSRAM, Default::default()));
    let (ptr, size) = psram.raw_parts();
    // SAFETY: PSRAM is mapped for the program's lifetime and only handed out here, once.
    let mem: &'static mut [u8] = unsafe { core::slice::from_raw_parts_mut(ptr, size) };
    let (body, rest) = mem.split_at_mut(BODY_LEN);
    let (tcp_tx, rest) = rest.split_at_mut(TCP_TX_LEN);
    let (ring_mem, image_mem) = rest.split_at_mut(RING_BYTES);
    info!("node image space: {} KB", image_mem.len() / 1024);
    IMAGE.lock(|i| i.borrow_mut().buf = Some(image_mem));
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
        mk_static!(StackResources<6>, StackResources::<6>::new()),
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
    spawner.spawn(http(stack, ring).unwrap());
    spawner.spawn(healthy(trial).unwrap());
    spawner.spawn(watchdog(mk_static!(Rtc<'static>, rtc)).unwrap());

    espnow(esp, ring).await
}

/// Answers every node: Hello to Discover, Reply to Data once its records are in the ring.
async fn espnow(mut esp: EspNow, ring: Shared) -> ! {
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
                let fw_len = status.fw.iter().position(|&b| b == 0).unwrap_or(12);
                let fw = core::str::from_utf8(&status.fw[..fw_len]).unwrap_or("?");
                let Some(dev) = SYMBOLS.intern(id_str(&id)) else {
                    warn!("symbol table full: new node {} ignored", id_str(&id));
                    continue;
                };
                // A full table only costs a new version its name, never the node its data
                let fw = intern_spare(fw).unwrap_or_else(|| {
                    warn!("symbol table full: {} fw {fw} logged as ?", id_str(&id));
                    fw_unknown
                });
                // The node's time at wake: the same in every frame and retry of a wake, so
                // QuestDB's dedup keeps one row. A node without time yet gets none, and neither
                // does a wild one: QuestDB would reject the batch, and other nodes' rows with it.
                let t_us = status.t_us;
                let t_ok = (EPOCH_MIN_US..EPOCH_MAX_US).contains(&t_us);
                let rssi = r.info.rx_control.rssi;
                let channel = r.info.rx_control.channel as u8;
                let (fw_bytes, battery_pct) = (status.fw, status.battery_pct);
                let status = t_ok.then_some(Status {
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
                let seen = Seen {
                    fw: fw_bytes,
                    id,
                    at_s: Instant::now().as_secs(),
                    temp_c: f32::NAN,
                    rh_pct: f32::NAN,
                    charging: false,
                    battery_pct,
                    rssi,
                };
                let stored = ring.lock(|q| {
                    store_frame(
                        &mut q.borrow_mut(),
                        dev as u16,
                        fw as u16,
                        &samples[..n as usize],
                        status,
                    )
                });
                UI.lock(|u| u.borrow_mut().1.heard(seen, &samples[..n as usize]));
                let offer = IMAGE
                    .lock(|i| {
                        i.borrow_mut().rollout.as_mut().map(|r| {
                            r.heard(&id, &fw_bytes);
                            r.offer(&id)
                        })
                    })
                    .unwrap_or(false);
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
                    wake_s: UI.lock(|u| u.borrow().0.wake_s),
                    flags: if offer { UPDATE } else { 0 },
                };
                proto::encode(&Frame::Reply(reply), &mut buf)
            }
            Some(Frame::Fetch { offset }) => {
                let id = device_id(&src);
                let chunk = IMAGE.lock(|i| {
                    let mut i = i.borrow_mut();
                    let Image {
                        buf: img,
                        sha,
                        rollout,
                    } = &mut *i;
                    let (img, r) = (img.as_ref()?, rollout.as_mut()?);
                    if offset >= r.len || !r.fetch(&id, offset) {
                        return None;
                    }
                    let n = (r.len - offset).min(CHUNK as u32) as usize;
                    let mut data = [0u8; CHUNK];
                    data[..n].copy_from_slice(&img[offset as usize..offset as usize + n]);
                    Some((
                        Frame::Chunk {
                            sha: *sha,
                            total: r.len,
                            offset,
                            n: n as u8,
                            data,
                        },
                        offset as usize + n == r.len as usize,
                    ))
                });
                let Some((chunk, last)) = chunk else {
                    continue;
                };
                if offset == 0 {
                    info!("{}: fetching the update", id_str(&id));
                } else if last {
                    info!("{}: fetched the update", id_str(&id));
                }
                proto::encode(&chunk, &mut buf)
            }
            _ => continue, // someone else's ESP-NOW, or ours from a newer version
        };
        // A peer only for the reply: ESP-NOW's peer table holds about 20, and boards come and go
        let peer = PeerInfo {
            interface: EspNowWifiInterface::Station,
            peer_address: src,
            lmk: None,
            channel: None,
            encrypt: false,
        };
        if !esp.peer_exists(&src)
            && let Err(e) = esp.add_peer(peer)
        {
            warn!("peer {}: {e:?}", id_str(&device_id(&src)));
            continue;
        }
        if let Err(e) = esp.send_async(&src, &buf[..len]).await {
            warn!("reply to {}: {e:?}", id_str(&device_id(&src)));
        }
        let _ = esp.remove_peer(&src);
    }
}

/// Interns firmware versions and room names only while a quarter of the table is free, so they
/// can't crowd out a new node's ID. Names already in the table always resolve.
fn intern_spare(s: &str) -> Option<u64> {
    if SYMBOLS.len() < 96 {
        SYMBOLS.intern(s)
    } else {
        SYMBOLS.with(|all| all.iter().position(|x| *x == s).map(|i| i as u64))
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

/// Seconds without a feed before the RTC watchdog resets the gateway.
const WATCHDOG_S: u64 = 30;

#[embassy_executor::task]
async fn watchdog(rtc: &'static mut Rtc<'static>) {
    loop {
        rtc.rwdt.feed();
        Timer::after_secs(5).await;
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
        addr: &QWP_ADDR,
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
        let settings = UI.lock(|u| u.borrow().0);
        let server = match settings.ntp() {
            "" => NTP_SERVER,
            s => s,
        };
        let ok = with_timeout(Duration::from_secs(5), async {
            let addrs = stack.dns_query(server, DnsQueryType::A).await.ok()?;
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

/// The web page, one connection at a time.
#[embassy_executor::task]
async fn http(stack: Stack<'static>, ring: Shared) {
    let (mut rx, mut tx) = ([0u8; 1536], [0u8; 2048]);
    let mut req = [0u8; 1536];
    stack.wait_config_up().await;
    loop {
        let mut sock = TcpSocket::new(stack, &mut rx, &mut tx);
        sock.set_timeout(Some(Duration::from_secs(10)));
        if sock.accept(80).await.is_err() {
            continue;
        }
        let mut len = 0;
        let mut restart = false;
        let head = loop {
            if let Some(h) = web::parse_head(&req[..len]) {
                if h.post && h.path == b"/update" {
                    let (at, cl) = (h.len, h.content_length);
                    let mut to = [0u8; 128];
                    let q = h.query.len().min(to.len());
                    to[..q].copy_from_slice(&h.query[..q]);
                    let early = len.min(at.saturating_add(cl));
                    let resp = upload(&mut sock, &req[at..early], cl, &to[..q]).await;
                    write_all(&mut sock, resp.as_bytes()).await;
                    break None;
                }
                if h.post && h.path == b"/update/gateway" {
                    let (at, cl) = (h.len, h.content_length);
                    let early = len.min(at.saturating_add(cl));
                    let resp;
                    (resp, restart) = self_update(&mut sock, &req[at..early], cl).await;
                    write_all(&mut sock, resp.as_bytes()).await;
                    break None;
                }
                let (post, path, at, body_len) = (h.post, h.path, h.len, h.content_length);
                let route = match (post, path) {
                    (false, b"/") => Route::Home(None),
                    (false, b"/saved") => Route::Home(Some(Ok("Saved."))),
                    (false, b"/settings") => Route::Settings(None),
                    (false, b"/settings/saved") => Route::Settings(Some(Ok("Saved."))),
                    (true, b"/name") => Route::Rename,
                    (true, b"/settings") => Route::Save,
                    _ => Route::NotFound,
                };
                break Some((route, at, at.saturating_add(body_len)));
            }
            if len == req.len() {
                break None; // a head too big for us
            }
            match sock.read(&mut req[len..]).await {
                Ok(n) if n > 0 => len += n,
                _ => break None, // closed or timed out
            }
        };
        if let Some((route, at, end)) = head
            && end <= req.len()
        {
            while len < end {
                match sock.read(&mut req[len..end]).await {
                    Ok(n) if n > 0 => len += n,
                    _ => break,
                }
            }
            if len >= end {
                let resp;
                (resp, restart) = respond(route, &req[at..end], ring);
                write_all(&mut sock, resp.as_bytes()).await;
            }
        }
        sock.close();
        let _ = with_timeout(Duration::from_secs(2), sock.flush()).await;
        if restart {
            restart_when_drained(ring).await;
        }
    }
}

/// Restarts once the ring has reached QuestDB, or after DRAIN: its records are acked to nodes,
/// which deleted their copies.
async fn restart_when_drained(ring: Shared) -> ! {
    let t0 = Instant::now();
    while ring.lock(|r| r.borrow().len()) > 0 && t0.elapsed() < DRAIN {
        Timer::after_millis(200).await;
    }
    let left = ring.lock(|r| r.borrow().len());
    if left > 0 {
        warn!("restarting with {left} records QuestDB hasn't acked");
    }
    info!("restarting");
    Timer::after_millis(100).await;
    esp_hal::system::software_reset()
}

/// Waits for WiFi, the time and a QuestDB ack. Then the panic count starts over, and a new image
/// on trial is kept. One that doesn't get there within TRIAL restarts, and the bootloader goes
/// back to the old one.
#[embassy_executor::task]
async fn healthy(trial: bool) {
    let t0 = Instant::now();
    loop {
        if WIFI_CHANNEL.load(Relaxed) != 0
            && SYNCED.load(Relaxed)
            && QWP_STATS.acks.load(Relaxed) > 0
        {
            set_panics(0);
            if trial {
                with_flash(flash::confirm);
            }
            return;
        }
        if trial && t0.elapsed() > TRIAL {
            error!(
                "on trial for {} s without WiFi, time and QuestDB: going back",
                TRIAL.as_secs()
            );
            Timer::after_millis(100).await;
            esp_hal::system::software_reset();
        }
        Timer::after_secs(5).await;
    }
}

enum Route {
    Home(web::Note<'static>),
    Settings(web::Note<'static>),
    Rename,
    Save,
    NotFound,
}

/// The response, and whether the gateway restarts after sending it.
fn respond(route: Route, body: &[u8], ring: Shared) -> (alloc::string::String, bool) {
    let mut page = alloc::string::String::new();
    let (settings, nodes) = UI.lock(|u| *u.borrow());
    let rollout = IMAGE.lock(|i| i.borrow().rollout);
    let questdb = match settings.questdb() {
        "" => QUESTDB,
        q => q,
    };
    let gateway = || web::Gateway {
        fw: GW_FW.lock(|f| f.get()),
        up_s: Instant::now().as_secs(),
        channel: WIFI_CHANNEL.load(Relaxed),
        time_synced: SYNCED.load(Relaxed),
        waiting: ring.lock(|r| r.borrow().len()),
        last_error: QWP_STATS.last_error.load(Relaxed),
        questdb,
        questdb_default: QUESTDB,
        ntp_default: NTP_SERVER,
    };
    let _ = match route {
        Route::Home(n) => web::render_home(
            &mut page,
            &settings,
            &nodes,
            &gateway(),
            rollout.as_ref(),
            n,
        ),
        Route::Settings(n) => web::render_settings(
            &mut page,
            &settings,
            &nodes,
            rollout.as_ref(),
            &gateway(),
            n,
        ),
        Route::Rename => match rename(body, ring) {
            Ok(()) => return (see_other("/saved"), false),
            Err(msg) => web::render_home(
                &mut page,
                &settings,
                &nodes,
                &gateway(),
                rollout.as_ref(),
                Some(Err(msg)),
            ),
        },
        Route::Save => {
            let mut b = [0u8; 8];
            let fahrenheit = web::form_value(body, "unit", &mut b) != Some("c");
            let mut b = [0u8; 8];
            let wake =
                web::form_value(body, "wake", &mut b).and_then(|w| w.trim().parse::<u16>().ok());
            let mut s = settings;
            s.fahrenheit = fahrenheit;
            if let Some(w) = wake {
                s.wake_s = w.clamp(WAKE_MIN_S, WAKE_MAX_S);
            }
            let (mut qb, mut nb) = ([0u8; 64], [0u8; 128]);
            let fields = (
                web::form_value(body, "questdb", &mut qb),
                web::form_value(body, "ntp", &mut nb),
            );
            let checked = match fields {
                (Some(q), Some(n)) => s.set_questdb(q).and_then(|()| s.set_ntp(n)),
                _ => Err("Those fields are too long."),
            };
            if let Err(msg) = checked {
                let _ = web::render_settings(
                    &mut page,
                    &settings,
                    &nodes,
                    rollout.as_ref(),
                    &gateway(),
                    Some(Err(msg)),
                );
                return (html(&page), false);
            }
            UI.lock(|u| u.borrow_mut().0 = s);
            save(&s);
            info!(
                "settings: {} wake {} s, questdb {:?}, ntp {:?}",
                if s.fahrenheit { "°F" } else { "°C" },
                s.wake_s,
                s.questdb(),
                s.ntp()
            );
            // the sender moves over, taking what QuestDB hasn't acked with it
            QWP_ADDR.set(questdb_addr(&s));
            return (see_other("/settings/saved"), false);
        }
        Route::NotFound => {
            return (
                "HTTP/1.0 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
                false,
            );
        }
    };
    (html(&page), false)
}

/// The settings' QuestDB address, or the build's.
fn questdb_addr(s: &Settings) -> core::net::SocketAddrV4 {
    s.questdb()
        .parse()
        .ok()
        .unwrap_or_else(|| QUESTDB.parse().expect("QUESTDB_ADDR must be ipv4:port"))
}

fn html(page: &str) -> alloc::string::String {
    use core::fmt::Write;
    let mut resp = alloc::string::String::with_capacity(page.len() + 128);
    let _ = write!(
        resp,
        "HTTP/1.0 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nCache-Control: no-store\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{page}",
        page.len()
    );
    resp
}

fn see_other(to: &str) -> alloc::string::String {
    alloc::format!(
        "HTTP/1.0 303 See Other\r\nLocation: {to}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )
}

/// A rename from the form: saved to flash, and to `device_locations` once the gateway has time.
fn rename(body: &[u8], ring: Shared) -> Result<(), &'static str> {
    let (mut ib, mut nb) = ([0u8; 6], [0u8; 128]);
    let id: [u8; 6] = web::form_value(body, "id", &mut ib)
        .filter(|i| i.len() == 6 && i.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or("That isn't a node ID.")?
        .as_bytes()
        .try_into()
        .map_err(|_| "That isn't a node ID.")?;
    let name = web::form_value(body, "name", &mut nb).ok_or("That name is too long.")?;
    let mut copy = [0u8; web::NAME_MAX];
    let (s, stored) = UI.lock(|u| {
        let mut u = u.borrow_mut();
        let stored = u.0.rename(id, name).map(|n| {
            copy[..n.len()].copy_from_slice(n.as_bytes());
            n.len()
        });
        (u.0, stored)
    });
    let stored = stored.ok_or("No room for another name: clear one first.")?;
    save(&s);
    let room = core::str::from_utf8(&copy[..stored]).unwrap_or("");
    info!("rename {}: {room:?}", id_str(&id));
    if room.is_empty() {
        return Ok(());
    }
    if !SYNCED.load(Relaxed) {
        warn!(
            "rename {}: no time yet, so no device_locations row",
            id_str(&id)
        );
        return Ok(());
    }
    match (SYMBOLS.intern(id_str(&id)), intern_spare(room)) {
        (Some(dev), Some(room)) => ring.lock(|q| {
            q.borrow_mut().push(Rec::Location {
                dev: dev as u16,
                room: room as u16,
                t_us: now_us(),
            })
        }),
        _ => warn!(
            "rename {}: symbol table full, no device_locations row",
            id_str(&id)
        ),
    }
    Ok(())
}

fn save(s: &Settings) {
    if let Some(Err(e)) = with_flash(|f| f.write(SETTINGS_ADDR, &s.to_bytes())) {
        error!("settings: flash write failed: {e:?}");
    }
}

async fn write_all(sock: &mut TcpSocket<'_>, mut out: &[u8]) {
    while !out.is_empty() {
        match sock.write(out).await {
            Ok(n) if n > 0 => out = &out[n..],
            _ => break,
        }
    }
}

fn text(status: &str, body: &str) -> alloc::string::String {
    alloc::format!(
        "HTTP/1.0 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    )
}

/// `POST /update?to=all|id,id`: node firmware as the raw body, from espflash save-image. Checked
/// whole before any node is offered it.
async fn upload(
    sock: &mut TcpSocket<'_>,
    early: &[u8],
    len: usize,
    query: &[u8],
) -> alloc::string::String {
    let (ids, n) = match targets(query).and_then(|t| fits(len).map(|()| t)) {
        Ok(t) => t,
        Err(e) => return text("400 Bad Request", &alloc::format!("Refused: {e}")),
    };
    let Some(img) = IMAGE.lock(|i| {
        let mut i = i.borrow_mut();
        i.rollout = None;
        i.buf.take()
    }) else {
        return text("409 Conflict", "Another upload is running.");
    };
    let res = receive(sock, img, early, len)
        .await
        .and_then(|()| check(&img[..len], &ids[..n]));
    let msg = match &res {
        Ok((r, _)) => {
            let n = r.targets.iter().flatten().count();
            let fw = core::str::from_utf8(&r.fw).unwrap_or("?");
            info!("update {fw}: {len} bytes for {n} nodes");
            alloc::format!("Got {fw}. {n} node(s) will fetch it at their next wake.")
        }
        Err(e) => {
            warn!("update refused: {e}");
            alloc::format!("Refused: {e}")
        }
    };
    IMAGE.lock(|i| {
        let mut i = i.borrow_mut();
        i.buf = Some(img);
        if let Ok((r, sha)) = res {
            i.rollout = Some(r);
            i.sha = sha;
        }
    });
    text(
        if res.is_ok() {
            "200 OK"
        } else {
            "400 Bad Request"
        },
        &msg,
    )
}

async fn receive(
    sock: &mut TcpSocket<'_>,
    img: &mut [u8],
    early: &[u8],
    len: usize,
) -> Result<(), &'static str> {
    if len > img.len() {
        return Err("too big for the gateway's image space");
    }
    img[..early.len()].copy_from_slice(early);
    let mut got = early.len();
    while got < len {
        match sock.read(&mut img[got..len]).await {
            Ok(n) if n > 0 => got += n,
            _ => return Err("the upload was cut short"),
        }
    }
    Ok(())
}

/// A whole, intact app image of `project`: its version and its own SHA-256.
fn check_image<'a>(img: &'a [u8], project: &str) -> Result<(&'a str, [u8; 32]), &'static str> {
    let read = |o: u32, b: &mut [u8]| match img.get(o as usize..o as usize + b.len()) {
        Some(s) => {
            b.copy_from_slice(s);
            true
        }
        None => false,
    };
    let at = image::digest_offset(read, img.len() as u32).ok_or("not an ESP app image")? as usize;
    if at + 32 != img.len() {
        return Err("bytes after the image's end");
    }
    let digest: [u8; 32] = Sha256::digest(&img[..at]).into();
    if digest[..] != img[at..] {
        return Err("the image is damaged: its SHA-256 doesn't match");
    }
    let (version, p) = image::app_desc(img).ok_or("no app description")?;
    if p != project {
        return Err(if project == image::NODE_PROJECT {
            "that isn't node firmware"
        } else {
            "that isn't gateway firmware"
        });
    }
    Ok((version, digest))
}

/// A node image to roll out, and which nodes `to` names.
fn check(img: &[u8], ids: &[[u8; 6]]) -> Result<(Rollout, [u8; 32]), &'static str> {
    let (version, digest) = check_image(img, image::NODE_PROJECT)?;
    let fw = image::build_id(version, &digest);
    Ok((
        Rollout::new(fw, img.len() as u32, ids),
        Sha256::digest(img).into(),
    ))
}

/// The nodes `?to=` names: `all` for every node the page knows, or IDs.
fn targets(query: &[u8]) -> Result<([[u8; 6]; web::MAX_NODES], usize), &'static str> {
    let mut b = [0u8; 128];
    let to = web::form_value(query, "to", &mut b).ok_or("say which nodes: ?to=all or ?to=id,id")?;
    let mut ids = [[0u8; 6]; web::MAX_NODES];
    let mut n = 0;
    if to == "all" {
        let (settings, nodes) = UI.lock(|u| *u.borrow());
        let (known, k) = web::known_ids(&settings, &nodes);
        for id in &known[..k.min(web::MAX_NODES)] {
            ids[n] = *id;
            n += 1;
        }
    } else {
        for id in to.split(',').filter(|i| !i.is_empty()) {
            if n == web::MAX_NODES {
                break;
            }
            ids[n] = id
                .as_bytes()
                .try_into()
                .ok()
                .filter(|i: &[u8; 6]| i.iter().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
                .ok_or("node IDs are 6 lowercase hex digits")?;
            n += 1;
        }
    }
    if n == 0 {
        return Err("no nodes to update");
    }
    Ok((ids, n))
}

/// Refuses an upload that can't be one before the image space, and any rollout in it, is touched.
fn fits(len: usize) -> Result<(), &'static str> {
    let room = IMAGE.lock(|i| i.borrow().buf.as_ref().map(|b| b.len()));
    match room {
        None => Err("another upload is running"),
        Some(_) if len == 0 => Err("no image: send it as the body"),
        Some(r) if len > r => Err("too big for the gateway's image space"),
        Some(_) => Ok(()),
    }
}

/// `POST /update/gateway`: gateway firmware as the raw body. Written to the other slot, checked
/// there, and booted on trial after the response. Returns the response and whether to restart.
async fn self_update(
    sock: &mut TcpSocket<'_>,
    early: &[u8],
    len: usize,
) -> (alloc::string::String, bool) {
    if let Err(e) = fits(len) {
        return (
            text("400 Bad Request", &alloc::format!("Refused: {e}")),
            false,
        );
    }
    let Some(img) = IMAGE.lock(|i| {
        let mut i = i.borrow_mut();
        i.rollout = None; // the image space is shared with node rollouts
        i.buf.take()
    }) else {
        return (text("409 Conflict", "Another upload is running."), false);
    };
    let res = match receive(sock, img, early, len).await {
        Ok(()) => install(&img[..len]).await,
        Err(e) => Err(e),
    };
    IMAGE.lock(|i| i.borrow_mut().buf = Some(img));
    match res {
        Ok(fw) => {
            let fw = core::str::from_utf8(&fw).unwrap_or("?");
            info!("gateway update {fw}: written, restarting");
            let msg = alloc::format!(
                "Wrote {fw}. Restarting into it once QuestDB has what the gateway holds."
            );
            (text("200 OK", &msg), true)
        }
        Err(e) => {
            warn!("gateway update refused: {e}");
            (
                text("400 Bad Request", &alloc::format!("Refused: {e}")),
                false,
            )
        }
    }
}

/// Writes a gateway image to the slot we didn't boot from and points otadata at it. Returns its
/// build ID.
async fn install(img: &[u8]) -> Result<[u8; 12], &'static str> {
    let (version, digest) = check_image(img, GATEWAY_PROJECT)?;
    let slot = with_flash(flash::spare)
        .flatten()
        .ok_or("can't find the other app slot")?;
    if img.len() as u32 > slot.len {
        return Err("bigger than an app slot");
    }
    for (i, sector) in img.chunks(flash::SECTOR as usize).enumerate() {
        let at = slot.offset + i as u32 * flash::SECTOR;
        with_flash(|f| f.write(at, sector))
            .and_then(|r| r.ok())
            .ok_or("flash write")?;
        Timer::after_millis(1).await; // let ESP-NOW and the network run between sectors
    }
    with_flash(|f| flash::verify(f, slot, GATEWAY_PROJECT)).ok_or("no flash")??;
    with_flash(|f| flash::boot_next(f, slot)).ok_or("no flash")??;
    Ok(image::build_id(version, &digest))
}
