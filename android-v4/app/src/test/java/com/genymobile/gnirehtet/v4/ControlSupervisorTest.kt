package com.genymobile.gnirehtet.v4

import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.ServerSocket
import java.util.UUID
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class ControlSupervisorTest {
    @Test
    fun reconnectBackoffStaysInsideLifecycleGate() {
        assertEquals(500L, nextControlReconnectDelayMs(250L))
        assertEquals(1_000L, nextControlReconnectDelayMs(500L))
        assertEquals(1_000L, nextControlReconnectDelayMs(1_000L))
        assertEquals(1_000L, nextControlReconnectDelayMs(Long.MAX_VALUE))
    }

    @Test
    fun usesIpv4LoopbackAndAcknowledgesStop() {
        val session = UUID.fromString("00112233-4455-6677-8899-aabbccddeeff")
        val server = ServerSocket().apply {
            bind(InetSocketAddress(InetAddress.getByName("127.0.0.1"), 0))
        }
        val executor = Executors.newSingleThreadExecutor()
        val exchange = executor.submit {
            server.use {
                it.accept().use { connection ->
                    assertEquals("127.0.0.1", connection.inetAddress.hostAddress)
                    assertEquals(Gnr4MessageType.HELLO, Gnr4.read(connection.getInputStream(), session).type)
                    Gnr4.write(
                        connection.getOutputStream(),
                        Gnr4Frame(Gnr4MessageType.HELLO_ACK, session),
                    )
                    assertEquals(Gnr4MessageType.STARTED, Gnr4.read(connection.getInputStream(), session).type)
                    Gnr4.write(connection.getOutputStream(), Gnr4Frame(Gnr4MessageType.STOP, session))
                    assertEquals(Gnr4MessageType.STOPPED, Gnr4.read(connection.getInputStream(), session).type)
                }
            }
        }
        val connected = CountDownLatch(1)
        val supervisor = ControlSupervisor(
            session,
            server.localPort,
            object : ControlSupervisor.Listener {
                override fun shouldReportStarted(): Boolean = true

                override fun onControlConnected() {
                    connected.countDown()
                }

                override fun onControlDegraded(error: Exception?) = Unit

                override fun onControlRttSample(rttNanos: Long) = Unit

                override fun onControlStopRequested(sendStopped: () -> Unit) = sendStopped()
            },
        )

        supervisor.start()
        assertTrue(connected.await(2, TimeUnit.SECONDS))
        exchange.get(2, TimeUnit.SECONDS)
        supervisor.close()
        executor.shutdownNow()
    }

    @Test
    fun suspendedSupervisorWaitsForWakeBeforeConnecting() {
        val session = UUID.fromString("10213243-5465-7687-98a9-bacbdcedfe0f")
        val server = ServerSocket().apply {
            bind(InetSocketAddress(InetAddress.getByName("127.0.0.1"), 0))
        }
        val executor = Executors.newSingleThreadExecutor()
        val exchange = executor.submit {
            server.use {
                it.accept().use { connection ->
                    assertEquals(Gnr4MessageType.HELLO, Gnr4.read(connection.getInputStream(), session).type)
                    Gnr4.write(
                        connection.getOutputStream(),
                        Gnr4Frame(Gnr4MessageType.HELLO_ACK, session),
                    )
                    assertEquals(Gnr4MessageType.STARTED, Gnr4.read(connection.getInputStream(), session).type)
                }
            }
        }
        val connected = CountDownLatch(1)
        val supervisor = ControlSupervisor(
            session,
            server.localPort,
            object : ControlSupervisor.Listener {
                override fun shouldReportStarted(): Boolean = true
                override fun onControlConnected() = connected.countDown()
                override fun onControlDegraded(error: Exception?) = Unit
                override fun onControlRttSample(rttNanos: Long) = Unit
                override fun onControlStopRequested(sendStopped: () -> Unit) = Unit
            },
        )

        supervisor.start(startPaused = true)
        assertFalse(connected.await(150, TimeUnit.MILLISECONDS))
        supervisor.resume()
        assertTrue(connected.await(2, TimeUnit.SECONDS))
        exchange.get(2, TimeUnit.SECONDS)
        supervisor.close()
        executor.shutdownNow()
    }

    @Test
    fun suspendIsAcknowledgedBeforeTheControlConnectionCloses() {
        val session = UUID.fromString("20314253-6475-8697-a8b9-cadbecfd0e1f")
        val server = ServerSocket().apply {
            bind(InetSocketAddress(InetAddress.getByName("127.0.0.1"), 0))
        }
        val executor = Executors.newSingleThreadExecutor()
        val exchange = executor.submit {
            server.use {
                it.accept().use { connection ->
                    assertEquals(Gnr4MessageType.HELLO, Gnr4.read(connection.getInputStream(), session).type)
                    Gnr4.write(
                        connection.getOutputStream(),
                        Gnr4Frame(Gnr4MessageType.HELLO_ACK, session),
                    )
                    assertEquals(Gnr4MessageType.STARTED, Gnr4.read(connection.getInputStream(), session).type)
                    assertEquals(Gnr4MessageType.SUSPEND, Gnr4.read(connection.getInputStream(), session).type)
                    Gnr4.write(
                        connection.getOutputStream(),
                        Gnr4Frame(Gnr4MessageType.SUSPENDED, session),
                    )
                    assertEquals(-1, connection.getInputStream().read())
                }
            }
        }
        val connected = CountDownLatch(1)
        val supervisor = ControlSupervisor(
            session,
            server.localPort,
            object : ControlSupervisor.Listener {
                override fun shouldReportStarted(): Boolean = true
                override fun onControlConnected() = connected.countDown()
                override fun onControlDegraded(error: Exception?) = Unit
                override fun onControlRttSample(rttNanos: Long) = Unit
                override fun onControlStopRequested(sendStopped: () -> Unit) = Unit
            },
        )

        supervisor.start()
        assertTrue(connected.await(2, TimeUnit.SECONDS))
        supervisor.suspend()
        exchange.get(2, TimeUnit.SECONDS)
        supervisor.close()
        executor.shutdownNow()
    }

    @Test
    fun missingSuspendAcknowledgementStillClosesWithinTheBound() {
        val session = UUID.fromString("30415263-7485-96a7-b8c9-daebfc0d1e2f")
        val server = ServerSocket().apply {
            bind(InetSocketAddress(InetAddress.getByName("127.0.0.1"), 0))
        }
        val executor = Executors.newSingleThreadExecutor()
        val exchange = executor.submit {
            server.use {
                it.accept().use { connection ->
                    assertEquals(Gnr4MessageType.HELLO, Gnr4.read(connection.getInputStream(), session).type)
                    Gnr4.write(
                        connection.getOutputStream(),
                        Gnr4Frame(Gnr4MessageType.HELLO_ACK, session),
                    )
                    assertEquals(Gnr4MessageType.STARTED, Gnr4.read(connection.getInputStream(), session).type)
                    assertEquals(Gnr4MessageType.SUSPEND, Gnr4.read(connection.getInputStream(), session).type)
                    assertEquals(-1, connection.getInputStream().read())
                }
            }
        }
        val connected = CountDownLatch(1)
        val supervisor = ControlSupervisor(
            session,
            server.localPort,
            object : ControlSupervisor.Listener {
                override fun shouldReportStarted(): Boolean = true
                override fun onControlConnected() = connected.countDown()
                override fun onControlDegraded(error: Exception?) = Unit
                override fun onControlRttSample(rttNanos: Long) = Unit
                override fun onControlStopRequested(sendStopped: () -> Unit) = Unit
            },
        )

        supervisor.start()
        assertTrue(connected.await(2, TimeUnit.SECONDS))
        val started = System.nanoTime()
        supervisor.suspend()
        val elapsedMs = TimeUnit.NANOSECONDS.toMillis(System.nanoTime() - started)
        assertTrue("suspend exceeded its bounded wait: ${elapsedMs}ms", elapsedMs < 1_000)
        exchange.get(2, TimeUnit.SECONDS)
        supervisor.close()
        executor.shutdownNow()
    }
}
