package com.measurement.logger;

import androidx.annotation.Nullable;

import org.json.JSONArray;
import org.json.JSONObject;

import java.io.IOException;
import java.util.concurrent.TimeUnit;

import okhttp3.MediaType;
import okhttp3.OkHttpClient;
import okhttp3.Request;
import okhttp3.RequestBody;
import okhttp3.Response;

/**
 * REST client for the device control plane + 3-phase upload (server-main).
 * Mirrors contracts/openapi/openapi.yaml. Plain HTTP on the configured base in
 * dev (TLS is a Phase-5 concern).
 */
public final class ServerClient {
    private static final MediaType JSON = MediaType.parse("application/json");
    private static final MediaType OCTET = MediaType.parse("application/octet-stream");

    private final String base;
    /** One client (one connection pool) for the whole app: kept-alive
     *  connections stay on the network they were opened on, so the pool is
     *  emptied when the default network changes ({@link #dropConnections}). */
    private static final OkHttpClient http = new OkHttpClient.Builder()
            .callTimeout(30, TimeUnit.SECONDS)
            .build();
    /** File PUTs: no cap on the whole call (a large file on a weak link may
     *  legitimately take minutes), only on stalls — a link that moves no bytes
     *  for 60 s fails and the upload queue retries later. */
    private static final OkHttpClient uploadHttp = http.newBuilder()
            .callTimeout(0, TimeUnit.SECONDS)
            .connectTimeout(15, TimeUnit.SECONDS)
            .writeTimeout(60, TimeUnit.SECONDS)
            .readTimeout(60, TimeUnit.SECONDS)
            .build();
    /** Command long polls: the server holds each up to {@code wait_s}. */
    private static final OkHttpClient pollHttp = http.newBuilder()
            .callTimeout(LoggerService.POLL_WAIT_S + 30L, TimeUnit.SECONDS)
            .readTimeout(LoggerService.POLL_WAIT_S + 30L, TimeUnit.SECONDS)
            .build();
    /** The long poll in flight, cancelled when the network changes. */
    private static volatile okhttp3.Call poll;

    public ServerClient(String base) {
        this.base = base.endsWith("/") ? base.substring(0, base.length() - 1) : base;
    }

    /** The default network changed (e.g. back on Wi-Fi): close the pooled
     *  connections so the next request goes out over the new network. The
     *  command polls would otherwise keep the old (mobile) connection alive
     *  for good, and the server would keep holding uploads as "not home". The
     *  long poll in flight is cancelled too: it would otherwise sit on the dead
     *  network until its timeout, holding back a command meanwhile. */
    static void dropConnections() {
        okhttp3.Call c = poll;
        if (c != null) {
            c.cancel();
        }
        http.connectionPool().evictAll();
    }

    // --- device onboarding ---

    public JSONObject enroll(String enrollToken, JSONObject body) throws Exception {
        return json(post(Contract.Api.ENROLL, body, "Bearer " + enrollToken));
    }

    public JSONObject challenge(String deviceId) throws Exception {
        return json(post(Contract.Api.CHALLENGE, Wire.challenge(deviceId), null));
    }

    public JSONObject verify(String challengeId, String signatureB64) throws Exception {
        return json(post(Contract.Api.VERIFY, Wire.verify(challengeId, signatureB64), null));
    }

    public JSONObject register(String session, JSONObject capability) throws Exception {
        return json(post(Contract.Api.REGISTER, capability, "Bearer " + session));
    }

    // --- command queue + status ---

    /** Oldest pending command, or null on 204 — the server holds the request
     *  up to {@code waitS} seconds for one to arrive (long poll). */
    @Nullable
    public JSONObject pollCommand(String session, String deviceId, int waitS) throws Exception {
        Request req = new Request.Builder()
                .url(okhttp3.HttpUrl.get(base + Contract.Api.path(Contract.Api.POLL_COMMAND, deviceId))
                        .newBuilder()
                        .addQueryParameter(Contract.Api.Query.WAIT_S, Integer.toString(waitS))
                        .build())
                .header("Authorization", "Bearer " + session)
                .get()
                .build();
        okhttp3.Call call = pollHttp.newCall(req);
        poll = call;
        try (Response r = call.execute()) {
            if (r.code() == 204) {
                return null;
            }
            return new JSONObject(bodyString(r));
        } finally {
            poll = null;
        }
    }

