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

import java.util.Locale;
import java.util.regex.Matcher;
import java.util.regex.Pattern;

final class AndroidServiceStatus {

    private static final Pattern STATE_PATTERN = Pattern.compile("gnirehtet\\.state=([A-Za-z_]+)");
    private static final Pattern VPN_FD_PATTERN = Pattern.compile("vpnFdOpen=(true|false)", Pattern.CASE_INSENSITIVE);

    private final boolean servicePresent;
    private final String lifecycleState;
    private final Boolean vpnFdOpen;

    private AndroidServiceStatus(boolean servicePresent, String lifecycleState, Boolean vpnFdOpen) {
        this.servicePresent = servicePresent;
        this.lifecycleState = lifecycleState;
        this.vpnFdOpen = vpnFdOpen;
    }

    static AndroidServiceStatus parse(String output) {
        Matcher stateMatcher = STATE_PATTERN.matcher(output);
        Matcher vpnMatcher = VPN_FD_PATTERN.matcher(output);
        String lower = output.toLowerCase(Locale.ENGLISH);
        boolean explicitAbsent = lower.contains("no services match") || lower.contains("(nothing)");
        String state = stateMatcher.find() ? stateMatcher.group(1).toUpperCase(Locale.ENGLISH) : null;
        Boolean fdOpen = vpnMatcher.find() ? Boolean.valueOf(vpnMatcher.group(1)) : null;
        boolean present = state != null || fdOpen != null || (!explicitAbsent && lower.contains("gnirehtetservice"));
        return new AndroidServiceStatus(present, state, fdOpen);
    }

    boolean isStoppedAndVpnClosed() {
        if (!servicePresent) {
            return true;
        }
        return "STOPPED".equals(lifecycleState) && Boolean.FALSE.equals(vpnFdOpen);
    }

    boolean isServicePresent() {
        return servicePresent;
    }

    String getLifecycleState() {
        return lifecycleState != null ? lifecycleState : "UNKNOWN";
    }

    Boolean getVpnFdOpen() {
        return vpnFdOpen;
    }
}
