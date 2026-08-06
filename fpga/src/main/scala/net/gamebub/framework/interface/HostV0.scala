package net.gamebub.framework.interface

import chisel3._
import lib.video.ColorGrayscale
import lib.video.Color
import lib.mem.MemoryInterface

class HostV0(
    private val overlayColorDepth: Color = ColorGrayscale(1, 3)
) extends Bundle {
    val enable = Input(Bool())
    val reset = Input(Bool())

    val mem = new MemoryInterface(addressWidth = 32, dataWidth = 32)

    // TODO

    def getOverlayColorDepth: Color = overlayColorDepth
}
