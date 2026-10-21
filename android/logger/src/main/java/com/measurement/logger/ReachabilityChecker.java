package com.measurement.logger;

import android.content.Context;
import android.net.ConnectivityManager;
import android.net.Network;
import android.net.NetworkCapabilities;
import android.net.wifi.WifiInfo;
import android.net.wifi.WifiManager;
import android.os.Handler;
import android.os.HandlerThread;
import android.telephony.TelephonyManager;

import org.json.JSONArray;
import org.json.JSONObject;

import java.io.BufferedReader;
import java.io.File;
import java.io.InputStreamReader;
import java.io.OutputStream;
import java.net.ConnectException;
import java.net.DatagramPacket;
import java.net.DatagramSocket;
import java.net.HttpURLConnection;
import java.net.InetAddress;
import java.net.InetSocketAddress;
import java.net.NoRouteToHostException;
import java.net.Socket;
import java.net.SocketTimeoutException;
import java.net.URL;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.security.SecureRandom;
import java.util.Arrays;
import java.util.Comparator;

/**
 * Which ports/protocols toward the server get through the current network
 * (contracts/schemas/reachability_report.schema.json, wire protocol
 * contracts/packets/reach.md). Runs on the device by itself — at service
 * start, whenever the default network changes, and on request — because on a
 * restrictive network the server cannot reach the device with a command.
 * Every probe socket is bound to the network being checked. Reports are kept
 * as files and uploaded by {@link #flush} once the server is reachable.
 */
final class ReachabilityChecker {
    /** One probe target: a public port on the server and how to test it. */
    static final class Target {
        final String protocol; // "tcp" | "udp"
        final int port;
        final String kind;     // "reach" | "http"

        Target(String protocol, int port, String kind) {
            this.protocol = protocol;
            this.port = port;
            this.kind = kind;
        }
    }

    /** Public ports dialled on the server. The router maps them onto the
     *  server's reach responders (MP_REACH_LISTEN); 8080 is the control port. */
    static final Target[] TARGETS = {
        new Target(Contract.ReachProtocol.TCP, 8080, Contract.ReachKind.HTTP),
        new Target(Contract.ReachProtocol.TCP, 443, Contract.ReachKind.REACH),
        new Target(Contract.ReachProtocol.UDP, 443, Contract.ReachKind.REACH),
        new Target(Contract.ReachProtocol.TCP, 80, Contract.ReachKind.REACH),
        new Target(Contract.ReachProtocol.TCP, 3478, Contract.ReachKind.REACH),
        new Target(Contract.ReachProtocol.UDP, 3478, Contract.ReachKind.REACH),
        new Target(Contract.ReachProtocol.TCP, 41100, Contract.ReachKind.REACH),
        new Target(Contract.ReachProtocol.UDP, 41100, Contract.ReachKind.REACH),
    };

    private static final int TIMEOUT_MS = 3000;
    private static final int UDP_TRIES = 3;
    private static final int UDP_WAIT_MS = 500;
    /** Let a network settle (DHCP, captive checks) before probing it. */
    private static final long SETTLE_MS = 3000;
    private static final int MAX_PENDING = 200;

    /** Last result in one line, for the app screen. */
    static volatile String SUMMARY = "reachability: not checked yet";

    private final Context ctx;
    private final String serverBase; // http://host:port
    private final String host;
    private final File pendingDir;
    private final ConnectivityManager cm;
    private final SecureRandom random = new SecureRandom();
    private HandlerThread thread;
    private Handler handler;
    private ConnectivityManager.NetworkCallback callback;
    private String lastNetworkKey;

    ReachabilityChecker(Context ctx, String serverBase) throws Exception {
        this.ctx = ctx.getApplicationContext();
        this.serverBase = serverBase.replaceAll("/+$", "");
        this.host = new URL(this.serverBase).getHost();
        this.pendingDir = new File(ctx.getFilesDir(), "reach_pending");
        this.cm = (ConnectivityManager) ctx.getSystemService(Context.CONNECTIVITY_SERVICE);
    }

