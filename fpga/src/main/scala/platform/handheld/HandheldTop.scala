package platform.handheld

import chisel3._
import chisel3.util._
import _root_.circt.stage.ChiselStage
import lib.mem.{HandshakeMemoryCdc, MemoryInterface, MemoryMap, RegisterMap}
import lib.video.{Color, ColorARGB, ColorCorrection, ColorGrayscale}
import xilinx.{XpmCdcHandshake, XpmCdcSingle, XpmCdcSyncRst}
import net.gamebub.framework.interface._
import net.gamebub.framework.Core
import lib.util.FractionalDivider
import platform.handheld.display.DisplayDriverIO
import platform.handheld.display.ILI9806E
import platform.handheld.display.ILI9488
import platform.handheld.display.ST7262E43
import platform.handheld.display.DpiSignals
import net.gamebub.framework.CoreException

object HandheldTop extends App {
  // Parse arguments.
  if (args.length < 2) {
    throw new IllegalArgumentException("missing arg 0: core class, arg 1: revision")
  }
  val argCoreClassName :: argRevision :: argRest = args.toList

  // Generate verilog.
  val coreFactory = () =>
    Class
      .forName(argCoreClassName)
      .getDeclaredConstructor()
      .newInstance()
      .asInstanceOf[Core]

  ChiselStage.emitSystemVerilogFile(
    new HandheldTop(coreFactory, getRevision(argRevision)),
    argRest.toArray,
    firtoolOpts = Array(
      "--preserve-aggregate=1d-vec",
    )
  )

  private def getRevision(name: String): Revision = {
    name match {
      case "1" | "2" => Revision(
        displayWidth = 480,
        displayHeight = 320,
        displayRotate = true,
        displayColorDepth = 6,
        displayDriverFactory = (sourceFramePeriod, clockHz) => {
          val driver = Module(new ILI9488(
            clockHz,
            sourceFramePeriod,
          ))
          (driver, driver.io)
        },
        getClockDisplayHz = ILI9488.getClockDisplayHz,
        overlayWidth = 240,
        overlayHeight = 160,
      )
      case "3" => Revision(
        displayWidth = 800,
        displayHeight = 480,
        displayColorDepth = 6,
        displayDriverFactory = (sourceFramePeriod, clockHz) => {
          val driver = Module(new ST7262E43(
            clockHz,
            sourceFramePeriod,
          ))
          (driver, driver.io)
        },
        getClockDisplayHz = (_) => (26_099_000, 26_100_000),
        overlayWidth = 360,
        overlayHeight = 240,
      )
      case "4" => Revision(
        displayWidth = 800,
        displayHeight = 480,
        displayRotate = true,
        displayOffsetX = -28,
        displayColorDepth = 8,
        displayDriverFactory = (sourceFramePeriod, clockHz) => {
          val driver = Module(new ILI9806E(
            clockHz,
            sourceFramePeriod,
          ))
          (driver, driver.io)
        },
        getClockDisplayHz = ILI9806E.getClockDisplayHz,
        overlayWidth = 360,
        overlayHeight = 240,
      )
      case _ => throw new IllegalArgumentException("invalid revision " + name)
    }
  }
}

class HandheldInterrupts extends Bundle {
  val spiResponseFifoUnderflow = Bool()
  val spiRequestFifoOverflow = Bool()
  val buttonEdge = Bool()
  val coreVblank = Bool()
}

/**
 * Top-level Chisel module for the Handheld.
 */
class HandheldTop[T <: Core](coreFactory: () => T, revision: Revision) extends Module {
  val io = IO(new Bundle {
    /** Clocking **/
    val clockIn50Mhz = Input(Clock())
    val clockOutSys = Output(Clock())
    val clockOutDpi = Output(Clock())
    val clockOutLocked = Output(Bool())

    /** Audio/video clock: DPI when HDMI disabled, 27.027 MHz when HDMI enabled */
    val clock_av = Input(Clock())

    /** MCU interrupt: true to pull it low (active) */
    val mcuIrq = Output(Bool())
    val mcuSpiChipSelect = Input(Bool())
    val mcuSpiClock = Input(Bool())
    val mcuSpiDataIn = Input(UInt(4.W))
    val mcuSpiDataOut = Output(UInt(4.W))
    val mcuSpiDataDir = Output(UInt(4.W))

    val lcd = Output(new DpiSignals)
    val lcdDataR = Output(UInt(revision.displayColorDepth.W))
    val lcdDataG = Output(UInt(revision.displayColorDepth.W))
    val lcdDataB = Output(UInt(revision.displayColorDepth.W))
    val dac = Output(new I2sSignals)

    /** HDMI */
    val hdmiEnable = Output(Bool())
    val hdmiClockPowerDown = Output(Bool())
    val hdmiAudioClock = Output(Clock())
    val hdmiAudio = Output(Vec(2, UInt(16.W)))
    val hdmiRgb = Output(UInt(24.W))
    val hdmiCx = Input(UInt(10.W))
    val hdmiCy = Input(UInt(10.W))

    /** Raw button input, not registered or inverted. */
    val buttons = Input(new InputV0.Buttons)

    // Cartridge I/O
    val cartridge3V3Enable = Output(Bool())
    val cartridge5V0Enable = Output(Bool())

    val cartridge = new CartridgePortV0

    val vibrate = Output(Bool())
    val pmod = new PmodV0
    val link = new LinkPortV0

    // SRAM
    val sram = new SramV0(addressWidth = 18, dataWidth = 16)

    // SDRAM
    val sdram = new SdramV0(addressWidth = 13, dataWidth = 16, bankWidth = 2, chips = 1)
  })

