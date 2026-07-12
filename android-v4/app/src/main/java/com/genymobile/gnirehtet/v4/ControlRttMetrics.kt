package com.genymobile.gnirehtet.v4

import kotlin.math.ceil

internal data class ControlRttSnapshot(
    val samples: Long,
    val p99Micros: Long,
    val maxMicros: Long,
    val histogram: LongArray,
)

internal class ControlRttMetrics {
    private val buckets = LongArray(BUCKET_UPPER_BOUNDS_NANOS.size + 1)
    private var samples = 0L
    private var maxNanos = 0L

    @Synchronized
    fun reset() {
        buckets.fill(0)
        samples = 0
        maxNanos = 0
    }

    @Synchronized
    fun record(rttNanos: Long) {
        if (rttNanos !in 0..MAX_SAMPLE_NANOS) return
        val index = BUCKET_UPPER_BOUNDS_NANOS.indexOfFirst { rttNanos <= it }
            .let { if (it < 0) buckets.lastIndex else it }
        buckets[index]++
        samples++
        if (rttNanos > maxNanos) maxNanos = rttNanos
    }

    @Synchronized
    fun snapshot(): ControlRttSnapshot {
        if (samples == 0L) return ControlRttSnapshot(0, 0, 0, buckets.copyOf())
        val target = ceil(samples * 0.99).toLong().coerceAtLeast(1)
        var cumulative = 0L
        var percentileNanos = MAX_SAMPLE_NANOS
        for (index in buckets.indices) {
            cumulative += buckets[index]
            if (cumulative >= target) {
                percentileNanos = BUCKET_UPPER_BOUNDS_NANOS.getOrElse(index) { MAX_SAMPLE_NANOS }
                break
            }
        }
        return ControlRttSnapshot(
            samples,
            percentileNanos / NANOS_PER_MICROSECOND,
            maxNanos / NANOS_PER_MICROSECOND,
            buckets.copyOf(),
        )
    }

    companion object {
        private const val NANOS_PER_MICROSECOND = 1_000L
        private const val MAX_SAMPLE_NANOS = 60_000_000_000L
        private val BUCKET_UPPER_BOUNDS_NANOS = longArrayOf(
            250_000,
            500_000,
            1_000_000,
            2_000_000,
            5_000_000,
            10_000_000,
            25_000_000,
            50_000_000,
            100_000_000,
        )
    }
}
