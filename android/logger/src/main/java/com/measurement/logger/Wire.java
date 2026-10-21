package com.measurement.logger;

import androidx.annotation.Nullable;

import org.json.JSONArray;
import org.json.JSONObject;

import java.util.Iterator;

/**
 * Every JSON body the Logger sends, built from plain values (no Android APIs)
 * so the unit tests can hold each one to its contract: the JSON Schemas in
 * contracts/schemas/ (header, status, capability, reachability, upload
 * manifest), the OpenAPI types for enrollment and auth, and the core's
 * UeTestConfig. Enum values come from the generated {@link Contract}.
 */
final class Wire {
    private Wire() {}

    // --- enrollment + auth (OpenAPI EnrollRequest, ChallengeRequest, VerifyRequest) ---

    static JSONObject enroll(String pubkeySpkiB64, String model, String manufacturer, String osVersion)
            throws Exception {
        return new JSONObject()
                .put("device_pubkey", pubkeySpkiB64)
                .put("pubkey_format", Contract.AUTH_PUBKEY_FORMAT)
                .put("sig_alg", Contract.AUTH_SIGNING_ALG)
                .put("sig_format", Contract.AUTH_SIG_FORMAT)
                .put("platform", Contract.Platforms.ANDROID)
                .put("hw_info", new JSONObject()
                        .put("model", model)
                        .put("manufacturer", manufacturer)
                        .put("os_version", osVersion))
                .put("protocol_version", Contract.PROTOCOL_VERSION);
    }

    static JSONObject challenge(String deviceId) throws Exception {
        return new JSONObject().put("device_id", deviceId);
    }

    static JSONObject verify(String challengeId, String signatureB64) throws Exception {
        return new JSONObject().put("challenge_id", challengeId).put("signature", signatureB64);
    }

    // --- capability (capability.schema.json) ---

    /** The Pixel 9 Pro baseline: extended radio, QoS and mobility fields are
     *  not available on it (out of MVP scope). */
    static JSONObject capability() throws Exception {
        JSONArray serving = new JSONArray();
        for (String f : new String[] {
                Contract.ServingCellField.PCI, Contract.ServingCellField.NCI, Contract.ServingCellField.TAC,
                Contract.ServingCellField.NRARFCN, Contract.ServingCellField.RSRP,
                Contract.ServingCellField.RSRQ, Contract.ServingCellField.SINR}) {
            serving.put(f);
        }
        return new JSONObject()
                .put("radio", new JSONObject()
                        .put("serving_cell", serving)
                        .put("neighbor_cells", true)
                        .put("lte_fallback", true)
                        .put("cqi", false)
                        .put("mcs", false)
                        .put("bler", false)
                        .put("harq", false)
                        .put("drx", false)
                        .put("bwp", false)
                        .put("mimo_layers", false)
                        .put("tx_power", false))
                .put("qos", new JSONObject().put("5qi", false).put("qfi", false).put("slice_info", false))
                .put("mobility", new JSONObject()
                        .put("explicit_ho_events", false)
                        .put("rrc_state", false)
                        .put("beam_switch", false))
                .put("timestamp_sources", new JSONArray()
                        .put(Contract.TimestampSource.MONO)
                        .put(Contract.TimestampSource.UTC)
                        .put(Contract.TimestampSource.GPS));
    }

    // --- status (device_status.schema.json) ---

    static JSONObject status(boolean runningTest, int loggingConfigVersion, long monoNs, long utcNs,
                             @Nullable JSONObject battery, @Nullable JSONObject storage, int pendingUploads,
                             @Nullable JSONObject radio, @Nullable String currentTestId) throws Exception {
        JSONObject s = new JSONObject()
                .put("logger_state", runningTest ? Contract.LoggerStates.RUNNING_TEST : Contract.LoggerStates.IDLE)
                .put("logging_config_version", loggingConfigVersion)
                .put("sampled_at", new JSONObject().put("mono_ns", monoNs).put("utc_ns", utcNs))
                .put("pending_uploads", pendingUploads);
        if (battery != null) {
            s.put("battery", battery);
        }
        if (storage != null) {
            s.put("storage", storage);
        }
        if (radio != null && radio.length() > 0) {
            s.put("radio", radio);
        }
        if (currentTestId != null) {
            s.put("current_test_id", currentTestId);
        }
        return s;
    }

    static JSONObject battery(int levelPct, boolean charging) throws Exception {
        return new JSONObject().put("level_pct", levelPct).put("charging", charging);
    }

    static JSONObject storage(long freeBytes, long totalBytes) throws Exception {
        return new JSONObject().put("free_bytes", freeBytes).put("total_bytes", totalBytes);
    }

    // --- header.json (header.schema.json) ---

    /** What ran: versions, identities, start time, the exact plan and port
     *  bindings from the RUN_TEST payload, and the logging config. */
    static JSONObject header(JSONObject payload, JSONObject loggingConfig, String deviceId, String appVersion,
                             String model, String manufacturer, String osVersion, long monoNs, long utcNs)
            throws Exception {
        return new JSONObject()
                .put("schema_version", Contract.SCHEMA_VERSION)
                .put("protocol_version", Contract.PROTOCOL_VERSION)
                .put("app_version", appVersion)
                .put("session_id", payload.getString(Contract.Fields.RunTestPayload.SESSION_ID))
                .put("test_id", payload.getString(Contract.Fields.RunTestPayload.TEST_ID))
                .put("test_start_utc_ns", utcNs)
                .put("test_start_mono_ns", monoNs)
                .put("device", new JSONObject()
                        .put("device_id", deviceId)
                        .put("node_id", Contract.LogNodes.MOBILE)
                        .put("platform", Contract.Platforms.ANDROID)
                        .put("model", model)
                        .put("manufacturer", manufacturer)
                        .put("os_version", osVersion))
                .put("server", new JSONObject()
                        .put("node_id", Contract.LogNodes.SERVER)
                        .put("host", payload.getString(Contract.Fields.RunTestPayload.SERVER_HOST)))
                .put("test_plan", payload.getJSONObject(Contract.Fields.RunTestPayload.TEST_PLAN))
                .put("flow_bindings", payload.getJSONArray(Contract.Fields.RunTestPayload.FLOW_BINDINGS))
                .put("logging_config", loggingConfig);
    }

