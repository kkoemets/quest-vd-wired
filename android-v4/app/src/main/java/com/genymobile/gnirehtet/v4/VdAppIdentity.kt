package com.genymobile.gnirehtet.v4

internal data class VdAppIdentity(
    val packageName: String,
    val uid: Int,
    val isSystemApplication: Boolean,
    val packagesForUid: Set<String>,
)

internal object VdAppIdentityValidator {
    fun validate(candidate: VdAppIdentity, ownPackage: String, ownUid: Int) {
        require(candidate.packageName != ownPackage) { "Virtual Desktop package resolves to this application" }
        require(candidate.uid >= FIRST_APPLICATION_UID) { "Virtual Desktop has a non-application UID" }
        require(candidate.uid != ownUid) { "Virtual Desktop shares this application's UID" }
        require(!candidate.isSystemApplication) { "Virtual Desktop unexpectedly resolves to a system application" }
        require(candidate.packagesForUid == setOf(candidate.packageName)) {
            "Virtual Desktop uses a shared or inconsistent UID"
        }
    }

    private const val FIRST_APPLICATION_UID = 10_000
}
