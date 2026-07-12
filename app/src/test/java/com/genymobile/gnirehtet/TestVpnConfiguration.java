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

import java.net.InetAddress;

public class TestVpnConfiguration {

    @Test
    public void testDefaultRoutesOnlyVirtualDesktop() {
        VpnConfiguration configuration = new VpnConfiguration();

        Assert.assertFalse(configuration.isAllTraffic());
        Assert.assertEquals(VpnConfiguration.DEFAULT_ALLOWED_APPLICATION, configuration.getAllowedApplication());
    }

    @Test
    public void testDiagnosticAllTrafficMode() {
        VpnConfiguration configuration = new VpnConfiguration(new InetAddress[0], new CIDR[0], true,
                " com.example.virtualdesktop ");

        Assert.assertTrue(configuration.isAllTraffic());
        Assert.assertEquals("com.example.virtualdesktop", configuration.getAllowedApplication());
    }

    @Test
    public void testBlankAllowedApplicationUsesVirtualDesktop() {
        VpnConfiguration configuration = new VpnConfiguration(new InetAddress[0], new CIDR[0], false, "  ");

        Assert.assertEquals(VpnConfiguration.DEFAULT_ALLOWED_APPLICATION, configuration.getAllowedApplication());
    }
}
