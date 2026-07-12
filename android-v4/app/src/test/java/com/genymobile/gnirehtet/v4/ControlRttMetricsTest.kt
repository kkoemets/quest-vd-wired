package com.genymobile.gnirehtet.v4

import org.junit.Assert.assertEquals
import org.junit.Test

class ControlRttMetricsTest {
    @Test
    fun reportsBoundedP99AndMaximum() {
        val metrics = ControlRttMetrics()
        repeat(99) { metrics.record(1_500_000) }
        metrics.record(4_500_000)

        val snapshot = metrics.snapshot()
        assertEquals(100, snapshot.samples)
        assertEquals(2_000, snapshot.p99Micros)
        assertEquals(4_500, snapshot.maxMicros)
        assertEquals(100, snapshot.histogram.sum())
    }

    @Test
    fun rejectsImpossibleSamplesAndResets() {
        val metrics = ControlRttMetrics()
        metrics.record(-1)
        metrics.record(60_000_000_001)
        assertEquals(0, metrics.snapshot().samples)
        metrics.record(500_000)
        metrics.reset()
        assertEquals(0, metrics.snapshot().samples)
    }
}
