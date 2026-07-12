package com.genymobile.gnirehtet.v4

import org.junit.Test

class VdAppIdentityValidatorTest {
    @Test
    fun acceptsDedicatedApplicationUid() {
        VdAppIdentityValidator.validate(
            VdAppIdentity("VirtualDesktop.Android", 10_321, false, setOf("VirtualDesktop.Android")),
            "com.genymobile.gnirehtet",
            10_999,
        )
    }

    @Test(expected = IllegalArgumentException::class)
    fun rejectsSystemUid() {
        VdAppIdentityValidator.validate(
            VdAppIdentity("VirtualDesktop.Android", 1_000, true, setOf("VirtualDesktop.Android")),
            "com.genymobile.gnirehtet",
            10_999,
        )
    }

    @Test(expected = IllegalArgumentException::class)
    fun rejectsOwnUid() {
        VdAppIdentityValidator.validate(
            VdAppIdentity("VirtualDesktop.Android", 10_999, false, setOf("VirtualDesktop.Android")),
            "com.genymobile.gnirehtet",
            10_999,
        )
    }

    @Test(expected = IllegalArgumentException::class)
    fun rejectsSharedUid() {
        VdAppIdentityValidator.validate(
            VdAppIdentity(
                "VirtualDesktop.Android",
                10_321,
                false,
                setOf("VirtualDesktop.Android", "com.example.unexpected"),
            ),
            "com.genymobile.gnirehtet",
            10_999,
        )
    }
}
