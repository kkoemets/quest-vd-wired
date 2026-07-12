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

import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.util.List;
import java.util.concurrent.TimeUnit;

/** Runs external commands with a hard deadline. */
public final class ProcessRunner {

    private static final int MAX_CAPTURE_BYTES = 1024 * 1024;
    private static final int COPY_BUFFER_BYTES = 8192;
    private static final long FORCE_KILL_GRACE_MS = 1000;

    public static final class Result {
        private final int exitCode;
        private final String output;

        Result(int exitCode, String output) {
            this.exitCode = exitCode;
            this.output = output;
        }

        public int getExitCode() {
            return exitCode;
        }

        public String getOutput() {
            return output;
        }
    }

    private ProcessRunner() {
        // not instantiable
    }

    public static Result runInherited(List<String> command, long timeoutMs) throws IOException, InterruptedException {
        ProcessBuilder builder = new ProcessBuilder(command);
        builder.redirectOutput(ProcessBuilder.Redirect.INHERIT);
        builder.redirectError(ProcessBuilder.Redirect.INHERIT);
        Process process = builder.start();
        return waitFor(command, process, timeoutMs, null);
    }

    public static Result runCaptured(List<String> command, long timeoutMs) throws IOException, InterruptedException {
        ProcessBuilder builder = new ProcessBuilder(command);
        builder.redirectErrorStream(true);
        Process process = builder.start();
        BoundedCapture capture = new BoundedCapture(process.getInputStream());
        Thread pump = new Thread(capture, "gnirehtet-process-output");
        pump.setDaemon(true);
        pump.start();
        return waitFor(command, process, timeoutMs, new CaptureThread(capture, pump));
    }

    private static Result waitFor(List<String> command, Process process, long timeoutMs, CaptureThread capture)
            throws IOException, InterruptedException {
        boolean finished;
        try {
            finished = process.waitFor(timeoutMs, TimeUnit.MILLISECONDS);
        } catch (InterruptedException e) {
            terminate(process);
            throw e;
        }
        if (!finished) {
            terminate(process);
            finishCapture(capture);
            throw new IOException("Timed out after " + timeoutMs + "ms executing " + command);
        }
        finishCapture(capture);
        String output = capture != null ? capture.capture.getOutput() : "";
        return new Result(process.exitValue(), output);
    }

    private static void terminate(Process process) throws InterruptedException {
        process.destroy();
        if (!process.waitFor(FORCE_KILL_GRACE_MS, TimeUnit.MILLISECONDS)) {
            process.destroyForcibly();
            process.waitFor(FORCE_KILL_GRACE_MS, TimeUnit.MILLISECONDS);
        }
    }

    private static void finishCapture(CaptureThread capture) throws IOException, InterruptedException {
        if (capture == null) {
            return;
        }
        capture.thread.join(FORCE_KILL_GRACE_MS);
        if (capture.capture.failure != null) {
            throw capture.capture.failure;
        }
    }

    private static final class CaptureThread {
        private final BoundedCapture capture;
        private final Thread thread;

        CaptureThread(BoundedCapture capture, Thread thread) {
            this.capture = capture;
            this.thread = thread;
        }
    }

    private static final class BoundedCapture implements Runnable {
        private final InputStream input;
        private final ByteArrayOutputStream output = new ByteArrayOutputStream();
        private IOException failure;

        BoundedCapture(InputStream input) {
            this.input = input;
        }

        @Override
        public void run() {
            byte[] buffer = new byte[COPY_BUFFER_BYTES];
            try {
                int count;
                while ((count = input.read(buffer)) != -1) {
                    int remaining = MAX_CAPTURE_BYTES - output.size();
                    if (remaining > 0) {
                        output.write(buffer, 0, Math.min(count, remaining));
                    }
                }
            } catch (IOException e) {
                failure = e;
            } finally {
                try {
                    input.close();
                } catch (IOException e) {
                    if (failure == null) {
                        failure = e;
                    }
                }
            }
        }

        String getOutput() {
            return new String(output.toByteArray(), StandardCharsets.UTF_8);
        }
    }
}
