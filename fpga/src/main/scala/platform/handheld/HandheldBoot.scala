package platform.handheld

import chisel3._
import chisel3.util._
import lib.mem.MemoryMap
import lib.video.ColorARGB
import platform.handheld.boot.{CartridgeUtility, Logo, picorv32}
import net.gamebub.framework.interface._
import lib.mem.MemoryInterface
import chisel3.util.SRAM
import lib.mem.RegisterMap
import chisel3.simulator.PeekPokeAPI.TestableData
import gba.MmioMap.ReadFn

class HandheldBoot extends Module with HandheldModule {
    val io = IO(new HandheldIo {
        val clocks = new ClocksFixedV0(sysDivider = 56, sdramDivider = 14)
        val video = new VideoV0(
            videoWidth = 240,
            videoHeight = 160,
            colorDepth = 5,
            framePeriod = 1.0 / 64.0,
        )
        val audio = new AudioV0()
        val host = new HostV0(overlayColorDepth = ColorARGB.argb1555())
        val pmod = new PmodV0()
        val input = new InputV0()
        val cartridge = new CartridgePortV0()
        val link = new LinkPortV0()

        // TODO: exclude sram and sdram
        val sram = new SramV0()
        val sdram = new SdramV0()
    })

    stubUnused()

    // Logo animation
    val logo = Module(new Logo(io.video))
    io.video := logo.io.video_

    // CPU / Host communication
    // Byte queue for CPU -> Host
    val txQueue = Module(new Queue(UInt(8.W), 4096, useSyncReadMem = true))
    txQueue.io.deq.ready := false.B
    txQueue.io.enq.valid := false.B
    txQueue.io.enq.bits := DontCare
    // Byte queue for Host -> CPU
    val rxQueue = Module(new Queue(UInt(8.W), 4096, useSyncReadMem = true))
    rxQueue.io.deq.ready := false.B
    rxQueue.io.enq.valid := false.B
    rxQueue.io.enq.bits := DontCare

    val registerInterface = Wire(new MemoryInterface(addressWidth = 16, dataWidth = 32))
    val hostMemInterface = Wire(new MemoryInterface(addressWidth = 16, dataWidth = 32))
    io.host.mem <> MemoryMap(
        addressWidth = 32,
        dataWidth = 32,
        entries = Seq(
            0x00.U(8.W) -> logo.io.registers,
            0x04.U(8.W) -> registerInterface,
            0x05.U(8.W) -> hostMemInterface,
        ))

    // TODO: fix the fundmental issue with the MemoryInterface
    // The problem is that the 'enable' signal is held high for 2 clock cycles,
    // so the reads and writes are actually done twice. Not great for interacting
    // with a FIFO.
    // requires read/write clock of 10 MHz or less for 8-bit transfer
    val doTransfer = RegNext(registerInterface.enable) && registerInterface.enable
    // Require host MCU to specify how many bytes it intends to read.
    // This is because the SPI receiver will pre-fetch reads (so we'll read an extra
    // few bytes, which would cause us to lose data from the FIFO).
    val regTxQueueReadLimit = RegInit(0.U(16.W))

    val regCpuResetN = RegInit(false.B)
    val cpuTrap = Wire(Bool())
    val regLastCpuPc = RegInit(0.U(32.W))
    val cartPowerOn = Wire(Bool())
    registerInterface <> RegisterMap(
        addressWidth = 16,
        dataWidth = 32,
        entries = Seq(
            0x0 -> RegisterMap.Entry.rw(regCpuResetN),
            0x4 -> RegisterMap.Entry.r(cpuTrap),
            0x8 -> RegisterMap.Entry.r(regLastCpuPc),
            0x100 -> RegisterMap.Entry.r(txQueue.io.count),
            0x104 -> RegisterMap.Entry.w(regTxQueueReadLimit),
            0x108 -> RegisterMap.Entry(
                width = 8,
                read = RegisterMap.ReadFn((read: Bool) => {
                    when (read && doTransfer && regTxQueueReadLimit > 0.U) {
                        txQueue.io.deq.ready := true.B
                        regTxQueueReadLimit := regTxQueueReadLimit - 1.U
                    }
                    txQueue.io.deq.bits
                }),
                write = RegisterMap.WriteFn(),
            ),
            0x110 -> RegisterMap.Entry.r(rxQueue.io.count),
            0x118 -> RegisterMap.Entry(
                width = 8,
                read = RegisterMap.ReadFn(),
                write = RegisterMap.WriteFn((write: Bool, data: UInt) =>
                    when (write && doTransfer) {
                        rxQueue.io.enq.valid := true.B
                        rxQueue.io.enq.bits := data
                    }
                ),
            ),
            0x120 -> RegisterMap.Entry.r(cartPowerOn),
        )
    )

