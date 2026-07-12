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

import com.genymobile.gnirehtet.relay.CommandExecutionException;
import com.genymobile.gnirehtet.relay.Diagnostics;
import com.genymobile.gnirehtet.relay.Log;
import com.genymobile.gnirehtet.relay.Relay;

import java.io.IOException;
import java.net.InetAddress;
import java.net.InetSocketAddress;
import java.net.Socket;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.Collections;
import java.util.HashSet;
import java.util.List;
import java.util.Map;
import java.util.Scanner;
import java.util.Set;
import java.util.concurrent.ConcurrentHashMap;
import java.util.regex.Matcher;
import java.util.regex.Pattern;

public final class Main {
    private static final String TAG = "Gnirehtet";
    private static final String NL = System.lineSeparator();
    private static final String REQUIRED_APK_VERSION_CODE = "11";
    private static final int START_RETRY_ATTEMPTS = 5;
    private static final long START_RETRY_DELAY_STEP_MS = 1000;
    private static final long RETRY_DELAY_AFTER_START_SEQUENCE_MS = 1000;
    private static final long ADB_COMMAND_TIMEOUT_MS = 30000;
    private static final long ADB_QUERY_TIMEOUT_MS = 10000;
    private static final long STOP_VERIFICATION_TIMEOUT_MS = 10000;
    private static final long STOP_POLL_INTERVAL_MS = 250;
    private static final long TUNNEL_HEALTH_INTERVAL_MS = 2000;
    private static final int RELAY_PROBE_TIMEOUT_MS = 500;
    private static final String DEFAULT_START_KEY = "<default>";
    private static final Set<String> STARTING_SERIALS = Collections.synchronizedSet(new HashSet<>());
    private static final Set<String> MONITORED_TUNNELS = Collections.synchronizedSet(new HashSet<>());
    private static final Set<String> STOP_REQUESTED_SERIALS = Collections.synchronizedSet(new HashSet<>());
    private static final Map<String, Object> TUNNEL_LOCKS = new ConcurrentHashMap<>();

    private Main() {
        // not instantiable
    }

    private static String getAdbPath() {
        String adb = System.getenv("ADB");
        return adb != null ? adb : "adb";
    }

    private static String getApkPath() {
        String apk = System.getenv("GNIREHTET_APK");
        return apk != null ? apk : "gnirehtet.apk";
    }