    // --- RustCore.startTest config (core/src/ue.rs UeTestConfig) ---

    /** The RUN_TEST payload as the core reads it: each plan flow passed whole,
     *  plus its server port from flow_bindings. Nothing is picked field by
     *  field, so a new flow field reaches the core without a change here
     *  (core/tests/test_plan_contract.rs checks the core reads them all). */
    static String ueConfig(JSONObject payload, String deviceId, String mobileDir) throws Exception {
        JSONObject ports = new JSONObject();
        JSONArray fb = payload.getJSONArray(Contract.Fields.RunTestPayload.FLOW_BINDINGS);
        for (int i = 0; i < fb.length(); i++) {
            JSONObject b = fb.getJSONObject(i);
            if (!b.isNull(Contract.Fields.FlowBinding.SERVER_PORT)) {
                ports.put(String.valueOf(b.getInt(Contract.Fields.FlowBinding.FLOW_ID)), b.getInt(Contract.Fields.FlowBinding.SERVER_PORT));
            }
        }
        JSONArray planFlows = payload.getJSONObject(Contract.Fields.RunTestPayload.TEST_PLAN).getJSONArray(Contract.Fields.TestPlan.FLOWS);
        JSONArray flows = new JSONArray();
        for (int i = 0; i < planFlows.length(); i++) {
            JSONObject f = copy(planFlows.getJSONObject(i));
            String id = String.valueOf(f.getInt("flow_id"));
            if (ports.has(id)) {
                f.put("server_port", ports.getInt(id));
            }
            flows.put(f);
        }
        return new JSONObject()
                .put("test_id", payload.getString(Contract.Fields.RunTestPayload.TEST_ID))
                .put("device_id", deviceId)
                .put("session_id", payload.getString(Contract.Fields.RunTestPayload.SESSION_ID))
                .put("server_host", payload.getString(Contract.Fields.RunTestPayload.SERVER_HOST))
                .put("log_dir", mobileDir)
                .put("flows", flows)
                .toString();
    }

    private static JSONObject copy(JSONObject o) throws Exception {
        JSONObject out = new JSONObject();
        for (Iterator<String> it = o.keys(); it.hasNext(); ) {
            String k = it.next();
            out.put(k, o.get(k));
        }
        return out;
    }

    // --- upload (upload_manifest.schema.json) ---

    static JSONObject manifestFile(String path, long sizeBytes, String sha256Hex) throws Exception {
        return new JSONObject()
                .put("path", path)
                .put("size_bytes", sizeBytes)
                .put("sha256", sha256Hex)
                .put("encoding", Contract.FileEncoding.IDENTITY);
    }

    /** PARTIAL if any flow's log ring dropped rows (the core's stop summary,
     *  contracts/schemas/core_stop_summary.schema.json), else COMPLETED. */
    static String testResult(String coreStopSummary) {
        try {
            JSONArray flows = new JSONArray(coreStopSummary);
            for (int i = 0; i < flows.length(); i++) {
                if (flows.getJSONObject(i).optLong(Contract.Fields.CoreStopSummary.DROPPED_ROWS, 0) > 0) {
                    return Contract.TestResults.PARTIAL;
                }
            }
        } catch (Exception e) {
            // unreadable: the logs on disk still tell (events.parquet)
        }
        return Contract.TestResults.COMPLETED;
    }

    static JSONObject manifest(JSONArray files, String testResult) throws Exception {
        return new JSONObject()
                .put("test_result", testResult)
                .put("node", Contract.UploadNode.MOBILE)
                .put("files", files);
    }

    // --- reachability (reachability_report.schema.json) ---

    static JSONObject reachReport(long monoNs, long utcNs, String trigger, JSONObject network, JSONArray checks)
            throws Exception {
        return new JSONObject()
                .put("checked_at", new JSONObject().put("mono_ns", monoNs).put("utc_ns", utcNs))
                .put("trigger", trigger)
                .put("network", network)
                .put("checks", checks);
    }

    static JSONObject reachCheck(String protocol, int port, String kind, boolean ok) throws Exception {
        return new JSONObject()
                .put("protocol", protocol)
                .put("port", port)
                .put("kind", kind)
                .put("ok", ok);
    }

    /** wifiFreqMhz <= 0 and null values are left out. */
    static JSONObject network(String transport, @Nullable Boolean validated, @Nullable String wifiSsid,
                              int wifiFreqMhz, @Nullable String operator) throws Exception {
        JSONObject n = new JSONObject().put("transport", transport);
        if (validated != null) {
            n.put("validated", validated);
        }
        if (wifiSsid != null) {
            n.put("wifi_ssid", wifiSsid);
        }
        if (wifiFreqMhz > 0) {
            n.put("wifi_freq_mhz", wifiFreqMhz);
        }
        if (operator != null) {
            n.put("operator", operator);
        }
        return n;
    }
}
