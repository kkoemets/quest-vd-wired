package com.genymobile.gnirehtet.v4

import android.view.Display
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class HeadsetDisplayStateTest {
    @Test
    fun onAndVrStatesAreAwakeOnlyWhileInteractive() {
        assertFalse(isHeadsetDisplaySuspended(Display.STATE_ON, isInteractive = true))
        assertFalse(isHeadsetDisplaySuspended(Display.STATE_VR, isInteractive = true))
        assertTrue(isHeadsetDisplaySuspended(Display.STATE_ON, isInteractive = false))
        assertTrue(isHeadsetDisplaySuspended(Display.STATE_VR, isInteractive = false))
    }

    @Test
    fun offAndSuspendedStatesAreAsleep() {
        assertTrue(isHeadsetDisplaySuspended(Display.STATE_OFF, isInteractive = true))
        assertTrue(isHeadsetDisplaySuspended(Display.STATE_DOZE, isInteractive = true))
        assertTrue(isHeadsetDisplaySuspended(Display.STATE_DOZE_SUSPEND, isInteractive = true))
        assertTrue(isHeadsetDisplaySuspended(Display.STATE_ON_SUSPEND, isInteractive = true))
    }

    @Test
    fun unavailableStateFallsBackToInteractiveState() {
        assertTrue(isHeadsetDisplaySuspended(null, isInteractive = false))
        assertFalse(isHeadsetDisplaySuspended(null, isInteractive = true))
        assertTrue(isHeadsetDisplaySuspended(Display.STATE_UNKNOWN, isInteractive = false))
        assertFalse(isHeadsetDisplaySuspended(Display.STATE_UNKNOWN, isInteractive = true))
    }
}