    enum Command {
        INSTALL("install", CommandLineArguments.PARAM_SERIAL) {
            @Override
            String getDescription() {
                return "Install the client on the Android device and exit.\n"
                        + "If several devices are connected via adb, then serial must be\n"
                        + "specified.";
            }

            @Override
            void execute(CommandLineArguments args) throws Exception {
                cmdInstall(args.getSerial());
            }
        },
        UNINSTALL("uninstall", CommandLineArguments.PARAM_SERIAL) {
            @Override
            String getDescription() {
                return "Uninstall the client from the Android device and exit.\n"
                        + "If several devices are connected via adb, then serial must be\n"
                        + "specified.";
            }

            @Override
            void execute(CommandLineArguments args) throws Exception {
                cmdUninstall(args.getSerial());
            }
        },
        REINSTALL("reinstall", CommandLineArguments.PARAM_SERIAL) {
            @Override
            String getDescription() {
                return "Uninstall then install.";
            }

            @Override
            void execute(CommandLineArguments args) throws Exception {
                cmdReinstall(args.getSerial());
            }
        },
        RUN("run", CommandLineArguments.PARAM_SERIAL | CommandLineArguments.PARAM_DNS_SERVER | CommandLineArguments.PARAM_ROUTES
                | CommandLineArguments.PARAM_PORT | CommandLineArguments.PARAM_ALL_TRAFFIC) {
            @Override
            String getDescription() {
                return "Enable reverse tethering for exactly one device:\n"
                        + "  - install the client if necessary;\n"
                        + "  - start the client;\n"
                        + "  - start the relay server;\n"
                        + "  - on Ctrl+C, stop both the relay server and the client.";
            }

            @Override
            void execute(CommandLineArguments args) throws Exception {
                cmdRun(args.getSerial(), args.getDnsServers(), args.getRoutes(), args.getPort(), args.isAllTraffic());
            }
        },
        AUTORUN("autorun", CommandLineArguments.PARAM_DNS_SERVER | CommandLineArguments.PARAM_ROUTES | CommandLineArguments.PARAM_PORT
                | CommandLineArguments.PARAM_ALL_TRAFFIC) {
            @Override
            String getDescription() {
                return "Enable reverse tethering for all devices:\n"
                        + "  - monitor devices and start clients (autostart);\n"
                        + "  - start the relay server.";
            }

            @Override
            void execute(CommandLineArguments args) throws Exception {
                cmdAutorun(args.getDnsServers(), args.getRoutes(), args.getPort(), args.isAllTraffic());
            }
        },
        START("start", CommandLineArguments.PARAM_SERIAL | CommandLineArguments.PARAM_DNS_SERVER | CommandLineArguments.PARAM_ROUTES
                | CommandLineArguments.PARAM_PORT | CommandLineArguments.PARAM_ALL_TRAFFIC) {
            @Override
            String getDescription() {
                return "Start a client on the Android device and exit.\n"
                        + "If several devices are connected via adb, then serial must be\n"
                        + "specified.\n"
                        + "If -d is given, then make the Android device use the specified\n"
                        + "DNS server(s). Otherwise, use 8.8.8.8 (Google public DNS).\n"
                        + "If -r is given, then only reverse tether the specified routes.\n"
                        + "Otherwise, use 0.0.0.0/0.\n"
                        + "If -p is given, then make the relay server listen on the specified\n"
                        + "port. Otherwise, use port 31416.\n"
                        + "If --all-traffic is given, route every app for diagnostics.\n"
                        + "Otherwise, route Virtual Desktop only.\n"
                        + "If the client is already started, then do nothing, and ignore\n"
                        + "the other parameters.\n"
                        + "10.0.2.2 is mapped to the host 'localhost'.";
            }

            @Override
            void execute(CommandLineArguments args) throws Exception {
                cmdStartWithRetries(args.getSerial(), args.getDnsServers(), args.getRoutes(), args.getPort(), args.isAllTraffic());
            }
        },
        AUTOSTART("autostart", CommandLineArguments.PARAM_DNS_SERVER | CommandLineArguments.PARAM_ROUTES | CommandLineArguments.PARAM_PORT
                | CommandLineArguments.PARAM_ALL_TRAFFIC) {
            @Override
            String getDescription() {
                return "Listen for device connexions and start a client on every detected\n"
                        + "device.\n"
                        + "Accept the same parameters as the start command (excluding the\n"
                        + "serial, which will be taken from the detected device).";
            }

            @Override
            void execute(CommandLineArguments args) throws Exception {
                cmdAutostart(args.getDnsServers(), args.getRoutes(), args.getPort(), args.isAllTraffic());
            }
        },
        STOP("stop", CommandLineArguments.PARAM_SERIAL) {
            @Override
            String getDescription() {
                return "Stop the client on the Android device and exit.\n"
                        + "If several devices are connected via adb, then serial must be\n"
                        + "specified.";
            }

            @Override
            void execute(CommandLineArguments args) throws Exception {
                cmdStop(args.getSerial());
            }
        },
        RESTART("restart", CommandLineArguments.PARAM_SERIAL | CommandLineArguments.PARAM_DNS_SERVER | CommandLineArguments.PARAM_ROUTES
                | CommandLineArguments.PARAM_PORT | CommandLineArguments.PARAM_ALL_TRAFFIC) {
            @Override
            String getDescription() {
                return "Stop then start.";
            }

            @Override
            void execute(CommandLineArguments args) throws Exception {
                cmdRestart(args.getSerial(), args.getDnsServers(), args.getRoutes(), args.getPort(), args.isAllTraffic());
            }
        },
        TUNNEL("tunnel", CommandLineArguments.PARAM_SERIAL | CommandLineArguments.PARAM_PORT) {
            @Override
            String getDescription() {
                return "Set up the 'adb reverse' tunnel.\n"
                        + "If a device is unplugged then plugged back while gnirehtet is\n"
                        + "active, resetting the tunnel is sufficient to get the\n"
                        + "connection back.";
            }

            @Override
            void execute(CommandLineArguments args) throws Exception {
                cmdTunnel(args.getSerial(), args.getPort());
            }
        },
        STATUS("status", CommandLineArguments.PARAM_SERIAL | CommandLineArguments.PARAM_PORT) {
            @Override
            String getDescription() {
                return "Print Android VPN, reverse-tunnel, and relay metric state.";
            }

            @Override
            void execute(CommandLineArguments args) throws Exception {
                cmdStatus(args.getSerial(), args.getPort());
            }
        },
        DOCTOR("doctor", CommandLineArguments.PARAM_SERIAL | CommandLineArguments.PARAM_PORT) {
            @Override
            String getDescription() {
                return "Run read-only Android, adb tunnel, and Virtual Desktop checks.";
            }

            @Override
            void execute(CommandLineArguments args) throws Exception {
                cmdDoctor(args.getSerial(), args.getPort());
            }
        },
        RELAY("relay", CommandLineArguments.PARAM_PORT) {
            @Override
            String getDescription() {
                return "Start the relay server in the current terminal.";
            }

            @Override
            void execute(CommandLineArguments args) throws Exception {
                cmdRelay(args.getPort());
            }
        };

