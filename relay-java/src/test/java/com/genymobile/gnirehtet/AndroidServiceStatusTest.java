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

public class AndroidServiceStatusTest {

    @Test
    public void testRunningService() {
        AndroidServiceStatus status = AndroidServiceStatus.parse(
                "gnirehtet.state=RUNNING vpnFdOpen=true\n");

        Assert.assertTrue(status.isServicePresent());
        Assert.assertEquals("RUNNING", status.getLifecycleState());
        Assert.assertEquals(Boolean.TRUE, status.getVpnFdOpen());
        Assert.assertFalse(status.isStoppedAndVpnClosed());
    }

    @Test
    public void testStoppedServiceWithClosedVpn() {
        AndroidServiceStatus status = AndroidServiceStatus.parse(
                "gnirehtet.state=STOPPED vpnFdOpen=false\n");

        Assert.assertTrue(status.isServicePresent());
        Assert.assertTrue(status.isStoppedAndVpnClosed());
    }

    @Test
    public void testStoppedServiceWithOpenVpnIsNotVerified() {
        AndroidServiceStatus status = AndroidServiceStatus.parse(
                "gnirehtet.state=STOPPED vpnFdOpen=true\n");

        Assert.assertFalse(status.isStoppedAndVpnClosed());
    }

    @Test
    public void testMissingServiceIsStopped() {
        AndroidServiceStatus status = AndroidServiceStatus.parse(
                "No services match: com.genymobile.gnirehtet/.GnirehtetService\n");

        Assert.assertFalse(status.isServicePresent());
        Assert.assertTrue(status.isStoppedAndVpnClosed());
    }

    @Test
    public void testPresentServiceWithoutDumpContractIsUnknown() {
        AndroidServiceStatus status = AndroidServiceStatus.parse(
                "SERVICE com.genymobile.gnirehtet/.GnirehtetService 123 pid=42\n");

        Assert.assertTrue(status.isServicePresent());
        Assert.assertEquals("UNKNOWN", status.getLifecycleState());
        Assert.assertFalse(status.isStoppedAndVpnClosed());
    }
}
