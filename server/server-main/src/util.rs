//! Small shared helpers: ids, time, hashing, random tokens.

use base64::Engine as _;
use rand::RngCore;
use sha2::{Digest, Sha256};

/// Prefixed ULID id, e.g. `dev_01HZ...` (26 Crockford chars, matches the
/// contract id patterns).
pub fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", ulid::Ulid::new())
}

/// Current UTC time in nanoseconds.
pub fn now_utc_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as i64
}

/// Lowercase-hex SHA-256 of a string (used to store token hashes, never the
/// token itself).
pub fn hash_token(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// `n` random bytes, base64 (standard).
pub fn random_b64(n: usize) -> String {
    let mut buf = vec![0u8; n];
    rand::rngs::OsRng.fill_bytes(&mut buf);
    base64::engine::general_purpose::STANDARD.encode(buf)
}
