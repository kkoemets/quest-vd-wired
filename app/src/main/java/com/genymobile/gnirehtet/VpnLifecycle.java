/*
 * Copyright (C) 2017 Genymobile
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

package com.genymobile.gnirehtet;

/**
 * Process-local lifecycle state shared by the VPN service, its notification and the status UI.
 */
final class VpnLifecycle {

    enum State {
        STOPPED,
        STARTING,
        RUNNING,
        DEGRADED,
        STOPPING,
        ERROR
    }

    private static State state = State.STOPPED;
    private static String detail = "not started";

    private VpnLifecycle() {
        // utility class
    }

    static synchronized void transition(State newState, String newDetail) {
        state = newState;
        detail = newDetail != null ? newDetail : "";
    }

    static synchronized State getState() {
        return state;
    }

    static synchronized String getDetail() {
        return detail;
    }

    /**
     * Stable, machine-readable contract consumed through {@code dumpsys activity service}.
     */
    static synchronized String formatDumpLine(boolean vpnFdOpen) {
        return "gnirehtet.state=" + state.name() + " vpnFdOpen=" + vpnFdOpen;
    }

    static synchronized void resetForTests() {
        state = State.STOPPED;
        detail = "not started";
    }
}
