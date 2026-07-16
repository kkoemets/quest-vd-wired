package com.genymobile.gnirehtet.v4

import android.content.Intent
import android.view.Display
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
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

    @Test
    fun screenBroadcastsApplyTheInteractiveStateImmediately() {
        assertEquals(true, screenSuspendedFromBroadcast(Intent.ACTION_SCREEN_OFF))
        assertEquals(false, screenSuspendedFromBroadcast(Intent.ACTION_SCREEN_ON))
        assertNull(screenSuspendedFromBroadcast(Intent.ACTION_USER_PRESENT))
    }

    @Test
    fun staleDisplaySleepIsDeferredImmediatelyAfterInteractiveWake() {
        assertTrue(
            shouldDeferDisplaySuspension(
                suspended = true,
                nowElapsedRealtimeMs = 1_000,
                wakeDebounceUntilMs = 2_000,
            ),
        )
        assertFalse(
            shouldDeferDisplaySuspension(
                suspended = false,
                nowElapsedRealtimeMs = 1_000,
                wakeDebounceUntilMs = 2_000,
            ),
        )
        assertFalse(
            shouldDeferDisplaySuspension(
                suspended = true,
                nowElapsedRealtimeMs = 2_000,
                wakeDebounceUntilMs = 2_000,
            ),
        )
    }
}
