# justicetattoo — Setup & Build Guide

Repeatable setup for building and flashing the ESP32 OLED movie player.
Design rationale lives in [`PLAN.md`](./PLAN.md); this file is the procedure.

Verified on **Ubuntu 24.04.4 LTS, x86_64**, 2026-05-22.

---

## 0. Hardware & wiring

| OLED pin (Waveshare 1.51" transparent, 4-wire SPI) | ESP32 DevKit V1 |
|----------------------------------------------------|-----------------|
| VCC                                                | 3V3 |
| GND                                                | GND |
| DIN / MOSI                                         | GPIO23 |
| CLK / SCK                                          | GPIO18 |
| CS                                                 | GPIO5 |
| DC                                                 | GPIO16 |
| RST                                                | GPIO17 |

Confirm the Waveshare driver board is jumpered for **4-wire SPI** (its default
resistor/jumper config). Flashing uses the DevKit's on-board USB-UART — the
ESP-Prog is optional (JTAG debugging only).

---

## 1. System dependencies (apt)

```bash
sudo apt-get update
sudo apt-get install -y \
  python3-pip python3-venv libudev-dev pkg-config \
  libusb-1.0-0 libusb-1.0-0-dev dfu-util \
  flex bison gperf ccache libffi-dev libssl-dev ninja-build \
  python3-pil
```

`python3-pil` (Pillow) is only needed to regenerate the placeholder GIF.
`git`, `wget`, `curl`, `cmake`, `gcc`, `python3` are assumed already present.

---

## 2. ESP Rust toolchain

The ESP32 is an Xtensa core, which needs Espressif's Rust fork — not stock
nightly. `espup` installs it.

```bash
cargo install espup --locked
espup install --targets esp32
```

This creates:
- the `esp` rustup toolchain (`~/.rustup/toolchains/esp`),
- the Xtensa GCC + LLVM,
- `~/export-esp.sh` — environment variables the build needs.

---

## 3. Flashing tools

```bash
cargo install espflash ldproxy --locked
```

- `espflash` — flashes and serial-monitors the ESP32.
- `ldproxy` — linker shim required by the `xtensa-esp32-espidf` target.
  (Running `ldproxy` directly panics — that is normal; it is not a CLI.)

---

## 4. Per-shell environment

**Every new terminal** that builds this project must first load the ESP
environment:

```bash
. $HOME/export-esp.sh
```

It sets `LIBCLANG_PATH` (for `esp-idf-sys`'s bindgen) and prepends the Xtensa
GCC to `PATH`. To make it automatic, append that line to `~/.bashrc`.

---

## 5. Build

```bash
cd /home/alex/Code/justicetattoo
. $HOME/export-esp.sh
cargo build
```

**The first build is slow (~10–20 min):** `esp-idf-sys` clones ESP-IDF
`v5.3.3` (set in `.cargo/config.toml`), downloads the IDF tools, and compiles
the framework. Everything is cached afterwards — later builds take seconds.

`build.rs` runs on every build and prints a `cargo:warning` line reporting the
default movie's frame count and JTM1 blob size.

---

## 6. Flash & run

With the DevKit connected by USB:

```bash
. $HOME/export-esp.sh
cargo run            # build + flash + open serial monitor
```

`cargo run` uses the `espflash flash --monitor` runner from
`.cargo/config.toml`. Serial output (movie frame count, init log) appears in
the monitor. The user `alex` is already in the `dialout` group, so no `sudo` is
needed for serial access. Exit the monitor with `Ctrl+]` or `Ctrl+C`.

Monitor only, without rebuilding:

```bash
espflash monitor
```

---

## 7. Loading a movie

### Over Wi-Fi — the normal way (no tools, no cable)

The device hosts its own network and an upload page:

1. Join the **`JusticeTattoo 192.168.71.1`** Wi-Fi network (password
   `justicetattoo`) — the network name itself carries the address.
2. The upload page opens automatically (captive portal). If it does not, open
   `http://192.168.71.1/` in any browser.
3. Drop in a `.gif`. The browser converts it; check the preview and the
   capacity bar, then **Send to device**.
4. The device stores the movie in its `movie` flash partition and reboots into
   it. Rejoin the Wi-Fi to upload another.

The GIF is decoded and dithered entirely in the browser — see
[`web/index.html`](./web/index.html). Anyone can do this: no Rust toolchain, no
cable, just a modern browser.

### The baked-in default movie

`assets/movie.gif` is converted at compile time and baked into the firmware as
the default — what a freshly flashed device plays before any upload, and the
fallback if the `movie` partition is blank or corrupt.

1. Replace `assets/movie.gif` (any size — it is stretched to 128×64).
2. `cargo build` — `build.rs` re-runs automatically (`rerun-if-changed`) and
   emits a JTM1 blob, raw or XOR-delta + RLE, whichever is smaller.
3. Reflash. The default lives in the app partition, so keep it short.

Regenerate the placeholder GIF with `python3 tools/make_placeholder_gif.py`.

---

## 8. Project layout

```
justicetattoo/
├── PLAN.md               # design decisions & rationale
├── README.md             # overview
├── SETUP.md              # this file
├── Cargo.toml            # deps: esp-idf-svc, embedded-hal; build-deps: image, embuild
├── build.rs              # GIF → default_movie.bin (JTM1), then ESP-IDF link setup
├── partitions.csv        # custom flash table: factory app + 2 MB movie partition
├── rust-toolchain.toml   # channel = "esp"
├── sdkconfig.defaults    # flash size, custom partition table, task stack
├── .cargo/config.toml    # target, linker, runner, build-std, ESP_IDF_VERSION
├── assets/movie.gif      # the default (baked-in) movie
├── web/index.html        # the browser upload page + GIF→JTM1 converter
├── tools/
│   ├── make_placeholder_gif.py
│   └── rle_roundtrip.rs  # host test: delta+RLE encoder vs decoder
└── src/
    ├── main.rs           # bring-up + playback loop
    ├── ssd1309.rs        # minimal SSD1309 driver (init + full-frame blit)
    ├── movie.rs          # JTM1 parsing + movie-partition I/O
    └── net.rs            # Wi-Fi SoftAP + HTTP upload server + captive DNS
```

Generated at build time (not committed): `target/`, and
`OUT_DIR/default_movie.bin` — the baked-in default movie as a JTM1 blob.

---

## 9. ESP-Prog JTAG — and the flash-voltage eFuse

The ESP-Prog is optional: flashing and serial both use the DevKit's own USB.
But connecting the ESP-Prog **JTAG** ribbon to the ESP32 classic has a hardware
gotcha worth knowing.

JTAG's TDI line drives **GPIO12 (MTDI)**, which is also the **flash-voltage
strapping pin**. Held high at boot it straps VDD_SDIO to 1.8 V — but the
DevKit's flash is 3.3 V. The flash then goes unreadable for both `espflash` and
the ESP32's own bootloader. Symptoms: `espflash` prints *"Failed to connect to
on-device flash"*; `esptool flash_id` reports `Manufacturer: ff` and
*"Flash voltage set by a strapping pin to 1.8 V"*.

**Fix — burn the flash-voltage eFuse, once per board (IRREVERSIBLE):**

```bash
. $HOME/export-esp.sh
VENV=$(echo "$PWD"/.embuild/espressif/python_env/*/)
"$VENV/bin/python" "$VENV/bin/espefuse.py" --port /dev/ttyUSB2 \
  set_flash_voltage 3.3V
```

This burns `XPD_SDIO_FORCE/REG/TIEH` so VDD_SDIO is permanently 3.3 V and GPIO12
is ignored — after which the JTAG ribbon can stay connected permanently. Verify
with `esptool.py ... flash_id`: it should report a real manufacturer and
*"Flash voltage set by eFuse to 3.3V"*.

> Already done on the current board (MAC `70:4b:ca:25:d3:78`) on 2026-05-22 —
> do not repeat it. Only needed once, and only per physical board.

JTAG wiring: ESP-Prog JTAG header → ESP32 MTDI=GPIO12, MTCK=GPIO13,
MTMS=GPIO14, MTDO=GPIO15, + GND. Drive it with `probe-rs`
(`cargo install probe-rs-tools`) for breakpoints and RTT logging.

---

## 10. Troubleshooting

| Symptom | Fix |
|---------|-----|
| `error: toolchain 'esp' is not installed` | Run `espup install --targets esp32`. |
| `libclang` / bindgen errors during build | `. $HOME/export-esp.sh` was not sourced in this shell. |
| `ldproxy: command not found` | `cargo install ldproxy`; ensure `~/.cargo/bin` is on `PATH`. |
| Permission denied on `/dev/ttyUSB*` | Confirm membership in `dialout`/`plugdev` (`groups`), then re-login. |
| `Failed to connect to on-device flash` | ESP-Prog JTAG straps flash to 1.8 V — burn the flash-voltage eFuse, see §9. |
| `espflash monitor`: *Failed to initialize input reader* | Monitor needs an interactive terminal; run it from a real shell, not a script. |
| App image too large to flash | The firmware outgrew the 1.875 MB `factory` partition — build with `--release`, or shrink the baked-in `assets/movie.gif`. |
| No `JusticeTattoo …` Wi-Fi network appears | Wi-Fi failed to start — check the serial monitor. The device still plays the default movie. |
| Upload page errors converting a GIF | The file isn't a valid GIF (e.g. a renamed WebP/APNG) — re-export it as a standard GIF. |
| Uploaded movie too big | The capacity bar on the page exceeded 100% — use a shorter GIF; the `movie` partition is ~2 MB. |
| First build seems hung | It is cloning + compiling ESP-IDF. Check progress: `tail -f /tmp/justicetattoo-build.log`. |
