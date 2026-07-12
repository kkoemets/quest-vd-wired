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

import java.io.IOException;
import java.util.Arrays;
import java.util.Locale;

/** Performs read-only checks of the Virtual Desktop Streamer on Windows. */
final class VirtualDesktopDoctor {

    private static final long COMMAND_TIMEOUT_MS = 10000;
    private static final String POWERSHELL = "$ErrorActionPreference='Stop';try{"
            + "$p=@(Get-Process | Where-Object {$_.ProcessName -like 'VirtualDesktop.Streamer*'});"
            + "$s=@(Get-Service | Where-Object {$_.Name -like '*VirtualDesktop*' -or "
            + "$_.DisplayName -like '*Virtual Desktop*'});"
            + "if($p.Count -eq 0){'process=absent'}else{'process=running'};"
            + "if($s.Count -eq 0){'service=absent'}elseif(@($s | Where-Object {$_.Status -eq 'Running'}).Count -gt 0)"
            + "{'service=running'}else{'service=stopped'};"
            + "if($p.Count -eq 0){'listener=not_listening'}else{"
            + "$ids=@($p | ForEach-Object {$_.Id});"
            + "$tcp=@(Get-NetTCPConnection -State Listen | Where-Object {$ids -contains $_.OwningProcess});"
            + "$udp=@(Get-NetUDPEndpoint | Where-Object {$ids -contains $_.OwningProcess});"
            + "if($tcp.Count + $udp.Count -gt 0){'listener=listening'}else{'listener=not_listening'}}}"
            + "catch{'check=failed'}";

    enum StreamerState {
        ABSENT,
        RUNNING_NOT_LISTENING,
        RUNNING_LISTENING,
        UNSUPPORTED,
        CHECK_FAILED
    }

    static final class Result {
        private final StreamerState streamerState;
        private final String serviceState;
        private final String detail;

        Result(StreamerState streamerState, String serviceState, String detail) {
            this.streamerState = streamerState;
            this.serviceState = serviceState;
            this.detail = detail;
        }

        StreamerState getStreamerState() {
            return streamerState;
        }

        String getServiceState() {
            return serviceState;
        }

        String getDetail() {
            return detail;
        }
    }

    private VirtualDesktopDoctor() {
        // not instantiable
    }

    static Result inspect() {
        if (!System.getProperty("os.name", "").toLowerCase(Locale.ENGLISH).contains("windows")) {
            return new Result(StreamerState.UNSUPPORTED, "unsupported", "Virtual Desktop checks require Windows");
        }
        try {
            ProcessRunner.Result result = ProcessRunner.runCaptured(Arrays.asList(
                    "powershell.exe", "-NoProfile", "-NonInteractive", "-Command", POWERSHELL), COMMAND_TIMEOUT_MS);
            if (result.getExitCode() != 0) {
                return new Result(StreamerState.CHECK_FAILED, "unknown",
                        "PowerShell exited with " + result.getExitCode());
            }
            return parse(result.getOutput());
        } catch (IOException e) {
            return new Result(StreamerState.CHECK_FAILED, "unknown", e.getMessage());
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            return new Result(StreamerState.CHECK_FAILED, "unknown", "Interrupted while checking Virtual Desktop");
        }
    }

    static Result parse(String output) {
        if (output.contains("check=failed")) {
            return new Result(StreamerState.CHECK_FAILED, "unknown",
                    "Windows process/service/socket check failed");
        }
        boolean running = output.contains("process=running");
        boolean listening = output.contains("listener=listening");
        String service = readValue(output, "service=");
        StreamerState state;
        if (!running) {
            state = StreamerState.ABSENT;
        } else if (listening) {
            state = StreamerState.RUNNING_LISTENING;
        } else {
            state = StreamerState.RUNNING_NOT_LISTENING;
        }
        return new Result(state, service != null ? service : "unknown", "read-only Windows process/service/socket check");
    }

    private static String readValue(String output, String prefix) {
        for (String line : output.split("\\r?\\n")) {
            if (line.startsWith(prefix)) {
                return line.substring(prefix.length()).trim();
            }
        }
        return null;
    }
}
