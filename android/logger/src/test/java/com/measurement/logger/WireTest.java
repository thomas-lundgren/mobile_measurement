package com.measurement.logger;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertNotNull;
import static org.junit.Assert.assertTrue;

import org.json.JSONArray;
import org.json.JSONObject;
import org.junit.Test;

import java.util.Iterator;

/** Every body the Logger sends, against its contract. The schemas reject
 *  unknown keys, so a misspelt or invented field fails here. */
public class WireTest {
    private static final long MONO = 81_115_870_780_858L;
    private static final long UTC = 1_792_556_320_539_545_262L;

    private static JSONArray runTestExamples() throws Exception {
        return Contracts.json("testvectors/commands/run_test_examples.json").getJSONArray("examples");
    }

    private static JSONObject loggingConfig() throws Exception {
        return Contracts.json("testvectors/logging_config_examples.json")
                .getJSONArray("examples").getJSONObject(0).getJSONObject("config");
    }

    @Test
    public void headerForEveryRunTestExample() throws Exception {
        JSONArray examples = runTestExamples();
        for (int i = 0; i < examples.length(); i++) {
            JSONObject payload = examples.getJSONObject(i).getJSONObject("payload");
            JSONObject h = Wire.header(payload, loggingConfig(), "dev_01M588ENHCVDGS8DZ9B5JCN7EP", "0.1.0",
                    "Pixel 9 Pro", "Google", "17", MONO, UTC);
            Contracts.assertValid("header.schema.json", h);
            // the plan and bindings are what ran, verbatim
            assertTrue(Contracts.same(h.get("test_plan"), payload.get("test_plan")));
            assertTrue(Contracts.same(h.get("flow_bindings"), payload.get("flow_bindings")));
            assertEquals(Contract.SCHEMA_VERSION, h.getInt("schema_version"));
        }
    }

    @Test
    public void statusFullAndMinimal() throws Exception {
        JSONObject radio = new JSONObject().put(Contract.RadioRow.PCI, 264).put(Contract.RadioRow.RSRP, -95);
        Contracts.assertValid("device_status.schema.json", Wire.status(true, 3, MONO, UTC,
                Wire.battery(80, true), Wire.storage(1L << 30, 1L << 37), 2, radio, "tst_01M5EAXEE6QKJ8REVRW133REPP"));
        JSONObject idle = Wire.status(false, 1, MONO, UTC, null, null, 0, null, null);
        Contracts.assertValid("device_status.schema.json", idle);
        assertEquals(Contract.LoggerStates.IDLE, idle.getString("logger_state"));
    }

    @Test
    public void capability() throws Exception {
        Contracts.assertValid("capability.schema.json", Wire.capability());
    }

    @Test
    public void uploadManifest() throws Exception {
        JSONArray files = new JSONArray()
                .put(Wire.manifestFile("header.json", 412, "a".repeat(64)))
                .put(Wire.manifestFile("mobile/packets_0000.parquet", 220_190, "b".repeat(64)));
        Contracts.assertValid("upload_manifest.schema.json", Wire.manifest(files, Contract.TestResults.COMPLETED));
        Contracts.assertValid("upload_manifest.schema.json", Wire.manifest(files, Contract.TestResults.PARTIAL));
    }

    /** A held upload (contracts/upload/README.md) is recognised from the
     *  server's answers; any other failure is not. */
    @Test
    public void heldUploadsAreRecognised() {
        assertTrue(Uploader.isHeld(new java.io.IOException("HTTP 503: upload held until the device is on the local network")));
        assertTrue(Uploader.isHeld(new java.io.IOException("complete -> 409")));
        assertFalse(Uploader.isHeld(new java.io.IOException("HTTP 404: unknown test")));
        assertFalse(Uploader.isHeld(new java.io.IOException("complete -> 500")));
        assertFalse(Uploader.isHeld(new java.io.IOException((String) null)));
    }

    /** A test cut off by process death: its complete files are queued, the
     *  open segment (no footer yet) and the markers are not. */
    @Test
    public void interruptedTestQueuesItsCompleteFiles() throws Exception {
        java.nio.file.Path root = java.nio.file.Files.createTempDirectory("tst_");
        java.io.File mobile = new java.io.File(root.toFile(), "mobile");
        mobile.mkdirs();
        byte[] parquet = "PAR1 rows footer PAR1".getBytes(java.nio.charset.StandardCharsets.US_ASCII);
        java.nio.file.Files.write(root.resolve("header.json"), "{}".getBytes());
        java.nio.file.Files.write(root.resolve("gps.parquet"), parquet);
        java.nio.file.Files.write(mobile.toPath().resolve("packets_0000.parquet"), parquet);
        java.nio.file.Files.write(mobile.toPath().resolve("packets_0001.parquet"), new byte[0]);
        java.nio.file.Files.write(mobile.toPath().resolve("packets_0002.parquet"), "PAR1 half-writ".getBytes());
        java.nio.file.Files.write(root.resolve("upload_pending.json.tmp"), "{}".getBytes());

        JSONArray got = Uploader.orphanFiles(root.toFile());
        java.util.List<String> names = new java.util.ArrayList<>();
        for (int i = 0; i < got.length(); i++) {
            names.add(root.relativize(java.nio.file.Paths.get(got.getString(i))).toString());
        }
        assertEquals(java.util.Arrays.asList("gps.parquet", "header.json", "mobile/packets_0000.parquet"), names);
    }