    /** Server UTC (ns) for clock sync. Unauthenticated; the caller brackets this
     *  with its own send/recv timestamps to estimate offset + RTT. */
    public long serverTimeNs() throws Exception {
        Request req = new Request.Builder().url(base + Contract.Api.SERVER_TIME).get().build();
        try (Response r = http.newCall(req).execute()) {
            return new JSONObject(bodyString(r)).getLong(Contract.Fields.ServerTime.SERVER_UTC_NS);
        }
    }

    /** Host portion of the base URL — the address of the UDP time responder. */
    public String serverHost() {
        return java.net.URI.create(base).getHost();
    }

    /** UDP port of the low-overhead time responder (0 if not advertised). */
    public int timeUdpPort() throws Exception {
        Request req = new Request.Builder().url(base + Contract.Api.SERVER_TIME).get().build();
        try (Response r = http.newCall(req).execute()) {
            return new JSONObject(bodyString(r)).optInt(Contract.Fields.ServerTime.TIME_UDP_PORT, 0);
        }
    }

    public void ackCommand(String session, String deviceId, long commandId) throws Exception {
        Request req = new Request.Builder()
                .url(base + Contract.Api.path(Contract.Api.ACK_COMMAND, deviceId, commandId))
                .header("Authorization", "Bearer " + session)
                .post(RequestBody.create(new byte[0], null))
                .build();
        close(http.newCall(req).execute());
    }

    public void postStatus(String session, String deviceId, JSONObject status) throws Exception {
        close(post(Contract.Api.path(Contract.Api.POST_STATUS, deviceId), status, "Bearer " + session));
    }

    /** Returns the HTTP status; the caller decides whether to keep the report. */
    public int postReachability(String session, String deviceId, JSONObject report) throws Exception {
        try (Response r = post(Contract.Api.path(Contract.Api.POST_REACHABILITY, deviceId), report, "Bearer " + session)) {
            return r.code();
        }
    }

    // --- 3-phase upload ---

    public JSONObject uploadSummary(String session, String testId, JSONObject manifest) throws Exception {
        return json(post(Contract.Api.path(Contract.Api.SUMMARY, testId), manifest, "Bearer " + session));
    }

    public void putFile(String session, String testId, String uploadId, String relPath, byte[] bytes) throws Exception {
        Request req = new Request.Builder()
                .url(base + Contract.Api.path(Contract.Api.PUT_FILE, testId, uploadId, relPath))
                .header("Authorization", "Bearer " + session)
                .put(RequestBody.create(bytes, OCTET))
                .build();
        try (Response r = uploadHttp.newCall(req).execute()) {
            if (!r.isSuccessful()) {
                throw new IOException("PUT " + relPath + " -> " + r.code());
            }
        }
    }

    public void uploadComplete(String session, String testId, String uploadId) throws Exception {
        Request req = new Request.Builder()
                .url(base + Contract.Api.path(Contract.Api.COMPLETE, testId, uploadId))
                .header("Authorization", "Bearer " + session)
                .post(RequestBody.create(new byte[0], null))
                .build();
        try (Response r = http.newCall(req).execute()) {
            if (!r.isSuccessful()) {
                throw new IOException("complete -> " + r.code());
            }
        }
    }

    // --- helpers ---

    private Response post(String path, JSONObject body, @Nullable String auth) throws IOException {
        Request.Builder b = new Request.Builder()
                .url(base + path)
                .post(RequestBody.create(body.toString(), JSON));
        if (auth != null) {
            b.header("Authorization", auth);
        }
        return http.newCall(b.build()).execute();
    }

    private static JSONObject json(Response r) throws Exception {
        try (Response resp = r) {
            String body = bodyString(resp);
            if (!resp.isSuccessful()) {
                throw new IOException("HTTP " + resp.code() + ": " + body);
            }
            return new JSONObject(body);
        }
    }

    private static String bodyString(Response r) throws IOException {
        return r.body() != null ? r.body().string() : "";
    }

    private static void close(Response r) throws IOException {
        try (Response resp = r) {
            if (!resp.isSuccessful()) {
                throw new IOException("HTTP " + resp.code());
            }
        }
    }
}
