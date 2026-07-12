package com.genymobile.gnirehtet.v4

import java.util.UUID

internal data class SessionParameters(
    val sessionId: UUID,
    val vdPackage: String,
    val socksPort: Int,
    val udpPort: Int,
    val controlPort: Int,
    val allTraffic: Boolean,
) {
    companion object {
        fun parse(
            sessionId: String?,
            vdPackage: String?,
            socksPort: Int,
            udpPort: Int,
            controlPort: Int,
            allTraffic: Boolean,
        ): SessionParameters {
            val rawSession = requireNotNull(sessionId) { "sessionId is required" }
            val parsedSession = runCatching { UUID.fromString(rawSession) }
                .getOrElse { throw IllegalArgumentException("sessionId must be a UUID", it) }
            require(parsedSession.toString().equals(rawSession, ignoreCase = true)) {
                "sessionId must use canonical UUID syntax"
            }
            require(parsedSession != ZERO_UUID) { "sessionId must not be zero" }
            require(socksPort in NON_PRIVILEGED_PORTS) { "socksPort is outside 1024..65535" }
            require(udpPort in NON_PRIVILEGED_PORTS) { "udpPort is outside 1024..65535" }
            require(controlPort in NON_PRIVILEGED_PORTS) { "controlPort is outside 1024..65535" }
            require(setOf(socksPort, udpPort, controlPort).size == 3) {
                "socksPort, udpPort, and controlPort must differ"
            }

            val selectedPackage = vdPackage ?: VdLinkVpnService.DEFAULT_VD_PACKAGE
            require(selectedPackage == VdLinkVpnService.DEFAULT_VD_PACKAGE) {
                "vdPackage must identify the supported Virtual Desktop Quest application"
            }

            return SessionParameters(
                parsedSession,
                selectedPackage,
                socksPort,
                udpPort,
                controlPort,
                allTraffic,
            )
        }

        private val ZERO_UUID = UUID(0, 0)
        private val NON_PRIVILEGED_PORTS = 1_024..65_535
    }
}
