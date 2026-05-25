package com.measurement.logger;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.Service;
import android.content.Context;
import android.content.Intent;
import android.content.SharedPreferences;
import android.os.IBinder;

import androidx.annotation.Nullable;
import androidx.core.app.NotificationCompat;

import org.json.JSONArray;
import org.json.JSONObject;

import java.io.File;
import java.nio.file.Files;
import java.security.MessageDigest;

/**
 * Foreground service: the autonomous Logger. Onboards (enroll → challenge →
 * verify → register), polls the command queue, runs RUN_TEST via the shared
 * core (RustCore) with radio/GPS samplers feeding it, then uploads — logging
 * stays local-first; upload reads only sealed files.
 */
public final class LoggerService extends Service {
    public static final String EXTRA_SERVER = "server";
    public static final String EXTRA_TOKEN = "token";
    private static final String CHANNEL = "logger";
    private static final String PREFS = "mp_logger";

    /** Coarse state surfaced to the UI. */
    public static volatile String STATE = "idle";

    // Pixel 9 Pro baseline capability (extended fields false; out of MVP scope).
    private static final String CAPABILITY =
            "{\"radio\":{\"serving_cell\":[\"pci\",\"nci\",\"tac\",\"nrarfcn\",\"rsrp\",\"rsrq\",\"sinr\"],"
            + "\"neighbor_cells\":true,\"lte_fallback\":true,\"cqi\":false,\"mcs\":false,\"bler\":false,"
            + "\"harq\":false,\"drx\":false,\"bwp\":false,\"mimo_layers\":false,\"tx_power\":false},"
            + "\"qos\":{\"5qi\":false,\"qfi\":false,\"slice_info\":false},"
            + "\"mobility\":{\"explicit_ho_events\":false,\"rrc_state\":false,\"beam_switch\":false},"
            + "\"timestamp_sources\":[\"mono\",\"utc\",\"gps\"]}";

