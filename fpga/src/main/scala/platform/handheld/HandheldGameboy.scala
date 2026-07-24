package platform.handheld

import chisel3._
import chisel3.util._
import gameboy.Gameboy
import gameboy.cart.emu.{EmuCartConfig, EmuCartridge, Mbc3RtcAccess, RtcState}
import lib.mem.{MemoryInterface, MemoryMap, PipelineInterfaceBridge, RegisterMap}
import lib.util.ButtonFilter
import lib.video.ColorARGB
import net.gamebub.framework.interface._
import lib.mem.MemoryArbiter
import lib.mem.PipelineMemoryArbiter
import lib.mem.sdram.BurstSdramController
import lib.mem.PipelineMemoryBurstCdc
import xilinx.MMCM

object HandheldGameboy {
  class Config extends Bundle {
    val isCgb = Bool()
  }

  val mmcmVcoHz = 50_000_000.toDouble / 3 * 56.375
}

class HandheldGameboy extends Module with HandheldModule {
  val displayDivider = (HandheldGameboy.mmcmVcoHz / ClocksV0.clockDisplayHzMin).floor.toInt
  val io = IO(new HandheldIo {
    val clocks = new ClocksV0(
      // ~ 8.3886 MHz
      clockSystemHz = (HandheldGameboy.mmcmVcoHz / 112).toInt,
      clockDisplayHz = (HandheldGameboy.mmcmVcoHz / displayDivider).toInt,
      clockSpiHz = (HandheldGameboy.mmcmVcoHz / 5).toInt,
    )
    val video = new VideoV0(
      videoWidth = 160,
      videoHeight = 144,
      colorDepth = 5,
      framePeriod = (456 * 154).toDouble / (4 * 1024 * 1024),
    )
    val audio = new AudioV0()
    val host = new HostV0()
    val pmod = new PmodV0()
    val input = new InputV0()
    val cartridge = new CartridgePortV0()
    val link = new LinkPortV0()
    val sram = new SramV0()
    val sdram = new SdramV0()
  })

  // Main MMCM
  val mmcm = Module(new MMCM(
      clockInHz = 50_000_000,
      divide = 3,
      multiply = 56.375,
      clockOutConfig = Seq(
          MMCM.ClockOut(112), // System
          MMCM.ClockOut(28),  // SDRAM (4x)
          MMCM.ClockOut(displayDivider),  // Display
          MMCM.ClockOut(5),   // Host SPI
      )
  ))
  mmcm.io.clockIn := io.clocks.clockIn50M
  mmcm.io.powerDown := false.B
  io.clocks.clockOutSystem := mmcm.io.clockOuts(0)
  val clockSdram = mmcm.io.clockOuts(1)
  val clockSdramHz = io.clocks.clockSystemHz * 4
  io.clocks.clockOutDisplay := mmcm.io.clockOuts(2)
  io.clocks.clockOutSpi := mmcm.io.clockOuts(3)
  io.clocks.locked := mmcm.io.locked

  // Config
  val configRegSystem = RegInit(0.U.asTypeOf(new HandheldGameboy.Config))
  val configRegEmuCart = RegInit(0.U.asTypeOf(new EmuCartConfig))
  val configRegRomAddress = RegInit(0.U(19.W))
  val configRegRomMask = RegInit(0.U(23.W))
  val configRegRamAddress = RegInit(0.U(19.W))
  val configRegRamMask = RegInit(0.U(17.W))
  val configRegImuAccelX = RegInit(0.U(16.W))
  val configRegImuAccelY = RegInit(0.U(16.W))
  val configRegDmgOffColor = RegInit(0.U(16.W))
  val statRegStalls = RegInit(0.U(32.W))
  val statRegCycles = RegInit(0.U(32.W))

  val emuCartRtcAccess = Wire(new Mbc3RtcAccess)
  emuCartRtcAccess.writeEnable := false.B
  emuCartRtcAccess.writeState := DontCare
  emuCartRtcAccess.latchSelect := DontCare
  private def makeRtcAccess(latched: Boolean): RegisterMap.Entry = {
    RegisterMap.Entry(
      (new RtcState).getWidth,
      read = RegisterMap.ReadFn((read: Bool) => {
        when (read) { emuCartRtcAccess.latchSelect := latched.B }
        emuCartRtcAccess.readState.asUInt
      }),
      write = RegisterMap.WriteFn((write: Bool, data: UInt) =>
        when (write) {
          emuCartRtcAccess.latchSelect := latched.B
          emuCartRtcAccess.writeState := data.asTypeOf(new RtcState)
          emuCartRtcAccess.writeEnable := true.B
        }
      ),
    )
  }

