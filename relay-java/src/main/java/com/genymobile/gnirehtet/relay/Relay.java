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

package com.genymobile.gnirehtet.relay;

import java.io.IOException;
import java.nio.channels.SelectionKey;
import java.nio.channels.Selector;
import java.util.Set;
import java.util.concurrent.TimeUnit;

public class Relay {

    private static final String TAG = Relay.class.getSimpleName();

    private static final int CLEANING_INTERVAL = 60 * 1000;

    private final int port;

    public Relay(int port) {
        this.port = port;
    }

    public void run() throws IOException {
        Selector selector = Selector.open();

        // will register the socket on the selector
        TunnelServer tunnelServer = new TunnelServer(port, selector);

        Log.i(TAG, "Relay server started");

        long nextCleaningDeadline = System.currentTimeMillis() + CLEANING_INTERVAL;
        while (true) {
            long timeout = Math.max(1, nextCleaningDeadline - System.currentTimeMillis());
            long nowNanos = System.nanoTime();
            tunnelServer.expireQueuedUdp(nowNanos);
            long nextUdpExpiryNanos = tunnelServer.getNextUdpExpiryNanos();
            if (nextUdpExpiryNanos != Long.MAX_VALUE) {
                long remainingNanos = Math.max(0, nextUdpExpiryNanos - nowNanos);
                long udpTimeoutMillis = Math.max(1, TimeUnit.NANOSECONDS.toMillis(remainingNanos) + 1);
                timeout = Math.min(timeout, udpTimeoutMillis);
            }
            long selectStart = System.nanoTime();
            selector.select(timeout);
            long selectElapsed = System.nanoTime() - selectStart;
            Diagnostics.recordMaximum("selector.select_duration_ns_max", selectElapsed);
            Diagnostics.recordMaximum("selector.wakeup_delay_ns_max",
                    Math.max(0, selectElapsed - TimeUnit.MILLISECONDS.toNanos(timeout)));
            Set<SelectionKey> selectedKeys = selector.selectedKeys();
            Diagnostics.set("selector.ready_keys", selectedKeys.size());
            tunnelServer.expireQueuedUdp(System.nanoTime());

            long now = System.currentTimeMillis();
            if (now >= nextCleaningDeadline || selectedKeys.isEmpty()) {
                tunnelServer.cleanUp();
                nextCleaningDeadline = now + CLEANING_INTERVAL;
            }

            for (SelectionKey selectedKey : selectedKeys) {
                long handlerStart = System.nanoTime();
                SelectionHandler selectionHandler = (SelectionHandler) selectedKey.attachment();
                selectionHandler.onReady(selectedKey);
                Diagnostics.recordMaximum("selector.handler_duration_ns_max", System.nanoTime() - handlerStart);
            }
            // by design, we handled everything
            selectedKeys.clear();
        }
    }
}