  //////////////////////////////////
  // Core
  //////////////////////////////////
  ClocksV0.getClockDisplayHz = revision.getClockDisplayHz
  val core = Module(coreFactory())

  // Clocks
  val (
    clockSpi: Clock,
    clockDisplayHz: Int
  ) = core.io.elements.get("clocks") match {
    case Some(clocks: ClocksV0) => {
      clocks.clockIn50M := io.clockIn50Mhz
      io.clockOutLocked := clocks.locked
      io.clockOutSys := clocks.clockOutSystem
      io.clockOutDpi := clocks.clockOutDisplay

      (
        clocks.clockOutSpi,
        clocks.clockDisplayHz,
      )
    }
    case Some(x) => throw new CoreException("Unknown 'clocks': " + x.getClass())
    case None => throw new CoreException("'clocks' is required")
  }

  // Video
  val coreVideo = Wire(new Bundle {
    val dataR = UInt(8.W)
    val dataG = UInt(8.W)
    val dataB = UInt(8.W)
    val dataEnable = Bool()
    val vblank = Bool()
    val hblank = Bool()
  })
  val (
    videoWidth: Int,
    videoHeight: Int,
    videoFramePeriod: Double,
    videoColorDepth: Int,
  ) = core.io.elements.get("video") match {
    case Some(video: VideoV0) => {
      coreVideo.dataR := video.data.r
      coreVideo.dataG := video.data.g
      coreVideo.dataB := video.data.b
      coreVideo.dataEnable := video.dataEnable
      coreVideo.hblank := video.hblank
      coreVideo.vblank := video.vblank
      (
        video.videoWidth,
        video.videoHeight,
        video.framePeriod,
        video.colorDepth,
      )
    }
    case Some(x) => throw new CoreException("Unknown 'video': " + x.getClass())
    case None => throw new CoreException("'video' is required")
  }

  // Audio
  val coreAudioData = Wire(new Bundle {
    val left = SInt(16.W)
    val right = SInt(16.W)
  })
  core.io.elements.get("audio") match {
    case Some(audio: AudioV0) => {
      coreAudioData.left := audio.left
      coreAudioData.right := audio.right
    }
    case Some(x) => throw new CoreException("Unknown 'audio': " + x.getClass())
    case None => {
      coreAudioData.left := 0.S
      coreAudioData.right := 0.S
    }
  }

  // Host
  val coreHost = Wire(new Bundle {
    val enable = Bool()
    val reset = Bool()
  })
  val coreHostInterface = Wire(new MemoryInterface(addressWidth = 31, dataWidth = 32))
  val (
    overlayColorDepth: Color,
  ) = core.io.elements.get("host") match {
    case Some(host: HostV0) => {
      host.enable := coreHost.enable
      host.reset := coreHost.reset
      host.mem <> coreHostInterface
      (
        host.getOverlayColorDepth,
      )
    }
    case Some(x) => throw new CoreException("Unknown 'host': " + x.getClass())
    case None => throw new CoreException("'host' is required")
  }

  // PMOD
  core.io.elements.get("pmod") match {
    case Some(pmod: PmodV0) => {
      io.pmod <> pmod
    }
    case Some(x) => throw new CoreException("Unknown 'pmod': " + x.getClass())
    case None => {
      io.pmod.dir := 0.U // All inputs
      io.pmod.out := 0.U
    }
  }

  // Input
  val coreInput = Wire(new InputV0.Buttons)
  val coreVibrate = Wire(InputV0.Vibrate())
  core.io.elements.get("input") match {
    case Some(input: InputV0) => {
      input.buttons := coreInput
      coreVibrate := input.vibrate
    }
    case Some(x) => throw new CoreException("Unknown 'input': " + x.getClass())
    case None => {
      coreVibrate := InputV0.Vibrate.Off
    }
  }

