package com.genymobile.gnirehtet.v4

import java.io.DataInputStream
import java.io.DataOutputStream
import java.io.IOException
import java.io.InputStream
import java.io.OutputStream
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.util.UUID

enum class Gnr4MessageType(val wireValue: Int) {
    HELLO(1),
    HELLO_ACK(2),
    STARTED(3),
    HEARTBEAT(4),
    STOP(5),
    STOPPED(6),
    STATUS(7),
    ERROR(8);

    companion object {
        fun fromWire(value: Int): Gnr4MessageType = entries.firstOrNull { it.wireValue == value }
            ?: throw IOException("Unknown GNR4 message type: $value")
    }
}

data class Gnr4Frame(
    val type: Gnr4MessageType,
    val sessionId: UUID,
    val payload: ByteArray = ByteArray(0),
)

data class Gnr4Heartbeat(
    val sequence: Long,
    val monotonicNanos: Long,
)

object Gnr4 {
    const val VERSION = 4
    const val MAX_PAYLOAD = 65_536
    private val MAGIC = byteArrayOf('G'.code.toByte(), 'N'.code.toByte(), 'R'.code.toByte(), '4'.code.toByte())

    fun write(output: OutputStream, frame: Gnr4Frame) {
        require(frame.payload.size <= MAX_PAYLOAD) { "GNR4 payload exceeds $MAX_PAYLOAD bytes" }
        val stream = DataOutputStream(output)
        stream.write(MAGIC)
        stream.writeShort(VERSION)
        stream.writeShort(frame.type.wireValue)
        stream.writeInt(frame.payload.size)
        stream.writeLong(frame.sessionId.mostSignificantBits)
        stream.writeLong(frame.sessionId.leastSignificantBits)
        stream.write(frame.payload)
        stream.flush()
    }

    fun read(input: InputStream, expectedSession: UUID? = null): Gnr4Frame {
        val stream = DataInputStream(input)
        val magic = ByteArray(MAGIC.size)
        stream.readFully(magic)
        if (!magic.contentEquals(MAGIC)) {
            throw IOException("Invalid GNR4 magic")
        }
        val version = stream.readUnsignedShort()
        if (version != VERSION) {
            throw IOException("Unsupported GNR4 version: $version")
        }
        val type = Gnr4MessageType.fromWire(stream.readUnsignedShort())
        val length = stream.readInt()
        if (length !in 0..MAX_PAYLOAD) {
            throw IOException("Invalid GNR4 payload length: $length")
        }
        val session = UUID(stream.readLong(), stream.readLong())
        if (expectedSession != null && session != expectedSession) {
            throw IOException("Stale GNR4 session")
        }
        val payload = ByteArray(length)
        stream.readFully(payload)
        return Gnr4Frame(type, session, payload)
    }

    fun heartbeatPayload(sequence: Long, monotonicNanos: Long): ByteArray =
        ByteBuffer.allocate(Long.SIZE_BYTES * 2)
        .order(ByteOrder.BIG_ENDIAN)
        .putLong(sequence)
        .putLong(monotonicNanos)
        .array()

    fun parseHeartbeatPayload(payload: ByteArray): Gnr4Heartbeat? {
        if (payload.size != Long.SIZE_BYTES * 2) return null
        val buffer = ByteBuffer.wrap(payload).order(ByteOrder.BIG_ENDIAN)
        return Gnr4Heartbeat(buffer.long, buffer.long)
    }
}
