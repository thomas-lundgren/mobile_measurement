package com.measurement.logger;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.Service;
import android.content.Context;
import android.content.Intent;
import android.content.SharedPreferences;
import android.os.IBinder;
import android.os.PowerManager;

import androidx.annotation.Nullable;
import androidx.core.app.NotificationCompat;

import org.json.JSONArray;
import org.json.JSONObject;

import java.io.File;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;

/**
 * Foreground service: the autonomous Logger. Onboards (enroll → challenge →
 * verify → register), polls the command queue, runs RUN_TEST via the shared
 * core (RustCore) with radio/GPS samplers feeding it, then uploads — logging
 * stays local-first; upload reads only sealed files.
 */
public final class LoggerService extends Service {
    public static final String EXTRA_SERVER = "server";
    public static final String EXTRA_TOKEN = "token";
    /** Start-service action: run a reachability check now (the app's button). */
    public static final String ACTION_REACH_CHECK = "com.measurement.logger.REACH_CHECK";
    private static final String CHANNEL = "logger";
    private static final int MP_PROBE_PORT = 47600;
    /** contracts/versions/versions.yaml — carried in every header.json. */
    private static final int PROTOCOL_VERSION = 1;
    private static final int SCHEMA_VERSION = 1;
    static final String PREFS = "mp_logger";

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
    private Uploader uploads;
    /** Held for the service's lifetime: with the screen off (phone in a pocket)
     *  the CPU would otherwise suspend, stalling flow threads and timers and
     *  distorting every measurement. */
    private PowerManager.WakeLock wakeLock;
    static final String EXTRA_SCREEN_ON = "screen_on";
    static final String PREF_SCREEN_ON = "screen_on_during_tests";
    private int loggingConfigVersion = 1;
    private JSONObject loggingConfig; // latest server-pushed config (SET_LOGGING_CONFIG)
    private ReachabilityChecker reach;
    private android.net.ConnectivityManager.NetworkCallback networkWatch;

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        // The location type is what lets GPS and cell info (which needs
        // location permission) keep working while the app is not on screen —
        // a specialUse-only service gets location denied in the background.
        // Android refuses the location type when started from the background
        // without background-location permission (e.g. a sticky restart); then
        // fall back to specialUse so measuring continues, just without
        // location. Guard both so we stop cleanly instead of crash-looping.
        int special = android.content.pm.ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE;
        int location = android.content.pm.ServiceInfo.FOREGROUND_SERVICE_TYPE_LOCATION;
        try {
            startForeground(1, notification("starting"), special | location);
        } catch (Exception withLocation) {
            try {
                startForeground(1, notification("starting"), special);
                setState("WARNING: no background location (open the app to restore GPS/radio)");
            } catch (Exception e) {
                setState("STOPPED: cannot start foreground (" + e.getClass().getSimpleName() + ")");
                stopSelf();
                return START_NOT_STICKY;
            }
        }

