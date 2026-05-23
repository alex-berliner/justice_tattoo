# justicetattoo — Project Plan & Context

> Captured 2026-05-22; revised the same day for the **Wi-Fi upload**
> architecture. This file is the single source of truth for the project's design
> decisions and setup procedure.

## 1. Goal

A Rust firmware for the ESP32 that drives a transparent OLED to play a short,
seamlessly repeating "movie".

The movie is **uploaded wirelessly from a browser**: the device hosts its own
Wi-Fi network and a web page; the browser converts a `.gif` into display-ready
frames and POSTs the result; the device stores it in a dedicated flash partition
and plays it. No router, no app, no PC toolchain, no reflashing to change movies.

A movie is also **baked in at compile time** as a default, so a freshly flashed
device plays something immediately — and so the device has a fallback if the
movie partition is blank or corrupt.

## 2. Hardware inventory

| Item | Role | Notes |
|------|------|-------|
| ESP32 DevKit V1 | MCU board | WROOM-32, Xtensa LX6, ~4 MB flash. On-board CP2102 USB-UART for flashing. |
| ESP-Prog | Debug probe | USB JTAG + spare UART. Optional — enables `probe-rs` breakpoints / RTT logging. |
| E308847F-D OLED panel | Display panel | The transparent OLED glass itself. |
| Waveshare 1.51" transparent OLED driver board | Display driver | Carries the **SSD1309** controller. Configured in **4-wire SPI** mode. |

**Display facts that drive the design:** SSD1309 controller, **128×64**,
**monochrome** (transparent light-blue). Native framebuffer = 8 pages × 128
columns, vertical bit-packing → **1024 bytes per full frame**.

### Wiring — OLED → ESP32 (ESP32 VSPI defaults)

| OLED pin | ESP32 GPIO |
|----------|-----------|
| VCC      | 3V3 |
| GND      | GND |
| DIN/MOSI | GPIO23 |
| CLK/SCK  | GPIO18 |
| CS       | GPIO5 |
| DC       | GPIO16 |
| RST      | GPIO17 |

### Wiring — ESP-Prog JTAG → ESP32 (optional, debug only)

| ESP-Prog | Signal | ESP32 |
|----------|--------|-------|
| pin 2 | TMS | GPIO14 |
| pin 4 | TCK | GPIO13 |
| pin 6 | TDO | GPIO15 |
| pin 8 | TDI | GPIO12 |
| pin 3 | GND | GND |
| pin 1 | VJTAG | leave unconnected |

GPIO12 is the flash-voltage strapping pin; the board's flash-voltage eFuse was
burned to 3.3 V so JTAG can stay connected. The ESP-Prog is **not needed for
normal use** — flashing is over the DevKit's own USB, and the friend who runs
this device never touches JTAG.

## 3. Locked-in design decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Rust runtime | **`std` + `esp-idf-hal`** (via `esp-idf-svc`) | Official esp-rs crate; brings Wi-Fi, HTTP server, NVS, sockets. |
| Movie delivery | **Wi-Fi SoftAP + browser upload** | No router and no app: the device *is* the access point. The end user needs zero tooling. |
| GIF → frames conversion | **In the browser** (a small built-in JS GIF decoder) | The WROOM-32 has ~200 KB usable heap — not enough to decode arbitrary GIFs. The browser has GBs of RAM; a vendored decoder keeps the page dependency-free and works in any browser. The device stays dumb. |
| Movie storage | Dedicated **`movie` flash partition** (~2 MB) | Decouples the movie from the firmware; updated over the air without reflashing. |
| Default movie | Baked in at compile time by `build.rs` | A fresh device plays immediately; also the fallback when the partition is blank/corrupt. |
| Blob format | **JTM1** — one format for baked *and* uploaded movies | `build.rs` and the browser both emit it; the device has a single decoder path. |
| 1-bit conversion | **8×8 Bayer ordered dithering** | Temporally stable — static areas stay still between frames; no shimmer/boil that error-diffusion causes on video. |
| Aspect handling | **Stretch** to exactly 128×64 | No bars, no cropping; accepts distortion. |
| Frame encoding | **Raw**, or **XOR-delta + RLE** | `build.rs` encodes both and keeps the smaller; the browser emits raw (simpler JS). A `FORMAT` byte in the blob tells the device which decoder to use. See §8. |