  val sramArbiter = Module(new MemoryArbiter(addressWidth = 18, dataWidth = 16, n = 2))
  val sramHost = sramArbiter.io.initiator(0)
  val sramEmuCart = sramArbiter.io.initiator(1)

  // SDRAM
  val sdramArbiter = Module(new PipelineMemoryArbiter(addressWidth = 25, dataWidth = 32, n = 2))
  val sdramHost = Wire(new MemoryInterface(addressWidth = 25, dataWidth = 32))
  val sdramEmuCart = sdramArbiter.io.initiator(1)

  {
    val bridge = Module(new PipelineInterfaceBridge(addressWidth = 25, dataWidth = 32))
    bridge.io.source <> sdramHost
    bridge.io.dest <> sdramArbiter.io.initiator(0)
  }

  val registerInterface = Wire(new MemoryInterface(addressWidth = 16, dataWidth = 32))
  val biosInterface = Wire(new MemoryInterface(addressWidth = 12, dataWidth = 8)) // 4 KiB
  val dmgPaletteInterface = Wire(new MemoryInterface(addressWidth = 5, dataWidth = 16))
  io.host.mem <> MemoryMap(
    addressWidth = 32,
    dataWidth = 32,
    entries = Seq(
      0x0.U(4.W) -> registerInterface,
      0x1.U(4.W) -> biosInterface,
      0x2.U(4.W) -> dmgPaletteInterface,
      0x3.U(4.W) -> sdramHost,
      0x4.U(4.W) -> sramHost,
    ))

  suppressEnumCastWarning {
    registerInterface <> RegisterMap(
      addressWidth = 16,
      dataWidth = 32,
      entries = Seq(
        0x0000 -> RegisterMap.Entry.rw(configRegSystem),
        0x0004 -> RegisterMap.Entry.rw(configRegEmuCart), // Suppressing mbcType enum cast
        0x0008 -> RegisterMap.Entry.rw(configRegRomAddress),
        0x000C -> RegisterMap.Entry.rw(configRegRomMask),
        0x0010 -> RegisterMap.Entry.rw(configRegRamAddress),
        0x0014 -> RegisterMap.Entry.rw(configRegRamMask),
        0x0018 -> makeRtcAccess(latched = false),
        0x001C -> makeRtcAccess(latched = true),
        0x0020 -> RegisterMap.Entry.rw(configRegImuAccelX),
        0x0024 -> RegisterMap.Entry.rw(configRegImuAccelY),
        0x0030 -> RegisterMap.Entry.w(configRegDmgOffColor),

        0x1000 -> RegisterMap.Entry.rw(statRegStalls),
        0x1004 -> RegisterMap.Entry.rw(statRegCycles),
      )
    )
  }

  val dmgPalette = Reg(Vec(16, UInt(15.W)))
  when (dmgPaletteInterface.enable && dmgPaletteInterface.write) {
    dmgPalette(dmgPaletteInterface.address(4, 1)) := dmgPaletteInterface.dataWrite
  }
  dmgPaletteInterface.dataRead := 0.U
  dmgPaletteInterface.done := RegNext(dmgPaletteInterface.enable)

  // Gameboy
  val gameboyConfig = Gameboy.Configuration(
    skipBootrom = false,
    optimizeForSimulation = false,
  )
  val gameboy = Module(new Gameboy(gameboyConfig))
  when (io.host.reset) {
    gameboy.reset := true.B
  }
  gameboy.io.isCgb := configRegSystem.isCgb

  // Gameboy clock control
  val doStall = WireDefault(false.B)
  gameboy.io.clockConfig.enable := false.B
  when (io.host.enable) {
    when (doStall) {
      statRegStalls := statRegStalls + 1.U
    }.otherwise {
      gameboy.io.clockConfig.enable := true.B
      statRegCycles := statRegCycles + 1.U
    }
  }
  gameboy.io.clockConfig.provide8Mhz := true.B