    // 64 KiB read/write memory, 32 bit words with byte mask
    val cpuMem = {
        val mem = SRAM.masked(16 * 1024, Vec(4, UInt(8.W)), numReadPorts = 0, numWritePorts = 0, numReadwritePorts = 2)
        val memHostPort = mem.readwritePorts(0)
        val memDevicePort = mem.readwritePorts(1)

        // Work around a bug in Chisel 7:
        // https://github.com/chipsalliance/chisel/issues/5243
        // Without this, the constant "b1111".U turns into "4'h1" in Verilog in the
        // SRAM write mask, for some reason.
        // Note that due to the current implementation of SpiReceiverFifo,
        //   io.memInterface.writeStrobe is a constant "b1111".
        val writeMask = dontTouch(Wire(Vec(4, Bool())))
        writeMask := VecInit(true.B, true.B, true.B, true.B)

        memHostPort.enable := hostMemInterface.enable
        memHostPort.address := hostMemInterface.address >> 2
        memHostPort.isWrite := hostMemInterface.write
        memHostPort.mask.get := writeMask
        memHostPort.writeData := hostMemInterface.dataWrite.asTypeOf(memHostPort.writeData)
        hostMemInterface.dataRead := memHostPort.readData.asUInt
        hostMemInterface.done := RegNext(memHostPort.enable)

        memDevicePort
    }

    // CPU peripherals
    val regPeriphPmodOut = RegInit(0.U(4.W))
    val regPeriphPmodDir = RegInit(0.U(4.W))
    val regPeriphPmodIn = Reg(UInt(4.W))

    val regPeriphCartIn = RegInit(0.U(30.W))
    val regPeriphCartOut = RegInit(0.U(30.W))
    val regPeriphCartDir = RegInit(0.U(6.W))
    val regPeriphCartEn = RegInit(0.U(1.W))
    val regPeriphCartSwitch = RegInit(0.U(1.W))

    val regPeriphLinkOut = RegInit(0.U(4.W))
    val regPeriphLinkDir = RegInit(0.U(4.W))
    val regPeriphLinkIn = Reg(UInt(4.W))

    val cpuPeriph = RegisterMap(
        addressWidth = 16,
        dataWidth = 32,
        entries = Seq(
            // Host communication
            0x0100 -> RegisterMap.Entry.r(txQueue.io.count),
            0x0104 -> RegisterMap.Entry(
                width = 8,
                read = RegisterMap.ReadFn(),
                write = RegisterMap.WriteFn((write: Bool, data: UInt) => {
                    txQueue.io.enq.valid := write
                    txQueue.io.enq.bits := data
                }),
            ),
            0x0110 -> RegisterMap.Entry.r(rxQueue.io.count),
            0x0114 -> RegisterMap.Entry(
                width = 8,
                read = RegisterMap.ReadFn((read: Bool) => {
                    when (read) {
                        rxQueue.io.deq.ready := true.B
                    }
                    rxQueue.io.deq.bits
                }),
                write = RegisterMap.WriteFn(),
            ),
            // GPIO: PMOD
            0x0200 -> RegisterMap.Entry.w(regPeriphPmodDir),
            0x0204 -> RegisterMap.Entry.w(regPeriphPmodOut),
            0x0208 -> RegisterMap.Entry.r(regPeriphPmodIn),
            // GPIO: Cart
            0x0300 -> RegisterMap.Entry.rw(regPeriphCartDir),
            0x0304 -> RegisterMap.Entry(width = 6, read = RegisterMap.ReadFn(),
                write = RegisterMap.WriteFn((write: Bool, data: UInt) => {
                    when (write) {
                        regPeriphCartDir := regPeriphCartDir | data
                    }
                }),
            ),
            0x0308 -> RegisterMap.Entry(width = 6, read = RegisterMap.ReadFn(),
                write = RegisterMap.WriteFn((write: Bool, data: UInt) => {
                    when (write) {
                        regPeriphCartDir := regPeriphCartDir & (~data).asUInt
                    }
                }),
            ),
            0x0310 -> RegisterMap.Entry.rw(regPeriphCartOut),
            0x0314 -> RegisterMap.Entry(width = 30, read = RegisterMap.ReadFn(),
                write = RegisterMap.WriteFn((write: Bool, data: UInt) => {
                    when (write) {
                        regPeriphCartOut := regPeriphCartOut | data
                    }
                }),
            ),
            0x0318 -> RegisterMap.Entry(width = 30, read = RegisterMap.ReadFn(),
                write = RegisterMap.WriteFn((write: Bool, data: UInt) => {
                    when (write) {
                        regPeriphCartOut := regPeriphCartOut & (~data).asUInt
                    }
                }),
            ),
            0x0320 -> RegisterMap.Entry.r(regPeriphCartIn),
            0x0380 -> RegisterMap.Entry.rw(regPeriphCartEn),
            0x0384 -> RegisterMap.Entry.r(regPeriphCartSwitch),
            // GPIO: Link
            0x0400 -> RegisterMap.Entry.rw(regPeriphLinkDir),
            0x0410 -> RegisterMap.Entry.rw(regPeriphLinkOut),
            0x0420 -> RegisterMap.Entry.r(regPeriphLinkIn),
        )
    )

