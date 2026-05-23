//! Device auth (enroll / challenge / verify) + operator login (HS256 JWT) +
//! session/JWT validation helpers. See `planning/auth_openapi_sketch.yaml` and
//! `development_plan.md` §7.

use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::extract::State;
use axum::http::header::AUTHORIZATION;
use axum::http::HeaderMap;
use axum::routing::post;
use axum::{Json, Router};
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use mp_contracts::*;
use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::state::AppState;
use crate::util::{hash_token, new_id, now_utc_ns, random_b64};

const CHALLENGE_TTL_NS: i64 = 60 * 1_000_000_000; // 60s
const SESSION_TTL_NS: i64 = 8 * 3600 * 1_000_000_000; // 8h
const JWT_TTL_SECS: u64 = 3600; // 1h

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/devices/enroll", post(enroll))
        .route("/api/v1/auth/challenge", post(challenge))
        .route("/api/v1/auth/verify", post(verify))
        .route("/api/v1/auth/login", post(login))
}

fn bearer(headers: &HeaderMap) -> Option<String> {
    headers
        .get(AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::to_string)
}

// --- device: enroll --------------------------------------------------------

#[utoipa::path(
    post, path = "/api/v1/devices/enroll", tag = "device-auth",
    request_body = EnrollRequest,
    responses((status = 200, body = EnrollResponse), (status = 401), (status = 400))
)]
pub async fn enroll(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<EnrollRequest>,
) -> Result<Json<EnrollResponse>, AppError> {
    // Pin the cross-platform scheme.
    if req.pubkey_format != AUTH_PUBKEY_FORMAT
        || req.sig_alg != AUTH_SIGNING_ALG
        || req.sig_format != AUTH_SIG_FORMAT
    {
        return Err(AppError::BadRequest("unsupported auth scheme".into()));
    }
    if req.protocol_version != PROTOCOL_VERSION {
        return Err(AppError::BadRequest("protocol_version mismatch".into()));
    }
    if !fieldtest_core::auth::pubkey_is_valid(&req.device_pubkey) {
        return Err(AppError::BadRequest("invalid device_pubkey".into()));
    }

    let token = bearer(&headers).ok_or(AppError::Unauthorized)?;
    let token_hash = hash_token(&token);
    let now = now_utc_ns();
    let device_id = new_id("dev");
    let node_id = new_id("node");

    // Atomic: create the device, then consume the token (FK requires the
    // device to exist first). A bad/used/expired token rolls the device back.
    let mut tx = st.pool.begin().await?;
    sqlx::query(
        "INSERT INTO devices \
         (device_id, node_id, device_label, pubkey_spki, pubkey_format, sig_alg, platform, \
          model, manufacturer, os_version, capability_json, logging_config_version, status, \
          enrolled_at, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, ?, 'active', ?, ?)",
    )
    .bind(&device_id)
    .bind(&node_id)
    .bind(&req.hw_info.device_label)
    .bind(&req.device_pubkey)
    .bind(&req.pubkey_format)
    .bind(&req.sig_alg)
    .bind(req.platform.wire())
    .bind(&req.hw_info.model)
    .bind(&req.hw_info.manufacturer)
    .bind(&req.hw_info.os_version)
    .bind(LOGGING_CONFIG_VERSION_INITIAL as i64)
    .bind(now)
    .bind(now)
    .execute(&mut *tx)
    .await?;

    let consumed = sqlx::query(
        "UPDATE enrollment_tokens SET consumed_at = ?, consumed_by_device_id = ? \
         WHERE token_hash = ? AND consumed_at IS NULL AND expires_at > ?",
    )
    .bind(now)
    .bind(&device_id)
    .bind(&token_hash)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    if consumed.rows_affected() != 1 {
        tx.rollback().await?;
        return Err(AppError::Unauthorized);
    }
    tx.commit().await?;

    Ok(Json(EnrollResponse {
        device_id,
        node_id,
        server_time_utc_ns: now,
        schema_version: SCHEMA_VERSION,
        logging_config_version: LOGGING_CONFIG_VERSION_INITIAL,
    }))
}

// --- device: challenge -----------------------------------------------------

