//! Device registry — CRUD over the `devices`, `enrollment_tokens`, and
//! `device_status` tables. Runtime queries (no compile-time `query!` checking
//! yet; can be enabled later via `cargo sqlx prepare`).

use sqlx::SqlitePool;

/// A registered device (subset of the `devices` columns the control plane reads).
#[derive(Debug, Clone, sqlx::FromRow, PartialEq)]
pub struct Device {
    pub device_id: String,
    pub node_id: String,
    pub device_label: Option<String>,
    pub pubkey_spki: String,
    pub platform: String,
    pub model: Option<String>,
    pub logging_config_version: i64,
    pub status: String,
    pub enrolled_at: i64,
}

/// Fields needed to enroll a device.
pub struct NewDevice {
    pub device_id: String,
    pub node_id: String,
    pub device_label: Option<String>,
    pub pubkey_spki: String,
    pub platform: String,
    pub model: Option<String>,
    pub manufacturer: Option<String>,
    pub os_version: Option<String>,
    pub capability_json: Option<String>,
    pub enrolled_at: i64,
}

/// Insert a freshly enrolled device.
pub async fn insert_device(pool: &SqlitePool, d: &NewDevice) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO devices \
         (device_id, node_id, device_label, pubkey_spki, platform, model, manufacturer, \
          os_version, capability_json, logging_config_version, status, enrolled_at, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'active', ?, ?)",
    )
    .bind(&d.device_id)
    .bind(&d.node_id)
    .bind(&d.device_label)
    .bind(&d.pubkey_spki)
    .bind(&d.platform)
    .bind(&d.model)
    .bind(&d.manufacturer)
    .bind(&d.os_version)
    .bind(&d.capability_json)
    .bind(mp_contracts::LOGGING_CONFIG_VERSION_INITIAL as i64)
    .bind(d.enrolled_at)
    .bind(d.enrolled_at)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_device(pool: &SqlitePool, device_id: &str) -> Result<Option<Device>, sqlx::Error> {
    sqlx::query_as::<_, Device>(
        "SELECT device_id, node_id, device_label, pubkey_spki, platform, model, \
                logging_config_version, status, enrolled_at \
         FROM devices WHERE device_id = ?",
    )
    .bind(device_id)
    .fetch_optional(pool)
    .await
}

pub async fn list_devices(pool: &SqlitePool) -> Result<Vec<Device>, sqlx::Error> {
    sqlx::query_as::<_, Device>(
        "SELECT device_id, node_id, device_label, pubkey_spki, platform, model, \
                logging_config_version, status, enrolled_at \
         FROM devices ORDER BY created_at",
    )
    .fetch_all(pool)
    .await
}

/// Mint a one-time enrollment token (store only its hash).
pub async fn mint_enrollment_token(
    pool: &SqlitePool,
    token_hash: &str,
    created_at: i64,
    expires_at: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO enrollment_tokens(token_hash, created_at, expires_at) VALUES (?, ?, ?)",
    )
    .bind(token_hash)
    .bind(created_at)
    .bind(expires_at)
    .execute(pool)
    .await?;
    Ok(())
}

/// Atomically consume an enrollment token: succeeds only if it exists, is
/// unconsumed, and is unexpired. Returns true if it was consumed by this call.
pub async fn consume_enrollment_token(
    pool: &SqlitePool,
    token_hash: &str,
    now: i64,
    device_id: &str,
) -> Result<bool, sqlx::Error> {
    let res = sqlx::query(
        "UPDATE enrollment_tokens SET consumed_at = ?, consumed_by_device_id = ? \
         WHERE token_hash = ? AND consumed_at IS NULL AND expires_at > ?",
    )
    .bind(now)
    .bind(device_id)
    .bind(token_hash)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(res.rows_affected() == 1)
}

/// Latest device status heartbeat (upsert one row per device).
pub struct StatusUpdate {
    pub device_id: String,
    pub logger_state: String,
    pub current_test_id: Option<String>,
    pub logging_config_version: i64,
    pub battery_level_pct: Option<i64>,
    pub battery_charging: Option<bool>,
    pub storage_free_bytes: Option<i64>,
    pub pending_uploads: Option<i64>,
    pub radio_json: Option<String>,
    pub sampled_mono_ns: i64,
    pub sampled_utc_ns: i64,
    pub received_at: i64,
}

