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
import java.nio.channels.ClosedChannelException;
import java.nio.channels.SelectionKey;
import java.nio.channels.Selector;
import java.nio.channels.SocketChannel;
import java.util.ArrayList;
import java.util.Iterator;
import java.util.List;
import java.util.concurrent.TimeUnit;

public class Client {

    private static final String TAG = Client.class.getSimpleName();
    private static final int TUN_MTU = 0x4000;
    private static final int CLIENT_QUEUE_PACKETS = 16;
    private static final int CLIENT_QUEUE_BYTES = 8 * TUN_MTU;
    private static final int MAX_PENDING_PACKET_SOURCES = 128;
    private static final long MAX_UDP_QUEUE_AGE_NANOS = TimeUnit.MILLISECONDS.toNanos(10);

    private static int nextId = 0;

    private final int id;
    private final SocketChannel clientChannel;
    private final SelectionKey selectionKey;
    private final CloseListener<Client> closeListener;
    private int interests;

    private final IPv4PacketBuffer clientToNetwork = new IPv4PacketBuffer();
    private final PacketQueue networkToClient = new PacketQueue(CLIENT_QUEUE_BYTES, CLIENT_QUEUE_PACKETS);
    private final Router router;

    private final List<PacketSource> pendingPacketSources = new ArrayList<>();

    // store the remaining bytes of "id" to send to the client before relaying any data
    private ByteBuffer pendingIdBuffer;

    public Client(Selector selector, SocketChannel clientChannel, CloseListener<Client> closeListener) throws ClosedChannelException {
        id = nextId++;
        this.clientChannel = clientChannel;
        router = new Router(this, selector);
        pendingIdBuffer = createIntBuffer(id);

        SelectionHandler selectionHandler = (selectionKey) -> {
            if (selectionKey.isValid() && selectionKey.isWritable()) {
                processSend();
            }
            if (selectionKey.isValid() && selectionKey.isReadable()) {
                processReceive();
            }
            if (selectionKey.isValid()) {
                updateInterests();
            }
        };
        // on start, we are interested only in writing (we must first send the client id)
        interests = SelectionKey.OP_WRITE;
        selectionKey = clientChannel.register(selector, interests, selectionHandler);

        this.closeListener = closeListener;
    }

    private static ByteBuffer createIntBuffer(int value) {
        final int intSize = 4;
        ByteBuffer buffer = ByteBuffer.allocate(intSize);
        buffer.putInt(value);
        buffer.flip();
        return buffer;
    }

    public int getId() {
        return id;
    }

    public Router getRouter() {
        return router;
    }

    private void processReceive() {
        if (!read()) {
            close();
            return;
        }
        try {
            pushToNetwork();
        } catch (IOException | RuntimeException e) {
            Diagnostics.increment("clients.closed_malformed_input");
            Log.w(TAG, "Closing client after malformed packet input", e);
            close();
        }
    }

    private void processSend() {
        if (mustSendId()) {
            if (!sendId()) {
                close();
            }
            return;
        }
        if (!write()) {
            close();
            return;
        }
        processPending();
    }

    private boolean read() {
        try {
            int read = clientToNetwork.readFrom(clientChannel);
            if (read > 0) {
                Diagnostics.add("bytes.android_to_relay", read);
            }
            return read != -1;
        } catch (IOException e) {
            Log.e(TAG, "Cannot read", e);
            return false;
        }
    }

    private boolean write() {
        try {
            expireQueuedUdp(System.nanoTime());
            int packetCountBefore = networkToClient.sizePackets();
            int written = networkToClient.writeTo(clientChannel);
            if (written > 0) {
                Diagnostics.add("bytes.relay_to_android", written);
                Diagnostics.add("client.queue_bytes", -written);
            }
            if (networkToClient.sizePackets() != packetCountBefore) {
                Diagnostics.add("client.queue_packets", -1);
            }
            return written != -1;
        } catch (IOException e) {
            Log.e(TAG, "Cannot write", e);
            return false;
        }
    }

    private boolean mustSendId() {
        return pendingIdBuffer != null && pendingIdBuffer.hasRemaining();
    }

    private boolean sendId() {
        assert mustSendId();
        try {
            if (clientChannel.write(pendingIdBuffer) == -1) {
                Log.w(TAG, "Cannot write client id #" + id + " (EOF)");
                return false;
            }
            if (!pendingIdBuffer.hasRemaining()) {
                // we don't need this buffer anymore, release it
                Log.d(TAG, () -> "Client id #" + id + " sent to client");
                pendingIdBuffer = null;
            }
            return true;
        } catch (IOException e) {
            Log.e(TAG, "Cannot write client id #" + id, e);
            return false;
        }
    }

