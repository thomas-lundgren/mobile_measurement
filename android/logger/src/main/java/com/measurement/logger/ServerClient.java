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
    private final OkHttpClient http = new OkHttpClient.Builder()
            .callTimeout(30, TimeUnit.SECONDS)
            .build();
    /** File PUTs: no cap on the whole call (a large file on a weak link may
     *  legitimately take minutes), only on stalls — a link that moves no bytes
     *  for 60 s fails and the upload queue retries later. */
    private final OkHttpClient uploadHttp = http.newBuilder()
            .callTimeout(0, TimeUnit.SECONDS)
            .connectTimeout(15, TimeUnit.SECONDS)
            .writeTimeout(60, TimeUnit.SECONDS)
            .readTimeout(60, TimeUnit.SECONDS)
            .build();

    public ServerClient(String base) {
        this.base = base.endsWith("/") ? base.substring(0, base.length() - 1) : base;
    }

    // --- device onboarding ---

    public JSONObject enroll(String enrollToken, JSONObject body) throws Exception {
        return json(post("/api/v1/devices/enroll", body, "Bearer " + enrollToken));
    }

    public JSONObject challenge(String deviceId) throws Exception {
        JSONObject b = new JSONObject().put("device_id", deviceId);
        return json(post("/api/v1/auth/challenge", b, null));
    }

    public JSONObject verify(String challengeId, String signatureB64) throws Exception {
        JSONObject b = new JSONObject().put("challenge_id", challengeId).put("signature", signatureB64);
        return json(post("/api/v1/auth/verify", b, null));
    }

    public JSONObject register(String session, JSONObject capability) throws Exception {
        return json(post("/api/v1/devices/register", capability, "Bearer " + session));
    }

    // --- command queue + status ---

    /** Oldest pending command, or null on 204. */
    @Nullable
    public JSONObject pollCommand(String session, String deviceId) throws Exception {
        Request req = new Request.Builder()
                .url(base + "/api/v1/devices/" + deviceId + "/command")
                .header("Authorization", "Bearer " + session)
                .get()
                .build();
        try (Response r = http.newCall(req).execute()) {
            if (r.code() == 204) {
                return null;
            }
            return new JSONObject(bodyString(r));
        }
    }

    /** Server UTC (ns) for clock sync. Unauthenticated; the caller brackets this
     *  with its own send/recv timestamps to estimate offset + RTT. */
    public long serverTimeNs() throws Exception {
        Request req = new Request.Builder().url(base + "/api/v1/time").get().build();
        try (Response r = http.newCall(req).execute()) {
            return new JSONObject(bodyString(r)).getLong("server_utc_ns");
        }
    }

    /** Host portion of the base URL — the address of the UDP time responder. */
    public String serverHost() {
        return java.net.URI.create(base).getHost();
    }

    /** UDP port of the low-overhead time responder (0 if not advertised). */
    public int timeUdpPort() throws Exception {
        Request req = new Request.Builder().url(base + "/api/v1/time").get().build();
        try (Response r = http.newCall(req).execute()) {
            return new JSONObject(bodyString(r)).optInt("time_udp_port", 0);
        }
    }

    public void ackCommand(String session, String deviceId, long commandId) throws Exception {
        Request req = new Request.Builder()
                .url(base + "/api/v1/devices/" + deviceId + "/command/" + commandId + "/ack")
                .header("Authorization", "Bearer " + session)
                .post(RequestBody.create(new byte[0], null))
                .build();
        close(http.newCall(req).execute());
    }

    public void postStatus(String session, String deviceId, JSONObject status) throws Exception {
        close(post("/api/v1/devices/" + deviceId + "/status", status, "Bearer " + session));
    }

    // --- 3-phase upload ---

    public JSONObject uploadSummary(String session, String testId, JSONObject manifest) throws Exception {
        return json(post("/api/v1/tests/" + testId + "/summary", manifest, "Bearer " + session));
    }

    public void putFile(String session, String testId, String uploadId, String relPath, byte[] bytes) throws Exception {
        Request req = new Request.Builder()
                .url(base + "/api/v1/tests/" + testId + "/upload/" + uploadId + "/files/" + relPath)
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
                .url(base + "/api/v1/tests/" + testId + "/upload/" + uploadId + "/complete")
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
