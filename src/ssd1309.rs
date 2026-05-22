//! Minimal SSD1309 OLED driver: hardware reset, init, and full-frame blit.
//!
//! Written against `embedded-hal` 1.0 traits so it stays decoupled from the
//! esp-idf HAL. The panel is a 128x64 monochrome OLED addressed in horizontal
//! mode: one SPI transaction pushes a whole 1024-byte page-packed framebuffer.

use core::fmt;

use embedded_hal::delay::DelayNs;
use embedded_hal::digital::OutputPin;
use embedded_hal::spi::SpiDevice;

/// 128 columns x 8 pages x 1 byte = 1024-byte framebuffer.
pub const FRAME_BYTES: usize = 1024;

#[derive(Debug)]
pub enum DisplayError {
    Spi,
    Pin,
}

impl fmt::Display for DisplayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DisplayError::Spi => write!(f, "SPI transfer failed"),
            DisplayError::Pin => write!(f, "GPIO write failed"),
        }
    }
}

impl std::error::Error for DisplayError {}

pub struct Ssd1309<SPI, DC, RST> {
    spi: SPI,
    dc: DC,
    rst: RST,
}

impl<SPI, DC, RST> Ssd1309<SPI, DC, RST>
where
    SPI: SpiDevice,
    DC: OutputPin,
    RST: OutputPin,
{
    pub fn new(spi: SPI, dc: DC, rst: RST) -> Self {
        Self { spi, dc, rst }
    }

    /// Send command bytes (D/C low).
    fn cmd(&mut self, bytes: &[u8]) -> Result<(), DisplayError> {
        self.dc.set_low().map_err(|_| DisplayError::Pin)?;
        self.spi.write(bytes).map_err(|_| DisplayError::Spi)
    }

    /// Send data bytes (D/C high).
    fn data(&mut self, bytes: &[u8]) -> Result<(), DisplayError> {
        self.dc.set_high().map_err(|_| DisplayError::Pin)?;
        self.spi.write(bytes).map_err(|_| DisplayError::Spi)
    }

    /// Pulse RST low, then load a 128x64 init sequence. The SSD1309 needs a
    /// proper reset pulse before init - unlike the SSD1306 it is not optional.
    pub fn init<D: DelayNs>(&mut self, delay: &mut D) -> Result<(), DisplayError> {
        self.rst.set_high().map_err(|_| DisplayError::Pin)?;
        delay.delay_ms(1);
        self.rst.set_low().map_err(|_| DisplayError::Pin)?;
        delay.delay_ms(10);
        self.rst.set_high().map_err(|_| DisplayError::Pin)?;
        delay.delay_ms(10);

        self.cmd(&[
            0xAE, // display off
            0xD5, 0x80, // clock divide ratio / oscillator frequency
            0xA8, 0x3F, // multiplex ratio = 63 (64 rows)
            0xD3, 0x00, // display offset = 0
            0x40, // display start line = 0
            0xA1, // segment remap: column 127 -> SEG0
            0xC8, // COM output scan direction remapped
            0xDA, 0x12, // COM pins hardware configuration
            0x81, 0x7F, // contrast
            0xD9, 0xF1, // pre-charge period
            0xDB, 0x40, // VCOMH deselect level
            0x20, 0x00, // memory addressing mode = horizontal
            0xA4, // output follows RAM content
            0xA6, // normal display (not inverted)
            0xAF, // display on
        ])
    }

    /// Push a full 1024-byte page-packed framebuffer to the panel.
    pub fn blit(&mut self, frame: &[u8]) -> Result<(), DisplayError> {
        debug_assert_eq!(frame.len(), FRAME_BYTES, "frame must be a full page-packed buffer");
        self.cmd(&[0x21, 0x00, 0x7F])?; // column address range 0..=127
        self.cmd(&[0x22, 0x00, 0x07])?; // page address range 0..=7
        self.data(frame)
    }
}