        private String command;
        private int acceptedParameters;

        Command(String command, int acceptedParameters) {
            this.command = command;
            this.acceptedParameters = acceptedParameters;
        }

        abstract String getDescription();

        abstract void execute(CommandLineArguments args) throws Exception;
    }

    private static void cmdInstall(String serial) throws InterruptedException, IOException, CommandExecutionException {
        Log.i(TAG, "Installing gnirehtet client...");
        execAdb(serial, "install", "-r", getApkPath());
    }

    private static void cmdUninstall(String serial) throws InterruptedException, IOException, CommandExecutionException {
        Log.i(TAG, "Uninstalling gnirehtet client...");
        execAdb(serial, "uninstall", "com.genymobile.gnirehtet");
    }

    private static void cmdReinstall(String serial) throws InterruptedException, IOException, CommandExecutionException {
        cmdUninstall(serial);
        cmdInstall(serial);
    }

    private static void cmdRun(String serial, String dnsServers, String routes, int port, boolean allTraffic) throws IOException {
        String runSerial = resolveRunSerial(serial);
        if (runSerial != null) {
            // start in parallel so that the relay server is ready when the client connects
            asyncMonitorStart(runSerial, dnsServers, routes, port, allTraffic);
        } else {
            // fall back to the historical one-shot behavior if no single device can be identified
            asyncStart(serial, dnsServers, routes, port, allTraffic);
        }

        Runtime.getRuntime().addShutdownHook(new Thread(() -> {
            // executed on Ctrl+C
            try {
                cmdStop(runSerial != null ? runSerial : serial);
            } catch (Exception e) {
                Log.e(TAG, "Cannot stop client", e);
            }
        }));

        cmdRelay(port);
    }

    private static void cmdAutorun(final String dnsServers, final String routes, int port, boolean allTraffic) throws IOException {
        new Thread(() -> {
            try {
                cmdAutostart(dnsServers, routes, port, allTraffic);
            } catch (Exception e) {
                Log.e(TAG, "Cannot auto start clients", e);
            }
        }).start();

        cmdRelay(port);
    }

    @SuppressWarnings("checkstyle:MagicNumber")
    private static void cmdStart(String serial, String dnsServers, String routes, int port, boolean allTraffic)
            throws InterruptedException, IOException, CommandExecutionException {
        if (mustInstallClient(serial)) {
            cmdInstall(serial);
            // wait a bit after the app is installed so that intent actions are correctly registered
            Thread.sleep(500); // ms
        }

        Log.i(TAG, "Starting client...");
        cmdTunnel(serial, port);

        List<String> cmd = new ArrayList<>();
        Collections.addAll(cmd, "shell", "am", "start", "-a", "com.genymobile.gnirehtet.START", "-n",
                "com.genymobile.gnirehtet/.GnirehtetActivity");
        if (dnsServers != null) {
            Collections.addAll(cmd, "--esa", "dnsServers", dnsServers);
        }
        if (routes != null) {
            Collections.addAll(cmd, "--esa", "routes", routes);
        }
        if (allTraffic) {
            Collections.addAll(cmd, "--ez", "allTraffic", "true");
        }
        execAdb(serial, cmd);
    }

    private static void cmdStartWithRetries(String serial, String dnsServers, String routes, int port, boolean allTraffic)
            throws InterruptedException, IOException, CommandExecutionException {
        Exception lastException = null;
        for (int attempt = 1; attempt <= START_RETRY_ATTEMPTS; ++attempt) {
            try {
                cmdStart(serial, dnsServers, routes, port, allTraffic);
                return;
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
                throw e;
            } catch (IOException | CommandExecutionException e) {
                lastException = e;
                if (attempt == START_RETRY_ATTEMPTS) {
                    break;
                }
                long retryDelayMs = attempt * START_RETRY_DELAY_STEP_MS;
                Log.w(TAG, "Cannot start client, retrying in " + retryDelayMs + "ms (attempt " + attempt + "/" + START_RETRY_ATTEMPTS + ")", e);
                Thread.sleep(retryDelayMs);
            }
        }

        if (lastException instanceof IOException) {
            throw (IOException) lastException;
        }
        if (lastException instanceof CommandExecutionException) {
            throw (CommandExecutionException) lastException;
        }
        throw new AssertionError("Client start failed without an exception");
    }

