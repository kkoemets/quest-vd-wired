package com.genymobile.gnirehtet.v4

import android.util.Log
import java.io.Closeable
import java.io.IOException
import java.net.InetSocketAddress
import java.net.Socket
import java.net.SocketTimeoutException
import java.nio.charset.StandardCharsets
import java.util.UUID
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong
import kotlin.math.min

internal fun nextControlReconnectDelayMs(current: Long): Long {
    require(current >= 0) { "current delay must not be negative" }
    return min(current.saturatingMultiplyByTwo(), 1_000L)
}

private fun Long.saturatingMultiplyByTwo(): Long =
    if (this > Long.MAX_VALUE / 2) Long.MAX_VALUE else this * 2

class ControlSupervisor(
    private val sessionId: UUID,
    private val port: Int,
    private val listener: Listener,
) : Closeable {
    interface Listener {
        fun shouldReportStarted(): Boolean

        fun onControlConnected()

        fun onControlDegraded(error: Exception?)

        fun onControlRttSample(rttNanos: Long)

        fun onControlStopRequested(sendStopped: () -> Unit)
    }

    private val running = AtomicBoolean()
    private val sequence = AtomicLong()
    private val writeLock = Any()
    @Volatile private var socket: Socket? = null
    @Volatile private var thread: Thread? = null

    init {
        require(port in 1_024..65_535) { "control port is outside 1024..65535" }
    }

    fun start() {
        if (!running.compareAndSet(false, true)) return
        thread = Thread(::runLoop, "gnr4-control").apply { start() }
    }

    private fun runLoop() {
        var delayMs = 250L
        while (running.get()) {
            try {
                Socket().use { connected ->
                    connected.connect(InetSocketAddress(IPV4_LOOPBACK, port), CONNECT_TIMEOUT_MS)
                    socket = connected
                    connected.tcpNoDelay = true
                    connected.soTimeout = 1_000
                    val hello = "android-v4;hev-udp-in-tcp".toByteArray(StandardCharsets.UTF_8)
                    write(connected, Gnr4Frame(Gnr4MessageType.HELLO, sessionId, hello))
                    val acknowledgement = Gnr4.read(connected.getInputStream(), sessionId)
                    if (acknowledgement.type != Gnr4MessageType.HELLO_ACK) {
                        throw IllegalStateException("Expected HELLO_ACK, got ${acknowledgement.type}")
                    }
                    if (!listener.shouldReportStarted()) return
                    write(connected, Gnr4Frame(Gnr4MessageType.STARTED, sessionId))
                    listener.onControlConnected()
                    delayMs = 250L
                    var missed = 0
                    val outstandingHeartbeats = linkedMapOf<Long, Long>()
                    while (running.get() && !connected.isClosed) {
                        try {
                            val frame = Gnr4.read(connected.getInputStream(), sessionId)
                            when (frame.type) {
                                Gnr4MessageType.HEARTBEAT -> {
                                    missed = 0
                                    val heartbeat = Gnr4.parseHeartbeatPayload(frame.payload)
                                        ?: throw IOException("HEARTBEAT payload must be exactly 16 bytes")
                                    val sentAt = outstandingHeartbeats[heartbeat.sequence]
                                    if (sentAt == heartbeat.monotonicNanos) {
                                        outstandingHeartbeats.remove(heartbeat.sequence)
                                        listener.onControlRttSample(System.nanoTime() - sentAt)
                                    } else {
                                        write(
                                            connected,
                                            Gnr4Frame(Gnr4MessageType.HEARTBEAT, sessionId, frame.payload),
                                        )
                                    }
                                }
                                Gnr4MessageType.STATUS -> missed = 0
                                Gnr4MessageType.STOP -> {
                                    val acknowledgementSent = CountDownLatch(1)
                                    val acknowledgementClaimed = AtomicBoolean()
                                    listener.onControlStopRequested {
                                        if (acknowledgementClaimed.compareAndSet(false, true)) {
                                            try {
                                                write(connected, Gnr4Frame(Gnr4MessageType.STOPPED, sessionId))
                                            } finally {
                                                acknowledgementSent.countDown()
                                            }
                                        }
                                    }
                                    acknowledgementSent.await(STOP_ACK_TIMEOUT_MS, TimeUnit.MILLISECONDS)
                                    return
                                }
                                Gnr4MessageType.ERROR -> throw IllegalStateException("Host reported a control error")
                                else -> Unit
                            }
                        } catch (_: SocketTimeoutException) {
                            val heartbeatSequence = sequence.incrementAndGet()
                            val sentAt = System.nanoTime()
                            while (outstandingHeartbeats.size >= MAX_OUTSTANDING_HEARTBEATS) {
                                outstandingHeartbeats.remove(outstandingHeartbeats.keys.first())
                            }
                            outstandingHeartbeats[heartbeatSequence] = sentAt
                            write(
                                connected,
                                Gnr4Frame(
                                    Gnr4MessageType.HEARTBEAT,
                                    sessionId,
                                    Gnr4.heartbeatPayload(heartbeatSequence, sentAt),
                                ),
                            )
                            missed += 1
                            if (missed >= 3) throw SocketTimeoutException("Three host heartbeats missed")
                        }
                    }
                }
            } catch (error: Exception) {
                if (running.get()) {
                    Log.w(TAG, "Control lane degraded", error)
                    listener.onControlDegraded(error)
                    try {
                        Thread.sleep(delayMs)
                    } catch (_: InterruptedException) {
                        Thread.currentThread().interrupt()
                    }
                    delayMs = nextControlReconnectDelayMs(delayMs)
                }
            } finally {
                socket = null
            }
        }
    }

    override fun close() {
        if (!running.compareAndSet(true, false)) return
        socket?.close()
        thread?.interrupt()
        if (Thread.currentThread() !== thread) {
            thread?.join(2_000)
        }
        thread = null
    }

    private fun write(connected: Socket, frame: Gnr4Frame) {
        synchronized(writeLock) {
            Gnr4.write(connected.getOutputStream(), frame)
        }
    }

    companion object {
        private const val TAG = "Gnr4Control"
        private const val CONNECT_TIMEOUT_MS = 1_000
        private const val STOP_ACK_TIMEOUT_MS = 5_000L
        private const val MAX_OUTSTANDING_HEARTBEATS = 8
        private val IPV4_LOOPBACK = java.net.InetAddress.getByAddress(byteArrayOf(127, 0, 0, 1))
    }
}
