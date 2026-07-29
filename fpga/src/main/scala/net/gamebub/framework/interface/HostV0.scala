package net.gamebub.framework.interface

import chisel3._
import lib.mem.MemoryInterface

object HostV0 {
    val CommandGetStatus = 0x0000
    val CommandCoreRun = 0x0100
    val CommandCoreHalt = 0x0101
    val CommandNotifyFocus = 0x0200

    class CommandChannel extends Bundle {
        /** Whether a request is active: held high for the duration of the request. */
        val request = Input(Bool())
        /** High when the target acknowledges the command. */
        val busy = Output(Bool())
        /** High when the command is completed. */
        val done = Output(Bool())
        /** If high, indicates that the command completed with an error. */
        val error = Output(Bool())
    }
}

class HostV0 extends Bundle {
    val mem = new MemoryInterface(addressWidth = 32, dataWidth = 32)

    /** Command channel for Host -> Core commands **/
    val commandHost = new HostV0.CommandChannel
    /** Command channel for Core -> Host commands **/
    val commandCore = Flipped(new HostV0.CommandChannel)
}
