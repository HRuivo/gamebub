package platform.handheld

import chisel3._
import chisel3.util._
import platform.handheld.display.DisplayDriverIO

object AdaptiveDpiDriver {
  case class Config(
    clockHz: Int,

    /**
     * Whether the vsync period can vary by frame
     *
     * Certain drivers don't allow the vsync period to vary on a frame-by-frame basis
     * (e.g. ST7262E43). If variable vsync isn't allowed, this will adjust the
     * frame period by freezing the dot clock temporarily, rather than by adding
     * additional vblank scanlines.
     */
    variableVsync: Boolean,

    /** Width of the active area (pixels) */
    hActive: Int,
    /** Minimum horizontal sync period (clocks) */
    hSyncMin: Int = 3,
    /** Minimum horizontal back porch (clocks) */
    hBackPorchMin: Int = 3,
    /** Minimum horizontal front porch (clocks) */
    hFrontPorchMin: Int = 3,
    /** Height of the active area (pixels) */
    vActive: Int,
    /** Length of the vertical sync period (lines) */
    vSyncMin: Int = 1,
    /** Length of the vertical back porch (lines) */
    vBackPorchMin: Int = 2,
    /** Length of the vertical front porch (lines) */
    vFrontPorchMin: Int = 2,
    /** Maximum length of the vertical front porch (lines) */
    vFrontPorchMax: Int = 255,
  ) {
    assert(hActive > 0)
    assert(hSyncMin > 0)
    assert(hBackPorchMin > 0)
    assert(hFrontPorchMin > 0)
    assert(vActive > 0)
    assert(vSyncMin > 0)
    assert(vBackPorchMin > 0)
    assert(vFrontPorchMin > 0)
  }
}

/**
 * Display Parallel Interface (DPI) driver.
 * Manages the control signals, caller sends the pixel data.
 *
 * Adaptive: will vary the framerate (slightly) to match the input frame rate.
 */
