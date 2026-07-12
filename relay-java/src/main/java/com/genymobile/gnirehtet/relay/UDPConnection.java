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
import java.net.InetSocketAddress;
import java.net.StandardProtocolFamily;
import java.nio.channels.DatagramChannel;
import java.nio.channels.SelectionKey;
import java.nio.channels.Selector;

public class UDPConnection extends AbstractConnection {

    public static final long IDLE_TIMEOUT = 2 * 60 * 1000;

    private static final String TAG = UDPConnection.class.getSimpleName();
    private static final int READINESS_BUDGET = 32;
    private static final long MAX_QUEUE_AGE_MS = 10;
    private static final int TUN_MTU = 0x4000;
    private static final int MAX_UDP_PAYLOAD = TUN_MTU - 20 - 8;
    private static final int UDP_QUEUE_DATAGRAMS = 8;

    private final DatagramBuffer clientToNetwork = new DatagramBuffer(
            4 * TUN_MTU, MAX_UDP_PAYLOAD, UDP_QUEUE_DATAGRAMS);
    private final Packetizer networkToClient;
    private final InetSocketAddress destination;

    private final DatagramChannel channel;
    private final SelectionKey selectionKey;
    private int interests;

    private long idleSince;

    public UDPConnection(ConnectionId id, Client client, Selector selector, IPv4Header ipv4Header, UDPHeader udpHeader) throws IOException {
        super(id, client);

        networkToClient = new Packetizer(ipv4Header, udpHeader, TUN_MTU);
        networkToClient.getResponseIPv4Header().swapSourceAndDestination();
        networkToClient.getResponseTransportHeader().swapSourceAndDestination();

        touch();
        destination = getRewrittenDestination();

        SelectionHandler selectionHandler = (selectionKey) -> {
            touch();
            if (selectionKey.isValid() && selectionKey.isReadable()) {
                processReceiveReady();
            }
            if (selectionKey.isValid() && selectionKey.isWritable()) {
                processSendReady();
            }
            updateInterests();
        };
        channel = createChannel();
        interests = SelectionKey.OP_READ;
        selectionKey = channel.register(selector, interests, selectionHandler);
    }

    @Override
    public void sendToNetwork(IPv4Packet packet) {
        touch();
        int payloadLength = packet.getPayloadLength();
        if (!clientToNetwork.readFrom(packet.getPayload())) {
            logw(TAG, "Cannot send to network, dropping packet");
            Diagnostics.increment("drops.udp_queue_full");
            Diagnostics.add("drops.udp_queue_full_bytes", payloadLength);
            return;
        }
        Diagnostics.increment("udp.queue_datagrams");
        Diagnostics.add("udp.queue_bytes", payloadLength);
        Diagnostics.recordMaximum("udp.queue_datagrams_max", clientToNetwork.getDatagramCount());
        Diagnostics.recordMaximum("udp.queue_bytes_max", clientToNetwork.getQueuedPayloadBytes());
        updateInterests();
    }

    @Override
    public void disconnect() {
        logd(TAG, "Close");
        Diagnostics.add("udp.queue_datagrams", -clientToNetwork.getDatagramCount());
        Diagnostics.add("udp.queue_bytes", -clientToNetwork.getQueuedPayloadBytes());
        selectionKey.cancel();
        try {
            channel.close();
        } catch (IOException e) {
            loge(TAG, "Cannot close connection channel", e);
        }
    }

    @Override
    public boolean isExpired() {
        return System.currentTimeMillis() >= idleSince + IDLE_TIMEOUT;
    }

    private DatagramChannel createChannel() throws IOException {
        logd(TAG, "Open");
        DatagramChannel datagramChannel = DatagramChannel.open(StandardProtocolFamily.INET);
        datagramChannel.socket().setBroadcast(true);
        datagramChannel.configureBlocking(false);
        datagramChannel.bind(null);
        return datagramChannel;
    }

    private void touch() {
        idleSince = System.currentTimeMillis();
    }

    private void processReceiveReady() {
        for (int i = 0; i < READINESS_BUDGET; ++i) {
            IPv4Packet packet;
            try {
                packet = networkToClient.packetizeDatagram(channel);
            } catch (IOException e) {
                loge(TAG, "Cannot read", e);
                close();
                return;
            }
            if (packet == null) {
                return;
            }
            pushToClient(packet);
        }
        Diagnostics.increment("udp.read_fairness_yields");
    }

    private void processSendReady() {
        int queuedBytesBeforeExpiry = clientToNetwork.getQueuedPayloadBytes();
        Diagnostics.recordMaximum("udp.queue_age_ms_max", clientToNetwork.getOldestAgeMillis());
        int expired = clientToNetwork.discardExpired(MAX_QUEUE_AGE_MS);
        if (expired > 0) {
            Diagnostics.add("udp.queue_datagrams", -expired);
            Diagnostics.add("udp.queue_bytes", clientToNetwork.getQueuedPayloadBytes() - queuedBytesBeforeExpiry);
        }
        for (int i = 0; i < READINESS_BUDGET && !clientToNetwork.isEmpty(); ++i) {
            int count = clientToNetwork.getDatagramCount();
            int bytes = clientToNetwork.getQueuedPayloadBytes();
            if (!write()) {
                close();
                return;
            }
            if (count == clientToNetwork.getDatagramCount()) {
                return;
            }
            Diagnostics.add("bytes.client_to_network_udp", bytes - clientToNetwork.getQueuedPayloadBytes());
            Diagnostics.add("udp.queue_datagrams", -1);
            Diagnostics.add("udp.queue_bytes", clientToNetwork.getQueuedPayloadBytes() - bytes);
        }
        if (!clientToNetwork.isEmpty()) {
            Diagnostics.increment("udp.write_fairness_yields");
        }
    }

    private boolean write() {
        try {
            return clientToNetwork.sendTo(channel, destination);
        } catch (IOException e) {
            loge(TAG, "Cannot write", e);
            return false;
        }
    }

    private void pushToClient(IPv4Packet packet) {
        if (!sendToClient(packet)) {
            logw(TAG, "Cannot send to client, dropping packet");
            Diagnostics.increment("drops.client_queue_full_udp");
            Diagnostics.add("drops.client_queue_full_udp_bytes", packet.getPayloadLength());
            return;
        }
        Diagnostics.add("bytes.network_to_client_udp", packet.getPayloadLength());
        logd(TAG, () -> "Packet (" + packet.getPayloadLength() + " bytes) sent to client");
        if (Log.isVerboseEnabled()) {
            logv(TAG, Binary.buildPacketString(packet.getRaw()));
        }
    }

    protected void updateInterests() {
        if (!selectionKey.isValid()) {
            return;
        }
        int interestOps = SelectionKey.OP_READ;
        if (mayWrite()) {
            interestOps |= SelectionKey.OP_WRITE;
        }
        if (interests != interestOps) {
            // interests must be changed
            interests = interestOps;
            selectionKey.interestOps(interestOps);
        }
    }

    private boolean mayWrite() {
        return !clientToNetwork.isEmpty();
    }
}