    void start() {
        if (thread != null) {
            return;
        }
        thread = new HandlerThread("reachability");
        thread.start();
        handler = new Handler(thread.getLooper());
        callback = new ConnectivityManager.NetworkCallback() {
            @Override
            public void onCapabilitiesChanged(Network network, NetworkCapabilities caps) {
                // Fires on every default-network switch and on validation
                // changes; check once per distinct network + transport.
                String key = network + "/" + transport(caps) + "/"
                        + caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_VALIDATED);
                if (!key.equals(lastNetworkKey)) {
                    lastNetworkKey = key;
                    handler.removeCallbacksAndMessages(null);
                    handler.postDelayed(() -> check(Contract.ReachTrigger.NETWORK_CHANGE), SETTLE_MS);
                }
            }
        };
        try {
            cm.registerDefaultNetworkCallback(callback, handler);
        } catch (RuntimeException ignored) {
            callback = null; // no ACCESS_NETWORK_STATE: manual/start checks only
        }
        handler.post(() -> check(Contract.ReachTrigger.SERVICE_START));
    }

    void stop() {
        if (callback != null) {
            try {
                cm.unregisterNetworkCallback(callback);
            } catch (RuntimeException ignored) {
            }
            callback = null;
        }
        if (thread != null) {
            thread.quitSafely();
            thread = null;
        }
    }

    void requestCheck(String trigger) {
        if (handler != null) {
            handler.post(() -> check(trigger));
        }
    }

    private void check(String trigger) {
        Network net = cm.getActiveNetwork();
        try {
            long[] t = Clock.now();
            JSONObject network = networkContext(net);
            JSONArray checks = new JSONArray();
            StringBuilder line = new StringBuilder();
            for (Target target : TARGETS) {
                JSONObject c = net == null ? failed(target, "no_network") : probe(net, target);
                checks.put(c);
                line.append(' ').append(target.protocol).append(target.port)
                        .append(c.getBoolean("ok") ? "✓" : "✗");
            }
            JSONObject report = Wire.reachReport(t[0], t[1], trigger, network, checks);
            save(report);
            SUMMARY = "reach " + report.getJSONObject("network").getString("transport") + ":" + line;
        } catch (Exception e) {
            SUMMARY = "reachability check failed: " + e.getMessage();
        }
    }

    private JSONObject probe(Network net, Target target) throws Exception {
        try {
            if (Contract.ReachKind.HTTP.equals(target.kind)) {
                return httpProbe(net, target);
            }
            return Contract.ReachProtocol.UDP.equals(target.protocol) ? udpProbe(net, target) : tcpProbe(net, target);
        } catch (Exception e) {
            return failed(target, errorName(e));
        }
    }

    private JSONObject httpProbe(Network net, Target target) throws Exception {
        URL url = new URL("http://" + host + ":" + target.port + Contract.Api.SERVER_TIME);
        long t0 = System.nanoTime();
        HttpURLConnection conn = (HttpURLConnection) net.openConnection(url);
        try {
            conn.setConnectTimeout(TIMEOUT_MS);
            conn.setReadTimeout(TIMEOUT_MS);
            conn.setUseCaches(false);
            int code = conn.getResponseCode();
            double rtt = (System.nanoTime() - t0) / 1e6;
            JSONObject c = result(target, code == 200);
            if (code == 200) {
                c.put("rtt_ms", round(rtt));
            } else {
                c.put("error", "http_" + code);
            }
            return c;
        } finally {
            conn.disconnect();
        }
    }

    private JSONObject tcpProbe(Network net, Target target) throws Exception {
        String nonce = nonce();
        try (Socket s = net.getSocketFactory().createSocket()) {
            s.connect(new InetSocketAddress(host, target.port), TIMEOUT_MS);
            s.setSoTimeout(TIMEOUT_MS);
            OutputStream out = s.getOutputStream();
            long t0 = System.nanoTime();
            out.write(("MPREACH1 " + nonce + "\n").getBytes(StandardCharsets.US_ASCII));
            out.flush();
            String reply = new BufferedReader(
                    new InputStreamReader(s.getInputStream(), StandardCharsets.US_ASCII)).readLine();
            double rtt = (System.nanoTime() - t0) / 1e6;
            return fromReply(target, nonce, reply, rtt);
        }
    }

    private JSONObject udpProbe(Network net, Target target) throws Exception {
        String nonce = nonce();
        byte[] req = ("MPREACH1 " + nonce + "\n").getBytes(StandardCharsets.US_ASCII);
        InetAddress addr = net.getByName(host);
        try (DatagramSocket s = new DatagramSocket()) {
            net.bindSocket(s);
            s.setSoTimeout(UDP_WAIT_MS);
            byte[] buf = new byte[256];
            for (int i = 0; i < UDP_TRIES; i++) {
                long t0 = System.nanoTime();
                s.send(new DatagramPacket(req, req.length, addr, target.port));
                try {
                    DatagramPacket p = new DatagramPacket(buf, buf.length);
                    s.receive(p);
                    double rtt = (System.nanoTime() - t0) / 1e6;
                    String reply = new String(p.getData(), 0, p.getLength(), StandardCharsets.US_ASCII).trim();
                    return fromReply(target, nonce, reply, rtt);
                } catch (SocketTimeoutException retry) {
                    // lost or blocked: try again
                }
            }
            return failed(target, "timeout");
        }
    }

    /** "MPREACH1 OK <nonce> <ip:port>" with our nonce, else bad_reply. */
    private JSONObject fromReply(Target target, String nonce, String reply, double rttMs) throws Exception {
        String[] parts = reply == null ? new String[0] : reply.trim().split(" ");
        if (parts.length == 4 && "MPREACH1".equals(parts[0]) && "OK".equals(parts[1]) && nonce.equals(parts[2])) {
            return result(target, true).put("rtt_ms", round(rttMs)).put("seen_as", parts[3]);
        }
        return failed(target, "bad_reply");
    }

    private static JSONObject result(Target target, boolean ok) throws Exception {
        return Wire.reachCheck(target.protocol, target.port, target.kind, ok);
    }

    private static JSONObject failed(Target target, String error) throws Exception {
        return result(target, false).put("error", error);
    }

    private static String errorName(Exception e) {
        if (e instanceof SocketTimeoutException) return "timeout";
        if (e instanceof ConnectException) {
            String m = String.valueOf(e.getMessage());
            return m.contains("ECONNREFUSED") ? "refused" : "connect_failed";
        }
        if (e instanceof NoRouteToHostException) return "no_route";
        if (e instanceof java.net.UnknownHostException) return "dns";
        String m = String.valueOf(e.getMessage());
        if (m.contains("ECONNRESET") || m.contains("reset")) return "reset";
        return e.getClass().getSimpleName();
    }

    private static double round(double ms) {
        return Math.round(ms * 10) / 10.0;
    }

    private String nonce() {
        final String abc = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
        StringBuilder b = new StringBuilder(12);
        for (int i = 0; i < 12; i++) {
            b.append(abc.charAt(random.nextInt(abc.length())));
        }
        return b.toString();
    }

    private static String transport(NetworkCapabilities caps) {
        if (caps == null) return Contract.NetworkTransport.NONE;
        if (caps.hasTransport(NetworkCapabilities.TRANSPORT_WIFI)) return Contract.NetworkTransport.WIFI;
        if (caps.hasTransport(NetworkCapabilities.TRANSPORT_CELLULAR)) return Contract.NetworkTransport.CELLULAR;
        if (caps.hasTransport(NetworkCapabilities.TRANSPORT_ETHERNET)) return Contract.NetworkTransport.ETHERNET;
        return Contract.NetworkTransport.OTHER;
    }

    @SuppressWarnings({"MissingPermission", "deprecation"})
    private JSONObject networkContext(Network net) throws Exception {
        NetworkCapabilities caps = net == null ? null : cm.getNetworkCapabilities(net);
        String transport = transport(caps);
        Boolean validated = caps == null ? null : caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_VALIDATED);
        String ssid = null;
        int freq = 0;
        String operator = null;
        if (Contract.NetworkTransport.WIFI.equals(transport)) {
            WifiManager wm = (WifiManager) ctx.getSystemService(Context.WIFI_SERVICE);
            WifiInfo info = wm.getConnectionInfo();
            if (info != null) {
                String s = info.getSSID();
                if (s != null && !s.contains("unknown ssid")) {
                    ssid = s.replaceAll("^\"|\"$", "");
                }
                freq = info.getFrequency();
            }
        } else if (Contract.NetworkTransport.CELLULAR.equals(transport)) {
            TelephonyManager tm = (TelephonyManager) ctx.getSystemService(Context.TELEPHONY_SERVICE);
            String op = tm.getNetworkOperatorName();
            if (op != null && !op.isEmpty()) {
                operator = op;
            }
        }
        return Wire.network(transport, validated, ssid, freq, operator);
    }

    private void save(JSONObject report) throws Exception {
        if (!pendingDir.isDirectory() && !pendingDir.mkdirs()) {
            return;
        }
        File[] old = pendingDir.listFiles();
        if (old != null && old.length >= MAX_PENDING) {
            Arrays.sort(old, Comparator.comparingLong(File::lastModified));
            old[0].delete(); // keep the newest; a long offline spell must not fill storage
        }
        long utc = report.getJSONObject("checked_at").getLong("utc_ns");
        File tmp = new File(pendingDir, utc + ".json.tmp");
        Files.write(tmp.toPath(), report.toString().getBytes(StandardCharsets.UTF_8));
        tmp.renameTo(new File(pendingDir, utc + ".json"));
    }

    /** Upload stored reports (oldest first). A report the server rejects as
     *  invalid is dropped; any other failure leaves it for the next try. */
    void flush(ServerClient client, String session, String deviceId) {
        File[] files = pendingDir.listFiles((d, name) -> name.endsWith(".json"));
        if (files == null) {
            return;
        }
        Arrays.sort(files, Comparator.comparing(File::getName));
        for (File f : files) {
            try {
                JSONObject report = new JSONObject(new String(Files.readAllBytes(f.toPath()), StandardCharsets.UTF_8));
                int code = client.postReachability(session, deviceId, report);
                if ((code >= 200 && code < 300) || code == 400 || code == 422) {
                    f.delete();
                } else {
                    return;
                }
            } catch (Exception e) {
                return; // offline / server down: retry on the next flush
            }
        }
    }
}