    private static void cmdAutostart(final String dnsServers, final String routes, int port, boolean allTraffic) {
        AdbMonitor adbMonitor = new AdbMonitor((serial) -> {
            asyncStart(serial, dnsServers, routes, port, allTraffic);
        }, getAdbPath());
        adbMonitor.monitor();
    }

    private static String resolveRunSerial(String serial) {
        if (serial != null) {
            return serial;
        }

        try {
            String currentDeviceSerial = getCurrentDeviceSerial();
            if (currentDeviceSerial != null && !currentDeviceSerial.isEmpty() && !"unknown".equals(currentDeviceSerial)) {
                return currentDeviceSerial;
            }
            Log.w(TAG, "Cannot determine a device serial for automatic reconnects, keeping the previous one-shot behavior");
        } catch (InterruptedException | IOException | CommandExecutionException e) {
            Log.w(TAG, "Cannot determine a device serial for automatic reconnects, keeping the previous one-shot behavior", e);
        }
        return null;
    }

    private static String getCurrentDeviceSerial() throws InterruptedException, IOException, CommandExecutionException {
        List<String> command = createAdbCommand(null, "get-serialno");
        Log.d(TAG, "Execute: " + command);
        ProcessRunner.Result result = ProcessRunner.runCaptured(command, ADB_QUERY_TIMEOUT_MS);
        requireSuccess(command, result);
        Scanner scanner = new Scanner(result.getOutput());
        try {
            if (scanner.hasNextLine()) {
                return scanner.nextLine().trim();
            }
            return null;
        } finally {
            scanner.close();
        }
    }

    private static void asyncMonitorStart(String serial, String dnsServers, String routes, int port, boolean allTraffic) {
        new Thread(() -> {
            AdbMonitor adbMonitor = new AdbMonitor((connectedSerial) -> {
                if (serial.equals(connectedSerial)) {
                    asyncStart(serial, dnsServers, routes, port, allTraffic);
                }
            }, getAdbPath());
            adbMonitor.monitor();
        }).start();
    }

    private static void cmdStop(String serial) throws InterruptedException, IOException, CommandExecutionException {
        synchronized (getTunnelLock(serial)) {
            String stopKey = getStartKey(serial);
            // Latch explicit stop until a later explicit start. This also prevents
            // a repair queued just before STOP from recreating the mapping afterward.
            STOP_REQUESTED_SERIALS.add(stopKey);
            cmdStopLocked(serial);
        }
    }

    private static void cmdStopLocked(String serial) throws InterruptedException, IOException, CommandExecutionException {
        Log.i(TAG, "Stopping client transactionally...");
        Exception failure = null;
        boolean stopped = false;
        try {
            execAdb(serial, "shell", "am", "start", "-a", "com.genymobile.gnirehtet.STOP", "-n",
                    "com.genymobile.gnirehtet/.GnirehtetActivity");
            stopped = waitUntilAndroidVpnClosed(serial, STOP_VERIFICATION_TIMEOUT_MS);
            if (!stopped) {
                failure = new IOException("Android did not confirm a stopped service and closed VPN descriptor within "
                        + STOP_VERIFICATION_TIMEOUT_MS + "ms");
            }
        } catch (InterruptedException | IOException | CommandExecutionException e) {
            failure = e;
        }

        boolean mappingRemoved = false;
        try {
            removeTunnel(serial);
            mappingRemoved = true;
        } catch (InterruptedException | IOException | CommandExecutionException e) {
            failure = mergeFailures(failure, e);
        }

        Diagnostics.set("android.vpn_open", stopped ? 0 : -1);
        Diagnostics.set("adb.reverse_mapping_present", mappingRemoved ? 0 : -1);
        if (failure != null) {
            rethrowStopFailure(failure);
        }
        Log.i(TAG, "Client stopped, VPN closure verified, and reverse tunnel removed");
    }

    private static Exception mergeFailures(Exception primary, Exception additional) {
        if (primary == null) {
            return additional;
        }
        primary.addSuppressed(additional);
        return primary;
    }

    private static boolean waitUntilAndroidVpnClosed(String serial, long timeoutMs)
            throws InterruptedException, IOException, CommandExecutionException {
        long deadline = System.currentTimeMillis() + timeoutMs;
        do {
            AndroidServiceStatus status = getAndroidServiceStatus(serial);
            Diagnostics.set("android.service_present", status.isServicePresent() ? 1 : 0);
            Diagnostics.set("android.vpn_open", Boolean.TRUE.equals(status.getVpnFdOpen()) ? 1 : 0);
            if (status.isStoppedAndVpnClosed()) {
                return true;
            }
            long remaining = deadline - System.currentTimeMillis();
            if (remaining > 0) {
                Thread.sleep(Math.min(STOP_POLL_INTERVAL_MS, remaining));
            }
        } while (System.currentTimeMillis() < deadline);
        return false;
    }

