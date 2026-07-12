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

import org.junit.Assert;
import org.junit.Test;

import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.nio.ByteBuffer;
import java.nio.channels.WritableByteChannel;

@SuppressWarnings("checkstyle:MagicNumber")
public class PacketQueueTest {

    @Test
    public void testPartialWritesPreservePacketFraming() throws IOException {
        PacketQueue queue = new PacketQueue(32, 4);
        queue.offer(ByteBuffer.wrap(new byte[]{1, 2, 3, 4}), false, 0);
        queue.offer(ByteBuffer.wrap(new byte[]{5, 6, 7}), true, 0);
        LimitedChannel channel = new LimitedChannel(2);

        while (!queue.isEmpty()) {
            queue.writeTo(channel);
        }

        Assert.assertArrayEquals(new byte[]{1, 2, 3, 4, 5, 6, 7}, channel.toByteArray());
        Assert.assertEquals(0, queue.sizeBytes());
    }

    @Test
    public void testExpiredUdpIsRemovedBehindTcp() {
        PacketQueue queue = new PacketQueue(32, 4);
        queue.offer(ByteBuffer.wrap(new byte[]{1, 2}), false, 100);
        queue.offer(ByteBuffer.wrap(new byte[]{3, 4, 5}), true, 100);
        queue.offer(ByteBuffer.wrap(new byte[]{6}), false, 100);

        int discarded = queue.discardExpiredUdp(10, 111);

        Assert.assertEquals(3, discarded);
        Assert.assertEquals(2, queue.sizePackets());
        Assert.assertEquals(3, queue.sizeBytes());
    }

    @Test
    public void testPartiallyWrittenUdpIsNeverDiscarded() throws IOException {
        PacketQueue queue = new PacketQueue(32, 4);
        queue.offer(ByteBuffer.wrap(new byte[]{1, 2, 3, 4}), true, 100);
        queue.writeTo(new LimitedChannel(2));

        Assert.assertEquals(0, queue.discardExpiredUdp(10, 1000));
        Assert.assertEquals(1, queue.sizePackets());
        Assert.assertEquals(2, queue.sizeBytes());
        Assert.assertEquals(Long.MAX_VALUE, queue.getNextUdpExpiryNanos(10));
    }

    @Test
    public void testByteAndPacketBounds() {
        PacketQueue queue = new PacketQueue(4, 2);
        Assert.assertTrue(queue.offer(ByteBuffer.wrap(new byte[]{1, 2}), true, 0));
        Assert.assertTrue(queue.offer(ByteBuffer.wrap(new byte[]{3, 4}), true, 0));
        Assert.assertFalse(queue.offer(ByteBuffer.wrap(new byte[]{5}), true, 0));
        Assert.assertEquals(0, queue.remainingBytes());
    }

    private static final class LimitedChannel implements WritableByteChannel {
        private final int limit;
        private final ByteArrayOutputStream output = new ByteArrayOutputStream();
        private boolean open = true;

        LimitedChannel(int limit) {
            this.limit = limit;
        }

        @Override
        public int write(ByteBuffer source) {
            int count = Math.min(limit, source.remaining());
            byte[] data = new byte[count];
            source.get(data);
            output.write(data, 0, data.length);
            return count;
        }

        byte[] toByteArray() {
            return output.toByteArray();
        }

        @Override
        public boolean isOpen() {
            return open;
        }

        @Override
        public void close() {
            open = false;
        }
    }
}
