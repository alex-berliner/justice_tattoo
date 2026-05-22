# justicetattoo — Project Plan & Context

> Captured 2026-05-22. This file is the single source of truth for the project's
> design decisions and setup procedure. It is written before any setup work so
> the effort is repeatable and resumable.

## 1. Goal

A Rust project for the ESP32 that drives a transparent OLED to play a short,
seamlessly repeating "movie". The movie is supplied as a `.gif` in the project
directory and converted to display-ready frames **at compile time** by a build
script — the device itself only blits pre-baked frames.

## 2. Hardware inventory

| Item | Role | Notes |
|------|------|-------|
| ESP32 DevKit V1 | MCU board | WROOM-32, Xtensa LX6, ~4 MB flash. On-board CP2102 USB-UART for flashing. |
| ESP-Prog | Debug probe | USB JTAG + spare UART. Optional — enables `probe-rs` breakpoints / RTT logging. |
| E308847F-D OLED panel | Display panel | The transparent OLED glass itself. |
| Waveshare 1.51" transparent OLED driver board | Display driver | Carries the **SSD1309** controller. Configured in **4-wire SPI** mode. |

**Display facts that drive the design:** SSD1309 controller, **128×64**,
**monochrome** (transparent light-blue). Native framebuffer = 8 pages × 128
columns, vertical bit-packing → **1024 bytes per full frame**. A 4 MB flash
therefore holds thousands of frames; delta+RLE compression (§9) stretches that further.

### Suggested wiring (ESP32 VSPI defaults)

| OLED pin | ESP32 GPIO |
|----------|-----------|
| VCC      | 3V3 |
| GND      | GND |
| DIN/MOSI | GPIO23 |
| CLK/SCK  | GPIO18 |
| CS       | GPIO5 |
| DC       | GPIO16 |
| RST      | GPIO17 |

ESP-Prog JTAG (optional): MTDI=GPIO12, MTCK=GPIO13, MTMS=GPIO14, MTDO=GPIO15, + GND.
GPIO12 is the flash-voltage strapping pin — with JTAG connected it must not strap
the flash to 1.8 V. The board's flash-voltage eFuse was burned to 3.3 V on
2026-05-22 (one-time, irreversible), so JTAG can stay connected. See SETUP.md §9.

## 3. Locked-in design decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Rust runtime | **`std` + `esp-idf-hal`** (via `esp-idf-svc`) | Official esp-rs crate, mature. Leaves the door open to Wi-Fi/BLE/filesystem later. |
| 1-bit conversion | **Bayer / ordered dithering** (8×8) | Temporally stable — static areas stay still between frames; no shimmer/boil that error-diffusion causes on video. |
| Aspect handling | **Stretch** to exactly 128×64 | Decided by user. No bars, no cropping; accepts distortion. |
| Frame storage | **Raw or XOR-delta + RLE**, auto-selected | `build.rs` encodes both and keeps the smaller; a `FORMAT` byte tells the device which decoder to use. See §9. |

## 4. Architecture

### build.rs — host-side GIF → frames pipeline (runs on the dev machine)

1. `cargo:rerun-if-changed=assets/movie.gif`.
2. Decode `assets/movie.gif` with the `image` crate (`GifDecoder` →
   fully-composited RGBA frames + per-frame delays; handles GIF disposal).
3. Per frame: `resize_exact(128, 64)` (stretch) → luma → **8×8 Bayer ordered
   dither** to 1-bit → pack into native SSD1309 page layout (8 pages × 128 cols).
4. Encode all frames two ways — raw, and XOR-delta + RLE — and keep the smaller.
   Emit `OUT_DIR/frames.bin` (the chosen blob) and `OUT_DIR/meta.rs` with
   `FORMAT: u8` (0 = raw, 1 = delta+RLE), `FRAME_COUNT`, `FRAME_DELAYS_MS`, and
   `FRAME_OFFSETS` (per-frame byte ranges into the blob).
5. Then call `embuild::espidf::sysenv::output()` (the esp-idf-sys linker setup —
   the GIF pipeline and the IDF setup share one `build.rs`).

### Device side — src/main.rs

- `esp-idf-svc` init. `SpiDriver` + `SpiDeviceDriver` at ~10 MHz; `PinDriver` for
  DC and RST.
- Thin hand-written SSD1309 driver: ~20-command init (SSD1306-compatible set)
  with a proper RST low-pulse, plus a one-SPI-transaction full-frame blit.
