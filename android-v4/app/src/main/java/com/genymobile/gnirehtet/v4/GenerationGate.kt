package com.genymobile.gnirehtet.v4

internal class GenerationGate {
    private var sequence = 0L
    private var current = 0L

    @Synchronized
    fun begin(): Long {
        sequence = next(sequence)
        current = sequence
        return current
    }

    @Synchronized
    fun isCurrent(generation: Long): Boolean = generation != 0L && current == generation

    @Synchronized
    fun invalidate(generation: Long): Boolean {
        if (!isCurrent(generation)) return false
        current = 0L
        return true
    }

    private fun next(value: Long): Long = if (value == Long.MAX_VALUE) 1L else value + 1L
}
