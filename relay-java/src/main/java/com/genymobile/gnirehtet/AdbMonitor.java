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

import com.genymobile.gnirehtet.relay.Diagnostics;
import com.genymobile.gnirehtet.relay.Log;

import java.io.EOFException;
import java.io.IOException;
import java.net.Inet4Address;
import java.net.InetSocketAddress;
import java.net.Socket;
import java.net.SocketTimeoutException;
import java.nio.ByteBuffer;
import java.nio.channels.Channels;
import java.nio.channels.ClosedByInterruptException;
import java.nio.channels.ReadableByteChannel;
import java.nio.channels.WritableByteChannel;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.Collections;
import java.util.List;

public class AdbMonitor {

    public interface AdbDevicesCallback {
        void onNewDeviceConnected(String serial);
    }

    private static final String TAG = AdbMonitor.class.getSimpleName();
    private static final int ADBD_PORT = 5037;

    private static final String TRACK_DEVICES_REQUEST = "0012host:track-devices";
    private static final int BUFFER_SIZE = 1024;
    private static final int LENGTH_FIELD_SIZE = 4;
    private static final int OKAY_SIZE = 4;
    private static final long RETRY_DELAY_ADB_DAEMON_OK = 1000;
    // Keep detection comfortably inside the three-second reconnect gate once
    // ADB becomes available again. The command itself still has a hard timeout.
    private static final long RETRY_DELAY_ADB_DAEMON_KO = 1000;
    private static final long ADB_COMMAND_TIMEOUT_MS = 15000;
    private static final int ADB_CONNECT_TIMEOUT_MS = 5000;
    private static final int ADB_HANDSHAKE_TIMEOUT_MS = 5000;
    private static final int ADB_TRACK_POLL_TIMEOUT_MS = 1000;

    private List<String> connectedDevices = new ArrayList<>();

    private final AdbDevicesCallback callback;
    private final String adbPath;
    private final InetSocketAddress daemonAddress;
    private final int connectTimeoutMs;
    private final int handshakeTimeoutMs;
    private final int trackPollTimeoutMs;

    private final ByteBuffer socketBuffer = ByteBuffer.allocate(BUFFER_SIZE);

    public AdbMonitor(AdbDevicesCallback callback) {
        this(callback, getConfiguredAdbPath());
    }

    public AdbMonitor(AdbDevicesCallback callback, String adbPath) {
        this(callback, adbPath, new InetSocketAddress(Inet4Address.getLoopbackAddress(), ADBD_PORT),
                ADB_CONNECT_TIMEOUT_MS, ADB_HANDSHAKE_TIMEOUT_MS, ADB_TRACK_POLL_TIMEOUT_MS);
    }

    AdbMonitor(AdbDevicesCallback callback, String adbPath, InetSocketAddress daemonAddress,
            int connectTimeoutMs, int handshakeTimeoutMs, int trackPollTimeoutMs) {
        this.callback = callback;
        this.adbPath = adbPath;
        this.daemonAddress = daemonAddress;
        this.connectTimeoutMs = connectTimeoutMs;
        this.handshakeTimeoutMs = handshakeTimeoutMs;
        this.trackPollTimeoutMs = trackPollTimeoutMs;
    }

    public void monitor() {
        while (!Thread.currentThread().isInterrupted()) {
            try {
                trackDevices();
            } catch (Exception e) {
                clearConnectedDevices();
                if (Thread.currentThread().isInterrupted()) {
                    break;
                }
                Log.e(TAG, "Failed to monitor adb devices", e);
                repairAdbDaemon();
            } finally {
                // A new track-devices session must replay every connected device. Keeping
                // stale state here used to suppress reconnect callbacks after adb restarted.
                clearConnectedDevices();
            }
        }
    }

    private static String getConfiguredAdbPath() {
        String configured = System.getenv("ADB");
        return configured != null ? configured : "adb";
    }

    void trackDevices() throws IOException {
        try (Socket socket = new Socket()) {
            socket.connect(daemonAddress, connectTimeoutMs);
            socket.setSoTimeout(handshakeTimeoutMs);
            ReadableByteChannel input = Channels.newChannel(socket.getInputStream());
            WritableByteChannel output = Channels.newChannel(socket.getOutputStream());
            startTracking(input, output);
            socket.setSoTimeout(trackPollTimeoutMs);
            trackPackets(input);
        } catch (ClosedByInterruptException e) {
            if (!Thread.currentThread().isInterrupted()) {
                throw e;
            }
        }
    }

    private void startTracking(ReadableByteChannel input, WritableByteChannel output) throws IOException {
        socketBuffer.clear();
        writeRequest(output, TRACK_DEVICES_REQUEST);
        // the daemon initially sends "OKAY" if it understands the request
        if (!consumeOkay(input)) {
            throw new IOException("ADB daemon rejected host:track-devices");
        }
    }

    private void trackPackets(ReadableByteChannel input) throws IOException {
        while (!Thread.currentThread().isInterrupted()) {
            try {
                String packet = nextPacket(input);
                handlePacket(packet);
            } catch (SocketTimeoutException e) {
                // A quiet track-devices stream is normal. The finite read deadline is
                // only a cancellation point and prevents a wedged daemon from hiding interrupts.
                Diagnostics.increment("adb.track_poll_timeouts");
            }
        }
    }

