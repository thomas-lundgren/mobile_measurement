//! Generates the cross-platform auth-signing golden vector.
//! `cargo run -p fieldtest-core --example gen_auth_vector > \
//!   contracts/testvectors/auth_signing/vector.json`
//!
//! Uses a FIXED secret scalar so the output is reproducible; ECDSA signing is
//! deterministic (RFC 6979), so the signature is stable too.

use fieldtest_core::auth::{build_message, public_key_spki_b64, sign_der_b64, AUTH_DOMAIN};
use p256::ecdsa::SigningKey;
use p256::pkcs8::{EncodePrivateKey, LineEnding};

fn main() {
    // Fixed 32-byte secret scalar (0x01..=0x20) → reproducible key.
    let secret: [u8; 32] = std::array::from_fn(|i| (i + 1) as u8);
    let sk = SigningKey::from_bytes(&secret.into()).expect("valid scalar");

    let priv_pem = sk.to_pkcs8_pem(LineEnding::LF).expect("pem").to_string();
    let pub_b64 = public_key_spki_b64(&sk);

    let server_id = "srv_main_01";
    let challenge_id = "chl_01HZ8R9F0J2YK3P5Q7S9V1W3X5";
    let nonce = "Ymxvb2Jhc2U2NG5vbmNlMzJieXRlc3h4eHh4eHg=";
    let msg = build_message(AUTH_DOMAIN, server_id, challenge_id, nonce);
    let sig_b64 = sign_der_b64(&sk, &msg);

    let message_hex: String = msg.iter().map(|b| format!("{b:02x}")).collect();

    let doc = serde_json::json!({
        "description": "Cross-platform auth-signing golden vector. message = mp-auth-v1 0x1F server_id 0x1F challenge_id 0x1F nonce (UTF-8). ECDSA P-256 SHA-256, deterministic (RFC 6979); SPKI-DER pubkey + ASN.1-DER signature, base64. Every implementation (Rust verifier, Android/iOS signer) MUST reproduce signature_der_b64 and verify it.",
        "alg": "ECDSA_P256_SHA256",
        "domain": AUTH_DOMAIN,
        "server_id": server_id,
        "challenge_id": challenge_id,
        "nonce": nonce,
        "message_hex": message_hex,
        "priv_key_pem": priv_pem,
        "pub_key_spki_b64": pub_b64,
        "signature_der_b64": sig_b64,
    });
    println!("{}", serde_json::to_string_pretty(&doc).unwrap());
}