#[utoipa::path(
    post, path = "/api/v1/auth/challenge", tag = "device-auth",
    request_body = ChallengeRequest,
    responses((status = 200, body = ChallengeResponse), (status = 404))
)]
pub async fn challenge(
    State(st): State<AppState>,
    Json(req): Json<ChallengeRequest>,
) -> Result<Json<ChallengeResponse>, AppError> {
    let exists: Option<(String,)> =
        sqlx::query_as("SELECT device_id FROM devices WHERE device_id = ? AND status = 'active'")
            .bind(&req.device_id)
            .fetch_optional(&st.pool)
            .await?;
    if exists.is_none() {
        return Err(AppError::NotFound);
    }

    let challenge_id = new_id("chl");
    let nonce = random_b64(32);
    let now = now_utc_ns();
    let expires = now + CHALLENGE_TTL_NS;

    sqlx::query(
        "INSERT INTO auth_challenges(challenge_id, device_id, nonce, server_id, created_at, expires_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&challenge_id)
    .bind(&req.device_id)
    .bind(&nonce)
    .bind(&st.server_id)
    .bind(now)
    .bind(expires)
    .execute(&st.pool)
    .await?;

    Ok(Json(ChallengeResponse {
        challenge_id,
        nonce,
        server_id: st.server_id.clone(),
        expires_at_utc_ns: expires,
        server_time_utc_ns: now,
        auth_domain: AUTH_DOMAIN.to_string(),
    }))
}

// --- device: verify --------------------------------------------------------

#[utoipa::path(
    post, path = "/api/v1/auth/verify", tag = "device-auth",
    request_body = VerifyRequest,
    responses((status = 200, body = VerifyResponse), (status = 401))
)]
pub async fn verify(
    State(st): State<AppState>,
    Json(req): Json<VerifyRequest>,
) -> Result<Json<VerifyResponse>, AppError> {
    let now = now_utc_ns();

    let row: Option<(String, String, String, i64, Option<i64>)> = sqlx::query_as(
        "SELECT c.device_id, c.nonce, d.pubkey_spki, c.expires_at, c.consumed_at \
         FROM auth_challenges c JOIN devices d ON d.device_id = c.device_id \
         WHERE c.challenge_id = ?",
    )
    .bind(&req.challenge_id)
    .fetch_optional(&st.pool)
    .await?;

    let Some((device_id, nonce, pubkey, expires_at, consumed_at)) = row else {
        return Err(AppError::Unauthorized);
    };
    if consumed_at.is_some() || expires_at <= now {
        return Err(AppError::Unauthorized);
    }
    if !fieldtest_core::auth::verify_challenge(
        &pubkey,
        &st.server_id,
        &req.challenge_id,
        &nonce,
        &req.signature,
    ) {
        return Err(AppError::Unauthorized);
    }

    // Single-use: consume the challenge atomically; lose a race → reject.
    let consumed = sqlx::query(
        "UPDATE auth_challenges SET consumed_at = ? WHERE challenge_id = ? AND consumed_at IS NULL",
    )
    .bind(now)
    .bind(&req.challenge_id)
    .execute(&st.pool)
    .await?;
    if consumed.rows_affected() != 1 {
        return Err(AppError::Unauthorized);
    }

    let token = random_b64(32);
    let expires = now + SESSION_TTL_NS;
    sqlx::query("INSERT INTO device_sessions(session_token_hash, device_id, issued_at, expires_at) VALUES (?, ?, ?, ?)")
        .bind(hash_token(&token))
        .bind(&device_id)
        .bind(now)
        .bind(expires)
        .execute(&st.pool)
        .await?;

    let lcv: i64 =
        sqlx::query_scalar("SELECT logging_config_version FROM devices WHERE device_id = ?")
            .bind(&device_id)
            .fetch_one(&st.pool)
            .await?;

    Ok(Json(VerifyResponse {
        session_token: token,
        expires_at_utc_ns: expires,
        logging_config_version: lcv as u32,
    }))
}

// --- operator: login -------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub role: String,
    pub exp: usize,
}

#[utoipa::path(
    post, path = "/api/v1/auth/login", tag = "operator-auth",
    request_body = LoginRequest,
    responses((status = 200, body = LoginResponse), (status = 401))
)]
pub async fn login(
    State(st): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> Result<Json<LoginResponse>, AppError> {
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT password_hash, role FROM operators WHERE username = ?")
            .bind(&req.username)
            .fetch_optional(&st.pool)
            .await?;
    let Some((password_hash, role)) = row else {
        return Err(AppError::Unauthorized);
    };
    let parsed = PasswordHash::new(&password_hash).map_err(|_| AppError::Internal)?;
    if Argon2::default()
        .verify_password(req.password.as_bytes(), &parsed)
        .is_err()
    {
        return Err(AppError::Unauthorized);
    }

    let exp_secs = (now_utc_ns() / 1_000_000_000) as u64 + JWT_TTL_SECS;
    let claims = Claims {
        sub: req.username.clone(),
        role: role.clone(),
        exp: exp_secs as usize,
    };
    let jwt = encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(st.jwt_secret.as_bytes()),
    )
    .map_err(|_| AppError::Internal)?;
    let role_enum: OperatorRole =
        serde_json::from_value(serde_json::Value::String(role)).map_err(|_| AppError::Internal)?;

    Ok(Json(LoginResponse {
        jwt,
        expires_at_utc_ns: exp_secs as i64 * 1_000_000_000,
        role: role_enum,
    }))
}

// --- validation helpers (used by guarded routes in 1e, and tests) ----------

/// Validate a device session bearer token → device_id. (Becomes a proper
/// extractor/layer in 1e; a helper keeps 1d simple.)
pub async fn authed_device(st: &AppState, headers: &HeaderMap) -> Result<String, AppError> {
    let token = bearer(headers).ok_or(AppError::Unauthorized)?;
    let now = now_utc_ns();
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT device_id FROM device_sessions \
         WHERE session_token_hash = ? AND revoked_at IS NULL AND expires_at > ?",
    )
    .bind(hash_token(&token))
    .bind(now)
    .fetch_optional(&st.pool)
    .await?;
    row.map(|(d,)| d).ok_or(AppError::Unauthorized)
}

/// Validate an operator JWT → claims.
pub fn authed_operator(st: &AppState, headers: &HeaderMap) -> Result<Claims, AppError> {
    let token = bearer(headers).ok_or(AppError::Unauthorized)?;
    let data = decode::<Claims>(
        &token,
        &DecodingKey::from_secret(st.jwt_secret.as_bytes()),
        &Validation::new(Algorithm::HS256),
    )
    .map_err(|_| AppError::Unauthorized)?;
    Ok(data.claims)
}

/// Create an operator (argon2 password hash). Used by admin tooling + tests.
pub async fn create_operator(
    pool: &sqlx::SqlitePool,
    username: &str,
    password: &str,
    role: OperatorRole,
) -> Result<(), AppError> {
    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|_| AppError::Internal)?
        .to_string();
    sqlx::query("INSERT INTO operators(id, username, password_hash, role, created_at) VALUES (?, ?, ?, ?, ?)")
        .bind(new_id("opr"))
        .bind(username)
        .bind(hash)
        .bind(role.wire())
        .bind(now_utc_ns())
        .execute(pool)
        .await?;
    Ok(())
}
