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

/// A delivered-but-unacked command is handed out again after this long, so a
/// poll response lost in a coverage gap can't silently drop a command
/// (at-least-once; the device de-duplicates by command/test id).
pub const REDELIVER_AFTER_NS: i64 = 30_000_000_000;

/// Atomically claim the oldest pending command for a device — or the oldest
/// delivered one not acked within [`REDELIVER_AFTER_NS`] — marking it
/// delivered. Returns (id, command_wire, payload_json) or None.
pub async fn next_pending(
    pool: &SqlitePool,
    device_id: &str,
) -> Result<Option<(i64, String, Option<String>)>, sqlx::Error> {
    let now = now_utc_ns();
    sqlx::query_as(
        "UPDATE commands SET status='delivered', delivered_at=? \
         WHERE id = (SELECT id FROM commands WHERE device_id=? \
                     AND (status='pending' OR (status='delivered' AND delivered_at < ?)) \
                     ORDER BY id LIMIT 1) \
         RETURNING id, command, payload_json",
    )
    .bind(now)
    .bind(device_id)
    .bind(now - REDELIVER_AFTER_NS)
    .fetch_optional(pool)
    .await
}

/// The test a RUN_TEST command belongs to, or None for any other command.
pub async fn run_test_id(
    pool: &SqlitePool,
    device_id: &str,
    command_id: i64,
) -> Result<Option<String>, sqlx::Error> {
    let row: Option<(String, Option<String>)> =
        sqlx::query_as("SELECT command, payload_json FROM commands WHERE id=? AND device_id=?")
            .bind(command_id)
            .bind(device_id)
            .fetch_optional(pool)
            .await?;
    Ok(row.and_then(|(cmd, payload)| {
        if cmd != Command::RunTest.wire() {
            return None;
        }
        let v: serde_json::Value = serde_json::from_str(&payload?).ok()?;
        v.get("test_id")?.as_str().map(str::to_string)
    }))
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
