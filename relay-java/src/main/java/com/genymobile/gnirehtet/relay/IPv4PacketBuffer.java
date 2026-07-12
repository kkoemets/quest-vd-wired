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
import java.nio.channels.ReadableByteChannel;

public class IPv4PacketBuffer {

    private static final int IPV4_MIN_HEADER_LENGTH = 20;
    private static final int UDP_HEADER_LENGTH = 8;
    private static final int TCP_MIN_HEADER_LENGTH = 20;

    // Keeping two maximum-sized packets makes the common path a cursor advance. Bytes
    // are copied only when the write end is actually exhausted while a fragment remains.
    private final ByteBuffer buffer = ByteBuffer.allocate(2 * IPv4Packet.MAX_PACKET_LENGTH);
    private int cursor;
    private int currentPacketLength;

    public int readFrom(ReadableByteChannel channel) throws IOException {
        ensureWritable();
        return channel.read(buffer);
    }

    private void ensureWritable() throws IOException {
        if (buffer.hasRemaining()) {
            return;
        }
        if (cursor == 0) {
            throw new IOException("IPv4 packet parser buffer is full");
        }
        int remaining = buffer.position() - cursor;
        System.arraycopy(buffer.array(), cursor, buffer.array(), 0, remaining);
        buffer.position(remaining);
        cursor = 0;
        Diagnostics.increment("packet_parser.compactions");
    }

    @SuppressWarnings("checkstyle:MagicNumber")
    private int getAvailablePacketLength() throws IOException {
        int available = buffer.position() - cursor;
        if (available < 4) {
            // no packet
            return 0;
        }
        int length = Short.toUnsignedInt(buffer.getShort(cursor + 2));
        int version = (buffer.get(cursor) & 0xf0) >> 4;
        int headerLength = (buffer.get(cursor) & 0x0f) << 2;
        if (version != 4) {
            throw malformed("unsupported IP version " + version);
        }
        if (headerLength < IPV4_MIN_HEADER_LENGTH) {
            throw malformed("IPv4 header is shorter than " + IPV4_MIN_HEADER_LENGTH + " bytes");
        }
        if (length < headerLength) {
            throw malformed("IPv4 total length " + length + " is shorter than its header " + headerLength);
        }
        if (length > available) {
            // no full packet available
            return 0;
        }
        validateTransport(length, headerLength);
        return length;
    }

    @SuppressWarnings("checkstyle:MagicNumber")
    private void validateTransport(int length, int headerLength) throws IOException {
        int flagsAndOffset = Short.toUnsignedInt(buffer.getShort(cursor + 6));
        if ((flagsAndOffset & 0x3fff) != 0) {
            throw malformed("fragmented IPv4 packets are unsupported");
        }
        int protocol = buffer.get(cursor + 9) & 0xff;
        int transportLength = length - headerLength;
        int transportOffset = cursor + headerLength;
        if (protocol == 17) {
            if (transportLength < UDP_HEADER_LENGTH) {
                throw malformed("truncated UDP header");
            }
            int udpLength = Short.toUnsignedInt(buffer.getShort(transportOffset + 4));
            if (udpLength != transportLength || udpLength < UDP_HEADER_LENGTH) {
                throw malformed("inconsistent UDP length " + udpLength + "/" + transportLength);
            }
        } else if (protocol == 6) {
            if (transportLength < TCP_MIN_HEADER_LENGTH) {
                throw malformed("truncated TCP header");
            }
            int tcpHeaderLength = (buffer.get(transportOffset + 12) & 0xf0) >> 2;
            if (tcpHeaderLength < TCP_MIN_HEADER_LENGTH || tcpHeaderLength > transportLength) {
                throw malformed("invalid TCP header length " + tcpHeaderLength);
            }
        }
    }

    private static IOException malformed(String detail) {
        Diagnostics.increment("drops.malformed_packet");
        return new IOException("Malformed Android packet: " + detail);
    }

    public IPv4Packet asIPv4Packet() throws IOException {
        if (currentPacketLength != 0) {
            return createPacket(currentPacketLength);
        }
        int length = getAvailablePacketLength();
        if (length == 0) {
            return null;
        }
        currentPacketLength = length;
        return createPacket(length);
    }

    private IPv4Packet createPacket(int length) {
        ByteBuffer view = buffer.duplicate();
        view.limit(cursor + length).position(cursor);
        ByteBuffer packetBuffer = view.slice();
        // In order to avoid copies, packetBuffer is shared with this IPv4Packet instance that is returned.
        // Don't use it after another call to next()!
        return new IPv4Packet(packetBuffer);
    }

    public void next() {
        if (currentPacketLength == 0) {
            throw new IllegalStateException("No current IPv4 packet");
        }
        cursor += currentPacketLength;
        currentPacketLength = 0;
        if (cursor == buffer.position()) {
            cursor = 0;
            buffer.clear();
        }
    }
}
