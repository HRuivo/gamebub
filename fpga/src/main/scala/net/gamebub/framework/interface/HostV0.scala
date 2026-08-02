package net.gamebub.framework.interface

import chisel3._
import lib.mem.MemoryInterface

class HostV0 extends Bundle {
    val enable = Input(Bool())
    val reset = Input(Bool())

    val mem = new MemoryInterface(addressWidth = 32, dataWidth = 32)
}