  val buttonFilter = Module(new ButtonFilter(new InputV0.Buttons))
  buttonFilter.io.enable := io.host.enable
  buttonFilter.io.input := io.input.buttons
  gameboy.io.joypad.a := buttonFilter.io.output.a
  gameboy.io.joypad.b := buttonFilter.io.output.b
  gameboy.io.joypad.up := buttonFilter.io.output.up
  gameboy.io.joypad.down := buttonFilter.io.output.down
  gameboy.io.joypad.left := buttonFilter.io.output.left
  gameboy.io.joypad.right := buttonFilter.io.output.right
  gameboy.io.joypad.start := buttonFilter.io.output.start
  gameboy.io.joypad.select := buttonFilter.io.output.select

  // Vibration unused by default.
  io.input.vibrate := HandheldVibrate.Off

  // PMOD unused
  io.pmod.out := DontCare
  io.pmod.dir := 0.U(4.W)

  io.audio.left := gameboy.io.apu.left << 6
  io.audio.right := gameboy.io.apu.right << 6

  // Link port
  io.link.soOut := gameboy.io.serial.out
  io.link.soDir := true.B
  gameboy.io.serial.in := RegNext(RegNext(io.link.siIn))
  io.link.siOut := DontCare
  io.link.siDir := false.B
  io.link.sdOut := DontCare
  io.link.sdDir := false.B
  gameboy.io.serial.clockIn := RegNext(RegNext(io.link.scIn))
  io.link.scOut := gameboy.io.serial.clockOut
  io.link.scDir := gameboy.io.serial.clockEnable

  // Video output
  val videoX = RegInit(0.U(8.W))
  val videoY = RegInit(0.U(8.W))
  io.video.dataEnable := false.B
  io.video.data.a := DontCare
  io.video.data.r := DontCare
  io.video.data.g := DontCare
  io.video.data.b := DontCare
  val regDisplayOff = RegInit(false.B)

  when (regDisplayOff) {
    // Render a frame of "lcd off" color
    // When the display is turned off, it remains off until the next vblank when
    // the LCD is on. This ensures that the entire screen is blanked, and matches
    // Game Boy behavior.

    io.video.vblank := false.B
    io.video.hblank := false.B
    when (configRegSystem.isCgb) {
      io.video.data.r := 0x1F.U(5.W)
      io.video.data.g := 0x1F.U(5.W)
      io.video.data.b := 0x1F.U(5.W)
    } .otherwise {
      io.video.data := configRegDmgOffColor.asTypeOf(ColorARGB.rgb555())
    }

    when (videoY === 144.U) {
      io.video.vblank := true.B
    } .elsewhen (videoX === 160.U) {
      io.video.hblank := true.B
      videoX := 0.U
      videoY := videoY + 1.U
    } .otherwise {
      io.video.dataEnable := true.B
      videoX := videoX + 1.U
    }

    // Only end blanking after the *next* vblank when the LCD is on
    when (gameboy.io.ppu.lcdEnable && gameboy.io.ppu.vblank) {
      regDisplayOff := false.B
    }
  } .otherwise {
    io.video.vblank := gameboy.io.ppu.vblank
    io.video.hblank := gameboy.io.ppu.hblank

    when (!gameboy.io.clockConfig.enable) {
      // Do nothing.
    } .elsewhen (!gameboy.io.ppu.lcdEnable) {
      // Blank for at least a whole frame.
      regDisplayOff := true.B
      videoX := 0.U
      videoY := 0.U
    } .elsewhen (gameboy.io.ppu.valid) {
      io.video.dataEnable := true.B

      when (configRegSystem.isCgb) {
        io.video.data.r := gameboy.io.ppu.pixel(4, 0)
        io.video.data.g := gameboy.io.ppu.pixel(9, 5)
        io.video.data.b := gameboy.io.ppu.pixel(14, 10)
      } .otherwise {
        val index = gameboy.io.ppu.dmgColor.asUInt
        io.video.data := dmgPalette(index).asTypeOf(ColorARGB.rgb555())
      }
    }
  }

  // Emulated Cartridge
  val emuCart = Module(new EmuCartridge(8 * 1024 * 1024))
  when (io.host.reset) {
    emuCart.reset := true.B
  }
  emuCart.io.config := configRegEmuCart
  emuCart.io.tCycle := gameboy.io.tCycle
  emuCart.io.rtcAccess <> emuCartRtcAccess
  emuCart.io.imu.x := configRegImuAccelX
  emuCart.io.imu.y := configRegImuAccelY

