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

import java.io.InputStream;
import java.net.Inet4Address;
import java.net.InetSocketAddress;
import java.net.ServerSocket;
import java.net.Socket;
import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicReference;

@SuppressWarnings("checkstyle:MagicNumber")
public class AdbMonitorTest {

    private static ByteBuffer toByteBuffer(String s) {
        return ByteBuffer.wrap(s.getBytes(StandardCharsets.US_ASCII));
    }

    @Test
    public void testReadValidPacket() {
        String data = "00180123456789ABCDEF\tdevice\n";
        String result = AdbMonitor.readPacket(toByteBuffer(data));
        Assert.assertEquals("0123456789ABCDEF\tdevice\n", result);
    }

    @Test
    public void testReadValidPackets() {
        String data = "00300123456789ABCDEF\tdevice\nFEDCBA9876543210\tdevice\n";
        String result = AdbMonitor.readPacket(toByteBuffer(data));
        Assert.assertEquals("0123456789ABCDEF\tdevice\nFEDCBA9876543210\tdevice\n", result);
    }

    @Test
    public void testReadValidPacketWithGarbage() {
        String data = "00180123456789ABCDEF\tdevice\ngarbage";
        String result = AdbMonitor.readPacket(toByteBuffer(data));
        Assert.assertEquals("0123456789ABCDEF\tdevice\n", result);
    }

    @Test
    public void testReadShortPacket() {
        String data = "00180123456789ABCDEF\tdevi";
        String result = AdbMonitor.readPacket(toByteBuffer(data));
        Assert.assertNull(result);
    }

    @Test
    public void testHandlePacketDevice() {
        final String[] pSerial = new String[1];
        AdbMonitor monitor = new AdbMonitor((serial) -> pSerial[0] = serial);
        String packet = "0123456789ABCDEF\tdevice\n";
        monitor.handlePacket(packet);
        Assert.assertEquals("0123456789ABCDEF", pSerial[0]);
    }

    @Test
    public void testHandlePacketOffline() {
        final String[] pSerial = new String[1];
        AdbMonitor monitor = new AdbMonitor((serial) -> pSerial[0] = serial);
        String packet = "0123456789ABCDEF\toffline\n";
        monitor.handlePacket(packet);
        Assert.assertNull(pSerial[0]);
    }

    @Test
    public void testMultipleConnectedDevices() {
        final String[] pSerials = new String[2];
        AdbMonitor monitor = new AdbMonitor(new AdbMonitor.AdbDevicesCallback() {
            private int i;
            @Override
            public void onNewDeviceConnected(String serial) {
                pSerials[i++] = serial;
            }
        });
        String packet = "0123456789ABCDEF\tdevice\nFEDCBA9876543210\tdevice\n";
        monitor.handlePacket(packet);
        Assert.assertEquals("0123456789ABCDEF", pSerials[0]);
        Assert.assertEquals("FEDCBA9876543210", pSerials[1]);
    }

    @Test
    @SuppressWarnings("checkstyle:MagicNumber")
    public void testMultipleConnectedDevicesWithDisconnection() {
        final String[] pSerials = new String[3];
        AdbMonitor monitor = new AdbMonitor(new AdbMonitor.AdbDevicesCallback() {
            private int i;
            @Override
            public void onNewDeviceConnected(String serial) {
                pSerials[i++] = serial;
            }
        });
        String packet = "0123456789ABCDEF\tdevice\nFEDCBA9876543210\tdevice\n";
        monitor.handlePacket(packet);
        packet = "0123456789ABCDEF\tdevice\n";
        monitor.handlePacket(packet);
        packet = "0123456789ABCDEF\tdevice\nFEDCBA9876543210\tdevice\n";
        monitor.handlePacket(packet);
        Assert.assertEquals("0123456789ABCDEF", pSerials[0]);
        Assert.assertEquals("FEDCBA9876543210", pSerials[1]);
        Assert.assertEquals("FEDCBA9876543210", pSerials[2]);
    }

    @Test
    public void testClearConnectedDevicesReplaysDeviceAfterMonitorReconnect() {
        final int[] callbackCount = new int[1];
        AdbMonitor monitor = new AdbMonitor((serial) -> ++callbackCount[0]);
        String packet = "0123456789ABCDEF\tdevice\n";

        monitor.handlePacket(packet);
        monitor.clearConnectedDevices();
        monitor.handlePacket(packet);

        Assert.assertEquals(2, callbackCount[0]);
    }

