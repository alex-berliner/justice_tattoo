//! justicetattoo - plays a looping movie on an SSD1309 OLED, with wireless
//! movie upload over a self-hosted Wi-Fi access point.
//!
//! Wiring (ESP32 DevKit V1 -> Waveshare 1.51" transparent OLED, 4-wire SPI):
//!   CLK/SCK = GPIO18   DIN/MOSI = GPIO23   CS = GPIO5   DC = GPIO16   RST = GPIO17
//!   VCC = 3V3          GND = GND
//!
//! The movie is a JTM1 blob (see PLAN.md §4): either one uploaded over Wi-Fi
//! into the `movie` flash partition, or the default baked in by `build.rs`. The
//! device hosts a SoftAP and a browser upload page - see the `net` module.

mod movie;
mod net;
mod ssd1309;

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::delay::{Ets, FreeRtos};
use esp_idf_svc::hal::gpio::{AnyIOPin, PinDriver};
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::hal::prelude::*;
use esp_idf_svc::hal::spi::{config::Config, SpiDeviceDriver, SpiDriver, SpiDriverConfig};
use esp_idf_svc::nvs::EspDefaultNvsPartition;

use movie::Movie;
use ssd1309::{Ssd1309, FRAME_BYTES};

fn main() -> anyhow::Result<()> {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    log::info!("justicetattoo starting");

    let peripherals = Peripherals::take()?;

    // --- Display: VSPI (spi2), SCLK = GPIO18, MOSI = GPIO23, no MISO line. ---
    let spi_driver = SpiDriver::new(
        peripherals.spi2,
        peripherals.pins.gpio18,
        peripherals.pins.gpio23,
        None::<AnyIOPin>,
        &SpiDriverConfig::new(),
    )?;
    let config = Config::new().baudrate(10.MHz().into());
    let spi = SpiDeviceDriver::new(spi_driver, Some(peripherals.pins.gpio5), &config)?;
    let dc = PinDriver::output(peripherals.pins.gpio16)?;
    let rst = PinDriver::output(peripherals.pins.gpio17)?;

    let mut display = Ssd1309::new(spi, dc, rst);
    display.init(&mut Ets)?;
    log::info!("display initialised");

    // --- Networking: SoftAP + upload server. Playback continues if it fails. ---
    let sysloop = EspSystemEventLoop::take()?;
    let nvs = EspDefaultNvsPartition::take()?;
    let _net = match net::start(peripherals.modem, sysloop, nvs) {
        Ok(net) => Some(net),
        Err(e) => {
            log::error!("networking failed to start ({e}); wireless upload disabled");
            None
        }
    };

    // --- Movie: the uploaded blob in flash, or the baked-in default. ---
    let movie = Movie::load();
    let mut framebuf = [0u8; FRAME_BYTES];
    let mut scratch = vec![0u8; movie.max_frame_len()];
    log::info!("starting playback: {} frames", movie.frame_count());

    loop {
        for i in 0..movie.frame_count() {
            if let Err(e) = movie.render_frame(i, &mut framebuf, &mut scratch) {
                log::warn!("frame {i} decode failed: {e}");
                continue;
            }
            display.blit(&framebuf)?;
            FreeRtos::delay_ms(movie.delay_ms(i));
        }
    }
}
