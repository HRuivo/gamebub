# Game Bub Handheld Firmware

This directory contains the main ESP32-S3 firmware for Game Bub, an FPGA-based
Game Boy / Game Boy Color / Game Boy Advance handheld. The ESP32-S3 owns the UI,
storage, USB, power management, and FPGA configuration/control. Console
emulation and most latency-sensitive I/O run in the Xilinx FPGA.

## Start Here

- Read `../../docs/firmware.md` for toolchain, build, flash, and UF2 instructions.
- Read `../../README.md` for repository-level context.
- `Cargo.toml`, `.cargo/config.toml`, `sdkconfig.defaults`, and `partitions.csv`
  are the authoritative build and target configuration.
- The target is `xtensa-esp32s3-espidf`, using the Espressif Rust toolchain,
  ESP-IDF v5.4.3, and Rust 1.93. A normal host Rust toolchain is insufficient.
- Always select exactly one board feature: `rev1`, `rev2`, `rev3`, or `rev4`.
  The firmware checks the compiled revision against the hardware eFuse at boot.

## Common Commands

Run commands from this directory unless noted otherwise.

```sh
# Format/check formatting (does not require hardware)
cargo fmt
cargo fmt --check

# Build or check revision 4; substitute the actual target revision
cargo check --features rev4
cargo build --release --features rev4

# Build, flash, and open the serial monitor
cargo run --release --features rev4

# Reproducible rev4 release bundle (UF2, ELF, logs, source snapshot)
./build_firmware.sh [BUILD_LABEL]
```

`build_firmware.sh` has stricter prerequisites than a regular Cargo build. It
expects the pinned compiler/LLVM combination, an ESP-IDF installation, and all
required files in `system-data/`; see `./build_firmware.sh --help`.

There is no useful host test suite or standard Cargo test harness for this
binary (`harness = false`). Prefer `cargo fmt --check` plus `cargo check` or a
release build for the relevant revisions. Hardware-facing behavior must be
verified on the matching handheld. Do not claim hardware validation unless it
was actually performed.

## Architecture Map

- `src/main.rs`: startup order. It initializes logging/crash persistence, USB,
  NVS/KVS, the hardware singleton, background workers, power management, the
  boot FPGA image, and finally the UI event loop.
- `src/device/`: board support and physical drivers. `Device` is the global
  mutex-protected hardware container. Revision-specific pin maps and driver
  selection live in `device/mod.rs`; low-level LCD, FPGA, SD, USB, battery,
  audio, RTC, and sensor drivers live under `device/drivers/`.
- `src/bitstream/`: FPGA programming and built-in GB/GBC/GBA core integration.
  `boot.rs` describes the boot-core register interface. `gameboy/` and `gba/`
  handle ROM/save/RTC configuration and FPGA protocol details.
- `src/core/`: generic SD-card core discovery and runtime management. It reads
  core metadata/settings, stages files, programs the FPGA, and exchanges core
  commands. This is distinct from the built-in legacy GB/GBA bitstream layer.
- `src/ui/`: Rust side of the Slint UI. `ui/mod.rs` owns the main-thread event
  loop and line renderer; `ui/state/` binds application behavior to generated
  Slint components; `ui/buttons.rs` translates physical input.
- `res/ui/`: Slint markup. `main.slint` is the root; reusable controls are in
  `components/`, and feature screens are in `screens/`. `build.rs` compiles and
  embeds these assets using the software renderer and defaults to the `cosmic`
  style because the Xtensa backend miscompiles animations in the fluent style.
- `src/worker/`: the background message loop for blocking or hardware-heavy
  work. UI callbacks should enqueue work here rather than block rendering.
- `src/input/`: internal and external gamepad state/management.
- `src/power.rs`, `src/led.rs`: long-lived power and status-LED controllers.
- `src/kvs/`: typed cached ESP NVS settings. Define durable settings in
  `kvs/keys.rs` and include new keys in `flush_all()` when appropriate.
- `src/control/`, `src/cart_backup.rs`: vendor USB control requests and the
  cartridge-backup CDC mode.
- `src/crash_handler.rs`: captures panic information and persists it after
  reboot once storage is available.
- `generate_firmware_uf2.py`: packages the application and read-only FAT system
  data into an update UF2 using offsets from `partitions.csv`.