  // Cartridge Port
  core.io.elements.get("cartridge") match {
    case Some(cartridge: CartridgePortV0) => {
      io.cartridge <> cartridge
    }
    case Some(x) => throw new CoreException("Unknown 'cartridge': " + x.getClass())
    case None => {
      io.cartridge.enabled := false.B
      io.cartridge.bank0Out := DontCare
      io.cartridge.bank1Out := DontCare
      io.cartridge.bank2Out := DontCare
      io.cartridge.bank3Out := DontCare
      io.cartridge.pin30Out := DontCare
      io.cartridge.pin31Out := DontCare
      io.cartridge.bank0Dir := false.B
      io.cartridge.bank1Dir := false.B
      io.cartridge.bank2Dir := false.B
      io.cartridge.bank3Dir := false.B
      io.cartridge.pin30Dir := false.B
      io.cartridge.pin31Dir := false.B
    }
  }

  // Link Port
  core.io.elements.get("link") match {
    case Some(link: LinkPortV0) => {
      io.link <> link
    }
    case Some(x) => throw new CoreException("Unknown 'link': " + x.getClass())
    case None => {
      io.link.soOut := false.B
      io.link.siOut := false.B
      io.link.sdOut := false.B
      io.link.scOut := false.B
      io.link.soDir := false.B
      io.link.siDir := false.B
      io.link.sdDir := false.B
      io.link.scDir := false.B
    }
  }

  // SRAM
  core.io.elements.get("sram") match {
    case Some(sram: SramV0) => {
      io.sram <> sram
    }
    case Some(x) => throw new CoreException("Unknown 'sram': " + x.getClass())
    case None => {
      io.sram.ceN := true.B
      io.sram.weN := true.B
      io.sram.oeN := true.B
      io.sram.writeMaskN := true.B
      io.sram.address := DontCare
      io.sram.dataOut := DontCare
      io.sram.dataDir := false.B
    }
  }

  // SDRAM
  core.io.elements.get("sdram") match {
    case Some(sdram: SdramV0) => {
      io.sdram <> sdram
    }
    case Some(x) => throw new CoreException("Unknown 'sdram': " + x.getClass())
    case None => {
      io.sdram.clock := false.B.asClock
      io.sdram.cke := false.B
      io.sdram.cs := true.B
      io.sdram.ras := true.B
      io.sdram.cas := true.B
      io.sdram.we := true.B
      io.sdram.dqm := DontCare
      io.sdram.bank := DontCare
      io.sdram.address := DontCare
      io.sdram.dataOut := DontCare
      io.sdram.dataDir := false.B
    }
  }

  //////////////////////////////////
  // MCU Communication
  //////////////////////////////////
  // D0: PICO, D1: POCI
  // TODO: clock gate when nCS is high
  val clockSpiLocked = Wire(Bool())
  val spi = Module(new SpiReceiverFifo())
  spi.io.clockSpi := clockSpi
  spi.io.clockSpiLocked := clockSpiLocked
  io.mcuSpiDataDir := Mux(io.mcuSpiChipSelect, 0.U, spi.io.signals.serialDir)
  io.mcuSpiDataOut := spi.io.signals.serialOut
  spi.io.signals.serialClock := io.mcuSpiClock
  spi.io.signals.serialIn := io.mcuSpiDataIn
  spi.io.signals.chipSelect := io.mcuSpiChipSelect
  withClock (clockSpi) {
    clockSpiLocked := RegNext(!spi.io.clockSpiPowerDown)
  }

  val controlRegister = RegInit(0.U.asTypeOf(new Bundle() {
    /** True to enable vibration (if the core uses it) */
    val vibrate = Bool()
    /** Whether the core is currently in vblank. (TODO make read-only) */
    val coreVblank = Bool()
    /** Active-low reset for the inner core. */
    val coreReset = Bool()
    /** Active-high enable for the inner core. */
    val coreEnable = Bool()
  }))
  val displayRegister = RegInit(0.U.asTypeOf(new Bundle() {
    val docked = Bool()
  }))
  /// Buttons that are forced down by MCU
  val buttonForceRegister = RegInit(0.U.asTypeOf(new InputV0.Buttons))
  val interruptEnable = RegInit(0.U.asTypeOf(new HandheldInterrupts))
  val interruptFlags = RegInit(0.U.asTypeOf(new HandheldInterrupts))
  val statusRegister = Cat(
    // 0: cartridge switch state
    RegNext(RegNext(io.cartridge.switch)),
  )
  val colorCorrectionRegister = RegInit(0.U.asTypeOf(new Bundle() {
    val enableColorCorrections = Bool()
  }))

