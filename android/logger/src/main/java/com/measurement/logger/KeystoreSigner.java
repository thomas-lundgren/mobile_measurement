package com.measurement.logger;

import android.security.keystore.KeyGenParameterSpec;
import android.security.keystore.KeyProperties;

import java.io.ByteArrayOutputStream;
import java.security.KeyPairGenerator;
import java.security.KeyStore;
import java.security.PrivateKey;
import java.security.PublicKey;
import java.security.Signature;
import java.security.spec.ECGenParameterSpec;
import java.util.Base64;

/**
 * Device auth credential: a non-exportable P-256 keypair in the Android
 * Keystore. Produces the SPKI public key + ECDSA-P256-SHA256 (DER) signatures
 * over the mp-auth-v1 challenge blob — matching contracts/testvectors/auth_signing
 * and the server's core::auth verifier.
 */
public final class KeystoreSigner {
    private static final String ALIAS = "mp_device_key";
    private static final String STORE = "AndroidKeyStore";
    private static final String AUTH_DOMAIN = "mp-auth-v1";
    private static final byte SEP = 0x1F;

    private KeystoreSigner() {}

    /** Create the device keypair if it doesn't exist yet (first launch). */
    public static void ensureKey() throws Exception {
        KeyStore ks = KeyStore.getInstance(STORE);
        ks.load(null);
        if (ks.containsAlias(ALIAS)) {
            return;
        }
        KeyPairGenerator kpg = KeyPairGenerator.getInstance(KeyProperties.KEY_ALGORITHM_EC, STORE);
        kpg.initialize(new KeyGenParameterSpec.Builder(ALIAS, KeyProperties.PURPOSE_SIGN)
                .setAlgorithmParameterSpec(new ECGenParameterSpec("secp256r1"))
                .setDigests(KeyProperties.DIGEST_SHA256)
                .build());
        kpg.generateKeyPair();
    }

    /** SPKI-DER public key, base64 — what /devices/enroll registers. */
    public static String publicKeySpkiB64() throws Exception {
        KeyStore ks = KeyStore.getInstance(STORE);
        ks.load(null);
        PublicKey pk = ks.getCertificate(ALIAS).getPublicKey();
        return Base64.getEncoder().encodeToString(pk.getEncoded());
    }

    /** Sign the canonical challenge blob; returns base64 ASN.1-DER signature. */
    public static String signChallengeB64(String serverId, String challengeId, String nonce) throws Exception {
        byte[] msg = blob(serverId, challengeId, nonce);
        KeyStore ks = KeyStore.getInstance(STORE);
        ks.load(null);
        PrivateKey sk = (PrivateKey) ks.getKey(ALIAS, null);
        Signature s = Signature.getInstance("SHA256withECDSA");
        s.initSign(sk);
        s.update(msg);
        return Base64.getEncoder().encodeToString(s.sign());
    }

    // "mp-auth-v1" 0x1F server_id 0x1F challenge_id 0x1F nonce  (UTF-8)
    private static byte[] blob(String serverId, String challengeId, String nonce) {
        ByteArrayOutputStream b = new ByteArrayOutputStream();
        String[] parts = {AUTH_DOMAIN, serverId, challengeId, nonce};
        for (int i = 0; i < parts.length; i++) {
            if (i > 0) {
                b.write(SEP);
            }
            byte[] p = parts[i].getBytes(java.nio.charset.StandardCharsets.UTF_8);
            b.write(p, 0, p.length);
        }
        return b.toByteArray();
    }
}
