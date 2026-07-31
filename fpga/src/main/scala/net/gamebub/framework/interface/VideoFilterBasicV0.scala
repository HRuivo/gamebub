package net.gamebub.framework.interface

import chisel3._
import chisel3.util._
import lib.video.ColorRGB

class VideoFilterBasicV0(
  /// Color depth of the input video (must match Video interface)
  val colorInDepth: Int,
  /// Latency (in cycles) from filter input to output (pipeline depth)
  val latency: Int,
) extends Bundle {
  val clock = Input(Clock())
  val reset = Input(Reset())

  val dataIn = Input(ColorRGB(colorInDepth))

  val dataOut = Output(ColorRGB(8))
}