    io.pmod.out := regPeriphPmodOut
    io.pmod.dir := regPeriphPmodDir
    regPeriphPmodIn := RegNext(io.pmod.in)

    io.cartridge.enabled := regPeriphCartEn
    cartPowerOn := regPeriphCartEn
    io.cartridge.bank0Out := regPeriphCartOut(23, 16) // A16 to A23
    io.cartridge.bank1Out := regPeriphCartOut(15, 8) // AD8 to AD15
    io.cartridge.bank2Out := regPeriphCartOut(7, 0) // AD0 to AD7
    io.cartridge.bank3Out := regPeriphCartOut(27, 24) // [PHI, nWR, nRD, nCS1]
    io.cartridge.pin30Out := regPeriphCartOut(28)
    io.cartridge.pin31Out := regPeriphCartOut(29)
    io.cartridge.bank0Dir := regPeriphCartDir(2)
    io.cartridge.bank1Dir := regPeriphCartDir(1)
    io.cartridge.bank2Dir := regPeriphCartDir(0)
    io.cartridge.bank3Dir := regPeriphCartDir(3)
    io.cartridge.pin30Dir := regPeriphCartDir(4)
    io.cartridge.pin31Dir := regPeriphCartDir(5)
    regPeriphCartIn := RegNext(Cat(
        io.cartridge.pin31In,
        io.cartridge.pin30In,
        io.cartridge.bank3In,
        io.cartridge.bank0In,
        io.cartridge.bank1In,
        io.cartridge.bank2In,
    ))
    regPeriphCartSwitch := RegNext(io.cartridge.switch)

    io.link.soOut := regPeriphPmodOut(3)
    io.link.siOut := regPeriphPmodOut(2)
    io.link.sdOut := regPeriphPmodOut(1)
    io.link.scOut := regPeriphPmodOut(0)
    io.link.soDir := regPeriphPmodDir(3)
    io.link.siDir := regPeriphPmodDir(2)
    io.link.sdDir := regPeriphPmodDir(1)
    io.link.scDir := regPeriphPmodDir(0)
    regPeriphPmodIn := RegNext(Cat(
        io.link.soIn,
        io.link.siIn,
        io.link.sdIn,
        io.link.scIn,
    ))

    // PicoRV32 core
    val cpu = Module(new picorv32)
    cpu.io.clk := clock
    cpu.io.resetn := regCpuResetN
    cpuTrap := cpu.io.trap

    val cpuMemRegion = cpu.io.mem_la_addr(31, 24)

    cpuMem.enable := (cpuMemRegion === 0.U) && (cpu.io.mem_la_read || cpu.io.mem_la_write)
    cpuMem.address := cpu.io.mem_la_addr(15, 2)
    cpuMem.isWrite := cpu.io.mem_la_write
    cpuMem.mask.get := cpu.io.mem_la_wstrb.asTypeOf(cpuMem.mask.get)
    cpuMem.writeData := cpu.io.mem_la_wdata.asTypeOf(cpuMem.writeData)
    val didMemAccess = RegNext(cpuMem.enable)

    cpuPeriph.enable := (cpuMemRegion === 2.U) && (cpu.io.mem_la_read || cpu.io.mem_la_write)
    cpuPeriph.write := cpu.io.mem_la_write
    cpuPeriph.address := cpu.io.mem_la_addr
    cpuPeriph.dataWrite := cpu.io.mem_la_wdata
    cpuPeriph.writeStrobe := DontCare // Unused anyway
    val didPeriphAccess = RegNext(cpuPeriph.enable)
    val regPeriphRead = RegNext(cpuPeriph.dataRead)

    cpu.io.mem_rdata := 0.U
    cpu.io.mem_ready := true.B
    when (didMemAccess) {
        cpu.io.mem_rdata := cpuMem.readData.asUInt
    } .elsewhen (didPeriphAccess) {
        cpu.io.mem_rdata := regPeriphRead
    }
    when (cpu.io.mem_valid && cpu.io.mem_instr) {
        regLastCpuPc := cpu.io.mem_addr
    }

    private def stubUnused(): Unit = {
        io.input.vibrate := HandheldVibrate.Off
        io.audio.left := 0.S
        io.audio.right := 0.S

        // SRAM unused
        io.sram.ceN := true.B
        io.sram.weN := true.B
        io.sram.oeN := true.B
        io.sram.writeMaskN := true.B
        io.sram.address := DontCare
        io.sram.dataOut := DontCare
        io.sram.dataDir := false.B

        // SDRAM unused
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
