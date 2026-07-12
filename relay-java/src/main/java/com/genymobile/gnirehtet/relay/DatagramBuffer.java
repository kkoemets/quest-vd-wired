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
import java.net.SocketAddress;
import java.nio.ByteBuffer;
import java.nio.channels.DatagramChannel;
import java.nio.channels.WritableByteChannel;
import java.util.concurrent.TimeUnit;

/**
 * Circular buffer to store datagrams (preserving their boundaries).
 * <p>
 * <pre>
 *     circularBufferLength
 * |<------------------------->| extra space for storing the last datagram in one block
 * +---------------------------+------+
 * |                           |      |
 * |[D4]     [  D1  ][ D2 ][  D3  ]   |
 * +---------------------------+------+
 *     ^     ^
 *  head     tail
 * </pre>
 */
@SuppressWarnings("checkstyle:MagicNumber")
public class DatagramBuffer {

    private static final String TAG = DatagramBuffer.class.getSimpleName();

    // every datagram is stored along with a header storing its length, on 16 bits
    private static final int HEADER_LENGTH = 2;
    private static final int MAX_DATAGRAM_LENGTH = (1 << 16) - 1;
    private static final int MAX_QUEUED_DATAGRAMS = 4096;

    private final byte[] data;
    private final ByteBuffer wrapper;
    private int head;
    private int tail;
    private final int circularBufferLength;
    private final int maxDatagramLength;
    private final long[] enqueueTimes;
    private int timestampHead;
    private int timestampTail;
    private int datagramCount;
    private int queuedPayloadBytes;

    public DatagramBuffer(int capacity) {
        this(capacity, MAX_DATAGRAM_LENGTH, MAX_QUEUED_DATAGRAMS);
    }

    public DatagramBuffer(int capacity, int maxDatagramLength, int maxQueuedDatagrams) {
        if (capacity <= 0 || maxDatagramLength < 0 || maxDatagramLength > MAX_DATAGRAM_LENGTH
                || maxQueuedDatagrams <= 0) {
            throw new IllegalArgumentException("Invalid datagram buffer limits");
        }
        this.maxDatagramLength = maxDatagramLength;
        data = new byte[capacity + HEADER_LENGTH + maxDatagramLength];
        wrapper = ByteBuffer.wrap(data);
        circularBufferLength = capacity + 1;
        enqueueTimes = new long[maxQueuedDatagrams];
    }

    public boolean isEmpty() {
        return datagramCount == 0;
    }

    public boolean hasEnoughSpaceFor(int datagramLength) {
        if (datagramLength < 0 || datagramLength > maxDatagramLength || datagramCount == enqueueTimes.length) {
            return false;
        }
        if (datagramCount > 0 && head == tail) {
            // The circular portion is full. The queued datagram which crossed the
            // boundary is stored contiguously in the extra block at the end.
            return false;
        }
        if (head >= tail) {
            // there is at least the extra space for storing 1 packet
            return true;
        }
        int remaining = tail - head - 1; // 1 extra byte to distinguish empty vs full
        return HEADER_LENGTH + datagramLength <= remaining;
    }

    public int capacity() {
        return circularBufferLength - 1;
    }

    public boolean writeTo(WritableByteChannel channel) throws IOException {
        int length = peekLength();
        wrapper.limit(tail + HEADER_LENGTH + length).position(tail + HEADER_LENGTH);
        int w = channel.write(wrapper);
        if (w == 0 && length > 0) {
            return true;
        }
        if (w != length) {
            Log.e(TAG, "Cannot write the whole datagram to the channel (only " + w + "/" + length + ")");
            return false;
        }
        removeFirst(length);
        return true;
    }

    public boolean sendTo(DatagramChannel channel, SocketAddress destination) throws IOException {
        int length = peekLength();
        wrapper.limit(tail + HEADER_LENGTH + length).position(tail + HEADER_LENGTH);
        int sent = channel.send(wrapper, destination);
        if (sent == 0 && length > 0) {
            return true;
        }
        if (sent != length) {
            Log.e(TAG, "Cannot send the whole datagram (only " + sent + "/" + length + ")");
            return false;
        }
        removeFirst(length);
        return true;
    }

    public boolean readFrom(ByteBuffer buffer) {
        int length = buffer.remaining();
        if (length > maxDatagramLength) {
            throw new IllegalArgumentException("Datagram length (" + buffer.remaining() + ") may not be greater than "
                    + maxDatagramLength + " bytes");
        }
        if (!hasEnoughSpaceFor(length)) {
            return false;
        }
        writeLength(length);
        enqueueTimes[timestampHead] = System.nanoTime();
        timestampHead = (timestampHead + 1) % enqueueTimes.length;
        buffer.get(data, head, length);
        head += length;
        if (head >= circularBufferLength) {
            head = 0;
        }
        ++datagramCount;
        queuedPayloadBytes += length;
        return true;
    }

    private void writeLength(int length) {
        assert (length & ~0xffff) == 0 : "Length must be stored on 16 bits";
        data[head++] = (byte) ((length >> 8) & 0xff);
        data[head++] = (byte) (length & 0xff);
    }

    private int peekLength() {
        return ((data[tail] & 0xff) << 8) | (data[tail + 1] & 0xff);
    }

    private void removeFirst(int length) {
        tail += HEADER_LENGTH + length;
        if (tail >= circularBufferLength) {
            tail = 0;
        }
        timestampTail = (timestampTail + 1) % enqueueTimes.length;
        --datagramCount;
        queuedPayloadBytes -= length;
        if (datagramCount == 0) {
            head = 0;
            tail = 0;
            timestampHead = 0;
            timestampTail = 0;
        }
    }

    public int discardExpired(long maxAgeMillis) {
        return discardExpired(maxAgeMillis, System.nanoTime());
    }

    int discardExpired(long maxAgeMillis, long nowNanos) {
        int dropped = 0;
        long maxAgeNanos = TimeUnit.MILLISECONDS.toNanos(maxAgeMillis);
        while (datagramCount > 0 && nowNanos - enqueueTimes[timestampTail] > maxAgeNanos) {
            int length = peekLength();
            removeFirst(length);
            ++dropped;
            Diagnostics.increment("drops.udp_queue_age");
            Diagnostics.add("drops.udp_queue_age_bytes", length);
        }
        return dropped;
    }

    public int getDatagramCount() {
        return datagramCount;
    }

    public int getQueuedPayloadBytes() {
        return queuedPayloadBytes;
    }

    public long getOldestAgeMillis() {
        return datagramCount == 0 ? 0
                : TimeUnit.NANOSECONDS.toMillis(Math.max(0, System.nanoTime() - enqueueTimes[timestampTail]));
    }

    int getAllocatedBytes() {
        return data.length + enqueueTimes.length * Long.BYTES;
    }
}