- Playback loop: `render_frame(i)` decodes into one reusable 1 KB framebuffer —
  raw frames are copied in, delta+RLE frames apply an XOR delta in place (the
  buffer resets at the loop start). Then blit → sleep for the GIF frame delay
  (clamped to a ~30 ms minimum) → wrap around.

### Project layout

```
justicetattoo/
├── PLAN.md               # this file
├── SETUP.md              # step-by-step repeatable setup (written during setup)
├── Cargo.toml            # esp-idf-svc; [build-dependencies] image, embuild
├── build.rs              # GIF→frames.bin + meta.rs, then embuild output
├── rust-toolchain.toml   # channel = "esp"
├── sdkconfig.defaults    # flash size + large-app partition table
├── .cargo/config.toml    # target xtensa-esp32-espidf, build-std, espflash runner
├── assets/movie.gif      # the movie (placeholder until the real one is dropped in)
├── tools/                # make_placeholder_gif.py, rle_roundtrip.rs
└── src/
    ├── main.rs           # peripheral init, playback loop, delta+RLE decoder
    └── ssd1309.rs        # minimal SSD1309 driver (init + full-frame blit)
```

### Flash budget note

`include_bytes!` data lands in `.rodata` inside the **app partition**. `std` +
ESP-IDF already pushes the binary toward ~1 MB; embedded frames add on top.
The build uses ESP-IDF's built-in `SINGLE_APP_LARGE` table (~1.5 MB app) via
`sdkconfig.defaults`. A larger custom partition table is possible but needs
extra esp-idf-sys wiring — revisit only if a movie outgrows ~1.5 MB.

## 5. Environment baseline (this machine, 2026-05-22)

- Ubuntu 24.04.4 LTS, x86_64, ~751 GB free.
- Present: `rustc`/`cargo`/`rustup` 1.95, `python3` 3.12, `cmake` 3.28, `ninja`,
  `git`, `wget`, `curl`, `gcc`.
- Missing: `espup`, `espflash`, `ldproxy`, `pip3`, `dfu-util`.
- User `alex` is in `dialout` (serial flashing works without sudo) and `sudo`.

## 6. Setup plan (steps to execute)

1. **System deps (apt):** `python3-pip python3-venv libudev-dev pkg-config
   libusb-1.0-0 libusb-1.0-0-dev dfu-util flex bison gperf ccache libffi-dev
   libssl-dev ninja-build`.
2. **ESP Rust toolchain:** `cargo install espup` → `espup install` (fetches the
   Xtensa Rust fork + LLVM; writes `~/export-esp.sh`).
3. **Flashing tools:** `cargo install espflash ldproxy`.
4. **Scaffold** the project files listed in §4.
5. **Placeholder GIF** at `assets/movie.gif` so the pipeline has input.
6. **Build** for `xtensa-esp32-espidf` to validate the toolchain + pipeline
   (first build also compiles ESP-IDF — slow once, then cached).
7. **Write `SETUP.md`** documenting every step for a fresh machine.

## 7. Build, flash & monitor commands (once set up)

```bash
. $HOME/export-esp.sh        # put the ESP toolchain on PATH (per shell)
cargo build                  # debug build
cargo run                    # build + flash + serial monitor (espflash runner)
espflash monitor             # serial monitor only
```

## 8. Swapping in the real movie

Replace `assets/movie.gif`, then `cargo build`. `build.rs` re-runs automatically
(`rerun-if-changed`). Keep the GIF short; every frame costs 1 KB of flash.

## 9. Delta + RLE compression (implemented)

`build.rs` encodes the frames two ways and keeps the smaller:

- **Raw** — each frame verbatim, 1024 bytes.
- **XOR-delta + RLE** — frame 0 is XORed against zero, every later frame against
  its predecessor; the sparse delta is run-length encoded with 2-byte op headers
  (bit 15 = type: COPY of an unchanged run, or XOR of a literal run; bits 0..14 =
  run length).

The `FORMAT` byte in `meta.rs` records the choice; `main.rs` decodes both into
one reusable 1 KB framebuffer (`render_frame` / `apply_delta_rle`). For the
placeholder movie this is a ~7× win (36 KB raw → 5 KB). `tools/rle_roundtrip.rs`
is a host-side test proving the encoder and decoder agree (1904 cases).

Possible future step: a varint length field, or periodic keyframes for very long
movies — not needed yet.