## Runtime and Concurrency Model

- The initial ESP-IDF task becomes the Slint/UI loop and is raised to FreeRTOS
  priority 10. Keep it responsive.
- `worker::send` queues blocking work to the named worker thread.
- `ui::send` queues state changes back to the UI thread. Do not mutate Slint UI
  objects from background threads.
- Hardware access normally goes through `Device::lock()`. Keep lock lifetimes
  short, do not hold the device mutex while waiting for UI/worker messages, and
  preserve startup ordering: `Device::init()` must precede `Device::lock()`.
- Interrupt handlers should defer substantial work through messages; avoid
  allocation, blocking I/O, or lengthy device operations in interrupt context.
- The project deliberately uses `std` on ESP-IDF. Do not assume desktop OS
  facilities or abundant stack/heap merely because `std` is available.

## Hardware Revisions

Cargo revision features select both board pins and component drivers:

| Feature | Display | Fuel gauge | Other distinction |
| --- | --- | --- | --- |
| `rev1` | ILI9488 | MAX17048 | TCA9535 I/O expander |
| `rev2` | ILI9488 | MAX17048 | TCA9535 I/O expander |
| `rev3` | ST7262 | BQ27427 | direct board wiring |
| `rev4` | ILI9806E | BQ27427 | direct board wiring |

When changing pins, displays, power, storage, FPGA signaling, or feature-gated
types, inspect every revision branch in `src/device/mod.rs` and compile all
affected revisions. Never silently use rev4 behavior as a universal default.

## Filesystem and Packaging Contracts

- The SD card is mounted at `/sdcard`. The embedded read-only FAT partition is
  mounted at `/system`.
- `util::get_system_file_path()` intentionally lets `/sdcard/system/<name>`
  override `/system/<name>`.
- Built-in compressed FPGA images use exact names: `boot.bit.hs`,
  `gameboy.bit.hs`, and `gba.bit.hs`.
- BIOS and utility firmware filenames are also runtime contracts; search their
  call sites before renaming anything under `system-data/`.
- The 8 MiB flash layout and UF2 offsets come from `partitions.csv`. Changes to
  partition names, offsets, or sizes must be reconciled with the DFU firmware,
  flashing scripts, and UF2 generator.
- `generate_firmware_uf2.py --hw-revision`, the Cargo revision feature, and the
  FPGA bitstream target must all describe the same board revision.

## Change Guidelines

- Preserve the existing user's work. This repository may contain modified or
  generated artifacts; inspect `git status` and do not clean or rewrite
  unrelated files.
- Follow existing Rust style and run `cargo fmt` after Rust edits. Prefer the
  established `anyhow` context at orchestration boundaries and typed errors in
  reusable drivers/protocol code.
- Treat register addresses, SPI command formats, GPIO polarity, delays,
  partition offsets, and USB descriptors as hardware/protocol contracts. Find
  the matching FPGA, host, or board definition before changing them.
- Avoid `unwrap()` in new recoverable hardware or filesystem paths. Existing
  initialization invariants sometimes use it intentionally; do not broaden
  that pattern without a clear invariant.
- Keep unsafe code narrowly scoped and document the representation or lifetime
  invariant it relies on.
- For a UI feature, check both halves: Slint declarations under `res/ui/` and
  Rust bindings/callbacks under `src/ui/state/`. Long operations belong in the
  worker queue, with progress/results sent back through `ui::Message`.
- For a persistent setting, update the KVS key, UI model/callbacks, runtime
  consumer, and reset/flush behavior together.
- For an FPGA-facing feature, verify firmware register/command constants
  against the FPGA source in `../../fpga`; do not infer a protocol from one side.

## Verification Expectations

Choose checks proportional to the change:

1. Run `cargo fmt --check` for Rust changes.
2. Run `cargo check --features <revision>` (or a release build) for every
   affected board revision. Cross-compilation may require the developer's
   installed esp-rs/ESP-IDF environment and network-populated dependency cache.
3. For Slint changes, a firmware build is the syntax/type check because the UI
   is compiled by `build.rs`.
4. For packaging changes, use a disposable output path and verify partition
   bounds and metadata; do not flash hardware unless explicitly requested.
5. Report exactly what ran, what could not run, and whether testing was compile
   only or performed on physical hardware.