## 4. Architecture

### The JTM1 movie blob

One little-endian binary layout, produced by both `build.rs` (default movie) and
the browser (uploaded movie), consumed by one decoder on the device:

| Offset | Size | Field |
|--------|------|-------|
| 0 | 4 | magic = ASCII `"JTM1"` |
| 4 | 1 | `format`: 0 = raw, 1 = XOR-delta + RLE |
| 5 | 1 | reserved (0) |
| 6 | 2 | `frame_count` N (u16) |
| 8 | 2·N | `frame_delays_ms`: u16 each, clamped ≥ 30 ms |
| 8+2N | 4·(N+1) | `frame_offsets`: u32 each, into the data section; frame *i* = `data[off[i]..off[i+1]]` |
| 8+2N+4(N+1) | … | frame data, blobs back to back |

Raw frames are a verbatim 1024-byte page buffer. Delta+RLE frames are variable
length (see §8). The decoder is format-agnostic above the frame level.

### build.rs — host-side, compile time

1. `cargo:rerun-if-changed=assets/movie.gif`.
2. Decode `assets/movie.gif` with the `image` crate; stretch each frame to
   128×64; 8×8 Bayer dither to 1-bit; pack into the SSD1309 page layout.
3. Encode raw and delta+RLE; keep the smaller.
4. Emit `$OUT_DIR/default_movie.bin` — a complete **JTM1 blob**.
5. `embuild::espidf::sysenv::output()` for the ESP-IDF link step.

### Device firmware — src/

- **`main.rs`** — peripheral + service bring-up, then the playback loop.
- **`ssd1309.rs`** — minimal SSD1309 driver: reset, init, full-frame blit
  (unchanged from the playback-only design).
- **`movie.rs`** — JTM1 parsing; reads the `movie` partition on demand via
  `esp_partition_read` (small header in RAM, frame bytes read per frame);
  erases + writes it for uploads; picks the movie source (partition or default).
- **`net.rs`** — Wi-Fi SoftAP, the HTTP server, and the captive-portal DNS.

Boot sequence:

1. Take peripherals; bring up SPI + GPIO; reset and init the OLED.
2. NVS + system event loop; start Wi-Fi as a **SoftAP** — SSID
   `JusticeTattoo 192.168.71.1` (the name embeds the address to open), WPA2,
   channel 1, fixed gateway `192.168.71.1`.
3. Spawn the **captive-portal DNS** thread: a UDP socket on `:53` that answers
   every query with `192.168.71.1`, so joining the network auto-opens the page.
4. Start the **HTTP server**:
   - `GET /` → the upload page (served from flash, `include_str!`).
   - `POST /upload` → stream the request body straight into the `movie`
     partition (erase-on-demand, chunked write), then `esp_restart()` so
     playback re-reads the new movie cleanly.
   - catch-all `GET` → redirect to `/` (captive-portal nicety).
5. Select the movie: a valid JTM1 blob in the partition → read it; otherwise the
   baked-in default.
6. **Playback loop**: decode frame *i* into one reusable 1 KB framebuffer →
   blit in a single SPI transaction → sleep the frame delay → wrap around.

### Web — web/index.html

A single self-contained page — no build step, no external library, no CDN:

1. User drops a `.gif` onto the page.
2. A small built-in GIF decoder (LZW + frame compositing, handling disposal and
   transparency) decodes it; each frame is drawn to a 128×64 `<canvas>` (stretch).
3. JS applies the **same 8×8 Bayer dither** and SSD1309 page packing as
   `build.rs`, builds a **JTM1 raw blob**, and shows a preview + frame count.
4. If the frame count exceeds the partition budget it warns *before* sending.
5. `POST` the blob to `/upload`; the device stores it and reboots into it.

### Project layout

