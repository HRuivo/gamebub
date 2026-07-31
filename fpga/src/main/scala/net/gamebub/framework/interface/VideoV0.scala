package net.gamebub.framework.interface

import chisel3._
import chisel3.util._
import lib.video.ColorRGB

class VideoV0(
  /// Width of the video output, in pixels
  val videoWidth: Int,
  /// Height of the video output, in pixels
  val videoHeight: Int,
  /// Bits per each R, G, B color channel
  val colorDepth: Int,
  /// Target frame period, in seconds.
  val framePeriod: Double,
) extends Bundle {
  val data = Output(ColorRGB.apply(colorDepth))
  val dataEnable = Output(Bool())
  val vblank = Output(Bool())
  val hblank = Output(Bool())
}