    private static void writeRequest(WritableByteChannel channel, String request) throws IOException {
        ByteBuffer requestBuffer = ByteBuffer.wrap(request.getBytes(StandardCharsets.US_ASCII));
        while (requestBuffer.hasRemaining()) {
            channel.write(requestBuffer);
        }
    }

    private boolean consumeOkay(ReadableByteChannel channel) throws IOException {
        byte[] response = new byte[OKAY_SIZE];
        while (channel.read(socketBuffer) != -1) {
            if (socketBuffer.position() < OKAY_SIZE) {
                // not enough data
                continue;
            }
            socketBuffer.flip();
            socketBuffer.get(response, 0, OKAY_SIZE);
            socketBuffer.compact();
            socketBuffer.flip();
            String text = new String(response, StandardCharsets.US_ASCII);
            return "OKAY".equals(text);
        }
        return false;
    }

    private String nextPacket(ReadableByteChannel channel) throws IOException {
        String packet;
        while ((packet = readPacket(socketBuffer)) == null) {
            // need more data
            fillBufferFrom(channel);
        }
        return packet;
    }

    private void fillBufferFrom(ReadableByteChannel channel) throws IOException {
        socketBuffer.compact();
        try {
            if (channel.read(socketBuffer) == -1) {
                throw new EOFException("ADB daemon closed the track-devices connection");
            }
        } finally {
            socketBuffer.flip();
        }
    }

    static String readPacket(ByteBuffer input) {
        if (input.remaining() < LENGTH_FIELD_SIZE) {
            return null;
        }
        // each packet contains 4 bytes representing the String length in hexa, followed by a list of device states, one per line;
        // each line contains: the device serial, `\t', the state, '\n'
        // for example: "00360123456789abcdef\tdevice\nfedcba9876543210\tunauthorized\n":
        //  - 0036 indicates that the data is 0x36 (54) bytes length
        //  - the device with serial 0123456789abcdef is connected
        //  - the device with serial fedcba9876543210 is unauthorized
        input.mark();
        byte[] lengthField = new byte[LENGTH_FIELD_SIZE];
        input.get(lengthField);
        int length = parseLength(lengthField);
        if (length > BUFFER_SIZE - LENGTH_FIELD_SIZE) {
            throw new IllegalArgumentException("Packet size should not be that big: " + length);
        }
        if (input.remaining() < length) {
            // not enough data
            input.reset();
            return null;
        }
        byte[] payload = new byte[length];
        input.get(payload);
        return new String(payload, StandardCharsets.UTF_8);
    }

    void handlePacket(String packet) {
        List<String> currentConnectedDevices = parseConnectedDevices(packet);
        for (String serial : currentConnectedDevices) {
            if (!connectedDevices.contains(serial)) {
                callback.onNewDeviceConnected(serial);
            }
        }
        connectedDevices = currentConnectedDevices;
    }

    void clearConnectedDevices() {
        connectedDevices = Collections.emptyList();
    }

    private static List<String> parseConnectedDevices(String packet) {
        List<String> list = new ArrayList<>();
        for (String line : packet.split("\\n")) {
            String[] tokens = line.split("\\s+");
            if (tokens.length == 2) {
                String state = tokens[1];
                if ("device".equals(state)) {
                    String serial = tokens[0];
                    list.add(serial);
                }
            }
        }
        return list;
    }

    @SuppressWarnings("checkstyle:MagicNumber")
    private static int parseLength(byte[] data) {
        if (data.length < LENGTH_FIELD_SIZE) {
            throw new IllegalArgumentException("Length field must be at least 4 bytes length");
        }
        int result = 0;
        for (int i = 0; i < LENGTH_FIELD_SIZE; ++i) {
            char c = (char) data[i];
            int digit = Character.digit(c, 0x10);
            if (digit < 0) {
                throw new IllegalArgumentException("Invalid ADB packet length field");
            }
            result = (result << 4) + digit;
        }
        return result;
    }

    private void repairAdbDaemon() {
        if (startAdbDaemon()) {
            sleep(RETRY_DELAY_ADB_DAEMON_OK);
        } else {
            sleep(RETRY_DELAY_ADB_DAEMON_KO);
        }
    }

    private boolean startAdbDaemon() {
        Log.i(TAG, "Restarting adb daemon");
        try {
            ProcessRunner.Result result = ProcessRunner.runInherited(
                    Arrays.asList(adbPath, "start-server"), ADB_COMMAND_TIMEOUT_MS);
            if (result.getExitCode() != 0) {
                Log.e(TAG, "Could not restart adb daemon (exited on error)");
                return false;
            }
            return true;
        } catch (InterruptedException | IOException e) {
            if (e instanceof InterruptedException) {
                Thread.currentThread().interrupt();
            }
            Log.e(TAG, "Could not restart adb daemon", e);
            return false;
        }
    }

    private static void sleep(long delay) {
        try {
            Thread.sleep(delay);
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
        }
    }
}
