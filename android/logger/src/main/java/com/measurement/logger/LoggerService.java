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
    /** contracts/versions/versions.yaml — carried in every header.json. */
    static final String PREFS = "mp_logger";

    /** Coarse state surfaced to the UI. */
    public static volatile String STATE = "idle";

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
    private volatile StatusReporter statusReporter;

    /** Command long poll: the server holds each poll up to this long, so an
     *  idle Logger asks about once per POLL_WAIT_S yet a test starts at once. */
    static final int POLL_WAIT_S = 90;
    /** Idle status cadence; a network or serving-cell change is sent at once. */
    private static final long STATUS_IDLE_MS = 100_000;
    /** How often the network and serving cell are looked at (locally). */
    private static final long STATUS_CHECK_MS = 15_000;

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
            reach.requestCheck(Contract.ReachTrigger.MANUAL);
        }
        final String s = server;
        final String t = token;
        if (uploads == null) {
            // A new process: no test is running yet, so any started test
            // without a marker was cut off by the last one's death.
            uploads = new Uploader(getFilesDir(), s, this::setState);
            int recovered = uploads.recoverOrphans(this::isHandled);
            if (recovered > 0) {
                setState("queued " + recovered + " interrupted test(s) for upload as PARTIAL");
            }
        }
        // A duplicate start / resume leaves live threads alone, but restarts
        // either one that has died.
        if (worker == null || !worker.isAlive()) {
            worker = new Thread(() -> run(s, t), "logger-worker");
            worker.start();
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
                    StatusReporter st = statusReporter;
                    if (st != null) {
                        st.poke();
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
                setState(Contract.LoggerStates.REGISTERING);
                KeystoreSigner.ensureKey();
                String deviceId = enrollIfNeeded(client, prefs, token);
                String session = authenticate(client, deviceId);
                JSONObject cfg = client.register(session, Wire.capability());
                loggingConfigVersion = cfg.optInt(Contract.Fields.LoggingConfig.VERSION, 1);
                loggingConfig = cfg;

                setState(Contract.LoggerStates.IDLE);
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

    /** Command long poll; status goes out from its own thread
     *  ({@link StatusReporter}). Any poll exception propagates to the reconnect
     *  loop (which re-authenticates after backoff). Returns only when stopping. */
    private void pollLoop(ServerClient client, String session, String deviceId) throws Exception {
        StatusReporter status = new StatusReporter(client, session, deviceId);
        statusReporter = status;
        status.start();
        try {
            while (running) {
                long asked = System.currentTimeMillis();
                JSONObject cmd = client.pollCommand(session, deviceId, POLL_WAIT_S);
                String command = cmd != null ? cmd.optString(Contract.Fields.DeviceCommand.COMMAND) : "";
                if (Contract.Commands.RUN_TEST.equals(command)) {
                    runTest(client, session, deviceId, cmd);
                } else if (Contract.Commands.SET_LOGGING_CONFIG.equals(command)) {
                    applyLoggingConfig(client, session, deviceId, cmd);
                } else if (System.currentTimeMillis() - asked < 2000) {
                    Thread.sleep(2000);  // a server without long polling answers at once
                }
            }
        } finally {
            status.stop();
            statusReporter = null;
        }
    }

    /** Status to the server when the Logger state, the network or the serving cell changes
     *  (looked at every STATUS_CHECK_MS, and at once on a network change) and
     *  otherwise every STATUS_IDLE_MS: the server sees each cell and the moment
     *  the phone leaves Wi-Fi (the commute trigger), without a report every
     *  few seconds. */
    private final class StatusReporter {
        private final Thread thread;
        private final Object wake = new Object();
        private volatile boolean stopped;

        StatusReporter(ServerClient client, String session, String deviceId) {
            thread = new Thread(() -> loop(client, session, deviceId), "mp-status");
        }

        void start() {
            thread.start();
        }

        void stop() {
            stopped = true;
            thread.interrupt();
        }

        void poke() {
            synchronized (wake) {
                wake.notifyAll();
            }
        }

        private void loop(ServerClient client, String session, String deviceId) {
            String last = null;
            long lastPost = 0;
            while (running && !stopped) {
                String place = place();
                long now = System.currentTimeMillis();
                if (!place.equals(last) || now - lastPost >= STATUS_IDLE_MS) {
                    postStatus(client, session, deviceId, null);
                    ReachabilityChecker r = reach;
                    if (r != null) {
                        r.flush(client, session, deviceId);  // reports from networks the server could not be reached on
                    }
                    last = place;
                    lastPost = now;
                }
                try {
                    synchronized (wake) {
                        wake.wait(STATUS_CHECK_MS);
                    }
                } catch (InterruptedException e) {
                    return;
                }
            }
        }
    }

    /** The Logger state, the default network's transport and the serving
     *  cell, as a change key. */
    private String place() {
        String transport = "-";
        try {
            android.net.ConnectivityManager cm =
                    (android.net.ConnectivityManager) getSystemService(CONNECTIVITY_SERVICE);
            android.net.NetworkCapabilities caps = cm.getNetworkCapabilities(cm.getActiveNetwork());
            if (caps != null) {
                transport = caps.hasTransport(android.net.NetworkCapabilities.TRANSPORT_WIFI)
                        ? Contract.NetworkTransport.WIFI
                        : caps.hasTransport(android.net.NetworkCapabilities.TRANSPORT_CELLULAR)
                        ? Contract.NetworkTransport.CELLULAR : "other";
            }
        } catch (RuntimeException ignored) {
        }
        JSONObject cell = RadioSampler.serving(this);
        String key = STATE + " " + transport;
        return cell == null ? key
                : key + " " + cell.optString(Contract.RadioRow.PCI) + "/" + cell.optString(Contract.RadioRow.NCI);
    }

    /** Apply a server-pushed logging config: adopt its version and ack. The
     *  effective config governs what later tests log (server-configured
     *  logging, invariant 3). */
    private void applyLoggingConfig(ServerClient client, String session, String deviceId, JSONObject cmd)
            throws Exception {
        JSONObject payload = cmd.optJSONObject(Contract.Fields.DeviceCommand.PAYLOAD);
        if (payload != null) {
            loggingConfigVersion = payload.optInt(Contract.Fields.LoggingConfig.VERSION, loggingConfigVersion);
            loggingConfig = payload; // retained for the next test
        }
        client.ackCommand(session, deviceId, cmd.getLong(Contract.Fields.DeviceCommand.COMMAND_ID));
        setState("CONFIG_UPDATED v" + loggingConfigVersion);
    }

    private String enrollIfNeeded(ServerClient client, SharedPreferences prefs, String token) throws Exception {
        String existing = prefs.getString("device_id", null);
        if (existing != null) {
            return existing;
        }
        JSONObject body = Wire.enroll(KeystoreSigner.publicKeySpkiB64(),
                android.os.Build.MODEL, android.os.Build.MANUFACTURER, android.os.Build.VERSION.RELEASE);
        JSONObject resp = client.enroll(token == null ? "" : token, body);
        String deviceId = resp.getString(Contract.Fields.EnrollResponse.DEVICE_ID);
        prefs.edit().putString("device_id", deviceId).apply();
        return deviceId;
    }

    private String authenticate(ServerClient client, String deviceId) throws Exception {
        JSONObject c = client.challenge(deviceId);
        String sig = KeystoreSigner.signChallengeB64(
                c.getString(Contract.Fields.ChallengeResponse.SERVER_ID), c.getString(Contract.Fields.ChallengeResponse.CHALLENGE_ID),
                c.getString(Contract.Fields.ChallengeResponse.NONCE));
        return client.verify(c.getString(Contract.Fields.ChallengeResponse.CHALLENGE_ID), sig)
                .getString(Contract.Fields.VerifyResponse.SESSION_TOKEN);
    }

    /** Refuse to start below this much free space — logging-first means a full
     *  disk is the one failure that actually breaks measurement. */
    private static final long MIN_FREE_BYTES = 200L * 1024 * 1024;

    private void runTest(ServerClient client, String session, String deviceId, JSONObject cmd) throws Exception {
        JSONObject payload = cmd.getJSONObject(Contract.Fields.DeviceCommand.PAYLOAD);
        String testId = payload.getString(Contract.Fields.RunTestPayload.TEST_ID);
        String sessionId = payload.getString(Contract.Fields.RunTestPayload.SESSION_ID);
        long commandId = cmd.getLong(Contract.Fields.DeviceCommand.COMMAND_ID);

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
        String ueConfig = Wire.ueConfig(payload, deviceId, mobileDir.getAbsolutePath());

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

        setState(Contract.LoggerStates.RUNNING_TEST + " " + testId);
        int durationSec = payload.getJSONObject(Contract.Fields.RunTestPayload.TEST_PLAN).optInt(Contract.Fields.TestPlan.DURATION_SEC, 30);
        MpProbe.fast(durationSec + 30);
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
            MpProbe.fast(0);
            String testResult = Wire.testResult(RustCore.stopTest(h));
            JSONArray sealed = new JSONArray(RustCore.sealedFiles(h));
            if (header != null) {
                sealed.put(header.getAbsolutePath());
            }
            File probeLog = MpProbe.fetch(new File(mobileDir, Contract.MpProbe.TEST_FILE),
                    startUtcNs - 5_000_000_000L, Clock.now()[1] + 1_000_000_000L);
            if (probeLog != null) {
                sealed.put(probeLog.getAbsolutePath());
            }
            RustCore.destroy(h);
            uploads.queue(testRoot, testId, sealed, testResult);
            uploads.setMeasuring(false);
        }

        // The upload is queued on disk: ack now (the server stops its
        // generators) and leave the upload to the uploader thread, so it never
        // blocks the next test.
        ack(client, session, deviceId, commandId);
        setState(Contract.LoggerStates.IDLE);
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
            JSONObject loggingCfg = payload.optJSONObject(Contract.Fields.RunTestPayload.LOGGING_CONFIG_OVERRIDE);
            if (loggingCfg == null) {
                loggingCfg = loggingConfig != null ? loggingConfig : new JSONObject();
            }
            JSONObject header = Wire.header(payload, loggingCfg, deviceId,
                    getPackageManager().getPackageInfo(getPackageName(), 0).versionName,
                    android.os.Build.MODEL, android.os.Build.MANUFACTURER, android.os.Build.VERSION.RELEASE,
                    t[0], t[1]);
            File f = new File(testRoot, "header.json");
            Files.write(f.toPath(), header.toString(2).getBytes(StandardCharsets.UTF_8));
            return f;
        } catch (Exception e) {
            return null;
        }
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

    private void postStatus(ServerClient client, String session, String deviceId, @Nullable String currentTestId) {
        try {
            long[] t = Clock.now();
            JSONObject status = Wire.status(STATE.startsWith(Contract.LoggerStates.RUNNING_TEST),
                    loggingConfigVersion, t[0], t[1], battery(), storage(), uploads.pending(),
                    RadioSampler.serving(this), currentTestId);
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
            return Wire.battery(level, bm.isCharging());
        } catch (Exception e) {
            return null;
        }
    }

    /** Free/total bytes of the log volume, or null if unavailable. */
    private JSONObject storage() {
        try {
            android.os.StatFs sf = new android.os.StatFs(getFilesDir().getAbsolutePath());
            return Wire.storage(sf.getAvailableBytes(), sf.getTotalBytes());
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
        StatusReporter st = statusReporter;
        if (st != null) {
            st.poke();  // the scripts wait for IDLE after a test
        }
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
