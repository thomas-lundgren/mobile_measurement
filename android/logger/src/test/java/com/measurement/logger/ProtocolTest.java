package com.measurement.logger;

import static org.junit.Assert.assertArrayEquals;
import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertNull;
import static org.junit.Assert.assertTrue;

import org.json.JSONArray;
import org.json.JSONObject;
import org.junit.Test;

import java.io.BufferedReader;
import java.io.ByteArrayOutputStream;
import java.io.File;
import java.io.InputStream;
import java.io.InputStreamReader;
import java.io.OutputStream;
import java.net.ServerSocket;
import java.net.Socket;
import java.nio.charset.StandardCharsets;
import java.security.KeyFactory;
import java.security.PublicKey;
import java.security.Signature;
import java.security.spec.X509EncodedKeySpec;
import java.util.Base64;
import java.util.concurrent.atomic.AtomicReference;
import java.util.zip.GZIPInputStream;

/** The Logger's side of the small protocols, against their golden vectors. */
public class ProtocolTest {

    @Test
    public void apiPathFillsParametersInOrder() {
        assertEquals("/api/v1/devices/dev_1/command/7/ack",
                Contract.Api.path(Contract.Api.ACK_COMMAND, "dev_1", 7));
        assertEquals("/api/v1/time", Contract.Api.path(Contract.Api.SERVER_TIME));
    }

    @Test(expected = IllegalArgumentException.class)
    public void apiPathRejectsMissingParameters() {
        Contract.Api.path(Contract.Api.PUT_FILE, "tst_1", "upl_1");
    }

    /** contracts/testvectors/packets/time_sync_v1.json */
    @Test
    public void timeSyncRepliesParse() throws Exception {
        JSONArray cases = Contracts.json("testvectors/packets/time_sync_v1.json").getJSONArray("cases");
        for (int i = 0; i < cases.length(); i++) {
            JSONObject c = cases.getJSONObject(i);
            byte[] nonce = new byte[8];
            byte[] req = Contracts.hex(c.getString("request_hex"));
            System.arraycopy(req, 0, nonce, 0, Math.min(8, req.length));
            byte[] reply = Contracts.hex(c.getString("reply_hex"));
            long[] t = ClockSampler.parseTimeReply(nonce, reply, reply.length);
            assertArrayEquals(c.getString("name"),
                    new long[] {c.getLong("t2_recv_utc_ns"), c.getLong("t3_send_utc_ns")}, t);
            // a reply for another nonce, or of another length, is not ours
            byte[] other = nonce.clone();
            other[0] ^= 1;
            assertNull(ClockSampler.parseTimeReply(other, reply, reply.length));
            assertNull(ClockSampler.parseTimeReply(nonce, reply, 23));
        }
    }

    /** contracts/testvectors/auth_signing/vector.json: the blob the Keystore
     *  signs, and the signature format the server verifies. */
    @Test
    public void challengeBlobAndSignatureFormat() throws Exception {
        JSONObject v = Contracts.json("testvectors/auth_signing/vector.json");
        byte[] blob = KeystoreSigner.blob(v.getString("server_id"), v.getString("challenge_id"), v.getString("nonce"));
        assertArrayEquals(Contracts.hex(v.getString("message_hex")), blob);
        assertEquals(v.getString("domain"), Contract.AUTH_DOMAIN);

        PublicKey pub = KeyFactory.getInstance("EC").generatePublic(
                new X509EncodedKeySpec(Base64.getDecoder().decode(v.getString("pub_key_spki_b64"))));
        Signature s = Signature.getInstance("SHA256withECDSA");
        s.initVerify(pub);
        s.update(blob);
        assertTrue(s.verify(Base64.getDecoder().decode(v.getString("signature_der_b64"))));
    }

    /** contracts/testvectors/mp_probe/: the request lines are the vectors',
     *  and a fetch stores the records it was sent, gzipped. */
    @Test
    public void mpProbeRequestsMatchTheVectors() throws Exception {
        JSONArray cases = Contracts.json("testvectors/mp_probe/exchanges.json").getJSONArray("cases");
        for (int i = 0; i < cases.length(); i++) {
            JSONObject c = cases.getJSONObject(i);
            String name = c.getString("name");
            if (name.equals("fast_test_window")) {
                assertEquals(c.getString("request"), MpProbe.fastLine(95));
            } else if (name.equals("fast_end")) {
                assertEquals(c.getString("request"), MpProbe.fastLine(0));
            } else if (name.equals("range")) {
                JSONArray r = c.getJSONArray("range");
                assertEquals(c.getString("request"), MpProbe.rangeLine(r.getLong(0), r.getLong(1)));
            }
        }
    }

    @Test
    public void mpProbeFetchStoresTheRecords() throws Exception {
        String records = Contracts.text("testvectors/mp_probe/records.jsonl");
        AtomicReference<String> request = new AtomicReference<>();
        try (ServerSocket server = new ServerSocket(0)) {
            Thread probe = new Thread(() -> {
                try (Socket s = server.accept()) {
                    request.set(new BufferedReader(new InputStreamReader(s.getInputStream(),
                            StandardCharsets.US_ASCII)).readLine() + "\n");
                    OutputStream out = s.getOutputStream();
                    out.write(records.getBytes(StandardCharsets.UTF_8));
                } catch (Exception ignored) {
                }
            });
            probe.start();
            File out = File.createTempFile("probe", ".jsonl.gz");
            File got = MpProbe.fetch(server.getLocalPort(), out, 1L, 2L);
            probe.join(5_000);
            assertEquals(MpProbe.rangeLine(1L, 2L), request.get());
            assertEquals(records, gunzip(got));
            out.delete();
        }
    }

    @Test
    public void mpProbeAbsentGivesNoFile() throws Exception {
        int port;
        try (ServerSocket s = new ServerSocket(0)) {
            port = s.getLocalPort();
        }
        File out = File.createTempFile("probe", ".jsonl.gz");
        assertNull(MpProbe.fetch(port, out, 1L, 2L));
        assertTrue(!out.exists());
        MpProbe.fast(port, 10); // must not throw
    }

    private static String gunzip(File f) throws Exception {
        try (InputStream in = new GZIPInputStream(new java.io.FileInputStream(f))) {
            ByteArrayOutputStream b = new ByteArrayOutputStream();
            in.transferTo(b);
            return b.toString(StandardCharsets.UTF_8);
        }
    }
}
