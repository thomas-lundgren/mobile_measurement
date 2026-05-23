//! Device auth primitive — challenge/response signing & verification.
//!
//! Shared by the server (verify) and the client/device (sign). Pins the
//! cross-platform scheme: `ECDSA_P256_SHA256`, SPKI-DER public keys (base64),
//! ASN.1-DER signatures (base64). See `contracts/` auth model and
//! `contracts/testvectors/auth_signing/`.

use base64::Engine as _;
use p256::ecdsa::signature::{Signer, Verifier};
use p256::ecdsa::{Signature, SigningKey, VerifyingKey};
use spki::{DecodePublicKey, EncodePublicKey};

/// Domain separator prepended to every signed challenge blob.
pub const AUTH_DOMAIN: &str = "mp-auth-v1";

/// Field separator in the canonical signed message. 0x1F (ASCII Unit
/// Separator) cannot appear in the field values (ULIDs / base64), so the
/// concatenation is unambiguous. This is the precise byte realization of the
/// `domain || server_id || challenge_id || nonce` blob in the contract.
const SEP: u8 = 0x1F;

fn b64() -> base64::engine::general_purpose::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

/// Build the canonical message bytes that get signed/verified:
/// `domain 0x1F server_id 0x1F challenge_id 0x1F nonce` (all UTF-8).
pub fn build_message(domain: &str, server_id: &str, challenge_id: &str, nonce: &str) -> Vec<u8> {
    let mut m = Vec::new();
    for (i, part) in [domain, server_id, challenge_id, nonce].iter().enumerate() {
        if i > 0 {
            m.push(SEP);
        }
        m.extend_from_slice(part.as_bytes());
    }
    m
}

/// Verify a base64 ASN.1-DER signature over `message` against a base64
/// SPKI-DER P-256 public key. Returns false on any decode/verify failure
/// (never panics).
pub fn verify(pubkey_spki_b64: &str, message: &[u8], sig_der_b64: &str) -> bool {
    let Ok(spki) = b64().decode(pubkey_spki_b64) else {
        return false;
    };
    let Ok(vk) = VerifyingKey::from_public_key_der(&spki) else {
        return false;
    };
    let Ok(sig_der) = b64().decode(sig_der_b64) else {
        return false;
    };
    let Ok(sig) = Signature::from_der(&sig_der) else {
        return false;
    };
    vk.verify(message, &sig).is_ok()
}

/// Convenience: build the standard challenge blob and verify it.
pub fn verify_challenge(
    pubkey_spki_b64: &str,
    server_id: &str,
    challenge_id: &str,
    nonce: &str,
    sig_der_b64: &str,
) -> bool {
    let msg = build_message(AUTH_DOMAIN, server_id, challenge_id, nonce);
    verify(pubkey_spki_b64, &msg, sig_der_b64)
}

/// Sign `message`, returning a base64 ASN.1-DER signature. Deterministic
/// (RFC 6979), so the same key + message always yields the same signature.
pub fn sign_der_b64(signing_key: &SigningKey, message: &[u8]) -> String {
    let sig: Signature = signing_key.sign(message);
    b64().encode(sig.to_der().as_bytes())
}

/// SPKI-DER (base64) public key for a signing key — what a device registers.
pub fn public_key_spki_b64(signing_key: &SigningKey) -> String {
    let vk = VerifyingKey::from(signing_key);
    let der = vk.to_public_key_der().expect("SPKI DER encode");
    b64().encode(der.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::pkcs8::DecodePrivateKey;
    use std::path::PathBuf;

    fn vector_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../contracts/testvectors/auth_signing/vector.json")
    }

    #[test]
    fn golden_auth_signing() {
        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(vector_path()).unwrap()).unwrap();

        let priv_pem = doc["priv_key_pem"].as_str().unwrap();
        let pub_b64 = doc["pub_key_spki_b64"].as_str().unwrap();
        let message = {
            let h = doc["message_hex"].as_str().unwrap();
            (0..h.len()).step_by(2).map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap()).collect::<Vec<u8>>()
        };
        let sig_b64 = doc["signature_der_b64"].as_str().unwrap();

        let sk = SigningKey::from_pkcs8_pem(priv_pem).unwrap();

        // public key derived from the private key matches the committed SPKI
        assert_eq!(public_key_spki_b64(&sk), pub_b64, "pub key mismatch");
        // deterministic signing reproduces the committed signature
        assert_eq!(sign_der_b64(&sk, &message), sig_b64, "signature mismatch");
        // verification succeeds, and fails on a tampered message
        assert!(verify(pub_b64, &message, sig_b64), "verify should pass");
        let mut bad = message.clone();
        bad[0] ^= 0x01;
        assert!(!verify(pub_b64, &bad, sig_b64), "verify must fail on tamper");
    }

    #[test]
    fn challenge_blob_roundtrip() {
        // a freshly generated key signs and verifies a challenge end-to-end
        let sk = SigningKey::from_pkcs8_pem(
            &serde_json::from_str::<serde_json::Value>(
                &std::fs::read_to_string(vector_path()).unwrap(),
            )
            .unwrap()["priv_key_pem"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        let pub_b64 = public_key_spki_b64(&sk);
        let (server_id, challenge_id, nonce) =
            ("srv_main_01", "chl_01HZ8R9F0J2YK3P5Q7S9V1W3X5", "Ymxvb2Jhc2U2NG5vbmNlMzJieXRlc3h4eHh4eHg=");
        let msg = build_message(AUTH_DOMAIN, server_id, challenge_id, nonce);
        let sig = sign_der_b64(&sk, &msg);
        assert!(verify_challenge(&pub_b64, server_id, challenge_id, nonce, &sig));
    }
}