  val overlayXControlRegister = RegInit(0.U.asTypeOf(new Bundle() {
    val start = UInt(8.W)
    val end = UInt(8.W)
    val scroll = UInt(8.W)
  }))
  val overlayYControlRegister = RegInit(0.U.asTypeOf(new Bundle() {
    val start = UInt(8.W)
    val end = UInt(8.W)
    val scroll = UInt(8.W)
  }))
  /// Synchronized physical button state (without MCU force override)
  val buttonState = Wire(new InputV0.Buttons)

  val registerMap = RegisterMap(
    addressWidth = 16,
    dataWidth = 32,
    entries = Seq(
      0x0 -> RegisterMap.Entry.rw(controlRegister),
      0x4 -> RegisterMap.Entry.rw(buttonForceRegister),
      0x8 -> RegisterMap.Entry.rw(displayRegister),
      0xC -> RegisterMap.Entry.rw(interruptEnable),
      0x10 -> RegisterMap.Entry(
        interruptFlags.getWidth,
        read = RegisterMap.ReadFn((_: Bool) => interruptFlags.asUInt),
        write = RegisterMap.WriteFn((write: Bool, data: UInt) =>
          when (write) {
            // Write set bits to ack interrupts.
            interruptFlags := (interruptFlags.asUInt & (~data).asUInt).asTypeOf(interruptFlags)
          }
        ),
      ),
      0x14 -> RegisterMap.Entry.r(statusRegister),
      0x18 -> RegisterMap.Entry.rw(colorCorrectionRegister),
      0x1C -> RegisterMap.Entry.r(buttonState),
      // Overlay control
      0x100 -> RegisterMap.Entry.rw(overlayXControlRegister),
      0x104 -> RegisterMap.Entry.rw(overlayYControlRegister),
      // Framebuffer dimensions
      0x200 -> RegisterMap.Entry.r(
        Cat(videoWidth.U(16.W), videoHeight.U(16.W))),
      // Stats
      0x300 -> RegisterMap.Entry.r(0.U),
      0x304 -> RegisterMap.Entry.r(0.U),
    )
  )

  val overlayInterface = Wire(new MemoryInterface(addressWidth = 18, dataWidth = 16))
  val framebufferInterface = Wire(new MemoryInterface(addressWidth = 18, dataWidth = 16))
  val colorCorrectInterface = Wire(new MemoryInterface(addressWidth = 9, dataWidth = 16))
  // 16 bit prefix: 64 KiB
  // 12 bit prefix: 1 MiB
  // 8 bit prefix: 16 MiB
  // 4 bit prefix: 256 MiB
  spi.io.mem <> MemoryMap(
    addressWidth = 32,
    dataWidth = 32,
    entries = Seq(
      // 2 GiB region 0x0000_0000 - 0x7FFF_FFFF
      0x00.U(1.W) -> coreHostInterface,

      0x80.U(8.W) -> registerMap,
      0x81.U(8.W) -> overlayInterface,
      0x82.U(8.W) -> framebufferInterface,
      0xC00.U(12.W) -> colorCorrectInterface,
    ))

  controlRegister.coreVblank := coreVideo.vblank
  when (spi.io.debugRequestOverflow) {
    interruptFlags.spiRequestFifoOverflow := true.B

  }
  when (spi.io.debugResponseUnderflow) {
    interruptFlags.spiResponseFifoUnderflow := true.B
  }

  //////////////////////////////////
  // Interrupts
  //////////////////////////////////
  io.mcuIrq := (interruptFlags.asUInt & interruptEnable.asUInt).orR
  when (coreVideo.vblank && !RegNext(coreVideo.vblank)) {
    interruptFlags.coreVblank := true.B
  }

  //////////////////////////////////
  // Input & Vibrate
  //////////////////////////////////
  {
    // Invert and synchronize buttons
    val regButtons = RegNext(RegNext(~io.buttons.asUInt)).asTypeOf(new InputV0.Buttons)
    buttonState := regButtons

    when (regButtons.asUInt =/= RegNext(regButtons.asUInt)) {
      // Button edge, mark interrupt
      interruptFlags.buttonEdge := true.B
    }
  }
  coreInput := (buttonState.asUInt | buttonForceRegister.asUInt).asTypeOf(new InputV0.Buttons)

  val vibrateEnabled = coreHost.enable && controlRegister.vibrate && !displayRegister.docked
  io.vibrate := RegNext(coreVibrate === InputV0.Vibrate.On && vibrateEnabled)

  //////////////////////////////////
  // Video
  //////////////////////////////////
  io.hdmiEnable := displayRegister.docked

  // Double buffering
  val framebuffers = (0 until 2).map(_ =>
    SRAM(
      videoWidth * videoHeight, UInt((videoColorDepth * 3).W),
      readPortClocks = Seq(io.clock_av), writePortClocks = Seq(), readwritePortClocks = Seq(clock)
    )
  )
  /// Last completed frame
  val regLastFrameComplete = RegInit(0.U(1.W))

