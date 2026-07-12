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
import org.junit.Rule;
import org.junit.Test;
import org.junit.rules.TemporaryFolder;

import java.io.IOException;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.HashSet;
import java.util.Set;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.Future;
import java.util.zip.ZipEntry;
import java.util.zip.ZipInputStream;

@SuppressWarnings("checkstyle:MagicNumber")
public class DiagnosticsTest {

    @Rule
    public final TemporaryFolder temporaryFolder = new TemporaryFolder();

    @Test
    public void testRotationKeepsExactlyConfiguredFiles() throws Exception {
        Path directory = temporaryFolder.newFolder("rotation").toPath();
        for (int i = 0; i < 8; ++i) {
            Diagnostics.writeSnapshot(directory, 1, 5);
        }
        for (int i = 0; i < 5; ++i) {
            Assert.assertTrue(Files.exists(directory.resolve("relay-metrics-" + i + ".jsonl")));
        }
        Assert.assertFalse(Files.exists(directory.resolve("relay-metrics-5.jsonl")));
    }

    @Test
    public void testLatestSnapshotReadsOnlyLastNonEmptyLine() throws Exception {
        Path log = temporaryFolder.newFile("tail.jsonl").toPath();
        String prefix = "x".repeat(1024 * 1024);
        Files.write(log, (prefix + "\n{\"last\":true}\n\n").getBytes(StandardCharsets.UTF_8));
        Assert.assertEquals("{\"last\":true}", Diagnostics.latestSnapshotJson(log));
    }

    @Test
    public void testConcurrentSnapshotsAndExportsStayValid() throws Exception {
        Path directory = temporaryFolder.newFolder("concurrent").toPath();
        Path export1 = directory.resolve("bundle-1.zip");
        Path export2 = directory.resolve("bundle-2.zip");
        ExecutorService executor = Executors.newFixedThreadPool(4);
        try {
            Future<?> writer1 = executor.submit(() -> writeSnapshots(directory));
            Future<?> writer2 = executor.submit(() -> writeSnapshots(directory));
            Future<?> exporter1 = executor.submit(() -> export(export1, directory));
            Future<?> exporter2 = executor.submit(() -> export(export2, directory));
            writer1.get();
            writer2.get();
            exporter1.get();
            exporter2.get();
        } finally {
            executor.shutdownNow();
        }
        assertBundle(export1);
        assertBundle(export2);
    }

    private static void writeSnapshots(Path directory) {
        try {
            for (int i = 0; i < 25; ++i) {
                Diagnostics.writeSnapshot(directory, 4096, 5);
            }
        } catch (IOException e) {
            throw new AssertionError(e);
        }
    }

    private static void export(Path target, Path directory) {
        try {
            Diagnostics.export(target, directory, 4096, 5);
        } catch (IOException e) {
            throw new AssertionError(e);
        }
    }

    private static void assertBundle(Path bundle) throws IOException {
        Set<String> entries = new HashSet<>();
        try (InputStream input = Files.newInputStream(bundle);
             ZipInputStream zip = new ZipInputStream(input, StandardCharsets.UTF_8)) {
            ZipEntry entry;
            while ((entry = zip.getNextEntry()) != null) {
                entries.add(entry.getName());
                Assert.assertTrue(entry.getName().equals("manifest.json")
                        || entry.getName().matches("relay-metrics-[0-4]\\.jsonl"));
            }
        }
        Assert.assertTrue(entries.contains("manifest.json"));
        Assert.assertTrue(entries.stream().anyMatch((name) -> name.endsWith(".jsonl")));
    }
}
