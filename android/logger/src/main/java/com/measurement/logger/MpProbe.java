package com.measurement.logger;

import androidx.annotation.Nullable;

import java.io.File;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.InetSocketAddress;
import java.net.Socket;
import java.nio.charset.StandardCharsets;
import java.util.zip.GZIPOutputStream;

/**
 * Client of mp-probe, the adb-started on-phone collector
 * (contracts/internal/mp_probe.yaml). Best-effort throughout: a test never
 * depends on mp-probe running (it is gone after a reboot until restarted).
 */
final class MpProbe {
    private MpProbe() {}

    static String fastLine(int seconds) {
        return Contract.MpProbe.FAST + " " + seconds + "\n";
    }

    static String rangeLine(long t0Ns, long t1Ns) {
        return Contract.MpProbe.RANGE + " " + t0Ns + " " + t1Ns + "\n";
    }

    /** Poll the modem queues at full rate for this long; 0 = back to idle now. */
    static void fast(int seconds) {
        fast(Contract.MpProbe.PORT, seconds);
    }

    static void fast(int port, int seconds) {
        try (Socket s = new Socket()) {
            s.connect(new InetSocketAddress("127.0.0.1", port), 200);
            s.setSoTimeout(500);
            s.getOutputStream().write(fastLine(seconds).getBytes(StandardCharsets.US_ASCII));
            s.getInputStream().read(); // the OK reply: the window is set
        } catch (Exception ignored) {
            // not running, or anything else
        }
    }

    /** The records between t0 and t1, gzipped into {@code out}
     *  (mobile/probe.jsonl.gz). Null if mp-probe isn't running or had none. */
    @Nullable
    static File fetch(File out, long t0Ns, long t1Ns) {
        return fetch(Contract.MpProbe.PORT, out, t0Ns, t1Ns);
    }

    @Nullable
    static File fetch(int port, File out, long t0Ns, long t1Ns) {
        long lines = 0;
        try (Socket s = new Socket()) {
            s.connect(new InetSocketAddress("127.0.0.1", port), 200);
            s.setSoTimeout(10_000);
            s.getOutputStream().write(rangeLine(t0Ns, t1Ns).getBytes(StandardCharsets.US_ASCII));
            try (InputStream in = s.getInputStream();
                 OutputStream gz = new GZIPOutputStream(new FileOutputStream(out))) {
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
        } catch (Exception e) {
            // not running, or cut off: keep what arrived (whole lines are valid)
        }
        if (lines == 0) {
            out.delete();
            return null;
        }
        return out;
    }
}