  val overlayWidth = revision.overlayWidth
  val overlayHeight = revision.overlayHeight
  val overlayFramebuffer = SRAM(
    overlayWidth * overlayHeight, UInt(overlayColorDepth.getWidth.W),
    readPortClocks = Seq(io.clock_av), writePortClocks = Seq(clock), readwritePortClocks = Seq(),
  )

  // Keep HDMI MMCM powered for a few more cycles after switching away
  // from it to ensure the clock mux functions correctly.
  val hdmiClockPowerTimer = RegInit(0.U(3.W))
  when (displayRegister.docked) {
    hdmiClockPowerTimer := 7.U
  } .elsewhen (hdmiClockPowerTimer > 0.U) {
    hdmiClockPowerTimer := hdmiClockPowerTimer - 1.U
  }
  io.hdmiClockPowerDown := hdmiClockPowerTimer === 0.U

  val reset_av = withClock(io.clock_av) { XpmCdcSyncRst(reset) }
  withClockAndReset (clock = io.clock_av, reset = reset_av) {
    val videoX = Wire(UInt(10.W))
    val videoY = Wire(UInt(10.W))
    val framebufferReadAddress = Wire(UInt(log2Ceil(videoWidth * videoHeight).W))
    val overlayReadAddress = Wire(UInt(log2Ceil(overlayWidth * overlayHeight).W))

    val audioData = XpmCdcHandshake.continuous(clock, coreAudioData)

    // Buffering the read allows this to be a block ram instead of distributed ram
    // and an additional output buffer allows Vivado to improve timing.
    //
    // Read from the correct framebuffer.
    val framebufferIndex = Wire(UInt(1.W))
    val lastFrameComplete = XpmCdcSingle(clock, regLastFrameComplete.asBool).asUInt
    for (i <- 0 until 2) {
      framebuffers(i).readPorts(0).enable := framebufferIndex === i.U
      framebuffers(i).readPorts(0).address := framebufferReadAddress
    }
    val framebufferRead = MuxLookup(framebufferIndex, 0.U)(
      (0 until 2).map(i => i.U -> RegNext(RegNext(framebuffers(i).readPorts(0).data)))
    ).asTypeOf(ColorARGB(0, videoColorDepth, videoColorDepth, videoColorDepth))

    // Color corrections
    val colorCorrector = Module(new ColorCorrection(inputDepth = 5, outputDepth = 6))
    colorCorrector.io.enable := XpmCdcSingle(clock, colorCorrectionRegister.enableColorCorrections)
    colorCorrector.io.in := framebufferRead
    val framebufferColor = colorCorrector.io.out

    {
      val cdc = Module(new HandshakeMemoryCdc(addressWidth = 9, dataWidth = 16))
      cdc.io.sourceClock := clock
      cdc.io.sourceReset := reset
      cdc.io.initiator <> colorCorrectInterface
      val mem = cdc.io.target
      mem.done := true.B
      mem.dataRead := DontCare
      val matrix = RegInit(VecInit(Seq(1, 0, 0, 0, 1, 0, 0, 0, 1).map(x => (x << 10).S(12.W))))
      val inputTable = RegInit(VecInit((0 until 32).map(i => {
        val normal = i.toDouble / 31.0
        (normal * 1024).floor.min(1023).toInt.S(11.W)
      })))
      val outputTable = RegInit(VecInit((0 until 64).map(i => {
        val normal = i.toDouble / 63.0
        (normal * 64).floor.min(63).toInt.U(6.W)
      })))
      when (mem.enable && mem.write) {
        when (mem.address(8, 7) === 0.U) {
          matrix(mem.address(4, 1)) := mem.dataWrite.asSInt
        }
        when (mem.address(8, 7) === 1.U) {
          inputTable(mem.address(5, 1)) := mem.dataWrite.asSInt
        }
        when (mem.address(8, 7) === 2.U) {
          outputTable(mem.address(6, 1)) := mem.dataWrite
        }
      }
      colorCorrector.io.matrixR := VecInit(matrix(0), matrix(1), matrix(2))
      colorCorrector.io.matrixG := VecInit(matrix(3), matrix(4), matrix(5))
      colorCorrector.io.matrixB := VecInit(matrix(6), matrix(7), matrix(8))
      colorCorrector.io.inputTable := inputTable
      colorCorrector.io.outputTable := outputTable
    }

    // Similar for overlay framebuffer.
    val overlayXControl = XpmCdcHandshake.continuous(clock, overlayXControlRegister)
    val overlayYControl = XpmCdcHandshake.continuous(clock, overlayYControlRegister)

    overlayFramebuffer.readPorts(0).enable := true.B
    overlayFramebuffer.readPorts(0).address := overlayReadAddress
    val overlayRead = RegNext(RegNext(overlayFramebuffer.readPorts(0).data))
      .asTypeOf(overlayColorDepth)
      .convertTo(ColorARGB(1, 8, 8, 8))

    val framebufferInBounds = Wire(Bool())
    val overlayInBounds = Wire(Bool())
    val videoOutput = ColorARGB(0, 8, 8, 8).makeBlack()
    when (framebufferInBounds) {
      videoOutput := framebufferColor.convertTo(videoOutput)
    }
    when (overlayRead.a.asBool && overlayInBounds) {
      videoOutput := overlayRead.convertTo(videoOutput)
    }

    // DPI video signal output
    val (dpiDriver, dpiDriverIo) = revision.displayDriverFactory(
      /* sourceFramePeriod = */ videoFramePeriod,
      /* clockHz = */ clockDisplayHz,
    )
    dpiDriverIo.lastRenderedFrame := lastFrameComplete
    io.lcd := dpiDriverIo.signals
    val lcdData = videoOutput.convertTo(
      ColorARGB(0,
        revision.displayColorDepth,
        revision.displayColorDepth,
        revision.displayColorDepth,
      ))
    io.lcdDataR := lcdData.r
    io.lcdDataG := lcdData.g
    io.lcdDataB := lcdData.b

    /**
     * HDMI audio and video signal output
     * Video ID Code 2: 720x480 @ 60Hz
     */
    val hdmiFrameWidth = 858
    val hdmiFrameHeight = 525
    io.hdmiAudio := VecInit(audioData.left.asUInt, audioData.right.asUInt)
    io.hdmiAudioClock := DontCare
    // Pad to 24-bit RGB.
    io.hdmiRgb := videoOutput.convertTo(ColorARGB(0, 8, 8, 8)).asUInt
    val regHdmiFrame = RegInit(0.U(1.W))

    val hdmiEnable = XpmCdcSingle(clock, displayRegister.docked)
    when (hdmiEnable) {
      dpiDriver.reset := true.B
      val screenWidth = 720
      val screenHeight = 480

      // Correct HDMI video X and Y
      videoX := io.hdmiCx
      videoY := io.hdmiCy
      when (io.hdmiCx >= screenWidth.U) {
        // Make it so that adding wraps around to 0.
        // (frameWidth - 1) should be (2**width - 1)
        videoX := io.hdmiCx + ((1 << io.hdmiCx.getWidth) - hdmiFrameWidth).U
        videoY := io.hdmiCy + 1.U
        when (io.hdmiCy === (hdmiFrameHeight - 1).U) {
          videoY := 0.U
        }
      }
      val hdmiFramePulse = io.hdmiCy === (hdmiFrameHeight - 1).U
      framebufferIndex := regHdmiFrame
      when (hdmiFramePulse && !RegNext(hdmiFramePulse)) {
        regHdmiFrame := lastFrameComplete
      }

      // Scale and center framebuffer within output video.
      val videoScale = (screenWidth / videoWidth).min(screenHeight / videoHeight)
      val videoOffsetX = (screenWidth - (videoWidth * videoScale)) / 2
      val videoOffsetY = (screenHeight - (videoHeight * videoScale)) / 2
      val framebufferReadDelay = 3 /* reading */ + 3 /* color corrections */
      framebufferReadAddress :=
        (((videoY - videoOffsetY.U) / videoScale.U) * videoWidth.U) +
          ((videoX - videoOffsetX.U + framebufferReadDelay.U) / videoScale.U)
      framebufferInBounds := videoX >= videoOffsetX.U &&
        videoX < (videoOffsetX + (videoWidth * videoScale)).U &&
        videoY >= videoOffsetY.U &&
        videoY < (videoOffsetY + (videoHeight * videoScale)).U

      // Scale overlay
      val overlayScale = (screenWidth / overlayWidth).min(screenHeight / overlayHeight)
      val overlayOffsetX = (screenWidth - (overlayWidth * overlayScale)) / 2
      val overlayOffsetY = (screenHeight - (overlayHeight * overlayScale)) / 2
      val overlayReadDelay = 3
      overlayReadAddress :=
        (((videoY - overlayOffsetY.U) / overlayScale.U)(8, 0) * overlayWidth.U) +
          ((videoX - overlayOffsetX.U + overlayReadDelay.U) / overlayScale.U)(8, 0)
      overlayInBounds :=
        videoX >= overlayOffsetX.U &&
          videoX < (overlayOffsetX + (overlayWidth * overlayScale)).U &&
          videoY >= overlayOffsetY.U &&
          videoY < (overlayOffsetY + (overlayHeight * overlayScale)).U

      // HDMI Audio
      val audioClock = RegInit(false.B)
      val audioCounter = Counter(27027000 / (48000 * 2))
      when (audioCounter.inc()) {
        audioClock := !audioClock
      }
      io.hdmiAudioClock := audioClock.asClock
    } .otherwise {
      val screenWidth = revision.displayWidth
      val screenHeight = revision.displayHeight

      val dpiX = if (revision.displayRotate) dpiDriverIo.pixelY else dpiDriverIo.pixelX
      val dpiY = if (revision.displayRotate) dpiDriverIo.pixelX else dpiDriverIo.pixelY
      videoX := dpiX
      videoY := dpiY
      framebufferIndex := dpiDriverIo.displayFrame

      // Scale and center framebuffer without output video.
      val videoScale = (screenWidth / videoWidth).min(screenHeight / videoHeight)
      val videoOffsetX = (screenWidth - (videoWidth * videoScale)) / 2 + revision.displayOffsetX
      val videoOffsetY = (screenHeight - (videoHeight * videoScale)) / 2
      val framebufferReadDelay = 3 /* reading */ + 3 /* color corrections */
      val framebufferReadDelayX = if (revision.displayRotate) 0 else framebufferReadDelay
      val framebufferReadDelayY = if (revision.displayRotate) framebufferReadDelay else 0
      framebufferReadAddress :=
        (((dpiY - videoOffsetY.U + framebufferReadDelayY.U) / videoScale.U) * videoWidth.U) +
          ((dpiX - videoOffsetX.U + framebufferReadDelayX.U) / videoScale.U)
      framebufferInBounds :=
        dpiX >= videoOffsetX.U &&
        dpiX < (videoOffsetX + (videoWidth * videoScale)).U &&
        dpiY >= videoOffsetY.U &&
        dpiY < (videoOffsetY + (videoHeight * videoScale)).U

      // Scale overlay
      val overlayScale = (screenWidth / overlayWidth).min(screenHeight / overlayHeight)
      val overlayOffsetX = (screenWidth - (overlayWidth * overlayScale)) / 2 + revision.displayOffsetX
      val overlayOffsetY = (screenHeight - (overlayHeight * overlayScale)) / 2
      val overlayReadDelay = 3
      val overlayReadDelayX = if (revision.displayRotate) 0 else overlayReadDelay
      val overlayReadDelayY = if (revision.displayRotate) overlayReadDelay else 0
      overlayReadAddress :=
        (((dpiY - overlayOffsetY.U + overlayReadDelayY.U) / overlayScale.U)(8, 0) * overlayWidth.U) +
          ((dpiX - overlayOffsetX.U + overlayReadDelayX.U) / overlayScale.U)(8, 0)
      overlayInBounds :=
        dpiX >= overlayOffsetX.U &&
        dpiX < (overlayOffsetX + (overlayWidth * overlayScale)).U &&
        dpiY >= overlayOffsetY.U &&
        dpiY < (overlayOffsetY + (overlayHeight * overlayScale)).U
      // TODO: re-add overlay X/Y positioning control if needed
    }
  }

