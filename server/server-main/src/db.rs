//! Database pool + migrations (sqlx / SQLite).
//!
//! Control-plane metadata only (see `contracts/db/`). Foreign-key enforcement
//! and WAL are set per connection (SQLite's `PRAGMA foreign_keys` is
//! per-connection, and `journal_mode=WAL` cannot run inside a migration
//! transaction), so the migration file holds DDL only.

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::str::FromStr;

/// Embedded migrations from `server/server-main/migrations/`.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// Open (creating if missing) the SQLite pool with FK enforcement + WAL, then
/// apply migrations.
pub async fn connect(database_url: &str) -> Result<SqlitePool, sqlx::Error> {
    let opts = SqliteConnectOptions::from_str(database_url)?
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal);

    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .connect_with(opts)
        .await?;

    MIGRATOR.run(&pool).await?;
    Ok(pool)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn migrations_apply_and_fk_enforced() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}/mp.db", dir.path().display());
        let pool = connect(&url).await.unwrap();

        // all 12 contract tables exist
        let n: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name NOT LIKE '_sqlx%'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(n, 12, "expected 12 contract tables, got {n}");

        // FK enforcement is on: an orphan session is rejected
        let orphan = sqlx::query("INSERT INTO sessions(session_id, device_id, started_at) VALUES('ses_x','dev_missing',0)")
            .execute(&pool)
            .await;
        assert!(orphan.is_err(), "FK should reject orphan session");
    }

    #[test]
    fn migration_matches_contract_schema() {
        // Drift gate: the migration must equal contracts/db/schema.sql with the
        // two per-connection PRAGMAs removed (those are set in connect()).
        let migration = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/migrations/0001_init.sql"
        ))
        .unwrap();
        let contract = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../contracts/db/schema.sql"
        ))
        .unwrap();
        let stripped = contract
            .replace("PRAGMA foreign_keys = ON;\n", "")
            .replace("PRAGMA journal_mode = WAL;\n", "");
        assert_eq!(
            migration
                .replace('\n', " ")
                .split_whitespace()
                .collect::<Vec<_>>(),
            stripped
                .replace('\n', " ")
                .split_whitespace()
                .collect::<Vec<_>>(),
            "migration drifted from contracts/db/schema.sql (regen: strip the two PRAGMAs)"
        );
    }
}
