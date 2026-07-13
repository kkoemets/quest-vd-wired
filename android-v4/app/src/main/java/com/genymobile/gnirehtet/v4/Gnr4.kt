package com.genymobile.gnirehtet.v4

import java.io.DataInputStream
import java.io.DataOutputStream
import java.io.IOException
import java.io.InputStream
import java.io.OutputStream
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.nio.charset.CharacterCodingException
import java.nio.charset.CodingErrorAction
import java.nio.charset.StandardCharsets
import java.util.UUID

enum class Gnr4MessageType(val wireValue: Int) {
    HELLO(1),
    HELLO_ACK(2),
    STARTED(3),
    HEARTBEAT(4),
    STOP(5),
    STOPPED(6),
    STATUS(7),
    ERROR(8),
    SUSPEND(9),
    SUSPENDED(10),
    METRICS(11);

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

data class Gnr4Metrics(
    val txPackets: Long,
    val txBytes: Long,
    val rxPackets: Long,
    val rxBytes: Long,
    val controlRttSamples: Long,
    val controlRttP99Micros: Long,
    val controlRttMaxMicros: Long,
)

object Gnr4 {
    const val VERSION = 4
    const val MAX_PAYLOAD = 65_536
    const val METRICS_PAYLOAD_SIZE = 60
    const val HELLO_CAPABILITIES = "android-v4;hev-udp-in-tcp"
    private const val METRICS_VERSION = 1
    private const val METRICS_FLAGS = 0
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

    fun metricsPayload(metrics: Gnr4Metrics): ByteArray {
        require(
            metrics.txPackets >= 0 &&
                metrics.txBytes >= 0 &&
                metrics.rxPackets >= 0 &&
                metrics.rxBytes >= 0 &&
                metrics.controlRttSamples >= 0 &&
                metrics.controlRttP99Micros >= 0 &&
                metrics.controlRttMaxMicros >= 0,
        ) { "GNR4 metrics must not be negative" }
        return ByteBuffer.allocate(METRICS_PAYLOAD_SIZE)
            .order(ByteOrder.BIG_ENDIAN)
            .putShort(METRICS_VERSION.toShort())
            .putShort(METRICS_FLAGS.toShort())
            .putLong(metrics.txPackets)
            .putLong(metrics.txBytes)
            .putLong(metrics.rxPackets)
            .putLong(metrics.rxBytes)
            .putLong(metrics.controlRttSamples)
            .putLong(metrics.controlRttP99Micros)
            .putLong(metrics.controlRttMaxMicros)
            .array()
    }

    fun parseMetricsPayload(payload: ByteArray): Gnr4Metrics? {
        if (payload.size != METRICS_PAYLOAD_SIZE) return null
        val buffer = ByteBuffer.wrap(payload).order(ByteOrder.BIG_ENDIAN)
        val version = buffer.short.toInt() and 0xffff
        val flags = buffer.short.toInt() and 0xffff
        if (version != METRICS_VERSION || flags != METRICS_FLAGS) return null
        val values = LongArray(7) { buffer.long }
        if (values.any { it < 0 }) return null
        return Gnr4Metrics(
            txPackets = values[0],
            txBytes = values[1],
            rxPackets = values[2],
            rxBytes = values[3],
            controlRttSamples = values[4],
            controlRttP99Micros = values[5],
            controlRttMaxMicros = values[6],
        )
    }