  val sdramBridge = Module(new PipelineInterfaceBridge(addressWidth = 25, dataWidth = 32))
  sdramBridge.io.dest <> sdramEmuCart
  val sdram = sdramBridge.io.source
  sdram.enable := false.B
  sdram.write := false.B
  sdram.address := DontCare
  sdram.dataWrite := DontCare
  sdram.writeStrobe := DontCare

  sramEmuCart.enable := false.B
  sramEmuCart.write := false.B
  sramEmuCart.address := DontCare
  sramEmuCart.dataWrite := DontCare
  sramEmuCart.writeStrobe := DontCare

  val regEmuCartBusy = RegInit(false.B)
  val regEmuCartDataRead = Reg(UInt(8.W))
  val regEmuCartDataWrite = Reg(UInt(8.W))
  val regEmuCartAddress = Reg(UInt(23.W))
  val regEmuCartIsWrite = Reg(Bool())
  val regEmuCartSelectRom = Reg(Bool())
  val emuCartDataWrite = WireDefault(regEmuCartDataWrite)
  val emuCartIsWrite = WireDefault(regEmuCartIsWrite)
  val emuCartAddress = WireDefault(regEmuCartAddress)
  val emuCartSelectRom = WireDefault(regEmuCartSelectRom)
  val emuCartAccessStart = emuCart.io.dataAccess.enable && !emuCart.reset.asBool
  when (emuCartAccessStart) {
    regEmuCartBusy := true.B

    regEmuCartDataWrite := emuCart.io.dataAccess.dataWrite
    regEmuCartAddress := emuCart.io.dataAccess.address
    regEmuCartIsWrite := emuCart.io.dataAccess.write
    regEmuCartSelectRom := emuCart.io.dataAccess.selectRom

    emuCartDataWrite := emuCart.io.dataAccess.dataWrite
    emuCartAddress := emuCart.io.dataAccess.address
    emuCartIsWrite := emuCart.io.dataAccess.write
    emuCartSelectRom := emuCart.io.dataAccess.selectRom
  }
  emuCart.io.dataAccess.valid := false.B
  emuCart.io.dataAccess.dataRead := regEmuCartDataRead
  when (emuCartAccessStart || regEmuCartBusy) {
    when (emuCartSelectRom) {
      when (emuCartIsWrite) {
        // Don't handle ROM writes.
        emuCart.io.dataAccess.valid := true.B
      } .otherwise {
        sdram.enable := true.B
        sdram.write := false.B
        sdram.address := configRegRomAddress + (Cat(emuCartAddress(22, 2), "b00".U(2.W)) & configRegRomMask)
        emuCart.io.dataAccess.dataRead := sdram.dataRead
          .asTypeOf(Vec(4, UInt(8.W)))(
            emuCartAddress(1, 0)
          )
        emuCart.io.dataAccess.valid := sdram.done
      }
    } .otherwise {
      sramEmuCart.enable := true.B
      sramEmuCart.write := emuCartIsWrite
      sramEmuCart.address := (configRegRamAddress + (Cat(emuCartAddress(16, 1), "b0".U(1.W)) & configRegRamMask))
      sramEmuCart.dataWrite := Fill(2, emuCartDataWrite)
      sramEmuCart.writeStrobe := Mux(emuCartAddress(0), "b10".U(2.W), "b01".U(2.W))
      emuCart.io.dataAccess.valid := sramEmuCart.done
      emuCart.io.dataAccess.dataRead := Mux(
        emuCartAddress(0),
        sramEmuCart.dataRead(15, 8),
        sramEmuCart.dataRead(7, 0)
      )
    }
  }
  when (regEmuCartBusy && emuCart.io.dataAccess.valid) {
    regEmuCartBusy := false.B
    regEmuCartDataRead := emuCart.io.dataAccess.dataRead
  }

