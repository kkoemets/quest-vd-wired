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
import java.util.concurrent.atomic.AtomicReference
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
    private val paused = AtomicBoolean()
    private val sequence = AtomicLong()
    private val writeLock = Any()
    private val pauseLock = Object()
    private val suspendAcknowledgement = AtomicReference<CountDownLatch?>()
    private val controlReady = AtomicBoolean()
    private val stopRequested = AtomicBoolean()
    @Volatile private var socket: Socket? = null
    @Volatile private var thread: Thread? = null

    init {
        require(port in 1_024..65_535) { "control port is outside 1024..65535" }
    }

    fun start(startPaused: Boolean = false) {
        synchronized(pauseLock) {
            if (!running.compareAndSet(false, true)) return
            paused.set(startPaused || paused.get())
            thread = Thread(::runLoop, "gnr4-control").apply { start() }
        }
    }

    fun suspend() {
        var connected: Socket? = null
        var acknowledgement: CountDownLatch? = null
        synchronized(pauseLock) {
            if (!paused.compareAndSet(false, true)) return
            connected = socket
            if (controlReady.get() && connected != null && !connected!!.isClosed) {
                acknowledgement = CountDownLatch(1)
                suspendAcknowledgement.set(acknowledgement)
            }
        }
        if (acknowledgement != null) {
            runCatching {
                write(connected!!, Gnr4Frame(Gnr4MessageType.SUSPEND, sessionId))
            }.onFailure {
                suspendAcknowledgement.compareAndSet(acknowledgement, null)
                acknowledgement = null
            }
        }
        try {
            acknowledgement?.await(SUSPEND_ACK_TIMEOUT_MS, TimeUnit.MILLISECONDS)
        } catch (_: InterruptedException) {
            Thread.currentThread().interrupt()
        }
        suspendAcknowledgement.compareAndSet(acknowledgement, null)
        if (!stopRequested.get()) connected?.close()
    }

    fun resume() {
        synchronized(pauseLock) {
            if (!paused.compareAndSet(true, false)) return
            pauseLock.notifyAll()
        }
    }

    private fun runLoop() {
        var delayMs = 250L
        while (running.get()) {
            if (!awaitResume()) return
            try {
                Socket().use { connected ->
                    synchronized(pauseLock) {
                        if (!running.get() || paused.get()) return@use
                        socket = connected
                    }
                    connected.connect(InetSocketAddress(IPV4_LOOPBACK, port), CONNECT_TIMEOUT_MS)
                    connected.tcpNoDelay = true
                    connected.soTimeout = 1_000
                    synchronized(pauseLock) {
                        if (!isActiveSocket(connected)) return@use
                        val hello = "android-v4;hev-udp-in-tcp".toByteArray(StandardCharsets.UTF_8)
                        write(connected, Gnr4Frame(Gnr4MessageType.HELLO, sessionId, hello))
                    }
                    val acknowledgement = Gnr4.read(connected.getInputStream(), sessionId)
                    if (acknowledgement.type != Gnr4MessageType.HELLO_ACK) {
                        throw IllegalStateException("Expected HELLO_ACK, got ${acknowledgement.type}")
                    }
                    synchronized(pauseLock) {
                        if (!isActiveSocket(connected) || !listener.shouldReportStarted()) return@use
                        write(connected, Gnr4Frame(Gnr4MessageType.STARTED, sessionId))
                        controlReady.set(true)
                        listener.onControlConnected()
                    }
                    delayMs = 250L
                    var missed = 0
                    val outstandingHeartbeats = linkedMapOf<Long, Long>()
                    while (
                        running.get() &&
                        (!paused.get() || suspendAcknowledgement.get() != null) &&
                        !connected.isClosed
                    ) {
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
                                Gnr4MessageType.SUSPENDED -> {
                                    missed = 0
                                    suspendAcknowledgement.getAndSet(null)?.countDown()
                                }
                                Gnr4MessageType.STOP -> {
                                    stopRequested.set(true)
                                    suspendAcknowledgement.getAndSet(null)?.countDown()
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
                            if (paused.get()) continue
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
                if (running.get() && !paused.get()) {
                    Log.w(TAG, "Control lane degraded", error)
                    listener.onControlDegraded(error)
                    waitForReconnect(delayMs)
                    delayMs = nextControlReconnectDelayMs(delayMs)
                }
            } finally {
                controlReady.set(false)
                suspendAcknowledgement.getAndSet(null)?.countDown()
                synchronized(pauseLock) {
                    socket = null
                }
            }
        }
    }

    override fun close() {
        synchronized(pauseLock) {
            if (!running.compareAndSet(true, false)) return
            paused.set(false)
            controlReady.set(false)
            suspendAcknowledgement.getAndSet(null)?.countDown()
            socket?.close()
            pauseLock.notifyAll()
        }
        thread?.interrupt()
        if (Thread.currentThread() !== thread) {
            thread?.join(2_000)
        }
        thread = null
    }

    private fun isActiveSocket(connected: Socket): Boolean =
        running.get() && !paused.get() && socket === connected && !connected.isClosed

    private fun awaitResume(): Boolean {
        synchronized(pauseLock) {
            while (running.get() && paused.get()) {
                try {
                    pauseLock.wait()
                } catch (_: InterruptedException) {
                    if (!running.get()) return false
                }
            }
        }
        return running.get()
    }

    private fun waitForReconnect(delayMs: Long) {
        synchronized(pauseLock) {
            if (!running.get() || paused.get()) return
            try {
                pauseLock.wait(delayMs)
            } catch (_: InterruptedException) {
                if (!running.get()) Thread.currentThread().interrupt()
            }
        }
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
        private const val SUSPEND_ACK_TIMEOUT_MS = 500L
        private const val MAX_OUTSTANDING_HEARTBEATS = 8
        private val IPV4_LOOPBACK = java.net.InetAddress.getByAddress(byteArrayOf(127, 0, 0, 1))
    }
}
