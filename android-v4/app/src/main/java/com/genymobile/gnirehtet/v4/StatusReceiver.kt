package com.genymobile.gnirehtet.v4

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

class StatusReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val rtt = VdLinkVpnService.controlRtt.snapshot()
        resultCode = 0
        resultData = buildString {
            append("state=").append(VdLinkVpnService.state.get())
            append(";error=").append(VdLinkVpnService.lastError.get() ?: "none")
            append(";controlRttSamples=").append(rtt.samples)
            append(";controlRttP99Us=").append(rtt.p99Micros)
            append(";controlRttMaxUs=").append(rtt.maxMicros)
        }
    }
}
