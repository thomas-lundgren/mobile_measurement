package com.measurement.logger;

import org.json.JSONArray;
import org.json.JSONObject;

import java.io.File;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.security.MessageDigest;
import java.util.ArrayList;
import java.util.List;
import java.util.function.Consumer;

/**
 * Persistent upload queue, on its own thread.
 *
 * Each finished test leaves an upload_pending.json marker (test id + sealed
 * file paths) in its test directory. The uploader works through the markers
 * oldest-first whenever a session is available, retrying with backoff through
 * any outage; the marker is renamed to upload_done.json once the server
 * confirms. The server side is resumable per file (re-posting the summary
 * returns only the files it still needs), so a retry after a partial upload
 * only sends what is missing.
 *
 * While a test runs ({@link #setMeasuring}) it stays off the network, so an
 * upload never shares the link with a measurement; it resumes after the seal.
 */
final class Uploader {
    private static final String UPLOAD_PENDING = "upload_pending.json";
    private static final String UPLOAD_DONE = "upload_done.json";
    private static final String UPLOAD_REJECTED = "upload_rejected.json";
    private static final long UPLOAD_BACKOFF_MIN_MS = 5_000;
    private static final long UPLOAD_BACKOFF_MAX_MS = 300_000;

    private final File filesDir;
    private final String server;
    /** Where problems are reported (the service's state line). */
    private final Consumer<String> report;
    private final Object signal = new Object();
    private volatile boolean running;
    /** Session of the connected worker; null while reconnecting (we wait). */
    private volatile String session;
    private volatile boolean measuring;
    private Thread thread;

    Uploader(File filesDir, String server, Consumer<String> report) {
        this.filesDir = filesDir;
        this.server = server;
        this.report = report;
    }

    /** Start the thread, or restart it if it has died. */
    synchronized void start() {
        running = true;
        if (thread == null || !thread.isAlive()) {
            thread = new Thread(this::loop, "logger-uploader");
            thread.start();
        }
    }

    synchronized void stop() {
        running = false;
        if (thread != null) {
            thread.interrupt();
        }
    }

    void setSession(String session) {
        this.session = session;
        wake();
    }

    /** True from a test's start to its seal; false wakes the queue. */
    void setMeasuring(boolean measuring) {
        this.measuring = measuring;
        if (!measuring) {
            wake();
        }
    }

    /** Durably queue a sealed test for upload. */
    void queue(File testRoot, String testId, JSONArray sealed) {
        try {
            JSONArray rel = new JSONArray();
            for (int i = 0; i < sealed.length(); i++) {
                rel.put(relPath(testRoot, new File(sealed.getString(i))));
            }
            JSONObject marker = new JSONObject().put("test_id", testId).put("files", rel);
            File tmp = new File(testRoot, UPLOAD_PENDING + ".tmp");
            Files.write(tmp.toPath(), marker.toString().getBytes(StandardCharsets.UTF_8));
            // atomic publish: the uploader never sees a half-written marker
            Files.move(tmp.toPath(), new File(testRoot, UPLOAD_PENDING).toPath(),
                    java.nio.file.StandardCopyOption.ATOMIC_MOVE,
                    java.nio.file.StandardCopyOption.REPLACE_EXISTING);
        } catch (Exception e) {
            // Logging-first: the data is on disk regardless; only the automatic
            // upload is lost.
            report.accept("ERROR: could not queue upload for " + testId + ": " + e.getMessage());
        }
        wake();
    }

    /** Tests waiting for upload. */
    int pending() {
        return pendingMarkers().size();
    }

    private void wake() {
        synchronized (signal) {
            signal.notifyAll();
        }
    }

    /** Pending markers, oldest first. Layout: files/<session>/<test>/marker. */
    private List<File> pendingMarkers() {
        List<File> out = new ArrayList<>();
        File[] sessions = filesDir.listFiles(File::isDirectory);
        if (sessions == null) {
            return out;
        }
        for (File s : sessions) {
            File[] tests = s.listFiles(File::isDirectory);
            if (tests == null) {
                continue;
            }
            for (File t : tests) {
                File m = new File(t, UPLOAD_PENDING);
                if (m.isFile()) {
                    out.add(m);
                }
            }
        }
        out.sort((a, b) -> Long.compare(a.lastModified(), b.lastModified()));
        return out;
    }