class AdaptiveDpiDriver(
  config: AdaptiveDpiDriver.Config,
  /** Typical source frame period (seconds)  */
  sourceFramePeriod: Double,
) extends Module {
  val io = IO(new DisplayDriverIO(config.hActive, config.vActive))

  val hActive = config.hActive
  val vActive = config.vActive

  val currentFrame = RegInit(0.U(1.W))
  /// Whether the display is currently synchronized with the render
  val regLocked = RegInit(true.B)
  val newFrameReady = io.lastRenderedFrame =/= currentFrame
  io.displayFrame := currentFrame

  val regHsync = RegInit(true.B)
  val regVsync = RegInit(true.B)
  val regActive = RegInit(false.B)
  io.signals.dotclk := clock
  io.signals.hsync := regHsync
  io.signals.vsync := regVsync
  io.signals.enable := regActive

  if (config.variableVsync) {
    doRegular()
  } else {
    doFrozen()
  }

  private def doRegular(): Unit = {
    // Strategy:
    // Make vsync and vbp as low as possible -- extend vfp if needed
    // Set things up such that VFP can extend the frame by about 1%

    // Calculate timing
    val vSync = config.vSyncMin
    val vBackPorch = config.vBackPorchMin
    val vFrontPorchMin = config.vFrontPorchMin
    val vFrontPorchMax = config.vFrontPorchMax
    val hSync = config.hSyncMin
    val hBackPorch = config.hBackPorchMin

    // With maximum vFrontPorch, target ~1.02x the sourceFramePeriod
    val totalHeightMin = vActive + vSync + vBackPorch + vFrontPorchMin
    val totalHeightMax = vActive + vSync + vBackPorch + vFrontPorchMax
    val frameCycles = config.clockHz * sourceFramePeriod
    val maxFrameCycles = 1.02 * config.clockHz * sourceFramePeriod
    val approxFrameWidth = (maxFrameCycles / totalHeightMax).round.toInt

    // TODO: unify ILI9488 and ILI9806E
    // ILI9488
    // "Recommendation: The porch number of VBP + VFP must be even."
    // val hFrontPorch = approxFrameWidth - (hActive + hSync + hBackPorch)
    // val totalWidth = hActive + hSync + hBackPorch + hFrontPorch

    // ILI9806E: the total h Inactive must be >= 2 microseconds
    val hFrontPorch = config.hFrontPorchMin
    val totalWidth = hActive + (config.clockHz.toFloat / 1000000.0 * 2).ceil.toInt

    val x = RegInit(0.U(log2Ceil(totalWidth).W))
    val y = RegInit(0.U(log2Ceil(totalHeightMax).W))
    io.pixelX := x - (hSync + hBackPorch).U
    io.pixelY := y - (vSync + vBackPorch).U

    when (x === (totalWidth - 1).U) {
      // Scanline is done
      regHsync := true.B
      x := 0.U
      y := y + 1.U

      val startFrame = WireDefault(false.B)
      when (startFrame) {
        regVsync := true.B
        y := 0.U
        currentFrame := io.lastRenderedFrame
      }

      // ILI9806E: don't interrupt V-active period.
      // when (!regLocked && newFrameReady) {
      //   // Immediately display new frame (interrupting current frame)
      //   regLocked := true.B
      //   startFrame := true.B
      // } .else
      when (y === (vSync - 1).U) {
        regVsync := false.B
      } .elsewhen ((y >= (totalHeightMin - 1).U) && newFrameReady) {
        // New frame available, start rendering.
        startFrame := true.B
      } .elsewhen (y === (totalHeightMax - 1).U) {
        // Hit the maximum allowed total height without a new frame coming in:
        // source is too slow, switch to rapid refresh (no longer locked)
        regLocked := false.B
        startFrame := true.B
      }
    } .otherwise {
      x := x + 1.U
      when (x === (hSync - 1).U) {
        regHsync := false.B
      }
      val isVActive = (y >= (vSync + vBackPorch).U) && (y < (vSync + vBackPorch + vActive).U)
      when (x === (hSync + hBackPorch - 1).U && isVActive) {
        regActive := true.B
      }
      when (x === (hSync + hBackPorch + hActive - 1).U) {
        regActive := false.B
      }
    }
  }

  private def doFrozen(): Unit = {
    // Calculate timing
    val vSync = config.vSyncMin + 2
    val vBackPorch = config.vBackPorchMin
    val vFrontPorchMin = config.vFrontPorchMin
    val vFrontPorchMax = 12
    val hSync = config.hSyncMin
    val hBackPorch = config.hBackPorchMin

    // Target ~99.5% the sourceFramePeriod, and make up the rest with frozen cycles.
    val totalHeightMin = vActive + config.vSyncMin + vBackPorch + vFrontPorchMin
    val totalHeightMax = vActive + config.vSyncMin + vBackPorch + vFrontPorchMax
    val minFrameCycles = 0.995 * config.clockHz * sourceFramePeriod
    val approxFrameWidth = (minFrameCycles / totalHeightMin).round.toInt
    val hFrontPorch = approxFrameWidth - (hActive + hSync + hBackPorch)
    val totalWidth = hActive + hSync + hBackPorch + hFrontPorch
    val totalHeight = vActive + vSync + vBackPorch + vFrontPorchMin
    // Maximum number of cycles the clock can be stopped before artifacts occur.
    val maximumFrozenCycles = totalWidth * 6

    /// Timer for stopping the dot clock.
    val freezeTimer = RegInit(0.U(16.W))
    io.signals.dotclk := (clock.asBool & RegNext(freezeTimer === 0.U)).asClock

    val x = RegInit(0.U(log2Ceil(totalWidth).W))
    val y = RegInit(0.U(log2Ceil(totalHeightMax).W))
    io.pixelX := x - (hSync + hBackPorch).U
    io.pixelY := y - (vSync + vBackPorch).U

    when (freezeTimer > 0.U) {
      freezeTimer := freezeTimer - 1.U

      when (newFrameReady) {
        currentFrame := io.lastRenderedFrame
        freezeTimer := 0.U
      } .elsewhen (freezeTimer === 1.U) {
        // TODO if freezeTimer expires without a new frame being ready,
        // consider switching refresh rate or similar to re-synchronize.
      }
    } .elsewhen (x === (totalWidth - 1).U) {
      // Scanline is done
      regHsync := false.B
      x := 0.U
      y := y + 1.U

      when (y === (vSync - 1).U) {
        regVsync := true.B
      } .elsewhen (y === (totalHeight - 1).U) {
        // Hit the regular number of display cycles.
        // Freeze the clock until the next frame comes in.
        freezeTimer := maximumFrozenCycles.U
        regVsync := false.B
        y := 0.U
      }
    } .otherwise {
      x := x + 1.U
      when (x === (hSync - 1).U) {
        regHsync := true.B
      }
      val isVActive = (y >= (vSync + vBackPorch).U) && (y < (vSync + vBackPorch + vActive).U)
      when (x === (hSync + hBackPorch - 1).U && isVActive) {
        regActive := true.B
      }
      when (x === (hSync + hBackPorch + hActive - 1).U) {
        regActive := false.B
      }
    }
  }
}
