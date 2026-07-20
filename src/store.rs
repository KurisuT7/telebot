use std::path::Path;

use anyhow::{Context, Result};
use libsql::{Connection, params};
use tokio::sync::Mutex;

pub struct Store {
    connection: Mutex<Connection>,
}

impl Store {
    pub async fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let database = libsql::Builder::new_local(path)
            .build()
            .await
            .with_context(|| format!("failed to open state database {}", path.display()))?;
        let connection = database.connect()?;
        connection
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 PRAGMA synchronous = NORMAL;
                 PRAGMA busy_timeout = 5000;
                 CREATE TABLE IF NOT EXISTS ai_history (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     scope TEXT NOT NULL,
                     role TEXT NOT NULL CHECK(role IN ('user', 'assistant')),
                     content TEXT NOT NULL,
                     created_at INTEGER NOT NULL DEFAULT (unixepoch())
                 );
                 CREATE INDEX IF NOT EXISTS ai_history_scope_id ON ai_history(scope, id DESC);
                 CREATE TABLE IF NOT EXISTS settings (
                     key TEXT PRIMARY KEY,
                     value TEXT NOT NULL,
                     updated_at INTEGER NOT NULL DEFAULT (unixepoch())
                 );",
            )
            .await?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    pub async fn clear_history(&self, scope: &str) -> Result<u64> {
        let connection = self.connection.lock().await;
        connection
            .execute("DELETE FROM ai_history WHERE scope = ?1", [scope])
            .await
            .map_err(Into::into)
    }

    pub async fn get_setting(&self, key: &str) -> Result<Option<String>> {
        let connection = self.connection.lock().await;
        let statement = connection
            .prepare("SELECT value FROM settings WHERE key = ?1")
            .await?;
        let mut rows = statement.query([key]).await?;
        match rows.next().await? {
            Some(row) => Ok(Some(row.get(0)?)),
            None => Ok(None),
        }
    }

    pub async fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        let connection = self.connection.lock().await;
        connection
            .execute(
                "INSERT INTO settings(key, value, updated_at) VALUES (?1, ?2, unixepoch())
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                params![key, value],
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn settings_round_trip() {
        let temporary = tempfile::tempdir().unwrap();
        let store = Store::open(&temporary.path().join("state.db"))
            .await
            .unwrap();
        store.set_setting("quote.test", "value").await.unwrap();
        assert_eq!(
            store.get_setting("quote.test").await.unwrap().as_deref(),
            Some("value")
        );
    }
}