    fun helloAckSupportsMetrics(payload: ByteArray): Boolean {
        if (payload.isEmpty()) return false
        val text = try {
            StandardCharsets.UTF_8.newDecoder()
                .onMalformedInput(CodingErrorAction.REPORT)
                .onUnmappableCharacter(CodingErrorAction.REPORT)
                .decode(ByteBuffer.wrap(payload))
                .toString()
        } catch (_: CharacterCodingException) {
            return false
        }
        return JsonCapabilityReader(text, "metrics_v1").read() == true
    }
}

private class JsonCapabilityReader(
    private val input: String,
    private val wantedCapability: String,
) {
    private var index = 0
    private var capabilitiesSeen = false
    private var capabilityFound = false

    fun read(): Boolean? {
        skipWhitespace()
        if (!readObject(depth = 0, root = true)) return null
        skipWhitespace()
        return if (index == input.length) capabilityFound else null
    }

    private fun readObject(depth: Int, root: Boolean): Boolean {
        if (depth > MAX_JSON_DEPTH || !consume('{')) return false
        skipWhitespace()
        if (consume('}')) return true
        while (true) {
            val key = readString() ?: return false
            skipWhitespace()
            if (!consume(':')) return false
            skipWhitespace()
            if (root && key == "capabilities") {
                if (capabilitiesSeen) return false
                capabilitiesSeen = true
                capabilityFound = readCapabilities(depth + 1) ?: return false
            } else if (!readValue(depth + 1)) {
                return false
            }
            skipWhitespace()
            if (consume('}')) return true
            if (!consume(',')) return false
            skipWhitespace()
        }
    }

    private fun readCapabilities(depth: Int): Boolean? {
        if (depth > MAX_JSON_DEPTH || !consume('[')) return null
        var found = false
        skipWhitespace()
        if (consume(']')) return false
        while (true) {
            val capability = readString() ?: return null
            if (capability == wantedCapability) found = true
            skipWhitespace()
            if (consume(']')) return found
            if (!consume(',')) return null
            skipWhitespace()
        }
    }

    private fun readValue(depth: Int): Boolean {
        if (depth > MAX_JSON_DEPTH || index >= input.length) return false
        return when (input[index]) {
            '{' -> readObject(depth, root = false)
            '[' -> readArray(depth)
            '"' -> readString() != null
            't' -> readLiteral("true")
            'f' -> readLiteral("false")
            'n' -> readLiteral("null")
            '-', in '0'..'9' -> readNumber()
            else -> false
        }
    }

    private fun readArray(depth: Int): Boolean {
        if (depth > MAX_JSON_DEPTH || !consume('[')) return false
        skipWhitespace()
        if (consume(']')) return true
        while (true) {
            if (!readValue(depth + 1)) return false
            skipWhitespace()
            if (consume(']')) return true
            if (!consume(',')) return false
            skipWhitespace()
        }
    }

    private fun readString(): String? {
        if (!consume('"')) return null
        val result = StringBuilder()
        while (index < input.length) {
            val character = input[index++]
            when {
                character == '"' -> return result.toString()
                character == '\\' -> {
                    if (index >= input.length) return null
                    when (val escape = input[index++]) {
                        '"', '\\', '/' -> result.append(escape)
                        'b' -> result.append('\b')
                        'f' -> result.append('\u000c')
                        'n' -> result.append('\n')
                        'r' -> result.append('\r')
                        't' -> result.append('\t')
                        'u' -> {
                            if (index + 4 > input.length) return null
                            var codePoint = 0
                            repeat(4) {
                                val digit = input[index++].digitToIntOrNull(16) ?: return null
                                codePoint = codePoint * 16 + digit
                            }
                            result.append(codePoint.toChar())
                        }
                        else -> return null
                    }
                }
                character < ' ' -> return null
                else -> result.append(character)
            }
        }
        return null
    }

    private fun readNumber(): Boolean {
        if (consume('-') && index >= input.length) return false
        if (consume('0')) {
            if (index < input.length && input[index].isDigit()) return false
        } else {
            if (index >= input.length || input[index] !in '1'..'9') return false
            while (index < input.length && input[index].isDigit()) index++
        }
        if (consume('.')) {
            if (index >= input.length || !input[index].isDigit()) return false
            while (index < input.length && input[index].isDigit()) index++
        }
        if (index < input.length && input[index] in "eE") {
            index++
            if (index < input.length && input[index] in "+-") index++
            if (index >= input.length || !input[index].isDigit()) return false
            while (index < input.length && input[index].isDigit()) index++
        }
        return true
    }

    private fun readLiteral(value: String): Boolean {
        if (!input.regionMatches(index, value, 0, value.length)) return false
        index += value.length
        return true
    }

    private fun skipWhitespace() {
        while (index < input.length && input[index] in JSON_WHITESPACE) index++
    }

    private fun consume(expected: Char): Boolean {
        if (index >= input.length || input[index] != expected) return false
        index++
        return true
    }

    companion object {
        private const val MAX_JSON_DEPTH = 32
        private const val JSON_WHITESPACE = " \t\r\n"
    }
}