    private static void rethrowStopFailure(Exception failure)
            throws InterruptedException, IOException, CommandExecutionException {
        if (failure instanceof InterruptedException) {
            Thread.currentThread().interrupt();
            throw (InterruptedException) failure;
        }
        if (failure instanceof CommandExecutionException) {
            throw (CommandExecutionException) failure;
        }
        if (failure instanceof IOException) {
            throw (IOException) failure;
        }
        throw new IOException("Cannot stop client transactionally", failure);
    }

    private static void cmdRestart(String serial, String dnsServers, String routes, int port, boolean allTraffic)
            throws InterruptedException, IOException, CommandExecutionException {
        cmdStop(serial);
        STOP_REQUESTED_SERIALS.remove(getStartKey(serial));
        cmdStartWithRetries(serial, dnsServers, routes, port, allTraffic);
    }

    private static void cmdTunnel(String serial, int port) throws InterruptedException, IOException, CommandExecutionException {
        synchronized (getTunnelLock(serial)) {
            if (isStopInProgress(serial)) {
                throw new IOException("Cannot create an adb reverse mapping while stop is in progress");
            }
            execAdb(serial, "reverse", "localabstract:gnirehtet", "tcp:" + port);
            Diagnostics.set("adb.reverse_mapping_present", 1);
        }
    }

    private static void removeTunnel(String serial) throws InterruptedException, IOException, CommandExecutionException {
        synchronized (getTunnelLock(serial)) {
            if (hasProductMappingInOutput(getReverseMappings(serial))) {
                execAdb(serial, "reverse", "--remove", "localabstract:gnirehtet");
            }
            if (hasProductMappingInOutput(getReverseMappings(serial))) {
                throw new IOException("adb reverse mapping still exists after removal");
            }
            Diagnostics.set("adb.reverse_mapping_present", 0);
        }
    }

    private static Object getTunnelLock(String serial) {
        String key = getStartKey(serial);
        Object created = new Object();
        Object existing = TUNNEL_LOCKS.putIfAbsent(key, created);
        return existing != null ? existing : created;
    }

    private static void cmdRelay(int port) throws IOException {
        Log.i(TAG, "Starting relay server on port " + port + "...");
        Diagnostics.startPeriodicSnapshots();
        new Relay(port).run();
    }

    private static void cmdStatus(String serial, int port)
            throws InterruptedException, IOException, CommandExecutionException {
        AndroidServiceStatus android = getAndroidServiceStatus(serial);
        boolean tunnelPresent = hasTunnelMapping(serial, port);
        boolean relayListening = isRelayListening(port);
        Diagnostics.set("android.service_present", android.isServicePresent() ? 1 : 0);
        Diagnostics.set("android.vpn_open", Boolean.TRUE.equals(android.getVpnFdOpen()) ? 1 : 0);
        Diagnostics.set("adb.reverse_mapping_present", tunnelPresent ? 1 : 0);
        System.out.println("android.state=" + android.getLifecycleState());
        System.out.println("android.vpn_fd_open=" + formatNullableBoolean(android.getVpnFdOpen()));
        System.out.println("adb.reverse_mapping=" + (tunnelPresent ? "healthy" : "missing"));
        System.out.println("relay.listener=" + (relayListening ? "listening" : "unavailable"));
        System.out.println("diagnostics.directory=" + Diagnostics.getDirectory().toAbsolutePath());
        System.out.println("relay.metrics=" + Diagnostics.latestSnapshotJson());
    }

