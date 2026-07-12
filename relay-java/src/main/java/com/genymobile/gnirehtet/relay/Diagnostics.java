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

import com.genymobile.gnirehtet.ProcessRunner;

import java.io.BufferedWriter;
import java.io.IOException;
import java.io.OutputStream;
import java.io.OutputStreamWriter;
import java.io.RandomAccessFile;
import java.lang.management.ManagementFactory;
import java.nio.charset.StandardCharsets;
import java.nio.file.DirectoryStream;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.nio.file.StandardCopyOption;
import java.nio.file.StandardOpenOption;
import java.util.Arrays;
import java.util.Locale;
import java.util.Map;
import java.util.TreeMap;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.concurrent.atomic.AtomicLong;
import java.util.zip.ZipEntry;
import java.util.zip.ZipOutputStream;

/** Bounded, payload-free relay metrics and support-bundle export. */
@SuppressWarnings("checkstyle:MagicNumber")
public final class Diagnostics {

    private static final String TAG = Diagnostics.class.getSimpleName();
    private static final long SNAPSHOT_INTERVAL_MS = 5000;
    private static final long MAX_LOG_BYTES = 10L * 1024 * 1024;
    private static final int LOG_FILE_COUNT = 5;
    private static final long RSS_SAMPLE_INTERVAL_MS = 60000;
    private static final long RSS_COMMAND_TIMEOUT_MS = 5000;
    private static final String LOG_PREFIX = "relay-metrics-";
    private static final ConcurrentHashMap<String, AtomicLong> METRICS = new ConcurrentHashMap<>();
    private static final AtomicBoolean STARTED = new AtomicBoolean();
    private static final Object FILE_LOCK = new Object();
    private static long lastThroughputTimestamp;
    private static long lastClientToNetworkBytes;
    private static long lastNetworkToClientBytes;
    private static long lastRssSampleTimestamp;

    private Diagnostics() {
        // not instantiable
    }

    public static void increment(String name) {
        add(name, 1);
    }

    public static void add(String name, long delta) {
        metric(name).addAndGet(delta);
    }

    public static void set(String name, long value) {
        metric(name).set(value);
    }

    public static void recordMaximum(String name, long value) {
        AtomicLong metric = metric(name);
        long current = metric.get();
        while (value > current && !metric.compareAndSet(current, value)) {
            current = metric.get();
        }
    }

    private static AtomicLong metric(String name) {
        AtomicLong existing = METRICS.get(name);
        if (existing != null) {
            return existing;
        }
        AtomicLong created = new AtomicLong();
        AtomicLong raced = METRICS.putIfAbsent(name, created);
        return raced != null ? raced : created;
    }

    public static void startPeriodicSnapshots() {
        if (!STARTED.compareAndSet(false, true)) {
            return;
        }
        Thread thread = new Thread(() -> {
            while (!Thread.currentThread().isInterrupted()) {
                try {
                    writeSnapshot();
                    Thread.sleep(SNAPSHOT_INTERVAL_MS);
                } catch (InterruptedException e) {
                    Thread.currentThread().interrupt();
                } catch (IOException | RuntimeException e) {
                    Log.w(TAG, "Cannot write diagnostics snapshot", e);
                }
            }
        }, "gnirehtet-diagnostics");
        thread.setDaemon(true);
        thread.start();
    }

    public static String snapshotJson() {
        TreeMap<String, Long> snapshot = new TreeMap<>();
        for (Map.Entry<String, AtomicLong> entry : METRICS.entrySet()) {
            snapshot.put(entry.getKey(), entry.getValue().get());
        }
        Runtime runtime = Runtime.getRuntime();
        snapshot.put("process.heap_used_bytes", runtime.totalMemory() - runtime.freeMemory());
        snapshot.put("process.heap_committed_bytes", runtime.totalMemory());
        addOperatingSystemMetrics(snapshot);

        StringBuilder builder = new StringBuilder("{\"timestamp_ms\":");
        builder.append(System.currentTimeMillis()).append(",\"metrics\":{");
        boolean first = true;
        for (Map.Entry<String, Long> entry : snapshot.entrySet()) {
            if (!first) {
                builder.append(',');
            }
            first = false;
            builder.append('\"').append(entry.getKey()).append("\":").append(entry.getValue());
        }
        return builder.append("}}").toString();
    }

    private static void addOperatingSystemMetrics(Map<String, Long> snapshot) {
        java.lang.management.OperatingSystemMXBean bean = ManagementFactory.getOperatingSystemMXBean();
        if (bean instanceof com.sun.management.OperatingSystemMXBean) {
            com.sun.management.OperatingSystemMXBean os = (com.sun.management.OperatingSystemMXBean) bean;
            snapshot.put("process.cpu_time_ns", os.getProcessCpuTime());
            snapshot.put("process.cpu_load_ppm", Math.round(os.getProcessCpuLoad() * 1000000));
            snapshot.put("process.committed_virtual_bytes", os.getCommittedVirtualMemorySize());
        }
    }

    public static void writeSnapshot() throws IOException {
        writeSnapshot(getDirectory(), MAX_LOG_BYTES, LOG_FILE_COUNT);
    }

    static void writeSnapshot(Path directory, long maxLogBytes, int logFileCount) throws IOException {
        synchronized (FILE_LOCK) {
            updateThroughputMetrics();
            ensureDirectory(directory);
            Path current = logPath(directory, 0);
            if (Files.exists(current) && Files.size(current) >= maxLogBytes) {
                rotate(directory, logFileCount);
            }
            try (BufferedWriter writer = Files.newBufferedWriter(current, StandardCharsets.UTF_8,
                    StandardOpenOption.CREATE, StandardOpenOption.APPEND)) {
                writer.write(snapshotJson());
                writer.newLine();
            }
        }
    }