  //////////////////////////////////
  // Audio
  //////////////////////////////////
  val reset50M = withClock(io.clockIn50Mhz) { XpmCdcSyncRst(reset) }
  withClockAndReset (clock = io.clockIn50Mhz, reset = reset50M) {
    // Synchronize audio data into this domain
    val syncAudioData = XpmCdcHandshake.continuous(clock, coreAudioData)

    // 16-bit, 2 channel audio output at 48 kHz
    // MCLK = 48 KHz * 256 = 12.288 MHz
    val mclkFactor = 256
    val bitWidth = 16
    val channels = 2
    val regMClock = Reg(Bool())
    val divider = Module(new FractionalDivider(inputHz = 50_000_000, targetHz = 12_288_000 * 2))
    when (divider.io.pulse) {
      regMClock := !regMClock
    }
    val mclkEdge = divider.io.pulse && !regMClock

    val regSample = RegInit(0.U((bitWidth * channels).W))
    val regWordClock = RegInit(false.B)
    val regBitClock = RegInit(true.B)

    val bitClockCounter = Counter(mclkFactor / bitWidth / channels / 2)
    val sampleCounter = Counter(mclkFactor)

    when (mclkEdge) {
      when (bitClockCounter.inc()) {
        regBitClock := !regBitClock
        when (!regBitClock) {
          // Rising edge of bit clock
          regWordClock := false.B
          regSample := regSample << 1
        }
      }
      when (sampleCounter.inc()) {
        regSample := syncAudioData.asUInt
        regWordClock := true.B
      }
    }

    io.dac.mclk := regMClock
    io.dac.wclk := regWordClock
    io.dac.bclk := regBitClock
    io.dac.data := regSample(regSample.getWidth - 1)
  }

