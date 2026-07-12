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
import java.nio.ByteBuffer;
import java.nio.channels.WritableByteChannel;
import java.util.ArrayDeque;
import java.util.Deque;
import java.util.Iterator;

/** A bounded packet queue which keeps framing intact across partial stream writes. */
final class PacketQueue {

    private static final class Entry {
        private final ByteBuffer data;
        private final boolean udp;
        private final long enqueueNanos;

        Entry(ByteBuffer data, boolean udp, long enqueueNanos) {
            this.data = data;
            this.udp = udp;
            this.enqueueNanos = enqueueNanos;
        }

        boolean isStarted() {
            return data.position() != 0;
        }
    }

    private final int capacityBytes;
    private final int capacityPackets;
    private final Deque<Entry> entries = new ArrayDeque<>();
    private int sizeBytes;

    PacketQueue(int capacityBytes, int capacityPackets) {
        if (capacityBytes <= 0 || capacityPackets <= 0) {
            throw new IllegalArgumentException("Packet queue limits must be positive");
        }
        this.capacityBytes = capacityBytes;
        this.capacityPackets = capacityPackets;
    }

    boolean offer(ByteBuffer source, boolean udp, long nowNanos) {
        int length = source.remaining();
        if (length > remainingBytes() || entries.size() >= capacityPackets) {
            return false;
        }
        byte[] copy = new byte[length];
        source.get(copy);
        entries.addLast(new Entry(ByteBuffer.wrap(copy), udp, nowNanos));
        sizeBytes += length;
        return true;
    }

    int writeTo(WritableByteChannel channel) throws IOException {
        Entry entry = entries.peekFirst();
        if (entry == null) {
            return 0;
        }
        int written = channel.write(entry.data);
        if (written > 0) {
            sizeBytes -= written;
            if (!entry.data.hasRemaining()) {
                entries.removeFirst();
            }
        }
        return written;
    }

    int discardExpiredUdp(long maxAgeNanos, long nowNanos) {
        int discardedBytes = 0;
        Iterator<Entry> iterator = entries.iterator();
        while (iterator.hasNext()) {
            Entry entry = iterator.next();
            if (entry.udp && !entry.isStarted() && nowNanos - entry.enqueueNanos > maxAgeNanos) {
                discardedBytes += entry.data.remaining();
                iterator.remove();
            }
        }
        sizeBytes -= discardedBytes;
        return discardedBytes;
    }

    long getNextUdpExpiryNanos(long maxAgeNanos) {
        long next = Long.MAX_VALUE;
        for (Entry entry : entries) {
            if (entry.udp && !entry.isStarted()) {
                next = Math.min(next, entry.enqueueNanos + maxAgeNanos);
            }
        }
        return next;
    }

    boolean isEmpty() {
        return entries.isEmpty();
    }

    int sizeBytes() {
        return sizeBytes;
    }

    int sizePackets() {
        return entries.size();
    }

    int remainingBytes() {
        return capacityBytes - sizeBytes;
    }
}