    /** contracts/testvectors/core/stop_summary_examples.json: a ring overflow
     *  makes the upload PARTIAL. */
    @Test
    public void testResultFromTheCoreStopSummary() throws Exception {
        JSONArray examples = Contracts.json("testvectors/core/stop_summary_examples.json").getJSONArray("examples");
        for (int i = 0; i < examples.length(); i++) {
            JSONObject ex = examples.getJSONObject(i);
            assertEquals(ex.getString("name"), ex.getString("test_result"),
                    Wire.testResult(ex.getJSONArray("summary").toString()));
        }
        assertEquals(Contract.TestResults.COMPLETED, Wire.testResult("not json"));
    }

    @Test
    public void reachabilityReports() throws Exception {
        JSONArray checks = new JSONArray()
                .put(Wire.reachCheck(Contract.ReachProtocol.TCP, 8080, Contract.ReachKind.HTTP, true).put("rtt_ms", 41.2))
                .put(Wire.reachCheck(Contract.ReachProtocol.UDP, 443, Contract.ReachKind.REACH, true)
                        .put("rtt_ms", 38.0).put("seen_as", "90.129.122.158:53412"))
                .put(Wire.reachCheck(Contract.ReachProtocol.TCP, 3478, Contract.ReachKind.REACH, false)
                        .put("error", "timeout"));
        JSONObject[] networks = {
                Wire.network(Contract.NetworkTransport.WIFI, true, "home", 5180, null),
                Wire.network(Contract.NetworkTransport.CELLULAR, true, null, 0, "Telia"),
                Wire.network(Contract.NetworkTransport.NONE, null, null, 0, null),
        };
        for (JSONObject n : networks) {
            Contracts.assertValid("reachability_report.schema.json",
                    Wire.reachReport(MONO, UTC, Contract.ReachTrigger.NETWORK_CHANGE, n, checks));
        }
    }

    @Test
    public void enrollmentAndAuthBodies() throws Exception {
        Contracts.assertValidOpenApi("EnrollRequest",
                Wire.enroll("MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAE", "Pixel 9 Pro", "Google", "17"));
        Contracts.assertValidOpenApi("ChallengeRequest", Wire.challenge("dev_01M588ENHCVDGS8DZ9B5JCN7EP"));
        Contracts.assertValidOpenApi("VerifyRequest", Wire.verify("chl_1", "MEQCIA=="));
    }

    /** Each flow reaches the core whole, plus its port (the core's side:
     *  core/tests/test_plan_contract.rs). */
    @Test
    public void ueConfigPassesEveryFlowFieldThrough() throws Exception {
        JSONArray examples = runTestExamples();
        for (int i = 0; i < examples.length(); i++) {
            JSONObject payload = examples.getJSONObject(i).getJSONObject("payload");
            JSONObject cfg = new JSONObject(Wire.ueConfig(payload, "dev_1", "/data/mobile"));
            JSONArray plan = payload.getJSONObject("test_plan").getJSONArray("flows");
            JSONArray flows = cfg.getJSONArray("flows");
            assertEquals(plan.length(), flows.length());
            for (int f = 0; f < plan.length(); f++) {
                JSONObject want = plan.getJSONObject(f);
                JSONObject got = flows.getJSONObject(f);
                for (Iterator<String> it = want.keys(); it.hasNext(); ) {
                    String k = it.next();
                    assertTrue(k + " in " + got, got.has(k) && Contracts.same(want.get(k), got.get(k)));
                }
                Object port = binding(payload, want.getInt("flow_id"));
                if (port == null) {
                    assertFalse(got.has("server_port"));
                } else {
                    assertEquals(port, got.get("server_port"));
                }
            }
            assertEquals(payload.getString("test_id"), cfg.getString("test_id"));
            assertNotNull(cfg.getString("log_dir"));
        }
    }

    private static Object binding(JSONObject payload, int flowId) throws Exception {
        JSONArray b = payload.getJSONArray("flow_bindings");
        for (int i = 0; i < b.length(); i++) {
            JSONObject x = b.getJSONObject(i);
            if (x.getInt("flow_id") == flowId) {
                return x.isNull("server_port") ? null : x.get("server_port");
            }
        }
        return null;
    }
}
