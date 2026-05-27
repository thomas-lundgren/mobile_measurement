//! Version + auth constants. Parity-tested against `contracts/versions/versions.yaml`.

/// Control-plane wire version; agreed at enrollment.
pub const PROTOCOL_VERSION: u32 = 1;
/// Log schema version (header.json, parquet, summary).
pub const SCHEMA_VERSION: u32 = 1;
/// Auth scheme version (signing alg / blob layout / domain).
pub const AUTH_SCHEME_VERSION: u32 = 1;
/// Initial server-issued logging_config_version.
pub const LOGGING_CONFIG_VERSION_INITIAL: u32 = 1;

/// Signature algorithm pinned for cross-platform device auth.
pub const AUTH_SIGNING_ALG: &str = "ECDSA_P256_SHA256";
/// Public-key encoding (base64 on the wire).
pub const AUTH_PUBKEY_FORMAT: &str = "SPKI_DER";
/// Signature encoding (base64 on the wire).
pub const AUTH_SIG_FORMAT: &str = "ASN1_DER";
/// Domain separator prepended to the signed challenge blob.
pub const AUTH_DOMAIN: &str = "mp-auth-v1";

/// Android package name of MP Logger — hard-excluded from Monitor's VPN tunnel.
pub const MP_LOGGER_PACKAGE: &str = "com.measurement.logger";
/// Android package name of MP Monitor — excluded from its own VPN tunnel.
pub const MP_MONITOR_PACKAGE: &str = "com.measurement.monitor";
