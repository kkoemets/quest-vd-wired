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

import java.io.IOException;
import java.nio.file.Paths;
import java.util.Arrays;

public class ProcessRunnerTest {

    private static final long COMMAND_TIMEOUT_MS = 10000;
    private static final long EXPECTED_TIMEOUT_MS = 50;

    public static final class SleepProcess {
        private SleepProcess() {
            // not instantiable
        }

        public static void main(String... args) throws InterruptedException {
            Thread.sleep(COMMAND_TIMEOUT_MS);
        }
    }

    @Test
    public void testCapture() throws Exception {
        ProcessRunner.Result result = ProcessRunner.runCaptured(
                javaCommand("-version"), COMMAND_TIMEOUT_MS);

        Assert.assertEquals(0, result.getExitCode());
        Assert.assertTrue(result.getOutput().contains("version"));
    }

    @Test
    public void testTimeout() throws Exception {
        String classPath = Paths.get(ProcessRunnerTest.class.getProtectionDomain().getCodeSource().getLocation().toURI()).toString();
        try {
            ProcessRunner.runCaptured(javaCommand("-cp", classPath, SleepProcess.class.getName()), EXPECTED_TIMEOUT_MS);
            Assert.fail("Expected a command timeout");
        } catch (IOException e) {
            Assert.assertTrue(e.getMessage().contains("Timed out"));
        }
    }

    private static java.util.List<String> javaCommand(String... arguments) {
        String executable = Paths.get(System.getProperty("java.home"), "bin", isWindows() ? "java.exe" : "java").toString();
        java.util.List<String> command = new java.util.ArrayList<>();
        command.add(executable);
        command.addAll(Arrays.asList(arguments));
        return command;
    }

    private static boolean isWindows() {
        return System.getProperty("os.name", "").toLowerCase(java.util.Locale.ENGLISH).contains("windows");
    }
}
