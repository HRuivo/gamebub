package net.gamebub.framework

import chisel3._
import net.gamebub.framework.interface._

abstract class CoreIo extends Bundle {
  val clocks: ClocksV0
  val video: VideoV0
  val audio: AudioV0
  val host: HostV0
  val pmod: PmodV0
  val input: InputV0
  val cartridge: CartridgePortV0
  val link: LinkPortV0
  val sram: SramV0
  val sdram: SdramV0
}

trait Core extends Module {
  def io: CoreIo
}

class CoreException(message: String) extends RuntimeException(message)