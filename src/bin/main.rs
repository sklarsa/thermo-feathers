#![no_std]
#![no_main]

//! Feather ESP32-S3 room node: SHT41 temperature and humidity -> QuestDB over QWP. See SPEC.md.
//! Scaffold only so far: boots at 80 MHz and logs.

use esp_backtrace as _;
use esp_hal::{clock::CpuClock, delay::Delay, main};
use log::info;

esp_bootloader_esp_idf::esp_app_desc!();

#[main]
fn main() -> ! {
    esp_println::logger::init_logger_from_env();
    let _peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::_80MHz));
    info!("thermo-feathers {}", env!("CARGO_PKG_VERSION"));
    let delay = Delay::new();
    loop {
        delay.delay_millis(60_000);
        info!("tick");
    }
}