    private void pushToNetwork() throws IOException {
        IPv4Packet packet;
        while ((packet = clientToNetwork.asIPv4Packet()) != null) {
            router.sendToNetwork(packet);
            clientToNetwork.next();
        }
    }

    private void close() {
        selectionKey.cancel();
        try {
            clientChannel.close();
        } catch (IOException e) {
            Log.e(TAG, "Cannot close client connection", e);
        }
        router.clear();
        Diagnostics.add("client.pending_packet_sources", -pendingPacketSources.size());
        Diagnostics.add("client.queue_bytes", -networkToClient.sizeBytes());
        Diagnostics.add("client.queue_packets", -networkToClient.sizePackets());
        pendingPacketSources.clear();
        closeListener.onClosed(this);
    }

    private void updateInterests() {
        int interestOps = SelectionKey.OP_READ; // we always want to read
        if (mustSendId() || !networkToClient.isEmpty() || !pendingPacketSources.isEmpty()) {
            interestOps |= SelectionKey.OP_WRITE;
        }
        if (interests != interestOps) {
            // interests must be changed
            interests = interestOps;
            selectionKey.interestOps(interestOps);
        }
    }

    public boolean sendToClient(IPv4Packet packet) {
        long nowNanos = System.nanoTime();
        expireQueuedUdp(nowNanos);
        boolean udp = packet.getIpv4Header().getProtocol() == IPv4Header.Protocol.UDP;
        if (!networkToClient.offer(packet.getRaw(), udp, nowNanos)) {
            Log.w(TAG, "Client buffer full");
            Diagnostics.increment("drops.client_queue_full");
            return false;
        }
        Diagnostics.add("client.queue_bytes", packet.getRawLength());
        Diagnostics.increment("client.queue_packets");
        Diagnostics.recordMaximum("client.queue_bytes_max", networkToClient.sizeBytes());
        Diagnostics.recordMaximum("client.queue_packets_max", networkToClient.sizePackets());
        updateInterests();
        return true;
    }

    void expireQueuedUdp(long nowNanos) {
        int packetCountBefore = networkToClient.sizePackets();
        int discardedBytes = networkToClient.discardExpiredUdp(MAX_UDP_QUEUE_AGE_NANOS, nowNanos);
        int discardedPackets = packetCountBefore - networkToClient.sizePackets();
        if (discardedPackets > 0) {
            Diagnostics.add("client.queue_bytes", -discardedBytes);
            Diagnostics.add("client.queue_packets", -discardedPackets);
            Diagnostics.add("drops.client_queue_age_udp", discardedPackets);
            Diagnostics.add("drops.client_queue_age_udp_bytes", discardedBytes);
            updateInterests();
        }
    }

    long getNextUdpExpiryNanos() {
        return networkToClient.getNextUdpExpiryNanos(MAX_UDP_QUEUE_AGE_NANOS);
    }

    public void consume(PacketSource source) {
        IPv4Packet packet = source.get();
        if (sendToClient(packet)) {
            source.next();
            return;
        }
        assert !pendingPacketSources.contains(source);
        if (pendingPacketSources.size() >= MAX_PENDING_PACKET_SOURCES) {
            Diagnostics.increment("drops.pending_source_limit");
            if (source instanceof AbstractConnection) {
                ((AbstractConnection) source).close();
            }
            return;
        }
        pendingPacketSources.add(source);
        Diagnostics.increment("client.pending_packet_sources");
        Diagnostics.recordMaximum("client.pending_packet_sources_max", pendingPacketSources.size());
    }

    private void processPending() {
        Iterator<PacketSource> iterator = pendingPacketSources.iterator();
        while (iterator.hasNext()) {
            PacketSource packetSource = iterator.next();
            IPv4Packet packet = packetSource.get();
            if (sendToClient(packet)) {
                packetSource.next();
                Log.d(TAG, () -> "Pending packet sent to client (" + packet.getRawLength() + ")");
                iterator.remove();
                Diagnostics.add("client.pending_packet_sources", -1);
            } else {
                Log.w(TAG, "Pending packet not sent to client (" + packet.getRawLength() + "), client buffer full again");
                return;
            }
        }
    }

    public void cleanExpiredConnections() {
        router.cleanExpiredConnections();
    }
}
