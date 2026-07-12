package com.genymobile.gnirehtet.v4

import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import java.io.IOException
import java.io.InputStream
import java.util.UUID
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Test

class Gnr4Test {
    @Test
    fun roundTripsFrame() {
        val session = UUID.randomUUID()
        val expected = Gnr4Frame(Gnr4MessageType.STATUS, session, byteArrayOf(1, 2, 3))
        val bytes = ByteArrayOutputStream().also { Gnr4.write(it, expected) }.toByteArray()
        val actual = Gnr4.read(ByteArrayInputStream(bytes), session)
        assertEquals(expected.type, actual.type)
        assertEquals(expected.sessionId, actual.sessionId)
        assertArrayEquals(expected.payload, actual.payload)
    }

    @Test(expected = IllegalArgumentException::class)
    fun rejectsOversizedWrite() {
        Gnr4.write(
            ByteArrayOutputStream(),
            Gnr4Frame(Gnr4MessageType.ERROR, UUID.randomUUID(), ByteArray(Gnr4.MAX_PAYLOAD + 1)),
        )
    }

    @Test(expected = IOException::class)
    fun rejectsStaleSession() {
        val bytes = ByteArrayOutputStream().also {
            Gnr4.write(it, Gnr4Frame(Gnr4MessageType.HELLO, UUID.randomUUID()))
        }.toByteArray()
        Gnr4.read(ByteArrayInputStream(bytes), UUID.randomUUID())
    }

    @Test
    fun readsFragmentedFrame() {
        val session = UUID.randomUUID()
        val expected = Gnr4Frame(Gnr4MessageType.HEARTBEAT, session, Gnr4.heartbeatPayload(91, 123_456_789))
        val bytes = ByteArrayOutputStream().also { Gnr4.write(it, expected) }.toByteArray()
        val fragmented = object : InputStream() {
            private var offset = 0

            override fun read(): Int = if (offset < bytes.size) bytes[offset++].toInt() and 0xff else -1

            override fun read(target: ByteArray, targetOffset: Int, length: Int): Int {
                if (offset >= bytes.size) return -1
                target[targetOffset] = bytes[offset++]
                return 1
            }
        }

        val actual = Gnr4.read(fragmented, session)
        assertEquals(expected.type, actual.type)
        assertArrayEquals(expected.payload, actual.payload)
    }

    @Test(expected = IOException::class)
    fun rejectsNegativePayloadLength() {
        val session = UUID.randomUUID()
        val bytes = ByteArrayOutputStream().also {
            Gnr4.write(it, Gnr4Frame(Gnr4MessageType.STATUS, session))
        }.toByteArray()
        bytes[8] = 0xff.toByte()
        bytes[9] = 0xff.toByte()
        bytes[10] = 0xff.toByte()
        bytes[11] = 0xff.toByte()
        Gnr4.read(ByteArrayInputStream(bytes), session)
    }

    @Test
    fun matchesSharedCrossLanguageFixture() {
        val expectedHex = requireNotNull(javaClass.classLoader?.getResourceAsStream("gnr4-status-v4.hex")) {
            "shared GNR4 fixture is missing"
        }.bufferedReader().use { it.readText().trim() }
        val frame = Gnr4Frame(
            Gnr4MessageType.STATUS,
            UUID.fromString("00112233-4455-6677-8899-aabbccddeeff"),
            byteArrayOf(1, 2, 3),
        )
        val actualHex = ByteArrayOutputStream()
            .also { Gnr4.write(it, frame) }
            .toByteArray()
            .joinToString("") { "%02x".format(it.toInt() and 0xff) }
        assertEquals(expectedHex, actualHex)
    }

    @Test
    fun heartbeatCarriesSequenceAndMonotonicTimestamp() {
        val payload = Gnr4.heartbeatPayload(52, 987_654_321)
        assertEquals(Gnr4Heartbeat(52, 987_654_321), Gnr4.parseHeartbeatPayload(payload))
        assertEquals(null, Gnr4.parseHeartbeatPayload(ByteArray(8)))
    }
}