    private volatile boolean running;
    private Thread worker;
    private int loggingConfigVersion = 1;

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        startForeground(1, notification("starting"));
        String server = intent != null ? intent.getStringExtra(EXTRA_SERVER) : null;
        String token = intent != null ? intent.getStringExtra(EXTRA_TOKEN) : null;
        if (server == null) {
            stopSelf();
            return START_NOT_STICKY;
        }
        running = true;
        worker = new Thread(() -> run(server, token), "logger-worker");
        worker.start();
        return START_STICKY;
    }

    @Override
    public void onDestroy() {
        running = false;
        if (worker != null) {
            worker.interrupt();
        }
        super.onDestroy();
    }

    @Nullable
    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }

    private static final long BACKOFF_MIN_MS = 1000;
    private static final long BACKOFF_MAX_MS = 30_000;

    /**
     * Reconnecting worker: connect → onboard → poll, retrying with exponential
     * backoff so a transient error (server restart, coverage gap, session loss)
     * just reconnects instead of killing the worker. Enrollment is idempotent
     * (cached device_id); each reconnect re-authenticates for a fresh session.
     * Logging-first is unaffected — an in-flight test keeps logging locally.
     */
    private void run(String server, String token) {
        ServerClient client = new ServerClient(server);
        SharedPreferences prefs = getSharedPreferences(PREFS, Context.MODE_PRIVATE);
        long backoff = BACKOFF_MIN_MS;
        while (running) {
            try {
                setState("REGISTERING");
                KeystoreSigner.ensureKey();
                String deviceId = enrollIfNeeded(client, prefs, token);
                String session = authenticate(client, deviceId);
                JSONObject cfg = client.register(session, new JSONObject(CAPABILITY));
                loggingConfigVersion = cfg.optInt("version", 1);

                setState("IDLE");
                backoff = BACKOFF_MIN_MS; // connected → reset backoff
                pollLoop(client, session, deviceId);
            } catch (InterruptedException ignored) {
                return; // stopping
            } catch (Exception e) {
                if (!running) {
                    return;
                }
                setState("RECONNECTING: " + e.getMessage());
                try {
                    Thread.sleep(backoff);
                } catch (InterruptedException ie) {
                    return;
                }
                backoff = Math.min(backoff * 2, BACKOFF_MAX_MS);
            }
        }
    }

    /** Poll/status cadence. Any request exception propagates to the reconnect
     *  loop (which re-authenticates after backoff). Returns only when stopping. */
    private void pollLoop(ServerClient client, String session, String deviceId) throws Exception {
        long lastStatus = 0;
        while (running) {
            JSONObject cmd = client.pollCommand(session, deviceId);
            if (cmd != null && "RUN_TEST".equals(cmd.optString("command"))) {
                runTest(client, session, deviceId, cmd);
            } else {
                Thread.sleep(2000);
            }
            if (System.currentTimeMillis() - lastStatus > 5000) {
                postStatus(client, session, deviceId, null);
                lastStatus = System.currentTimeMillis();
            }
        }
    }

    private String enrollIfNeeded(ServerClient client, SharedPreferences prefs, String token) throws Exception {
        String existing = prefs.getString("device_id", null);
        if (existing != null) {
            return existing;
        }
        JSONObject body = new JSONObject()
                .put("device_pubkey", KeystoreSigner.publicKeySpkiB64())
                .put("pubkey_format", "SPKI_DER")
                .put("sig_alg", "ECDSA_P256_SHA256")
                .put("sig_format", "ASN1_DER")
                .put("platform", "android")
                .put("hw_info", new JSONObject()
                        .put("model", android.os.Build.MODEL)
                        .put("manufacturer", android.os.Build.MANUFACTURER)
                        .put("os_version", android.os.Build.VERSION.RELEASE))
                .put("protocol_version", 1);
        JSONObject resp = client.enroll(token == null ? "" : token, body);
        String deviceId = resp.getString("device_id");
        prefs.edit().putString("device_id", deviceId).apply();
        return deviceId;
    }

    private String authenticate(ServerClient client, String deviceId) throws Exception {
        JSONObject c = client.challenge(deviceId);
        String sig = KeystoreSigner.signChallengeB64(
                c.getString("server_id"), c.getString("challenge_id"), c.getString("nonce"));
        return client.verify(c.getString("challenge_id"), sig).getString("session_token");
    }

    /** Refuse to start below this much free space — logging-first means a full
     *  disk is the one failure that actually breaks measurement. */
    private static final long MIN_FREE_BYTES = 200L * 1024 * 1024;

    private void runTest(ServerClient client, String session, String deviceId, JSONObject cmd) throws Exception {
        JSONObject payload = cmd.getJSONObject("payload");
        String testId = payload.getString("test_id");
        String sessionId = payload.getString("session_id");

        if (freeBytes() < MIN_FREE_BYTES) {
            setState("ERROR: low storage, refusing " + testId);
            client.ackCommand(session, deviceId, cmd.getLong("command_id"));
            return;
        }

        File testRoot = new File(getFilesDir(), sessionId + "/" + testId);
        File mobileDir = new File(testRoot, "mobile");
        mobileDir.mkdirs();

        setState("PREPARING " + testId);
        String ueConfig = buildUeConfig(payload, deviceId, sessionId, mobileDir.getAbsolutePath());

        long h = RustCore.coreNew();
        if (RustCore.startTest(h, ueConfig) != 0) {
            RustCore.destroy(h);
            throw new Exception("startTest failed");
        }
        GpsSampler gps = new GpsSampler(this, h);
        RadioSampler radio = new RadioSampler(this, h);
        ClockSampler clock = new ClockSampler(client, h);
        gps.start();
        radio.start();
        clock.start();

        setState("RUNNING_TEST " + testId);
        int durationSec = payload.getJSONObject("test_plan").optInt("duration_sec", 30);
        for (int i = 0; i < durationSec && running; i++) {
            Thread.sleep(1000);
        }

        gps.stop();
        radio.stop();
        clock.stop();
        RustCore.stopTest(h);
        JSONArray sealed = new JSONArray(RustCore.sealedFiles(h));
        RustCore.destroy(h);

        setState("UPLOADING " + testId);
        upload(client, session, testId, testRoot, sealed);

        client.ackCommand(session, deviceId, cmd.getLong("command_id"));
        setState("IDLE");
    }

    /** Merge RUN_TEST payload (port-less plan + bindings) into the engine's lean
     *  UeTestConfig — the JNI-seam job. */
    private static String buildUeConfig(JSONObject payload, String deviceId, String sessionId, String mobileDir)
            throws Exception {
        JSONObject bindings = new JSONObject();
        JSONArray fb = payload.getJSONArray("flow_bindings");
        for (int i = 0; i < fb.length(); i++) {
            JSONObject b = fb.getJSONObject(i);
            bindings.put(String.valueOf(b.getInt("flow_id")), b.getInt("server_port"));
        }
        JSONArray planFlows = payload.getJSONObject("test_plan").getJSONArray("flows");
        JSONArray flows = new JSONArray();
        for (int i = 0; i < planFlows.length(); i++) {
            JSONObject f = planFlows.getJSONObject(i);
            JSONObject uf = new JSONObject();
            int flowId = f.getInt("flow_id");
            uf.put("flow_id", flowId);
            uf.put("type", f.getString("type"));
            uf.put("server_port", bindings.getInt(String.valueOf(flowId)));
            if (f.has("direction")) {
                uf.put("direction", f.getString("direction"));
            }
            if (f.has("rate_pps")) {
                uf.put("rate_pps", f.getInt("rate_pps"));
            }
            if (f.has("packet_size_bytes")) {
                uf.put("packet_size_bytes", f.getInt("packet_size_bytes"));
            }
            if (f.has("target_bytes")) {
                uf.put("target_bytes", f.getLong("target_bytes"));
            }
            flows.put(uf);
        }
        return new JSONObject()
                .put("test_id", payload.getString("test_id"))
                .put("device_id", deviceId)
                .put("session_id", sessionId)
                .put("server_host", payload.getString("server_host"))
                .put("log_dir", mobileDir)
                .put("flows", flows)
                .toString();
    }

    private void upload(ServerClient client, String session, String testId, File testRoot, JSONArray sealed)
            throws Exception {
        JSONArray files = new JSONArray();
        for (int i = 0; i < sealed.length(); i++) {
            File f = new File(sealed.getString(i));
            byte[] bytes = Files.readAllBytes(f.toPath());
            files.put(new JSONObject()
                    .put("path", relPath(testRoot, f))
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
            String rel = needed.getString(i);
            byte[] bytes = Files.readAllBytes(new File(testRoot, rel).toPath());
            client.putFile(session, testId, uploadId, rel, bytes);
        }
        client.uploadComplete(session, testId, uploadId);
    }

    private static String relPath(File root, File f) {
        String r = root.getAbsolutePath();
        String p = f.getAbsolutePath();
        return p.startsWith(r + "/") ? p.substring(r.length() + 1) : f.getName();
    }

    private void postStatus(ServerClient client, String session, String deviceId, @Nullable String currentTestId) {
        try {
            long[] t = Clock.now();
            JSONObject status = new JSONObject()
                    .put("logger_state", STATE.startsWith("RUNNING_TEST") ? "RUNNING_TEST" : "IDLE")
                    .put("logging_config_version", loggingConfigVersion)
                    .put("sampled_at", new JSONObject().put("mono_ns", t[0]).put("utc_ns", t[1]));
            JSONObject batt = battery();
            if (batt != null) {
                status.put("battery", batt);
            }
            JSONObject stor = storage();
            if (stor != null) {
                status.put("storage", stor);
            }
            if (currentTestId != null) {
                status.put("current_test_id", currentTestId);
            }
            client.postStatus(session, deviceId, status);
        } catch (Exception ignored) {
        }
    }

    /** Battery level (0–100) + charging, or null if unavailable. */
    private JSONObject battery() {
        try {
            android.os.BatteryManager bm =
                    (android.os.BatteryManager) getSystemService(Context.BATTERY_SERVICE);
            int level = bm.getIntProperty(android.os.BatteryManager.BATTERY_PROPERTY_CAPACITY);
            if (level < 0 || level > 100) {
                return null;
            }
            return new JSONObject().put("level_pct", level).put("charging", bm.isCharging());
        } catch (Exception e) {
            return null;
        }
    }

    /** Free/total bytes of the log volume, or null if unavailable. */
    private JSONObject storage() {
        try {
            android.os.StatFs sf = new android.os.StatFs(getFilesDir().getAbsolutePath());
            return new JSONObject()
                    .put("free_bytes", sf.getAvailableBytes())
                    .put("total_bytes", sf.getTotalBytes());
        } catch (Exception e) {
            return null;
        }
    }

    private long freeBytes() {
        try {
            return new android.os.StatFs(getFilesDir().getAbsolutePath()).getAvailableBytes();
        } catch (Exception e) {
            return Long.MAX_VALUE; // can't tell → don't block the test
        }
    }

    private static String sha256Hex(byte[] bytes) throws Exception {
        byte[] d = MessageDigest.getInstance("SHA-256").digest(bytes);
        StringBuilder sb = new StringBuilder(d.length * 2);
        for (byte b : d) {
            sb.append(String.format("%02x", b));
        }
        return sb.toString();
    }

    private void setState(String s) {
        STATE = s;
        NotificationManager nm = (NotificationManager) getSystemService(NOTIFICATION_SERVICE);
        nm.notify(1, notification(s));
    }

    private Notification notification(String text) {
        NotificationManager nm = (NotificationManager) getSystemService(NOTIFICATION_SERVICE);
        if (nm.getNotificationChannel(CHANNEL) == null) {
            nm.createNotificationChannel(
                    new NotificationChannel(CHANNEL, "Logger", NotificationManager.IMPORTANCE_LOW));
        }
        return new NotificationCompat.Builder(this, CHANNEL)
                .setContentTitle("Measurement Logger")
                .setContentText(text)
                .setSmallIcon(android.R.drawable.ic_menu_compass)
                .setOngoing(true)
                .build();
    }
}
