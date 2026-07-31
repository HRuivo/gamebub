package net.gamebub.framework

import chisel3._
import net.gamebub.framework.interface._
import chisel3.reflect.DataMirror

trait Core extends Module {
  final def getInterface(name: String): Option[Data] = {
    val io = DataMirror.modulePorts(this).find((x) => x._1 == "io").map(_._2) match {
      case Some(io: Bundle) => io;
      case Some(_) => throw new CoreException("Core 'io' port must be Bundle");
      case None => throw new CoreException("Core missing 'io' port");
    }
    io.elements.get(name)
  }
}

class CoreException(message: String) extends RuntimeException(message)