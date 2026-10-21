package com.measurement.logger;

import org.json.JSONObject;

import java.net.DatagramPacket;
import java.net.DatagramSocket;
import java.net.InetAddress;
import java.util.Random;

/** Periodic clock-calibration sampler. Each tick estimates the offset from the
 *  server clock and records a clock.parquet row, so the analysis can offset-
 *  correct OWD without assuming path symmetry (invariant 2; spec §11.3, §12.6).
 *
 *  Preferred path: a UDP NTP-style burst against the server's time responder —
 *  four timestamps (t1 send, t2 server-recv, t3 server-send, t4 recv) subtract
 *  the server turnaround, and keeping the minimum-delay probe rejects queuing
 *  noise, giving a much tighter bound than the HTTP fallback. Falls back to
 *  GET /api/v1/time (single timestamp) if UDP is unavailable. */
public final class ClockSampler {
    private static final long INTERVAL_MS = 5_000L;
    private static final int PROBES_PER_TICK = 8;
    private static final int UDP_TIMEOUT_MS = 500;

    private final ServerClient client;
    private final long handle;
    private volatile boolean running;
    private Thread thread;

    private InetAddress serverAddr;
    private int udpPort;
    private final Random rnd = new Random();

    public ClockSampler(ServerClient client, long handle) {
        this.client = client;
        this.handle = handle;
    }

    public void start() {
        running = true;
        try {
            udpPort = client.timeUdpPort();
            if (udpPort > 0) {
                serverAddr = InetAddress.getByName(client.serverHost());
            }
        } catch (Exception ignored) {
            udpPort = 0; // UDP unavailable → HTTP fallback
        }
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
        sampleOnce(); // calibrate immediately
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
        if (udpPort > 0 && serverAddr != null && udpSyncOnce()) {
            return;
        }
        httpSyncOnce();
    }

    /** UDP NTP burst; keeps the minimum-delay probe. Returns true on success. */
    private boolean udpSyncOnce() {
        long bestDelay = Long.MAX_VALUE, bestOffset = 0, bestMono = 0, bestUtc = 0;
        try (DatagramSocket sock = new DatagramSocket()) {
            sock.setSoTimeout(UDP_TIMEOUT_MS);
            byte[] rx = new byte[24];
            for (int k = 0; k < PROBES_PER_TICK && running; k++) {
                byte[] nonce = new byte[8];
                rnd.nextBytes(nonce);
                long[] t1 = Clock.now();  // [mono, utc]
                sock.send(new DatagramPacket(nonce, nonce.length, serverAddr, udpPort));
                DatagramPacket reply = new DatagramPacket(rx, rx.length);
                try {
                    sock.receive(reply);
                } catch (Exception timeout) {
                    continue;
                }
                long[] t4 = Clock.now();
                long[] server = parseTimeReply(nonce, rx, reply.getLength());
                if (server == null) {
                    continue;
                }
                long t2 = server[0];  // server recv (utc ns)
                long t3 = server[1];  // server send (utc ns)
                long delay = (t4[0] - t1[0]) - (t3 - t2);  // RTT minus server turnaround
                if (delay < 0) {
                    continue;
                }
                long offset = ((t1[1] - t2) + (t4[1] - t3)) / 2;  // mobile_utc - server_utc
                if (delay < bestDelay) {
                    bestDelay = delay;
                    bestOffset = offset;
                    bestMono = t4[0];
                    bestUtc = t4[1];
                }
            }
        } catch (Exception e) {
            return false;
        }
        if (bestDelay == Long.MAX_VALUE) {
            return false;
        }
        push(bestMono, bestUtc, Contract.ClockSources.SERVER_UDP, bestDelay / 2, bestOffset);
        return true;
    }

    /** Single-timestamp HTTP fallback (looser bound: full control-plane RTT/2). */
    private void httpSyncOnce() {
        try {
            long[] t1 = Clock.now();
            long serverUtc = client.serverTimeNs();
            long[] t4 = Clock.now();
            long rtt = t4[0] - t1[0];
            if (rtt < 0) {
                return;
            }
            long offset = (t1[1] + (t4[1] - t1[1]) / 2) - serverUtc;
            push(t4[0], t4[1], Contract.ClockSources.SERVER_HTTP, rtt / 2, offset);
        } catch (Exception ignored) {
        }
    }

    private void push(long monoNs, long utcNs, String source, long uncNs, long offsetNs) {
        try {
            JSONObject o = new JSONObject();
            o.put(Contract.ClockRow.MONO_NS, monoNs);
            o.put(Contract.ClockRow.UTC_NS, utcNs);
            o.put(Contract.ClockRow.SOURCE, source);
            o.put(Contract.ClockRow.UNCERTAINTY_NS, uncNs);
            o.put(Contract.ClockRow.ESTIMATED_OFFSET_NS, offsetNs);
            RustCore.pushClock(handle, o.toString());
        } catch (Exception ignored) {
        }
    }

    /** A time-sync reply (contracts/packets/time_sync.md): {t2_recv_utc_ns,
     *  t3_send_utc_ns}, or null unless it is 24 bytes echoing our 8-byte nonce. */
    static long[] parseTimeReply(byte[] nonce, byte[] rx, int length) {
        if (length != 24 || !matches(nonce, rx)) {
            return null;
        }
        return new long[] {beLong(rx, 8), beLong(rx, 16)};
    }

    private static boolean matches(byte[] nonce, byte[] rx) {
        for (int i = 0; i < 8; i++) {
            if (nonce[i] != rx[i]) {
                return false;
            }
        }
        return true;
    }

    private static long beLong(byte[] b, int off) {
        long v = 0;
        for (int i = 0; i < 8; i++) {
            v = (v << 8) | (b[off + i] & 0xffL);
        }
        return v;
    }
}