    @Test
    public void testHandshakeDeadline() throws Exception {
        try (ServerSocket server = new ServerSocket(0, 1, Inet4Address.getLoopbackAddress())) {
            Thread peer = new Thread(() -> acceptAndRemainSilent(server), "silent-adb-peer");
            peer.setDaemon(true);
            peer.start();
            AdbMonitor monitor = new AdbMonitor((serial) -> { }, "adb",
                    new InetSocketAddress(Inet4Address.getLoopbackAddress(), server.getLocalPort()), 1000, 50, 50);

            try {
                monitor.trackDevices();
                Assert.fail("Expected handshake timeout");
            } catch (java.net.SocketTimeoutException expected) {
                // expected
            }
        }
    }

    @Test
    public void testRejectedHandshakeFailsInsteadOfTightLooping() throws Exception {
        try (ServerSocket server = new ServerSocket(0, 1, Inet4Address.getLoopbackAddress())) {
            Thread peer = new Thread(() -> acceptAndReply(server, "FAIL"), "rejecting-adb-peer");
            peer.setDaemon(true);
            peer.start();
            AdbMonitor monitor = new AdbMonitor((serial) -> { }, "adb",
                    new InetSocketAddress(Inet4Address.getLoopbackAddress(), server.getLocalPort()), 1000, 1000, 50);

            try {
                monitor.trackDevices();
                Assert.fail("Expected rejected handshake");
            } catch (java.io.IOException expected) {
                Assert.assertTrue(expected.getMessage().contains("rejected"));
            }
        }
    }

    @Test
    public void testTrackReadDeadlineMakesInterruptPrompt() throws Exception {
        CountDownLatch handshakeSent = new CountDownLatch(1);
        AtomicReference<Throwable> failure = new AtomicReference<>();
        try (ServerSocket server = new ServerSocket(0, 1, Inet4Address.getLoopbackAddress())) {
            Thread peer = new Thread(() -> acceptTrackRequest(server, handshakeSent), "tracking-adb-peer");
            peer.setDaemon(true);
            peer.start();
            AdbMonitor monitor = new AdbMonitor((serial) -> { }, "adb",
                    new InetSocketAddress(Inet4Address.getLoopbackAddress(), server.getLocalPort()), 1000, 1000, 50);
            Thread tracking = new Thread(() -> {
                try {
                    monitor.trackDevices();
                } catch (Throwable throwable) {
                    failure.set(throwable);
                }
            }, "adb-monitor-test");
            tracking.start();
            Assert.assertTrue(handshakeSent.await(1, TimeUnit.SECONDS));
            tracking.interrupt();
            tracking.join(1000);

            Assert.assertFalse("tracking thread ignored interrupt", tracking.isAlive());
            Assert.assertNull(failure.get());
        }
    }

    private static void acceptAndRemainSilent(ServerSocket server) {
        try (Socket ignored = server.accept()) {
            Thread.sleep(1000);
        } catch (Exception ignored) {
            // The client closing the test socket is expected.
        }
    }

    private static void acceptAndReply(ServerSocket server, String reply) {
        try (Socket socket = server.accept()) {
            socket.getOutputStream().write(reply.getBytes(StandardCharsets.US_ASCII));
            socket.getOutputStream().flush();
        } catch (Exception ignored) {
            // The test assertion reports client-side failures.
        }
    }

    private static void acceptTrackRequest(ServerSocket server, CountDownLatch handshakeSent) {
        try (Socket socket = server.accept()) {
            InputStream input = socket.getInputStream();
            byte[] request = new byte[22];
            int offset = 0;
            while (offset < request.length) {
                int count = input.read(request, offset, request.length - offset);
                if (count == -1) {
                    return;
                }
                offset += count;
            }
            socket.getOutputStream().write("OKAY0000".getBytes(StandardCharsets.US_ASCII));
            socket.getOutputStream().flush();
            handshakeSent.countDown();
            while (input.read() != -1) {
                // Wait for the monitor to close after observing its interrupt.
            }
        } catch (Exception ignored) {
            // The client closing the test socket is expected.
        }
    }
}
