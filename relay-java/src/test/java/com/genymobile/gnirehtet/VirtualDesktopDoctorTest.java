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

import org.junit.Assert;
import org.junit.Test;

public class VirtualDesktopDoctorTest {

    @Test
    public void testAbsent() {
        VirtualDesktopDoctor.Result result = VirtualDesktopDoctor.parse(
                "process=absent\nservice=stopped\nlistener=not_listening\n");

        Assert.assertEquals(VirtualDesktopDoctor.StreamerState.ABSENT, result.getStreamerState());
        Assert.assertEquals("stopped", result.getServiceState());
    }

    @Test
    public void testRunningWithoutListener() {
        VirtualDesktopDoctor.Result result = VirtualDesktopDoctor.parse(
                "process=running\nservice=running\nlistener=not_listening\n");

        Assert.assertEquals(VirtualDesktopDoctor.StreamerState.RUNNING_NOT_LISTENING, result.getStreamerState());
    }

    @Test
    public void testRunningAndListening() {
        VirtualDesktopDoctor.Result result = VirtualDesktopDoctor.parse(
                "process=running\nservice=running\nlistener=listening\n");

        Assert.assertEquals(VirtualDesktopDoctor.StreamerState.RUNNING_LISTENING, result.getStreamerState());
    }

    @Test
    public void testCheckFailureIsNotMisclassifiedAsMissingStreamer() {
        VirtualDesktopDoctor.Result result = VirtualDesktopDoctor.parse("check=failed\n");

        Assert.assertEquals(VirtualDesktopDoctor.StreamerState.CHECK_FAILED, result.getStreamerState());
        Assert.assertEquals("unknown", result.getServiceState());
    }
}