    private static void cmdDoctor(String serial, int port) {
        boolean mappingPresent = false;
        boolean relayListening = isRelayListening(port);
        try {
            AndroidServiceStatus android = getAndroidServiceStatus(serial);
            mappingPresent = hasTunnelMapping(serial, port);
            System.out.println("android=" + (android.isStoppedAndVpnClosed() ? "stopped" : android.getLifecycleState()));
            System.out.println("android.vpn_fd_open=" + formatNullableBoolean(android.getVpnFdOpen()));
            System.out.println("adb.reverse_mapping=" + (mappingPresent ? "healthy" : "missing"));
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            System.out.println("android=check_interrupted");
            System.out.println("adb.reverse_mapping=unavailable");
        } catch (IOException | CommandExecutionException e) {
            System.out.println("android=unavailable (" + e.getMessage() + ")");
            System.out.println("adb.reverse_mapping=unavailable");
        }
        System.out.println("relay.listener=" + (relayListening ? "listening" : "unavailable"));

        VirtualDesktopDoctor.Result virtualDesktop = VirtualDesktopDoctor.inspect();
        System.out.println("virtual_desktop.streamer=" + formatStreamerState(virtualDesktop.getStreamerState()));
        System.out.println("virtual_desktop.service=" + virtualDesktop.getServiceState());
        System.out.println("virtual_desktop.detail=" + virtualDesktop.getDetail());
        if (!mappingPresent || !relayListening) {
            System.out.println("diagnosis=tunnel_unavailable");
        } else if (virtualDesktop.getStreamerState() == VirtualDesktopDoctor.StreamerState.ABSENT) {
            System.out.println("diagnosis=virtual_desktop_streamer_not_running");
        } else if (virtualDesktop.getStreamerState() == VirtualDesktopDoctor.StreamerState.RUNNING_NOT_LISTENING) {
            System.out.println("diagnosis=virtual_desktop_running_but_not_listening");
        } else if (virtualDesktop.getStreamerState() == VirtualDesktopDoctor.StreamerState.CHECK_FAILED) {
            System.out.println("diagnosis=virtual_desktop_check_failed");
        } else {
            System.out.println("diagnosis=no_known_host_side_fault");
        }
    }

    private static String formatStreamerState(VirtualDesktopDoctor.StreamerState state) {
        switch (state) {
            case ABSENT:
                return "absent";
            case RUNNING_NOT_LISTENING:
                return "running_not_listening";
            case RUNNING_LISTENING:
                return "running_listening";
            case UNSUPPORTED:
                return "unsupported";
            case CHECK_FAILED:
                return "check_failed";
            default:
                throw new AssertionError("Unknown Virtual Desktop state: " + state);
        }
    }

    private static String formatNullableBoolean(Boolean value) {
        return value != null ? value.toString() : "unknown";
    }

    private static AndroidServiceStatus getAndroidServiceStatus(String serial)
            throws InterruptedException, IOException, CommandExecutionException {
        List<String> command = createAdbCommand(serial, "shell", "dumpsys", "activity", "service",
                "com.genymobile.gnirehtet/.GnirehtetService");
        ProcessRunner.Result result = ProcessRunner.runCaptured(command, ADB_QUERY_TIMEOUT_MS);
        requireSuccess(command, result);
        return AndroidServiceStatus.parse(result.getOutput());
    }

    private static boolean hasTunnelMapping(String serial, int port)
            throws InterruptedException, IOException, CommandExecutionException {
        return hasTunnelMappingInOutput(getReverseMappings(serial), port);
    }

    private static boolean isRelayListening(int port) {
        try (Socket socket = new Socket()) {
            socket.connect(new InetSocketAddress(InetAddress.getLoopbackAddress(), port), RELAY_PROBE_TIMEOUT_MS);
            return true;
        } catch (IOException e) {
            return false;
        }
    }

    static boolean hasTunnelMappingInOutput(String output, int port) {
        for (String line : output.split("\\r?\\n")) {
            String[] fields = line.trim().split("\\s+");
            if (fields.length >= 2 && "localabstract:gnirehtet".equals(fields[fields.length - 2])
                    && ("tcp:" + port).equals(fields[fields.length - 1])) {
                return true;
            }
        }
        return false;
    }

    static boolean hasProductMappingInOutput(String output) {
        for (String line : output.split("\\r?\\n")) {
            String[] fields = line.trim().split("\\s+");
            if (fields.length >= 2 && "localabstract:gnirehtet".equals(fields[fields.length - 2])) {
                return true;
            }
        }
        return false;
    }

    private static String getReverseMappings(String serial)
            throws InterruptedException, IOException, CommandExecutionException {
        List<String> command = createAdbCommand(serial, "reverse", "--list");
        ProcessRunner.Result result = ProcessRunner.runCaptured(command, ADB_QUERY_TIMEOUT_MS);
        requireSuccess(command, result);
        return result.getOutput();
    }

    private static void asyncStart(String serial, String dnsServers, String routes, int port, boolean allTraffic) {
        if (isStopInProgress(serial)) {
            Log.i(TAG, "Stop in progress for " + getStartTarget(serial) + ", skipping reconnect request");
            return;
        }
        ensureTunnelHealthMonitor(serial, port);
        if (!markStartPending(serial)) {
            Log.i(TAG, "Start already in progress for " + getStartTarget(serial) + ", skipping duplicate request");
            return;
        }
        new Thread(() -> {
            try {
                startMonitoredClientIndefinitely(serial, dnsServers, routes, port, allTraffic);
            } finally {
                clearStartPending(serial);
            }
        }).start();
    }

