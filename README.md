# justicetattoo

ESP32 firmware, written in Rust, that plays a short looping "movie" on a
transparent OLED — and lets you **change the movie from a phone, over Wi-Fi, with
no app, no cable, and no tools**.

The device hosts its own Wi-Fi network and a web page. Drop a GIF onto the page;
your browser converts it to display-ready frames and sends them to the device,
which stores them in flash and plays them. The MCU itself never decodes a GIF —
it only blits pre-baked 1-bit frames.

## How it works

```
        ┌─ your phone / laptop ───────────────┐        ┌─ ESP32 ───────────────┐
.gif ─▶ │ browser: decode GIF, stretch to     │ ─Wi-Fi▶ │ movie flash partition │ ─▶ OLED
        │ 128×64, Bayer-dither, pack → JTM1    │  POST  │  → blit frame, repeat │
        └─────────────────────────────────────┘        └───────────────────────┘
```

- **Upload.** The ESP32 runs a Wi-Fi access point and an HTTP server. The page
  (`web/index.html`) decodes the GIF with a small built-in JS decoder,
  dithers and packs every frame, and POSTs a single **JTM1** blob. The device
  streams it into a dedicated 2 MB flash partition and reboots into it.
- **Default movie.** `assets/movie.gif` is converted at *compile time* by
  `build.rs` and baked into the firmware, so a freshly flashed device plays
  something immediately — and falls back to it if the partition is empty.
- **The device stays dumb.** No GIF decoder, no image processing on the MCU —
  conversion happens in the browser, which has the RAM and a decoder already.

## Hardware

| Item | Role |
|------|------|
| ESP32 DevKit V1 (WROOM-32) | MCU, on-board USB-UART for flashing |
| Waveshare 1.51" transparent OLED, 4-wire SPI | SSD1309 driver, 128×64 monochrome |

Wiring (OLED → ESP32):

| OLED | DIN/MOSI | CLK/SCK | CS | DC | RST | VCC | GND |
|------|----------|---------|----|----|----|-----|-----|
| GPIO | 23       | 18      | 5  | 16 | 17 | 3V3 | GND |

## Loading a movie

### Over Wi-Fi — the normal way

1. Join the **`Tattoo http://192.168.71.1`** Wi-Fi network (password
   `justicetattoo`) — the network name itself is the URL to open.
2. The upload page opens automatically (captive portal); if not, browse to the
   URL in the network name, `http://192.168.71.1/`.
3. Drop in a `.gif`, check the preview, and **Send to device**.
4. The device stores it and reboots into the new movie.

No Rust, no toolchain, no cable — just a browser.

### The baked-in default

Replace `assets/movie.gif` and rebuild + reflash. `build.rs` re-runs
automatically, converts the GIF, and stores it raw or XOR-delta + RLE compressed
(whichever is smaller). Keep it short — it rides inside the firmware.

## Building & flashing

The ESP32 is an Xtensa core and needs Espressif's Rust fork. Full, verified
setup is in **[SETUP.md](./SETUP.md)**; the short version:

```bash
cargo install espup espflash ldproxy --locked
espup install --targets esp32

. $HOME/export-esp.sh    # load the ESP toolchain — required in every new shell
cargo run                # build + flash + open the serial monitor
```

The **first build is slow** (~10–20 min) while `esp-idf-sys` compiles ESP-IDF
v5.3.3; it is cached afterwards.

## Layout

```
build.rs              GIF → default_movie.bin (JTM1), then ESP-IDF link setup
partitions.csv        custom flash table: 1.875 MB app + 2 MB movie partition
web/index.html        browser upload page: GIF → JTM1 converter
src/main.rs           bring-up + playback loop
src/ssd1309.rs        minimal SSD1309 driver (reset, init, full-frame blit)
src/movie.rs          JTM1 parsing + movie-partition I/O
src/net.rs            Wi-Fi SoftAP + HTTP upload server + captive-portal DNS
assets/movie.gif      the default (baked-in) movie
```

## Docs

- **[PLAN.md](./PLAN.md)** — design decisions and rationale: the JTM1 format, the
  SoftAP/browser-conversion architecture, the flash budget, delta+RLE.
- **[SETUP.md](./SETUP.md)** — repeatable setup, build, flash, JTAG, and a
  troubleshooting table.