    private static void updateThroughputMetrics() {
        long now = System.currentTimeMillis();
        long clientToNetwork = metric("bytes.client_to_network_tcp").get()
                + metric("bytes.client_to_network_udp").get();
        long networkToClient = metric("bytes.network_to_client_tcp").get()
                + metric("bytes.network_to_client_udp").get();
        if (lastThroughputTimestamp != 0 && now > lastThroughputTimestamp) {
            long elapsedMillis = now - lastThroughputTimestamp;
            set("throughput.client_to_network_bps",
                    Math.max(0, clientToNetwork - lastClientToNetworkBytes) * 8000 / elapsedMillis);
            set("throughput.network_to_client_bps",
                    Math.max(0, networkToClient - lastNetworkToClientBytes) * 8000 / elapsedMillis);
        }
        lastThroughputTimestamp = now;
        lastClientToNetworkBytes = clientToNetwork;
        lastNetworkToClientBytes = networkToClient;
        updateResidentSetMetric(now);
    }

    private static void updateResidentSetMetric(long now) {
        if (now - lastRssSampleTimestamp < RSS_SAMPLE_INTERVAL_MS
                || !System.getProperty("os.name", "").toLowerCase(Locale.ENGLISH).contains("windows")) {
            return;
        }
        lastRssSampleTimestamp = now;
        String pid = Long.toString(ProcessHandle.current().pid());
        try {
            ProcessRunner.Result result = ProcessRunner.runCaptured(Arrays.asList(
                    "powershell.exe", "-NoProfile", "-NonInteractive", "-Command",
                    "$ErrorActionPreference='Stop';(Get-Process -Id " + pid + ").WorkingSet64"), RSS_COMMAND_TIMEOUT_MS);
            if (result.getExitCode() == 0) {
                set("process.rss_bytes", Long.parseLong(result.getOutput().trim()));
            } else {
                increment("process.rss_sample_failures");
            }
        } catch (IOException | NumberFormatException e) {
            increment("process.rss_sample_failures");
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
        }
    }

    private static void rotate(Path directory, int logFileCount) throws IOException {
        Files.deleteIfExists(logPath(directory, logFileCount - 1));
        for (int i = logFileCount - 1; i > 0; --i) {
            Path source = logPath(directory, i - 1);
            if (Files.exists(source)) {
                Files.move(source, logPath(directory, i), StandardCopyOption.REPLACE_EXISTING);
            }
        }
    }

    public static void export(Path target) throws IOException {
        export(target, getDirectory(), MAX_LOG_BYTES, LOG_FILE_COUNT);
    }

    static void export(Path target, Path directory, long maxLogBytes, int logFileCount) throws IOException {
        writeSnapshot(directory, maxLogBytes, logFileCount);
        Path absoluteTarget = target.toAbsolutePath();
        Path parent = absoluteTarget.getParent();
        if (parent != null) {
            ensureDirectory(parent);
        }
        synchronized (FILE_LOCK) {
            try (OutputStream output = Files.newOutputStream(absoluteTarget);
                 ZipOutputStream zip = new ZipOutputStream(output, StandardCharsets.UTF_8)) {
                addText(zip, "manifest.json", "{\"format\":1,\"packet_payloads_recorded\":false}\n");
                try (DirectoryStream<Path> files = Files.newDirectoryStream(directory, LOG_PREFIX + "*.jsonl")) {
                    for (Path file : files) {
                        if (file.toAbsolutePath().equals(absoluteTarget)) {
                            continue;
                        }
                        zip.putNextEntry(new ZipEntry(file.getFileName().toString()));
                        Files.copy(file, zip);
                        zip.closeEntry();
                    }
                }
            }
        }
    }

    private static void addText(ZipOutputStream zip, String name, String text) throws IOException {
        zip.putNextEntry(new ZipEntry(name));
        OutputStreamWriter writer = new OutputStreamWriter(zip, StandardCharsets.UTF_8);
        writer.write(text);
        writer.flush();
        zip.closeEntry();
    }

    private static void ensureDirectory(Path directory) throws IOException {
        if (!Files.isDirectory(directory)) {
            Files.createDirectories(directory);
        }
    }

    public static Path getDirectory() {
        String configured = System.getenv("GNIREHTET_DIAGNOSTICS_DIR");
        if (configured != null && !configured.trim().isEmpty()) {
            return Paths.get(configured);
        }
        return Paths.get(System.getProperty("user.home"), ".gnirehtet", "diagnostics");
    }

    public static String latestSnapshotJson() {
        return latestSnapshotJson(logPath(getDirectory(), 0));
    }

    static String latestSnapshotJson(Path current) {
        if (!Files.exists(current)) {
            return snapshotJson();
        }
        try (RandomAccessFile file = new RandomAccessFile(current.toFile(), "r")) {
            long position = file.length() - 1;
            StringBuilder reversed = new StringBuilder();
            while (position >= 0) {
                file.seek(position--);
                char value = (char) file.readUnsignedByte();
                if (value == '\n' || value == '\r') {
                    if (reversed.length() != 0) {
                        break;
                    }
                } else {
                    reversed.append(value);
                }
            }
            if (reversed.length() != 0) {
                return reversed.reverse().toString();
            }
        } catch (IOException e) {
            Log.w(TAG, "Cannot read latest diagnostics snapshot", e);
        }
        return snapshotJson();
    }

    private static Path logPath(Path directory, int index) {
        return directory.resolve(LOG_PREFIX + index + ".jsonl");
    }
}