```
justicetattoo/
├── PLAN.md               # this file
├── README.md             # overview
├── SETUP.md              # repeatable setup, build, flash, troubleshooting
├── Cargo.toml            # esp-idf-svc (Wi-Fi/HTTP/NVS); build-deps image, embuild
├── build.rs              # GIF → default_movie.bin (JTM1), then embuild output
├── partitions.csv        # custom table: factory app + ~2 MB movie partition
├── sdkconfig.defaults    # 4 MB flash, custom partition table, task stack
├── rust-toolchain.toml   # channel = "esp"
├── .cargo/config.toml    # target xtensa-esp32-espidf, build-std, espflash runner
├── assets/movie.gif      # the default movie (baked in)
├── web/index.html        # the browser upload page + GIF→JTM1 converter
├── tools/                # make_placeholder_gif.py, rle_roundtrip.rs
└── src/
    ├── main.rs           # bring-up + playback loop
    ├── ssd1309.rs        # SSD1309 driver
    ├── movie.rs          # JTM1 parsing + movie-partition I/O
    └── net.rs            # Wi-Fi SoftAP + HTTP server + captive DNS
```

### Flash budget (4 MB)

| Partition | Type | Size | Notes |
|-----------|------|------|-------|
| nvs | data | 24 KB | Wi-Fi calibration / key-value store |
| phy_init | data | 4 KB | RF calibration |
| factory | app | **1.875 MB** | the firmware (`std` + IDF + Wi-Fi/HTTP) |
| movie | data | **2.06 MB** | the uploaded JTM1 blob — ≈ 2100 raw frames |

`partitions.csv` replaces ESP-IDF's built-in `SINGLE_APP_LARGE` table. The
default movie still lives in `.rodata` inside the app partition, so keep it
short; uploaded movies live in the dedicated `movie` partition.

## 5. Environment baseline (this machine, 2026-05-22)

- Ubuntu 24.04.4 LTS, x86_64.
- Present: `rustc`/`cargo`/`rustup` (nightly 1.95), `python3`, `cmake`, `ninja`,
  `git`, `gcc`, **`espup`**, **`espflash`**, **`ldproxy`**, `pip3`.
- ESP toolchain installed (`~/export-esp.sh`); ESP-IDF v5.3.3 fetched under
  `.embuild/`.
- User `alex` is in `dialout` (serial flashing works without sudo) and `sudo`.

## 6. Build, flash & monitor

```bash
. $HOME/export-esp.sh        # put the ESP toolchain on PATH (per shell)
cargo build                  # debug build
cargo run                    # build + flash + serial monitor (espflash runner)
espflash monitor             # serial monitor only
```

The first build compiles ESP-IDF and is slow; afterwards it is cached. Adding
Wi-Fi/HTTP and the custom partition table forces one ESP-IDF reconfigure.

## 7. Changing the movie

- **End user (the normal path):** join the `JusticeTattoo 192.168.71.1` network, the
  upload page opens, drop in a `.gif`. The device converts it in the browser,
  stores it, and reboots into it. No cable, no tools.
- **The baked-in default:** replace `assets/movie.gif`, then `cargo build` +
  reflash. `build.rs` re-runs automatically (`rerun-if-changed`).

## 8. Delta + RLE compression (implemented)

`build.rs` encodes the frames two ways and keeps the smaller:

- **Raw** — each frame verbatim, 1024 bytes.
- **XOR-delta + RLE** — frame 0 is XORed against zero, every later frame against
  its predecessor; the sparse delta is run-length encoded with 2-byte op headers
  (bit 15 = type: COPY of an unchanged run, or XOR of a literal run; bits 0..14
  = run length).

The `format` byte in the JTM1 header records the choice; `main.rs` decodes both
into one reusable 1 KB framebuffer. `tools/rle_roundtrip.rs` is a host-side test
proving the encoder and decoder agree. The browser uploader emits **raw** only —
a 2 MB partition holds ~2100 raw frames, plenty for a short loop, and it keeps
the JS converter simple.

## 9. Future work

- **Shared converter via WASM** — compile the `build.rs` pipeline to WebAssembly
  and run *that* in the upload page, so the dither/pack logic has one source of
  truth instead of a Rust copy and a JS copy.
- **Delta+RLE in the browser** — port the encoder to JS if movies start
  outgrowing the raw-frame budget.
- **OTA firmware updates** — a second app partition would allow updating the
  firmware itself over the same Wi-Fi page.
