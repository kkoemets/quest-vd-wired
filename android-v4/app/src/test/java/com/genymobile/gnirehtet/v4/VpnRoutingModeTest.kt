package com.genymobile.gnirehtet.v4

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class VpnRoutingModeTest {
    @Test
    fun allTrafficDoesNotRequireVirtualDesktopPackage() {
        val mode = VpnRoutingMode.from(VdLinkVpnService.DEFAULT_ALL_TRAFFIC)

        assertTrue(VdLinkVpnService.DEFAULT_ALL_TRAFFIC)
        assertEquals(VpnRoutingMode.ALL_TRAFFIC, mode)
        assertFalse(mode.requiresVirtualDesktopPackage)
    }

    @Test
    fun virtualDesktopOnlyRetainsStrictPackageValidation() {
        val mode = VpnRoutingMode.from(allTraffic = false)

        assertEquals(VpnRoutingMode.VIRTUAL_DESKTOP_ONLY, mode)
        assertTrue(mode.requiresVirtualDesktopPackage)
    }
}
