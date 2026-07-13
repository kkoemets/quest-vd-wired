package com.genymobile.gnirehtet.v4

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class HevTunnelConfigTest {
    @Test
    fun rendersIndependentTcpAndUdpStreamEndpoints() {
        val config = renderHevConfig(mtu = 1_500, socksPort = 31_416, udpPort = 31_418)

        assertTrue(config.contains("port: 31416"))
        assertTrue(config.contains("udp-port: 31418"))
        assertTrue(config.contains("udp: 'tcp'"))
        assertFalse(config.contains("udp-port: 31416"))
    }

    @Test
    fun rendersBoundedPerformanceAndIdlePolicy() {
        val config = renderHevConfig(mtu = 1_500, socksPort = 31_416, udpPort = 31_418)

        assertTrue(config.contains("tcp-buffer-size: 65536"))
        assertTrue(config.contains("max-session-count: 256"))
        assertTrue(config.contains("connect-timeout: 5000"))
        assertTrue(config.contains("tcp-read-write-timeout: 0"))
        assertTrue(config.contains("udp-read-write-timeout: 120000"))
        assertFalse(config.contains("tcp-read-write-timeout: 300000"))
    }

    @Test(expected = IllegalArgumentException::class)
    fun rejectsSharedNativeEndpoints() {
        renderHevConfig(mtu = 1_500, socksPort = 31_416, udpPort = 31_416)
    }
}
