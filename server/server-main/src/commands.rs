//! Device command queue. Producers (test-control / operator actions, 1g/1e+)
//! enqueue; the device polls the oldest pending command and acks it.

use mp_contracts::Command;
use sqlx::SqlitePool;

use crate::util::now_utc_ns;

/// Enqueue a command for a device. Returns the new command id.
pub async fn enqueue(
    pool: &SqlitePool,
    device_id: &str,
    command: Command,
    payload: Option<&serde_json::Value>,
) -> Result<i64, sqlx::Error> {
    let payload_str = payload.map(|p| p.to_string());
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO commands(device_id, command, payload_json, status, created_at) \
         VALUES (?, ?, ?, 'pending', ?) RETURNING id",
    )
    .bind(device_id)
    .bind(command.wire())
    .bind(payload_str)
    .bind(now_utc_ns())
    .fetch_one(pool)
    .await?;
    Ok(id)
}

/// Atomically claim the oldest pending command for a device, marking it
/// delivered. Returns (id, command_wire, payload_json) or None.
pub async fn next_pending(
    pool: &SqlitePool,
    device_id: &str,
) -> Result<Option<(i64, String, Option<String>)>, sqlx::Error> {
    sqlx::query_as(
        "UPDATE commands SET status='delivered', delivered_at=? \
         WHERE id = (SELECT id FROM commands WHERE device_id=? AND status='pending' ORDER BY id LIMIT 1) \
         RETURNING id, command, payload_json",
    )
    .bind(now_utc_ns())
    .bind(device_id)
    .fetch_optional(pool)
    .await
}

/// Ack a delivered command. Returns true if it moved delivered → acked.
pub async fn ack(pool: &SqlitePool, device_id: &str, command_id: i64) -> Result<bool, sqlx::Error> {
    let res = sqlx::query(
        "UPDATE commands SET status='acked', acked_at=? \
         WHERE id=? AND device_id=? AND status='delivered'",
    )
    .bind(now_utc_ns())
    .bind(command_id)
    .bind(device_id)
    .execute(pool)
    .await?;
    Ok(res.rows_affected() == 1)
}
