import com.measurement.logger.RustCore;

import java.net.DatagramPacket;
import java.net.DatagramSocket;
import java.net.InetAddress;
import java.nio.file.Files;

/**
 * Desktop-JVM JNI conformance harness: load the Rust core .so and drive a probe
 * test through the real RustCore JNI surface against a Java UDP echo peer.
 * Proves the Java <-> core boundary (JSON in/out, handle lifecycle) on x86_64.
 * Exits non-zero on any check failure.
 */
public final class Harness {
    public static void main(String[] args) throws Exception {
        // UDP echo peer
        DatagramSocket sock = new DatagramSocket(0);
        int port = sock.getLocalPort();
        sock.setSoTimeout(200);
        Thread echo = new Thread(() -> {
            byte[] buf = new byte[2048];
            long end = System.currentTimeMillis() + 2000;
            while (System.currentTimeMillis() < end) {
                try {
                    DatagramPacket p = new DatagramPacket(buf, buf.length);
                    sock.receive(p);
                    sock.send(new DatagramPacket(p.getData(), p.getLength(), p.getAddress(), p.getPort()));
                } catch (Exception ignored) {
                }
            }
        });
        echo.setDaemon(true);
        echo.start();

        // log_dir is the test's mobile/ dir, so gps/radio land at the test root
        String logDir = Files.createTempDirectory("mp-harness").resolve("mobile").toString();
        String cfg = "{"
                + "\"test_id\":\"tst_01HZ8R9F0J2YK3P5Q7S9V1W3X5\","
                + "\"device_id\":\"dev_x\",\"session_id\":\"ses_x\","
                + "\"server_host\":\"127.0.0.1\",\"log_dir\":\"" + logDir + "\","
                + "\"flows\":[{\"flow_id\":1,\"type\":\"probe\",\"server_port\":" + port + ",\"rate_pps\":100}]"
                + "}";

        long h = RustCore.coreNew();
        check(h != 0, "coreNew returned a handle");

        System.out.println("clock=" + RustCore.sampleClock());

        int rc = RustCore.startTest(h, cfg);
        check(rc == 0, "startTest rc=0 (got " + rc + ")");

        // Java shell feeds low-rate aux rows over JNI
        check(RustCore.pushGps(h, "{\"mono_ns\":1,\"utc_ns\":2,\"latitude\":59.33,\"longitude\":18.06,\"accuracy_m\":5.0}") == 0, "pushGps");
        check(RustCore.pushClock(h, "{\"mono_ns\":1,\"utc_ns\":2,\"source\":\"gps\",\"uncertainty_ns\":400}") == 0, "pushClock");

        Thread.sleep(300);
        String status = RustCore.pollStatus(h);
        System.out.println("status=" + status);
        check(!status.equals("[]") && status.contains("packets_sent"), "pollStatus has counters");

        String summary = RustCore.stopTest(h);
        System.out.println("summary=" + summary);
        check(summary.contains("packets_received"), "stopTest returns summaries");

        String sealed = RustCore.sealedFiles(h);
        System.out.println("sealed=" + sealed);
        check(sealed.contains("packets"), "sealedFiles lists packets");
        check(sealed.contains("gps.parquet"), "sealedFiles lists gps");
        check(sealed.contains("clock.parquet"), "sealedFiles lists clock");

        RustCore.destroy(h);
        System.out.println("PASS: JNI boundary exercised end-to-end");
    }

    private static void check(boolean ok, String what) {
        if (!ok) {
            System.err.println("FAIL: " + what);
            System.exit(1);
        }
        System.out.println("ok: " + what);
    }
}