        // START_STICKY restarts deliver a null intent — resume from the last
        // server/token rather than crashing or silently stopping.
        SharedPreferences prefs = getSharedPreferences(PREFS, Context.MODE_PRIVATE);
        String server = intent != null ? intent.getStringExtra(EXTRA_SERVER) : null;
        String token = intent != null ? intent.getStringExtra(EXTRA_TOKEN) : null;
        if (server != null) {
            prefs.edit().putString("server", server).putString("token", token).apply();
        } else {
            server = prefs.getString("server", null);
            token = prefs.getString("token", null);
        }
        if (server == null) {
            stopSelf();
            return START_NOT_STICKY;
        }
        acquireWakeLock();
        running = true;
        if (reach == null) {
            try {
                reach = new ReachabilityChecker(this, server);
                reach.start();
            } catch (Exception e) {
                ReachabilityChecker.SUMMARY = "reachability unavailable: " + e.getMessage();
            }
        } else if (intent != null && ACTION_REACH_CHECK.equals(intent.getAction())) {
            reach.requestCheck("MANUAL");
        }
        final String s = server;
        final String t = token;
        // A duplicate start / resume leaves live threads alone, but restarts
        // either one that has died.
        if (worker == null || !worker.isAlive()) {
            worker = new Thread(() -> run(s, t), "logger-worker");
            worker.start();
        }
        if (uploads == null) {
            uploads = new Uploader(getFilesDir(), s, this::setState);
        }
        uploads.start();
        watchDefaultNetwork();
        return START_STICKY;
    }

    /** When the default network changes (Wi-Fi back at home, or lost), move
     *  the server connections to the new one and retry held uploads at once.
     *  Measurement flows follow on their own (core reopens its sockets). */
    private void watchDefaultNetwork() {
        if (networkWatch != null) {
            return;
        }
        android.net.ConnectivityManager cm =
                (android.net.ConnectivityManager) getSystemService(CONNECTIVITY_SERVICE);
        networkWatch = new android.net.ConnectivityManager.NetworkCallback() {
            private android.net.Network current;

            @Override
            public void onAvailable(android.net.Network network) {
                if (current != null && !current.equals(network)) {
                    ServerClient.dropConnections();
                    if (uploads != null) {
                        uploads.networkChanged();
                    }
                }
                current = network;
            }
        };
        try {
            cm.registerDefaultNetworkCallback(networkWatch);
        } catch (RuntimeException e) {
            networkWatch = null; // no ACCESS_NETWORK_STATE: restart the app after a switch
        }
    }

    /** Keep the screen on (dimmed) during a test unless turned off: with the
     *  screen off and not charging, Android stops the modem's signal-strength,
     *  physical-channel-config and link-capacity indications to save power,
     *  so CQI/SNR, carrier aggregation and LCE freeze for the whole test. */
    @SuppressWarnings("deprecation")
    private PowerManager.WakeLock screenOnDuringTest() {
        if (!getSharedPreferences(PREFS, Context.MODE_PRIVATE).getBoolean(PREF_SCREEN_ON, true)) {
            return null;
        }
        PowerManager pm = (PowerManager) getSystemService(POWER_SERVICE);
        PowerManager.WakeLock wl = pm.newWakeLock(
                PowerManager.SCREEN_DIM_WAKE_LOCK | PowerManager.ACQUIRE_CAUSES_WAKEUP, "mp:test-screen");
        wl.setReferenceCounted(false);
        wl.acquire(6 * 3600_000L); // safety timeout
        return wl;
    }

    private void acquireWakeLock() {
        if (wakeLock == null) {
            PowerManager pm = (PowerManager) getSystemService(POWER_SERVICE);
            wakeLock = pm.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "mp:logger");
            wakeLock.setReferenceCounted(false);
        }
        if (!wakeLock.isHeld()) {
            wakeLock.acquire();
        }
    }

    private void releaseWakeLock() {
        if (wakeLock != null && wakeLock.isHeld()) {
            wakeLock.release();
        }
    }

    private void stopThreads() {
        running = false;
        if (reach != null) {
            reach.stop();
            reach = null;
        }
        if (worker != null) {
            worker.interrupt();
        }
        if (uploads != null) {
            uploads.stop();
        }
        if (networkWatch != null) {
            try {
                ((android.net.ConnectivityManager) getSystemService(CONNECTIVITY_SERVICE))
                        .unregisterNetworkCallback(networkWatch);
            } catch (RuntimeException ignored) {
            }
            networkWatch = null;
        }
        releaseWakeLock();
    }

    // Android 14+ time-limits a dataSync foreground service; when the budget is
    // spent the system calls onTimeout and the service MUST stop promptly, else
    // it throws ForegroundServiceDidNotStopInTimeException and is killed (which
    // then crash-loops the sticky restart). Stop cleanly instead.
    @Override
    public void onTimeout(int startId) {
        stopForDataSyncTimeout();
    }

    @Override
    public void onTimeout(int startId, int fgsType) {
        stopForDataSyncTimeout();
    }

    private void stopForDataSyncTimeout() {
        setState("STOPPED: foreground-service time limit reached — restart to resume");
        stopThreads();
        stopForeground(STOP_FOREGROUND_REMOVE);
        stopSelf();
    }

    @Override
    public void onDestroy() {
        stopThreads();
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
                loggingConfig = cfg;

                setState("IDLE");
                backoff = BACKOFF_MIN_MS; // connected → reset backoff
                uploads.setSession(session);
                pollLoop(client, session, deviceId);
            } catch (InterruptedException ignored) {
                return; // stopping
            } catch (Exception e) {
                uploads.setSession(null);
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
            String command = cmd != null ? cmd.optString("command") : "";
            if ("RUN_TEST".equals(command)) {
                runTest(client, session, deviceId, cmd);
            } else if ("SET_LOGGING_CONFIG".equals(command)) {
                applyLoggingConfig(client, session, deviceId, cmd);
            } else {
                Thread.sleep(2000);
            }
            if (System.currentTimeMillis() - lastStatus > 5000) {
                postStatus(client, session, deviceId, null);
                lastStatus = System.currentTimeMillis();
                ReachabilityChecker r = reach;
                if (r != null) {
                    r.flush(client, session, deviceId);  // reports from networks the server could not be reached on
                }
            }
        }
    }

    /** Apply a server-pushed logging config: adopt its version and ack. The
     *  effective config governs what later tests log (server-configured
     *  logging, invariant 3). */
    private void applyLoggingConfig(ServerClient client, String session, String deviceId, JSONObject cmd)
            throws Exception {
        JSONObject payload = cmd.optJSONObject("payload");
        if (payload != null) {
            loggingConfigVersion = payload.optInt("version", loggingConfigVersion);
            loggingConfig = payload; // retained for the next test
        }
        client.ackCommand(session, deviceId, cmd.getLong("command_id"));
        setState("CONFIG_UPDATED v" + loggingConfigVersion);
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
        long commandId = cmd.getLong("command_id");

        // Delivery is at-least-once: a RUN_TEST comes back if its ack was lost
        // or the app died mid-test. Never run a test twice — just re-ack.
        if (isHandled(testId)) {
            ack(client, session, deviceId, commandId);
            return;
        }

        if (freeBytes() < MIN_FREE_BYTES) {
            setState("ERROR: low storage, refusing " + testId);
            markHandled(testId);
            ack(client, session, deviceId, commandId);
            return;
        }

        File testRoot = new File(getFilesDir(), sessionId + "/" + testId);
        File mobileDir = new File(testRoot, "mobile");
        mobileDir.mkdirs();

        setState("PREPARING " + testId);
        File header = writeHeader(testRoot, payload, deviceId);
        String ueConfig = buildUeConfig(payload, deviceId, sessionId, mobileDir.getAbsolutePath());

        uploads.setMeasuring(true);
        long h = RustCore.coreNew();
        // The core reports a failed start either as a non-zero return or as a
        // thrown exception (e.g. "start failed: Connection refused").
        String startError = null;
        try {
            if (RustCore.startTest(h, ueConfig) != 0) {
                startError = "startTest returned non-zero";
            }
        } catch (Exception e) {
            startError = e.getMessage();
        }
        if (startError != null) {
            RustCore.destroy(h);
            uploads.setMeasuring(false);
            // Leave it unacked so the server redelivers it (coverage may be
            // back by then), but give up after a few tries so one bad test
            // can't block the command queue forever.
            int attempts = startAttempts.merge(testId, 1, Integer::sum);
            if (attempts < MAX_START_ATTEMPTS) {
                throw new Exception("start of " + testId + " failed (attempt " + attempts + "): " + startError);
            }
            markHandled(testId);
            ack(client, session, deviceId, commandId);
            setState("ERROR: could not start " + testId + ": " + startError);
            return;
        }
        // Durably mark the test as taken before measuring, so a crash mid-test
        // plus a redelivery can't start it again.
        markHandled(testId);

        GpsSampler gps = new GpsSampler(this, h);
        RadioSampler radio = new RadioSampler(this, h);
        ClockSampler clock = new ClockSampler(client, h);
        gps.start();
        radio.start();
        clock.start();

        setState("RUNNING_TEST " + testId);
        int durationSec = payload.getJSONObject("test_plan").optInt("duration_sec", 30);
        probeFast(durationSec + 30);
        long startUtcNs = Clock.now()[1];
        PowerManager.WakeLock screen = screenOnDuringTest();
        try {
            for (int i = 0; i < durationSec && running; i++) {
                Thread.sleep(1000);
            }
        } finally {
            // Always seal and queue what was recorded — even if the test is cut
            // short — so no measured data is ever stranded on the device.
            if (screen != null && screen.isHeld()) {
                screen.release();
            }
            gps.stop();
            radio.stop();
            clock.stop();
            probeFast(0);
            RustCore.stopTest(h);
            JSONArray sealed = new JSONArray(RustCore.sealedFiles(h));
            if (header != null) {
                sealed.put(header.getAbsolutePath());
            }
            File probeLog = fetchProbeWindow(mobileDir, startUtcNs - 5_000_000_000L, Clock.now()[1] + 1_000_000_000L);
            if (probeLog != null) {
                sealed.put(probeLog.getAbsolutePath());
            }
            RustCore.destroy(h);
            uploads.queue(testRoot, testId, sealed);
            uploads.setMeasuring(false);
        }

        // The upload is queued on disk: ack now (the server stops its
        // generators) and leave the upload to the uploader thread, so it never
        // blocks the next test.
        ack(client, session, deviceId, commandId);
        setState("IDLE");
    }

    /** mp-probe (the adb-started collector, if running) polls the modem queues
     *  at full rate only for this long; 0 = back to idle now. Best-effort. */
    private static void probeFast(int seconds) {
        try (java.net.Socket s = new java.net.Socket()) {
            s.connect(new java.net.InetSocketAddress("127.0.0.1", MP_PROBE_PORT), 200);
            s.setSoTimeout(500);
            s.getOutputStream().write(("FAST " + seconds + "\n").getBytes(StandardCharsets.US_ASCII));
            s.getInputStream().read(); // "OK": the window is set
        } catch (java.io.IOException ignored) {
            // not running (e.g. after a reboot): nothing to slow down
        }
    }

    /** header.json at the test root (contracts/schemas/header.schema.json):
     *  what ran — versions, identities, start time, the exact plan and port
     *  bindings, the logging config — written before the first packet, so
     *  analysis never needs the server's database. Null if it can't be
     *  written (logging-first: the test runs regardless). */
    @Nullable
    private File writeHeader(File testRoot, JSONObject payload, String deviceId) {
        try {
            long[] t = Clock.now();
            JSONObject loggingCfg = payload.optJSONObject("logging_config_override");
            if (loggingCfg == null) {
                loggingCfg = loggingConfig != null ? loggingConfig : new JSONObject();
            }
            JSONObject header = new JSONObject()
                    .put("schema_version", SCHEMA_VERSION)
                    .put("protocol_version", PROTOCOL_VERSION)
                    .put("app_version", getPackageManager().getPackageInfo(getPackageName(), 0).versionName)
                    .put("session_id", payload.getString("session_id"))
                    .put("test_id", payload.getString("test_id"))
                    .put("test_start_utc_ns", t[1])
                    .put("test_start_mono_ns", t[0])
                    .put("device", new JSONObject()
                            .put("device_id", deviceId)
                            .put("node_id", "mobile")
                            .put("platform", "android")
                            .put("model", android.os.Build.MODEL)
                            .put("manufacturer", android.os.Build.MANUFACTURER)
                            .put("os_version", android.os.Build.VERSION.RELEASE))
                    .put("server", new JSONObject()
                            .put("node_id", "server")
                            .put("host", payload.getString("server_host")))
                    .put("test_plan", payload.getJSONObject("test_plan"))
                    .put("flow_bindings", payload.getJSONArray("flow_bindings"))
                    .put("logging_config", loggingCfg);
            File f = new File(testRoot, "header.json");
            Files.write(f.toPath(), header.toString(2).getBytes(StandardCharsets.UTF_8));
            return f;
        } catch (Exception e) {
            return null;
        }
    }

    /** mp-probe's records (modem queue stats, UDP counters, radio-log events)
     *  for the test window, gzipped to mobile/probe.jsonl.gz so they upload
     *  with the test. Null if mp-probe isn't running or had nothing. */
    @Nullable
    private static File fetchProbeWindow(File mobileDir, long t0Ns, long t1Ns) {
        File out = new File(mobileDir, "probe.jsonl.gz");
        long lines = 0;
        try (java.net.Socket s = new java.net.Socket()) {
            s.connect(new java.net.InetSocketAddress("127.0.0.1", MP_PROBE_PORT), 200);
            s.setSoTimeout(10_000);
            s.getOutputStream().write(("RANGE " + t0Ns + " " + t1Ns + "\n").getBytes(StandardCharsets.US_ASCII));
            try (java.io.InputStream in = s.getInputStream();
                 java.io.OutputStream gz = new java.util.zip.GZIPOutputStream(new java.io.FileOutputStream(out))) {
                byte[] buf = new byte[64 * 1024];
                for (int n; (n = in.read(buf)) > 0; ) {
                    gz.write(buf, 0, n);
                    for (int i = 0; i < n; i++) {
                        if (buf[i] == '\n') {
                            lines++;
                        }
                    }
                }
            }
        } catch (java.io.IOException e) {
            // not running, or cut off: keep what arrived (whole lines are valid)
        }
        if (lines == 0) {
            out.delete();
            return null;
        }
        return out;
    }

    /** Ack a command; a 404 means it was already acked (our earlier ack landed
     *  but its response was lost), which is success. */
    private static void ack(ServerClient client, String session, String deviceId, long commandId)
            throws Exception {
        try {
            client.ackCommand(session, deviceId, commandId);
        } catch (java.io.IOException e) {
            if (!"HTTP 404".equals(e.getMessage())) {
                throw e;
            }
        }
    }

    // --- RUN_TEST de-duplication (persisted: survives process death) ---

    private static final String KEY_HANDLED = "handled_tests";
    private static final int MAX_HANDLED = 500;
    private static final int MAX_START_ATTEMPTS = 3;
    private final java.util.Map<String, Integer> startAttempts = new java.util.HashMap<>();

    private boolean isHandled(String testId) {
        String raw = getSharedPreferences(PREFS, Context.MODE_PRIVATE).getString(KEY_HANDLED, "");
        return java.util.Arrays.asList(raw.split("\n")).contains(testId);
    }

    private void markHandled(String testId) {
        SharedPreferences prefs = getSharedPreferences(PREFS, Context.MODE_PRIVATE);
        java.util.List<String> ids = new java.util.ArrayList<>(
                java.util.Arrays.asList(prefs.getString(KEY_HANDLED, "").split("\n")));
        ids.remove("");
        ids.add(testId);
        while (ids.size() > MAX_HANDLED) {
            ids.remove(0);
        }
        // commit (not apply): this must be on disk before the test starts.
        prefs.edit().putString(KEY_HANDLED, String.join("\n", ids)).commit();
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
            if (f.has("sndbuf_bytes")) {
                uf.put("sndbuf_bytes", f.getInt("sndbuf_bytes"));
            }
            if (f.has("rate_kbps")) {
                uf.put("rate_kbps", f.getInt("rate_kbps"));
            }
            // app_traffic: user model, passed through as the core reads it
            for (String k : new String[] {"profile", "workers", "seed", "scale", "actions", "placement"}) {
                if (f.has(k)) {
                    uf.put(k, f.get(k));
                }
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
            status.put("pending_uploads", uploads.pending());
            JSONObject cell = RadioSampler.serving(this);
            if (cell != null && cell.length() > 0) {
                status.put("radio", cell);
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
