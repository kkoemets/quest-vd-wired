package com.genymobile.gnirehtet.v4

internal class StopAcknowledgements {
    enum class Registration {
        QUEUED,
        RUN_NOW,
        REJECTED,
    }

    private enum class DescriptorState {
        OPEN,
        CLOSED,
        FAILED,
    }

    private var descriptorState = DescriptorState.OPEN
    private val waiters = mutableListOf<() -> Unit>()

    @Synchronized
    fun reset() {
        check(waiters.isEmpty()) { "Cannot reset with pending STOP acknowledgements" }
        descriptorState = DescriptorState.OPEN
    }

    @Synchronized
    fun register(waiter: () -> Unit): Registration = when (descriptorState) {
        DescriptorState.OPEN -> {
            waiters += waiter
            Registration.QUEUED
        }
        DescriptorState.CLOSED -> Registration.RUN_NOW
        DescriptorState.FAILED -> Registration.REJECTED
    }

    @Synchronized
    fun descriptorClosed(): List<() -> Unit> {
        if (descriptorState != DescriptorState.OPEN) return emptyList()
        descriptorState = DescriptorState.CLOSED
        return drain()
    }

    @Synchronized
    fun descriptorCloseFailed() {
        if (descriptorState == DescriptorState.OPEN) {
            descriptorState = DescriptorState.FAILED
            waiters.clear()
        }
    }

    private fun drain(): List<() -> Unit> = waiters.toList().also { waiters.clear() }
}
