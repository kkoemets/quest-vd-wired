package com.genymobile.gnirehtet.v4

import java.io.File
import kotlin.io.path.createTempDirectory
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class HevTunnelLifecycleTest {
    @Test
    fun rejectsAStopForAStaleNativeGeneration() {
        val bridge = FakeBridge(stopAccepted = false)
        val tunnel = HevTunnel(testDirectory(), bridge)
        tunnel.start(tunFd = 42, socksPort = 31_416, udpPort = 31_418)

        val error = runCatching { tunnel.requestStop() }.exceptionOrNull()
        assertTrue(error is IllegalStateException)
        assertEquals(listOf(7L), bridge.stopTokens)
    }

    @Test
    fun clearsTokenOnlyAfterNativeTermination() {
        val bridge = FakeBridge(stopAccepted = true, stoppedResult = 0)
        val tunnel = HevTunnel(testDirectory(), bridge)
        tunnel.start(tunFd = 42, socksPort = 31_416, udpPort = 31_418)
        tunnel.requestStop()

        assertFalse(tunnel.awaitStopped(10))
        assertEquals(4, tunnel.stats().size)
        bridge.stoppedResult = 1
        assertTrue(tunnel.awaitStopped(10))
        assertTrue(tunnel.stats().all { it == 0L })
    }

    private fun testDirectory(): File =
        createTempDirectory("hev-tunnel-test-").toFile().apply { deleteOnExit() }

    private class FakeBridge(
        private val stopAccepted: Boolean,
        var stoppedResult: Int = 1,
    ) : HevNativeBridge {
        val stopTokens = mutableListOf<Long>()

        override fun start(configPath: String, tunFd: Int): Long = 7

        override fun awaitReady(token: Long, timeoutMs: Int): Int = 1

        override fun stop(token: Long): Boolean {
            stopTokens += token
            return stopAccepted
        }

        override fun awaitStopped(token: Long, timeoutMs: Int): Int = stoppedResult

        override fun stats(): LongArray = longArrayOf(1, 2, 3, 4)
    }
}
