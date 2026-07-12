package com.genymobile.gnirehtet.v4

import org.junit.Assert.assertEquals
import org.junit.Test

class SessionParametersTest {
    @Test
    fun parsesStrictHostContract() {
        val parsed = SessionParameters.parse(
            "00112233-4455-6677-8899-aabbccddeeff",
            "VirtualDesktop.Android",
            31_416,
            31_418,
            31_417,
            false,
        )
        assertEquals(31_416, parsed.socksPort)
        assertEquals(31_418, parsed.udpPort)
        assertEquals(31_417, parsed.controlPort)
        assertEquals("VirtualDesktop.Android", parsed.vdPackage)
    }

    @Test(expected = IllegalArgumentException::class)
    fun rejectsMissingSession() {
        SessionParameters.parse(null, null, 31_416, 31_418, 31_417, false)
    }

    @Test(expected = IllegalArgumentException::class)
    fun rejectsNonCanonicalSession() {
        SessionParameters.parse("1-1-1-1-1", null, 31_416, 31_418, 31_417, false)
    }

    @Test(expected = IllegalArgumentException::class)
    fun rejectsZeroSession() {
        SessionParameters.parse("00000000-0000-0000-0000-000000000000", null, 31_416, 31_418, 31_417, false)
    }

    @Test(expected = IllegalArgumentException::class)
    fun rejectsPrivilegedPort() {
        SessionParameters.parse(
            "00112233-4455-6677-8899-aabbccddeeff",
            null,
            443,
            31_418,
            31_417,
            false,
        )
    }

    @Test(expected = IllegalArgumentException::class)
    fun rejectsSharedSocksAndControlPorts() {
        SessionParameters.parse(
            "00112233-4455-6677-8899-aabbccddeeff",
            null,
            31_416,
            31_418,
            31_416,
            false,
        )
    }

    @Test(expected = IllegalArgumentException::class)
    fun rejectsSharedSocksAndUdpPorts() {
        SessionParameters.parse(
            "00112233-4455-6677-8899-aabbccddeeff",
            null,
            31_416,
            31_416,
            31_417,
            false,
        )
    }

    @Test(expected = IllegalArgumentException::class)
    fun rejectsSharedUdpAndControlPorts() {
        SessionParameters.parse(
            "00112233-4455-6677-8899-aabbccddeeff",
            null,
            31_416,
            31_417,
            31_417,
            false,
        )
    }

    @Test(expected = IllegalArgumentException::class)
    fun rejectsPrivilegedUdpPort() {
        SessionParameters.parse(
            "00112233-4455-6677-8899-aabbccddeeff",
            null,
            31_416,
            53,
            31_417,
            false,
        )
    }

    @Test(expected = IllegalArgumentException::class)
    fun rejectsUnexpectedPackage() {
        SessionParameters.parse(
            "00112233-4455-6677-8899-aabbccddeeff",
            "com.example.other",
            31_416,
            31_418,
            31_417,
            false,
        )
    }
}
