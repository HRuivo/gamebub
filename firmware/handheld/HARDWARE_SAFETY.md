# Hardware Safety Gate

Use this gate after the automated `post_change_check.py` pass and before any
firmware is flashed or run on a handheld. The automated pass is intentionally
non-flashing and cannot prove that a change is electrically safe.

## Manual Review

- Confirm the Cargo revision feature matches the physical PCB revision and the
  FPGA bitstream target. Never defeat the boot-time eFuse revision check.
- Compare every changed GPIO number, direction, pull, drive strength, and
  active-high/active-low assumption with the schematic and component datasheet.
  Check reset and boot states as well as the steady state.
- Review power-rail enables, charger/fuel-gauge writes, display/backlight PWM,
  audio gain, and DAC values against absolute maximum ratings and board limits.
- Verify SPI/I2C frequency, mode, register addresses, transaction widths,
  delays, and initialization order against both ends of the interface.
- Verify FPGA addresses and commands against `../../fpga`. Custom-core metadata
  may use aligned 32-bit addresses below `0xF000_0000`; the `0xFxxx_xxxx` range
  belongs to the handheld FPGA framework.
- For flash-layout changes, prove that partitions are aligned, non-overlapping,
  within 8 MiB, and consistent with DFU and UF2 tooling. Preserve a known-good
  recovery image.
- Review new `unsafe`, interrupt, concurrency, and buffer-length code for races,
  lifetime violations, out-of-bounds access, and blocking in interrupt context.

## First Hardware Test

- Start with a recoverable device and a known-good firmware image available.
- Use a current-limited bench supply when the board setup permits it. Begin at a
  conservative current limit and watch current draw, voltage rails, and device
  temperature during boot.
- Keep external peripherals and cartridges disconnected unless they are needed
  for the test. Avoid two independently powered connections that can back-feed
  a rail.
- Capture the serial log from power-on. Stop immediately on unexpected current,
  heat, odor, rail voltage, repeated resets, display artifacts, or bus errors.
- Exercise the changed function narrowly before a broader regression test.
  Record the board revision, firmware commit/diff, supply conditions, and result.

Physical testing is complete only when these checks were actually performed.
A successful compile or emulator/simulator run is not hardware validation.