  // Overlay access.
  // TODO: consider switching to (or adding) a method of writing where
  // there's a "target x" and "target y" register, and you write to a single
  // memory location, which auto-increments the x. Then, have registers for
  // minX (where it wraps to) and maxX (when it wraps), which allows for easy
  // partial rectangular updates.
  overlayInterface.dataRead := DontCare
  overlayInterface.done := false.B
  overlayFramebuffer.writePorts(0).enable := overlayInterface.enable && overlayInterface.write
  overlayFramebuffer.writePorts(0).address := (overlayInterface.address >> 1).asUInt
  overlayFramebuffer.writePorts(0).data :=
    overlayInterface.dataWrite
      .asTypeOf(ColorARGB.argb1555())
      .convertTo(overlayColorDepth)
      .asUInt
  overlayInterface.done := RegNext(overlayInterface.enable)

  // Framebuffer read via SPI.
  for (i <- 0 until 2) {
    framebuffers(i).readwritePorts(0).enable := false.B
    framebuffers(i).readwritePorts(0).address := DontCare
    framebuffers(i).readwritePorts(0).isWrite := DontCare
    framebuffers(i).readwritePorts(0).writeData := DontCare
  }
  val framebufferInterfaceRead = framebufferInterface.enable && !framebufferInterface.write
  when (framebufferInterfaceRead) {
    for (i <- 0 until 2) {
      when (regLastFrameComplete === i.U) {
        framebuffers(i).readwritePorts(0).enable := true.B
        framebuffers(i).readwritePorts(0).address := (framebufferInterface.address >> 1.U).asUInt
        framebuffers(i).readwritePorts(0).isWrite := false.B
      }
    }
  }
  framebufferInterface.dataRead := MuxLookup(regLastFrameComplete, 0.U)(
    (0 until 2).map(i => i.U ->
      RegNext(RegNext(framebuffers(i).readwritePorts(0).readData))
    ))
  framebufferInterface.done := RegNext(RegNext(framebufferInterface.enable))