    private void loop() {
        ServerClient client = new ServerClient(server);
        long backoff = UPLOAD_BACKOFF_MIN_MS;
        while (running) {
            try {
                List<File> pending = pendingMarkers();
                String s = session;
                if (pending.isEmpty() || s == null || measuring) {
                    synchronized (signal) {
                        signal.wait(pending.isEmpty() ? 60_000 : 2_000);
                    }
                    continue;
                }
                for (File marker : pending) {
                    if (!running) {
                        return;
                    }
                    if (measuring) {
                        throw new Paused();
                    }
                    uploadOne(client, s, marker);
                }
                backoff = UPLOAD_BACKOFF_MIN_MS;
            } catch (InterruptedException e) {
                return;
            } catch (Paused e) {
                // a test started: wait for it (its seal wakes us), no backoff
            } catch (Throwable e) {
                // Any failure (no coverage, timeout, expired session, server
                // down): keep the marker and retry later.
                try {
                    Thread.sleep(backoff);
                } catch (InterruptedException ie) {
                    return;
                }
                backoff = Math.min(backoff * 2, UPLOAD_BACKOFF_MAX_MS);
            }
        }
    }

    private void uploadOne(ServerClient client, String session, File marker) throws Exception {
        File testRoot = marker.getParentFile();
        JSONObject m = new JSONObject(new String(Files.readAllBytes(marker.toPath()), StandardCharsets.UTF_8));
        String testId = m.getString("test_id");
        try {
            upload(client, session, testId, testRoot, m.getJSONArray("files"));
        } catch (java.nio.file.NoSuchFileException e) {
            reject(marker, testId, "file missing: " + e.getMessage());
            return;
        } catch (java.io.IOException e) {
            // The server doesn't know this test (or it isn't ours) — retrying
            // can never succeed. Keep the data, stop retrying.
            String msg = String.valueOf(e.getMessage());
            if (msg.startsWith("HTTP 404") || msg.startsWith("HTTP 403")) {
                reject(marker, testId, msg);
                return;
            }
            throw e;
        }
        marker.renameTo(new File(testRoot, UPLOAD_DONE));
    }

    private void reject(File marker, String testId, String why) {
        marker.renameTo(new File(marker.getParentFile(), UPLOAD_REJECTED));
        report.accept("ERROR: upload of " + testId + " rejected (" + why + "); data kept on device");
    }

    private void upload(ServerClient client, String session, String testId, File testRoot, JSONArray relPaths)
            throws Exception {
        JSONArray files = new JSONArray();
        for (int i = 0; i < relPaths.length(); i++) {
            String rel = relPaths.getString(i);
            byte[] bytes = Files.readAllBytes(new File(testRoot, rel).toPath());
            files.put(new JSONObject()
                    .put("path", rel)
                    .put("size_bytes", bytes.length)
                    .put("sha256", sha256Hex(bytes))
                    .put("encoding", "identity"));
        }
        JSONObject manifest = new JSONObject()
                .put("test_result", "COMPLETED")
                .put("node", "mobile")
                .put("files", files);
        JSONObject init = client.uploadSummary(session, testId, manifest);
        String uploadId = init.getString("upload_id");
        JSONArray needed = init.getJSONArray("needed_paths");
        for (int i = 0; i < needed.length(); i++) {
            if (measuring) {
                throw new Paused();
            }
            String rel = needed.getString(i);
            byte[] bytes = Files.readAllBytes(new File(testRoot, rel).toPath());
            client.putFile(session, testId, uploadId, rel, bytes);
        }
        client.uploadComplete(session, testId, uploadId);
    }

    /** A test started mid-upload; the rest of the files go after it. */
    private static final class Paused extends Exception {
    }

    private static String relPath(File root, File f) {
        String r = root.getAbsolutePath();
        String p = f.getAbsolutePath();
        return p.startsWith(r + "/") ? p.substring(r.length() + 1) : f.getName();
    }

    private static String sha256Hex(byte[] bytes) throws Exception {
        byte[] d = MessageDigest.getInstance("SHA-256").digest(bytes);
        StringBuilder sb = new StringBuilder(d.length * 2);
        for (byte b : d) {
            sb.append(String.format("%02x", b));
        }
        return sb.toString();
    }
}
