package com.genymobile.gnirehtet.v4

internal enum class VpnRoutingMode(
    val requiresVirtualDesktopPackage: Boolean,
) {
    ALL_TRAFFIC(false),
    VIRTUAL_DESKTOP_ONLY(true),
    ;

    companion object {
        fun from(allTraffic: Boolean): VpnRoutingMode =
            if (allTraffic) ALL_TRAFFIC else VIRTUAL_DESKTOP_ONLY
    }
}