  //////////////////////////////////
  // Core Connections
  //////////////////////////////////

  // Framebuffer writes
  {
    val framebufferX = RegInit(0.U(log2Ceil(videoWidth).W))
    val framebufferY = RegInit(0.U(log2Ceil(videoHeight).W))
    val framebufferWriteIndex = RegInit(0.U(1.W))

    when (coreVideo.dataEnable && !framebufferInterfaceRead) {
      // Core framebuffer write and SPI framebuffer read share the same read/write port,
      // so ensure that they're not activated at the same time (so they can be inferred correctly).
      val address = (framebufferY * videoWidth.U(10.W)) + framebufferX
      val data = Wire(ColorARGB(0, videoColorDepth, videoColorDepth, videoColorDepth))
      data.a := 0.U
      data.r := coreVideo.dataR
      data.g := coreVideo.dataG
      data.b := coreVideo.dataB
      for (i <- 0 until 2) {
        framebuffers(i).readwritePorts(0).enable := (i.U === framebufferWriteIndex)
        framebuffers(i).readwritePorts(0).address := address
        framebuffers(i).readwritePorts(0).isWrite := true.B
        framebuffers(i).readwritePorts(0).writeData := data.asUInt
      }
    }

    val vblankEdge = coreVideo.vblank && !RegNext(coreVideo.vblank)
    val hblankEdge = coreVideo.hblank && !RegNext(coreVideo.hblank)
    when (vblankEdge) {
      regLastFrameComplete := framebufferWriteIndex
      framebufferWriteIndex := !framebufferWriteIndex
    }

    when (coreVideo.vblank) {
      // Frame ended
      framebufferX := 0.U
      framebufferY := 0.U
    } .elsewhen (coreVideo.hblank) {
      // Line ended
      when (hblankEdge) {
        framebufferX := 0.U
        framebufferY := framebufferY + 1.U
      }
    } .elsewhen (coreVideo.dataEnable) {
      framebufferX := framebufferX + 1.U
    }
  }

  // Cartridge voltage control: Rev1 and Rev2 only
  io.cartridge3V3Enable := RegNext(io.cartridge.enabled && !io.cartridge.switch)
  io.cartridge5V0Enable := RegNext(io.cartridge.enabled && io.cartridge.switch)

  coreHost.enable := controlRegister.coreEnable
  coreHost.reset := !controlRegister.coreReset
}

case class Revision(
  displayWidth: Int,
  displayHeight: Int,
  displayRotate: Boolean = false,
  displayOffsetX: Int = 0,
  displayColorDepth: Int,
  displayDriverFactory: (Double, Int) => (Module, DisplayDriverIO),
  /// A function that returns the clockDisplay clock min Hz and max Hz by frame period
  getClockDisplayHz: (Double) => (Int, Int),
  overlayWidth: Int,
  overlayHeight: Int,
)