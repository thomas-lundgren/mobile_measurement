package com.measurement.logger;

import org.json.JSONObject;

/** Periodic clock-calibration sampler. Each tick does an SNTP-over-HTTP exchange
 *  with the server (GET /api/v1/time) bracketed by local dual-clock reads, then
 *  records a clock.parquet row with the estimated offset from the server clock
 *  and the RTT/2 uncertainty. This is what lets the analysis offset-correct OWD
 *  without assuming path symmetry (invariant 2; spec §11.3, §12.6). */
public final class ClockSampler {
    private static final long INTERVAL_MS = 5_000L;

    private final ServerClient client;
    private final long handle;
    private volatile boolean running;
    private Thread thread;

    public ClockSampler(ServerClient client, long handle) {
        this.client = client;
        this.handle = handle;
    }

    public void start() {
        running = true;
        thread = new Thread(this::loop, "clock-sampler");
        thread.start();
    }

    public void stop() {
        running = false;
        if (thread != null) {
            try {
                thread.join(2000);
            } catch (InterruptedException ignored) {
            }
        }
    }

    private void loop() {
        sampleOnce(); // calibrate immediately, don't wait a full interval
        while (running) {
            try {
                Thread.sleep(INTERVAL_MS);
            } catch (InterruptedException e) {
                return;
            }
            if (running) {
                sampleOnce();
            }
        }
    }

    private void sampleOnce() {
        try {
            long[] t1 = Clock.now();           // [mono, utc] before
            long serverUtc = client.serverTimeNs();
            long[] t4 = Clock.now();           // [mono, utc] after
            long rttNs = t4[0] - t1[0];        // monotonic → offset-free
            if (rttNs < 0) {
                return;
            }
            long clientMidUtc = t1[1] + (t4[1] - t1[1]) / 2;
            long offsetNs = clientMidUtc - serverUtc;  // mobile_utc - server_utc

            JSONObject o = new JSONObject();
            o.put("mono_ns", t4[0]);
            o.put("utc_ns", t4[1]);
            o.put("source", "server_http");
            o.put("uncertainty_ns", rttNs / 2);
            o.put("estimated_offset_ns", offsetNs);
            RustCore.pushClock(handle, o.toString());
        } catch (Exception ignored) {
            // a missed calibration just means one fewer clock row; tests continue
        }
    }
}