    private static void startMonitoredClientIndefinitely(String serial, String dnsServers, String routes, int port,
            boolean allTraffic) {
        while (!Thread.currentThread().isInterrupted()) {
            try {
                Diagnostics.increment("adb.reconnect_generation");
                cmdStartWithRetries(serial, dnsServers, routes, port, allTraffic);
                return;
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
            } catch (IOException | CommandExecutionException e) {
                Diagnostics.increment("adb.reconnect_failures");
                Log.w(TAG, "Cannot start monitored client; retrying indefinitely", e);
                try {
                    Thread.sleep(RETRY_DELAY_AFTER_START_SEQUENCE_MS);
                } catch (InterruptedException interrupted) {
                    Thread.currentThread().interrupt();
                }
            }
        }
    }

    private static void ensureTunnelHealthMonitor(String serial, int port) {
        if (serial == null) {
            return;
        }
        String monitorKey = serial + ':' + port;
        if (!MONITORED_TUNNELS.add(monitorKey)) {
            return;
        }
        Thread monitor = new Thread(() -> monitorTunnelHealth(serial, port), "gnirehtet-tunnel-health-" + serial);
        monitor.setDaemon(true);
        monitor.start();
    }

    private static void monitorTunnelHealth(String serial, int port) {
        while (!Thread.currentThread().isInterrupted()) {
            try {
                if (!hasTunnelMapping(serial, port)) {
                    Diagnostics.set("adb.reverse_mapping_present", 0);
                    AndroidServiceStatus android = getAndroidServiceStatus(serial);
                    recordAndroidMetrics(android);
                    if (shouldRepairTunnel(isStopInProgress(serial), android)) {
                        Diagnostics.increment("adb.reverse_mapping_repairs");
                        Log.w(TAG, "Reverse tunnel mapping is missing for " + serial + ", repairing it");
                        cmdTunnel(serial, port);
                    }
                } else {
                    Diagnostics.set("adb.reverse_mapping_present", 1);
                }
                Thread.sleep(TUNNEL_HEALTH_INTERVAL_MS);
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
            } catch (IOException | CommandExecutionException e) {
                Diagnostics.set("adb.reverse_mapping_present", 0);
                Diagnostics.increment("adb.reverse_mapping_check_failures");
                Log.w(TAG, "Cannot verify reverse tunnel for " + serial + "; retrying indefinitely", e);
                try {
                    Thread.sleep(TUNNEL_HEALTH_INTERVAL_MS);
                } catch (InterruptedException interrupted) {
                    Thread.currentThread().interrupt();
                }
            }
        }
    }

    static boolean shouldRepairTunnel(boolean stopInProgress, AndroidServiceStatus android) {
        return !stopInProgress && !android.isStoppedAndVpnClosed();
    }

    private static boolean isStopInProgress(String serial) {
        return STOP_REQUESTED_SERIALS.contains(getStartKey(serial));
    }

    private static void recordAndroidMetrics(AndroidServiceStatus android) {
        Diagnostics.set("android.service_present", android.isServicePresent() ? 1 : 0);
        Boolean vpnOpen = android.getVpnFdOpen();
        Diagnostics.set("android.vpn_open", vpnOpen != null ? (vpnOpen ? 1 : 0) : -1);
    }

    private static boolean markStartPending(String serial) {
        return STARTING_SERIALS.add(getStartKey(serial));
    }

    private static void clearStartPending(String serial) {
        STARTING_SERIALS.remove(getStartKey(serial));
    }

    private static String getStartKey(String serial) {
        return serial != null ? serial : DEFAULT_START_KEY;
    }

    private static String getStartTarget(String serial) {
        return serial != null ? serial : "the default adb device";
    }

    private static void execAdb(String serial, String... adbArgs) throws InterruptedException, IOException, CommandExecutionException {
        execSync(createAdbCommand(serial, adbArgs));
    }

    private static List<String> createAdbCommand(String serial, String... adbArgs) {
        List<String> command = new ArrayList<>();
        command.add(getAdbPath());
        if (serial != null) {
            command.add("-s");
            command.add(serial);
        }
        Collections.addAll(command, adbArgs);
        return command;
    }

    private static void execAdb(String serial, List<String> adbArgList) throws InterruptedException, IOException, CommandExecutionException {
        String[] adbArgs = adbArgList.toArray(new String[adbArgList.size()]);
        execAdb(serial, adbArgs);
    }

    private static void execSync(List<String> command) throws InterruptedException, IOException, CommandExecutionException {
        Log.d(TAG, "Execute: " + command);
        ProcessRunner.Result result = ProcessRunner.runInherited(command, ADB_COMMAND_TIMEOUT_MS);
        requireSuccess(command, result);
    }

