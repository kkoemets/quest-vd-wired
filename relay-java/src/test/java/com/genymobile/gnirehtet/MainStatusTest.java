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

@SuppressWarnings("checkstyle:MagicNumber")
public class MainStatusTest {

    @Test
    public void testTunnelMappingMatchesOnSameLine() {
        String output = "QUEST localabstract:gnirehtet tcp:31416\n";
        Assert.assertTrue(Main.hasTunnelMappingInOutput(output, 31416));
    }

    @Test
    public void testTunnelMappingDoesNotMatchAcrossLines() {
        String output = "QUEST localabstract:gnirehtet tcp:1234\nQUEST localabstract:other tcp:31416\n";
        Assert.assertFalse(Main.hasTunnelMappingInOutput(output, 31416));
    }

    @Test
    public void testTunnelMappingRequiresExactFields() {
        Assert.assertFalse(Main.hasTunnelMappingInOutput(
                "QUEST localabstract:gnirehtet-control tcp:31416\n", 31416));
        Assert.assertFalse(Main.hasTunnelMappingInOutput(
                "QUEST localabstract:gnirehtet tcp:314160\n", 31416));
        Assert.assertTrue(Main.hasProductMappingInOutput(
                "QUEST localabstract:gnirehtet tcp:31416\n"));
        Assert.assertFalse(Main.hasProductMappingInOutput(
                "QUEST localabstract:gnirehtet-control tcp:31416\n"));
    }

    @Test
    public void testStopSuppressesMappingRepair() {
        AndroidServiceStatus running = AndroidServiceStatus.parse(
                "gnirehtet.state=RUNNING vpnFdOpen=true\n");
        AndroidServiceStatus stopped = AndroidServiceStatus.parse(
                "gnirehtet.state=STOPPED vpnFdOpen=false\n");

        Assert.assertTrue(Main.shouldRepairTunnel(false, running));
        Assert.assertFalse(Main.shouldRepairTunnel(true, running));
        Assert.assertFalse(Main.shouldRepairTunnel(false, stopped));
    }
}
