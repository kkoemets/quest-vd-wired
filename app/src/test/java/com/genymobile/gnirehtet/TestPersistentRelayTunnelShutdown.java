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

import java.io.InterruptedIOException;

public class TestPersistentRelayTunnelShutdown {

    @Test
    public void testCloseIsIdempotentAndPreventsSend() throws Exception {
        PersistentRelayTunnel tunnel = new PersistentRelayTunnel(null, null);

        tunnel.close();
        tunnel.close();

        try {
            tunnel.send(new byte[1], 1);
            Assert.fail("Send must stop after close");
        } catch (InterruptedIOException expected) {
            // expected
        }
    }

    @Test
    public void testClosedProviderDoesNotTryToReconnect() throws Exception {
        RelayTunnelProvider provider = new RelayTunnelProvider(null, null);
        provider.close();
        provider.close();

        try {
            provider.getCurrentTunnel();
            Assert.fail("Closed provider must not open a relay socket");
        } catch (InterruptedIOException expected) {
            // expected
        }
    }
}