  when (emuCart.io.config.enabled) {
    io.cartridge.enabled := false.B

    // Connect emulated cartridge
    emuCart.io.cartridge <> gameboy.io.cartridge
    io.input.vibrate := Mux(emuCart.io.rumble, HandheldVibrate.On, HandheldVibrate.Off)
    doStall := emuCart.io.stall

    // Disconnect physical cartridge
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
  } .otherwise {
    // Cartridge I/O
    io.cartridge.enabled := true.B

    // Bank 0: Data bus
    gameboy.io.cartridge.dataIn := io.cartridge.bank0In
    io.cartridge.bank0Out := gameboy.io.cartridge.dataOut
    io.cartridge.bank0Dir := gameboy.io.cartridge.dataDir

    // Bank 1: Address High
    io.cartridge.bank1Out := gameboy.io.cartridge.address(15, 8)
    io.cartridge.bank1Dir := true.B

    // Bank 2: Address Low
    io.cartridge.bank2Out := gameboy.io.cartridge.address(7, 0)
    io.cartridge.bank2Dir := true.B

    // Bank 3: Control signals (0: nCS, 1: nRD, 2: nWR, 3: PHI)
    io.cartridge.bank3Dir := true.B
    io.cartridge.bank3Out := Cat(
      gameboy.io.cartridge.phi,
      gameboy.io.cartridge.nWR,
      gameboy.io.cartridge.nRD,
      gameboy.io.cartridge.nCS,
    )

    // Pin 30: nRST
    // TODO: open-drain bidirectional
    io.cartridge.pin30Dir := true.B
    io.cartridge.pin30Out := gameboy.io.cartridge.nResetOut
    gameboy.io.cartridge.nResetIn := io.cartridge.pin30In

    // Pin 31: VIN
    io.cartridge.pin31Dir := false.B
    io.cartridge.pin31Out := DontCare

    // Disconnect emulated cartridge
    emuCart.io.cartridge := DontCare
    emuCart.io.cartridge.reqStart := false.B
  }

  // Boot ROM
  // The DMG one is 256 bytes, and starts at 0
  // The CGB one is 2304 bytes (2048 with 256 bytes padding), starts at 256
  val bios = SRAM(2048 + 256 + 256, UInt(8.W), numReadPorts = 1, numWritePorts = 1, numReadwritePorts = 0)
  bios.writePorts(0).enable := biosInterface.enable && biosInterface.write
  bios.writePorts(0).address := biosInterface.address
  bios.writePorts(0).data := biosInterface.dataWrite
  biosInterface.dataRead := 0.U
  biosInterface.done := RegNext(bios.writePorts(0).enable || bios.readPorts(0).enable)
  bios.readPorts(0).enable := gameboy.io.bootRom.read
  when (gameboy.io.isCgb) {
    bios.readPorts(0).address := gameboy.io.bootRom.address +& 256.U
  } .otherwise {
    bios.readPorts(0).address := gameboy.io.bootRom.address
  }
  gameboy.io.bootRom.data := bios.readPorts(0).data

  // SRAM controller
  val sramController = Module(new AsyncSramController(addressWidth = 18, dataWidth = 16))
  io.sram.ceN := false.B
  io.sram.weN := sramController.io.signals.weN
  io.sram.oeN := sramController.io.signals.oeN
  io.sram.writeMaskN := sramController.io.signals.writeMaskN
  io.sram.address := sramController.io.signals.address
  sramController.io.signals.dataIn := io.sram.dataIn
  io.sram.dataOut := sramController.io.signals.dataOut
  io.sram.dataDir := sramController.io.signals.dataDir
  sramController.io.mem <> sramArbiter.io.target

  // SDRAM controller
  withClock(clockSdram) {
    val config = BurstSdramController.Config(
      clockFrequency = clockSdramHz,
      accessLength = 2,
      timeRsc = (2 * 1_000_000_000) / clockSdramHz, /* 2 clocks */
      timeWr = (2 * 1_000_000_000) / clockSdramHz, /* 2 clocks */
      enableBurst = false,
    )
    val controller = Module(new BurstSdramController(config))
    val cdc = Module(new PipelineMemoryBurstCdc(
      addressWidth = 25,
      dataWidth = 32,
      addressBurstIncrement = 4,
      enablePrefetch = false,
    ))
    cdc.io.slowClock := clock
    cdc.io.initiator <> sdramArbiter.io.target
    cdc.io.target <> controller.io.mem
    
    io.sdram.clock := clockSdram
    io.sdram.cke := controller.io.signals.cke

    io.sdram.cs := controller.io.signals.cs
    io.sdram.ras := controller.io.signals.ras
    io.sdram.cas := controller.io.signals.cas
    io.sdram.we := controller.io.signals.we

    io.sdram.dqm := controller.io.signals.dqm
    io.sdram.bank := controller.io.signals.bank
    io.sdram.address := controller.io.signals.address
    controller.io.signals.dataIn := io.sdram.dataIn
    io.sdram.dataOut := controller.io.signals.dataOut
    io.sdram.dataDir := controller.io.signals.dataDir
  }
}