    private static void requireSuccess(List<String> command, ProcessRunner.Result result) throws CommandExecutionException {
        if (result.getExitCode() != 0) {
            throw new CommandExecutionException(command, result.getExitCode());
        }
    }

    private static boolean mustInstallClient(String serial) throws InterruptedException, IOException, CommandExecutionException {
        Log.i(TAG, "Checking gnirehtet client...");
        List<String> command = createAdbCommand(serial, "shell", "dumpsys", "package", "com.genymobile.gnirehtet");
        Log.d(TAG, "Execute: " + command);
        ProcessRunner.Result result = ProcessRunner.runCaptured(command, ADB_QUERY_TIMEOUT_MS);
        requireSuccess(command, result);
        Scanner scanner = new Scanner(result.getOutput());
        try {
            // read the versionCode of the installed package
            Pattern pattern = Pattern.compile("^    versionCode=(\\p{Digit}+).*");
            while (scanner.hasNextLine()) {
                Matcher matcher = pattern.matcher(scanner.nextLine());
                if (matcher.matches()) {
                    String installedVersionCode = matcher.group(1);
                    scanner.close();
                    return !REQUIRED_APK_VERSION_CODE.equals(installedVersionCode);
                }
            }
        } finally {
            scanner.close();
        }
        return true;
    }


    private static void printUsage() {
        StringBuilder builder = new StringBuilder("Syntax: gnirehtet (");
        Command[] commands = Command.values();
        for (int i = 0; i < commands.length; ++i) {
            if (i != 0) {
                builder.append('|');
            }
            builder.append(commands[i].command);
        }
        builder.append(") ...").append(NL);
        builder.append("  gnirehtet diagnostics export PATH").append(NL);

        for (Command command : commands) {
            builder.append(NL);
            appendCommandUsage(builder, command);
        }

        System.err.print(builder.toString());
    }

    private static void appendCommandUsage(StringBuilder builder, Command command) {
        builder.append("  gnirehtet ").append(command.command);
        if ((command.acceptedParameters & CommandLineArguments.PARAM_SERIAL) != 0) {
            builder.append(" [serial]");
        }
        if ((command.acceptedParameters & CommandLineArguments.PARAM_DNS_SERVER) != 0) {
            builder.append(" [-d DNS[,DNS2,...]]");
        }
        if ((command.acceptedParameters & CommandLineArguments.PARAM_PORT) != 0) {
            builder.append(" [-p PORT]");
        }
        if ((command.acceptedParameters & CommandLineArguments.PARAM_ROUTES) != 0) {
            builder.append(" [-r ROUTE[,ROUTE2,...]]");
        }
        if ((command.acceptedParameters & CommandLineArguments.PARAM_ALL_TRAFFIC) != 0) {
            builder.append(" [--all-traffic]");
        }
        builder.append(NL);
        String[] descLines = command.getDescription().split("\n");
        for (String descLine : descLines) {
            builder.append("      ").append(descLine).append(NL);
        }
    }

    private static void printCommandUsage(Command command) {
        StringBuilder builder = new StringBuilder();
        appendCommandUsage(builder, command);
        System.err.print(builder.toString());
    }

    public static void main(String... args) throws Exception {
        if (args.length == 0) {
            printUsage();
            return;
        }

        String cmd = args[0];
        if ("diagnostics".equals(cmd)) {
            handleDiagnosticsCommand(args);
            return;
        }
        for (Command command : Command.values()) {
            if (cmd.equals(command.command)) {
                // forget args[0] containing the command name
                String[] commandArgs = Arrays.copyOfRange(args, 1, args.length);

                CommandLineArguments arguments;
                try {
                    arguments = CommandLineArguments.parse(command.acceptedParameters, commandArgs);
                } catch (IllegalArgumentException e) {
                    Log.e(TAG, e.getMessage());
                    printCommandUsage(command);
                    return;
                }

                command.execute(arguments);
                return;
            }
        }

        if ("rt".equals(cmd)) {
            Log.e(TAG, "The 'rt' command has been renamed to 'run'. Try 'gnirehtet run' instead.");
            printCommandUsage(Command.RUN);
        } else {
            Log.e(TAG, "Unknown command: " + cmd);
            printUsage();
        }
    }

    @SuppressWarnings("checkstyle:MagicNumber")
    private static void handleDiagnosticsCommand(String... args) throws IOException {
        if (args.length != 3 || !"export".equals(args[1])) {
            throw new IllegalArgumentException("Syntax: gnirehtet diagnostics export PATH");
        }
        Path target = Paths.get(args[2]);
        Diagnostics.export(target);
        System.out.println("Diagnostics exported to " + target.toAbsolutePath());
    }
}