pub async fn upsert_device_status(pool: &SqlitePool, s: &StatusUpdate) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO device_status \
         (device_id, logger_state, current_test_id, logging_config_version, battery_level_pct, \
          battery_charging, storage_free_bytes, storage_total_bytes, pending_uploads, radio_json, \
          sampled_mono_ns, sampled_utc_ns, received_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, NULL, ?, ?, ?, ?, ?) \
         ON CONFLICT(device_id) DO UPDATE SET \
           logger_state=excluded.logger_state, current_test_id=excluded.current_test_id, \
           logging_config_version=excluded.logging_config_version, \
           battery_level_pct=excluded.battery_level_pct, battery_charging=excluded.battery_charging, \
           storage_free_bytes=excluded.storage_free_bytes, pending_uploads=excluded.pending_uploads, \
           radio_json=excluded.radio_json, sampled_mono_ns=excluded.sampled_mono_ns, \
           sampled_utc_ns=excluded.sampled_utc_ns, received_at=excluded.received_at",
    )
    .bind(&s.device_id)
    .bind(&s.logger_state)
    .bind(&s.current_test_id)
    .bind(s.logging_config_version)
    .bind(s.battery_level_pct)
    .bind(s.battery_charging)
    .bind(s.storage_free_bytes)
    .bind(s.pending_uploads)
    .bind(&s.radio_json)
    .bind(s.sampled_mono_ns)
    .bind(s.sampled_utc_ns)
    .bind(s.received_at)
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn pool() -> SqlitePool {
        let dir = Box::leak(Box::new(tempfile::tempdir().unwrap()));
        let url = format!("sqlite://{}/mp.db", dir.path().display());
        crate::db::connect(&url).await.unwrap()
    }

    fn sample_device(id: &str) -> NewDevice {
        NewDevice {
            device_id: id.into(),
            node_id: "node_mobile_01".into(),
            device_label: Some("Test Pixel".into()),
            pubkey_spki: "BASE64SPKI".into(),
            platform: "android".into(),
            model: Some("Pixel 9 Pro".into()),
            manufacturer: Some("Google".into()),
            os_version: Some("15".into()),
            capability_json: Some("{}".into()),
            enrolled_at: 1_000,
        }
    }

    #[tokio::test]
    async fn insert_get_list_device() {
        let pool = pool().await;
        insert_device(&pool, &sample_device("dev_a")).await.unwrap();
        insert_device(&pool, &sample_device("dev_b")).await.unwrap();

        let d = get_device(&pool, "dev_a").await.unwrap().unwrap();
        assert_eq!(d.platform, "android");
        assert_eq!(
            d.logging_config_version,
            mp_contracts::LOGGING_CONFIG_VERSION_INITIAL as i64
        );
        assert_eq!(d.status, "active");

        assert!(get_device(&pool, "dev_missing").await.unwrap().is_none());
        assert_eq!(list_devices(&pool).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn bad_platform_rejected_by_check() {
        let pool = pool().await;
        let mut d = sample_device("dev_x");
        d.platform = "windows".into();
        assert!(
            insert_device(&pool, &d).await.is_err(),
            "CHECK should reject platform"
        );
    }

    #[tokio::test]
    async fn enrollment_token_single_use() {
        let pool = pool().await;
        // The enroll flow inserts the device before consuming the token
        // (consumed_by_device_id FKs devices), so create it first.
        insert_device(&pool, &sample_device("dev_a")).await.unwrap();
        mint_enrollment_token(&pool, "hash1", 0, 10_000)
            .await
            .unwrap();

        assert!(
            consume_enrollment_token(&pool, "hash1", 100, "dev_a")
                .await
                .unwrap(),
            "first use ok"
        );
        assert!(
            !consume_enrollment_token(&pool, "hash1", 200, "dev_a")
                .await
                .unwrap(),
            "second use rejected"
        );
        assert!(
            !consume_enrollment_token(&pool, "missing", 100, "dev_a")
                .await
                .unwrap(),
            "unknown rejected"
        );

        mint_enrollment_token(&pool, "expired", 0, 50)
            .await
            .unwrap();
        assert!(
            !consume_enrollment_token(&pool, "expired", 100, "dev_a")
                .await
                .unwrap(),
            "expired rejected"
        );
    }

    #[tokio::test]
    async fn device_status_upserts() {
        let pool = pool().await;
        insert_device(&pool, &sample_device("dev_a")).await.unwrap();
        let mut s = StatusUpdate {
            device_id: "dev_a".into(),
            logger_state: "IDLE".into(),
            current_test_id: None,
            logging_config_version: 1,
            battery_level_pct: Some(84),
            battery_charging: Some(true),
            storage_free_bytes: Some(40_000_000_000),
            pending_uploads: Some(0),
            radio_json: None,
            sampled_mono_ns: 1,
            sampled_utc_ns: 2,
            received_at: 3,
        };
        upsert_device_status(&pool, &s).await.unwrap();
        s.logger_state = "RUNNING_TEST".into();
        s.battery_level_pct = Some(79);
        upsert_device_status(&pool, &s).await.unwrap();

        let (state, batt): (String, i64) = sqlx::query_as(
            "SELECT logger_state, battery_level_pct FROM device_status WHERE device_id='dev_a'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(state, "RUNNING_TEST");
        assert_eq!(batt, 79);
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM device_status")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(n, 1, "upsert keeps one row per device");
    }
}
