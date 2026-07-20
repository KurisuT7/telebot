use std::path::Path;

use anyhow::{Context, Result};
use libsql::{Connection, params};
use tokio::sync::Mutex;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AiHistoryEntry {
    pub role: String,
    pub content: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuoteHistorySummary {
    pub id: i64,
    pub output: String,
    pub preview: String,
    pub byte_len: usize,
    pub created_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuoteHistoryEntry {
    pub id: i64,
    pub output: String,
    pub preview: String,
    pub media: Vec<u8>,
    pub created_at: i64,
}

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
                 );
                 CREATE TABLE IF NOT EXISTS quote_history (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     output TEXT NOT NULL CHECK(output IN ('sticker', 'image', 'stories')),
                     preview TEXT NOT NULL,
                     media BLOB NOT NULL,
                     created_at INTEGER NOT NULL DEFAULT (unixepoch())
                 );
                 CREATE INDEX IF NOT EXISTS quote_history_id_desc ON quote_history(id DESC);",
            )
            .await?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    pub async fn ai_history(&self, scope: &str, max_items: usize) -> Result<Vec<AiHistoryEntry>> {
        if max_items == 0 {
            return Ok(Vec::new());
        }
        let connection = self.connection.lock().await;
        let statement = connection
            .prepare(
                "SELECT role, content FROM (
                     SELECT id, role, content FROM ai_history
                     WHERE scope = ?1 ORDER BY id DESC LIMIT ?2
                 ) ORDER BY id ASC",
            )
            .await?;
        let mut rows = statement.query(params![scope, max_items as i64]).await?;
        let mut entries = Vec::new();
        while let Some(row) = rows.next().await? {
            entries.push(AiHistoryEntry {
                role: row.get(0)?,
                content: row.get(1)?,
            });
        }
        Ok(entries)
    }

    pub async fn append_ai_turn(
        &self,
        scope: &str,
        user: &str,
        assistant: &str,
        max_turns: usize,
    ) -> Result<()> {
        if max_turns == 0 {
            return Ok(());
        }
        let connection = self.connection.lock().await;
        connection
            .execute(
                "INSERT INTO ai_history(scope, role, content) VALUES (?1, 'user', ?2)",
                params![scope, user],
            )
            .await?;
        connection
            .execute(
                "INSERT INTO ai_history(scope, role, content) VALUES (?1, 'assistant', ?2)",
                params![scope, assistant],
            )
            .await?;
        connection
            .execute(
                "DELETE FROM ai_history
                 WHERE scope = ?1 AND id NOT IN (
                     SELECT id FROM ai_history WHERE scope = ?1 ORDER BY id DESC LIMIT ?2
                 )",
                params![scope, max_turns.saturating_mul(2) as i64],
            )
            .await?;
        Ok(())
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

    pub async fn delete_setting(&self, key: &str) -> Result<u64> {
        let connection = self.connection.lock().await;
        connection
            .execute("DELETE FROM settings WHERE key = ?1", [key])
            .await
            .map_err(Into::into)
    }

    pub async fn delete_settings_prefix(&self, prefix: &str) -> Result<u64> {
        let connection = self.connection.lock().await;
        connection
            .execute(
                "DELETE FROM settings WHERE substr(key, 1, length(?1)) = ?1",
                [prefix],
            )
            .await
            .map_err(Into::into)
    }

    pub async fn add_quote_history(
        &self,
        output: &str,
        preview: &str,
        media: &[u8],
        max_items: usize,
        max_bytes: usize,
    ) -> Result<i64> {
        let connection = self.connection.lock().await;
        connection
            .execute(
                "INSERT INTO quote_history(output, preview, media) VALUES (?1, ?2, ?3)",
                params![output, preview, media.to_vec()],
            )
            .await?;
        let statement = connection.prepare("SELECT last_insert_rowid()").await?;
        let mut rows = statement.query(()).await?;
        let id = rows
            .next()
            .await?
            .context("missing quote history id")?
            .get(0)?;

        connection
            .execute(
                "DELETE FROM quote_history WHERE id IN (
                     SELECT id FROM quote_history ORDER BY id DESC LIMIT -1 OFFSET ?1
                 )",
                [max_items as i64],
            )
            .await?;

        loop {
            let statement = connection
                .prepare("SELECT COALESCE(SUM(length(media)), 0) FROM quote_history")
                .await?;
            let mut rows = statement.query(()).await?;
            let total: i64 = rows
                .next()
                .await?
                .context("missing quote history size")?
                .get(0)?;
            if total <= max_bytes as i64 {
                break;
            }
            let deleted = connection
                .execute(
                    "DELETE FROM quote_history WHERE id = (
                         SELECT id FROM quote_history ORDER BY id ASC LIMIT 1
                     )",
                    (),
                )
                .await?;
            if deleted == 0 {
                break;
            }
        }
        Ok(id)
    }

    pub async fn quote_history(&self, limit: usize) -> Result<Vec<QuoteHistorySummary>> {
        let connection = self.connection.lock().await;
        let statement = connection
            .prepare(
                "SELECT id, output, preview, length(media), created_at
                 FROM quote_history ORDER BY id DESC LIMIT ?1",
            )
            .await?;
        let mut rows = statement.query([limit as i64]).await?;
        let mut entries = Vec::new();
        while let Some(row) = rows.next().await? {
            let byte_len: i64 = row.get(3)?;
            entries.push(QuoteHistorySummary {
                id: row.get(0)?,
                output: row.get(1)?,
                preview: row.get(2)?,
                byte_len: byte_len.max(0) as usize,
                created_at: row.get(4)?,
            });
        }
        Ok(entries)
    }

    pub async fn quote_history_entry(&self, id: i64) -> Result<Option<QuoteHistoryEntry>> {
        let connection = self.connection.lock().await;
        let statement = connection
            .prepare(
                "SELECT id, output, preview, media, created_at
                 FROM quote_history WHERE id = ?1",
            )
            .await?;
        let mut rows = statement.query([id]).await?;
        match rows.next().await? {
            Some(row) => Ok(Some(QuoteHistoryEntry {
                id: row.get(0)?,
                output: row.get(1)?,
                preview: row.get(2)?,
                media: row.get(3)?,
                created_at: row.get(4)?,
            })),
            None => Ok(None),
        }
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
        store.delete_settings_prefix("quote.").await.unwrap();
        assert_eq!(store.get_setting("quote.test").await.unwrap(), None);
    }

    #[tokio::test]
    async fn ai_history_is_ordered_and_pruned_by_turn() {
        let temporary = tempfile::tempdir().unwrap();
        let store = Store::open(&temporary.path().join("state.db"))
            .await
            .unwrap();
        store.append_ai_turn("chat", "u1", "a1", 1).await.unwrap();
        store.append_ai_turn("chat", "u2", "a2", 1).await.unwrap();
        assert_eq!(
            store.ai_history("chat", 10).await.unwrap(),
            vec![
                AiHistoryEntry {
                    role: "user".to_owned(),
                    content: "u2".to_owned(),
                },
                AiHistoryEntry {
                    role: "assistant".to_owned(),
                    content: "a2".to_owned(),
                },
            ]
        );
    }

    #[tokio::test]
    async fn quote_history_round_trip_and_count_limit() {
        let temporary = tempfile::tempdir().unwrap();
        let store = Store::open(&temporary.path().join("state.db"))
            .await
            .unwrap();
        let first = store
            .add_quote_history("sticker", "first", b"one", 1, 1024)
            .await
            .unwrap();
        let second = store
            .add_quote_history("image", "second", b"two", 1, 1024)
            .await
            .unwrap();
        assert!(store.quote_history_entry(first).await.unwrap().is_none());
        let entry = store.quote_history_entry(second).await.unwrap().unwrap();
        assert_eq!(entry.output, "image");
        assert_eq!(entry.media, b"two");
        assert_eq!(store.quote_history(10).await.unwrap().len(), 1);
    }
}
