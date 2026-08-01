//! SQLite state: URL dedupe (analysis cache) + per-channel cooldown + /config
//! persistence (§5 Stage 8). Bundled rusqlite — no external DB service.
//!
//! `Cache` is `Send + Sync` (rusqlite `Connection` is `!Sync`, so we wrap it
//! in `Arc<Mutex<…>>`) — required for sharing behind `Arc` across tokio tasks
//! and the serenity TypeMap.

use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::{params, Connection};

use crate::error::PipelineError;
use crate::sha256_hex;

#[derive(Clone)]
pub struct Cache {
    conn: Arc<Mutex<Connection>>,
    /// DB path (kept for future re-open support); ":memory:" clones share
    /// the same in-memory connection via the Arc.
    #[allow(dead_code)]
    path: String,
}

#[derive(Debug, Clone)]
pub struct CachedAnalysis {
    pub url: String,
    pub analysis_json: String,
    pub window_used: String,
    pub bucket: String,
    pub created_at: i64,
}

impl Cache {
    pub fn open(path: &Path) -> Result<Self, PipelineError> {
        let conn =
            Connection::open(path).map_err(|e| PipelineError::CacheError(format!("open: {e}")))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS analyses (
                url_hash TEXT PRIMARY KEY,
                url TEXT NOT NULL,
                channel_id TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                analysis_json TEXT NOT NULL,
                window_used TEXT NOT NULL,
                bucket TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS cooldowns (
                channel_id TEXT PRIMARY KEY,
                last_analysis_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS config (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );",
        )
        .map_err(|e| PipelineError::CacheError(format!("schema: {e}")))?;
        Ok(Cache {
            conn: Arc::new(Mutex::new(conn)),
            path: path.to_string_lossy().to_string(),
        })
    }

    pub fn in_memory() -> Result<Self, PipelineError> {
        Self::open(Path::new(":memory:"))
    }

    // --- analyses ---

    pub fn get(&self, url: &str) -> Result<Option<CachedAnalysis>, PipelineError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| PipelineError::CacheError(e.to_string()))?;
        let hash = sha256_hex(url);
        let mut stmt = conn
            .prepare("SELECT url, analysis_json, window_used, bucket, created_at FROM analyses WHERE url_hash = ?1")
            .map_err(|e| PipelineError::CacheError(e.to_string()))?;
        let mut rows = stmt
            .query_map(params![hash], |r| {
                Ok(CachedAnalysis {
                    url: r.get(0)?,
                    analysis_json: r.get(1)?,
                    window_used: r.get(2)?,
                    bucket: r.get(3)?,
                    created_at: r.get(4)?,
                })
            })
            .map_err(|e| PipelineError::CacheError(e.to_string()))?;
        match rows.next() {
            Some(Ok(a)) => Ok(Some(a)),
            Some(Err(e)) => Err(PipelineError::CacheError(e.to_string())),
            None => Ok(None),
        }
    }

    pub fn put(
        &self,
        url: &str,
        channel_id: &str,
        analysis_json: &str,
        window_used: &str,
        bucket: &str,
        now_unix: i64,
    ) -> Result<(), PipelineError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| PipelineError::CacheError(e.to_string()))?;
        let hash = sha256_hex(url);
        conn.execute(
            "INSERT OR REPLACE INTO analyses (url_hash, url, channel_id, created_at, analysis_json, window_used, bucket)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![hash, url, channel_id, now_unix, analysis_json, window_used, bucket],
        )
        .map_err(|e| PipelineError::CacheError(e.to_string()))?;
        Ok(())
    }

    pub fn recent(&self, limit: usize) -> Result<Vec<CachedAnalysis>, PipelineError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| PipelineError::CacheError(e.to_string()))?;
        let mut stmt = conn
            .prepare("SELECT url, analysis_json, window_used, bucket, created_at FROM analyses ORDER BY created_at DESC LIMIT ?1")
            .map_err(|e| PipelineError::CacheError(e.to_string()))?;
        let rows = stmt
            .query_map(params![limit as i64], |r| {
                Ok(CachedAnalysis {
                    url: r.get(0)?,
                    analysis_json: r.get(1)?,
                    window_used: r.get(2)?,
                    bucket: r.get(3)?,
                    created_at: r.get(4)?,
                })
            })
            .map_err(|e| PipelineError::CacheError(e.to_string()))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| PipelineError::CacheError(e.to_string()))
    }

    // --- cooldowns ---

    pub fn last_analysis_at(&self, channel_id: &str) -> Result<Option<i64>, PipelineError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| PipelineError::CacheError(e.to_string()))?;
        let mut stmt = conn
            .prepare("SELECT last_analysis_at FROM cooldowns WHERE channel_id = ?1")
            .map_err(|e| PipelineError::CacheError(e.to_string()))?;
        let mut rows = stmt
            .query_map(params![channel_id], |r| r.get::<_, i64>(0))
            .map_err(|e| PipelineError::CacheError(e.to_string()))?;
        match rows.next() {
            Some(Ok(t)) => Ok(Some(t)),
            Some(Err(e)) => Err(PipelineError::CacheError(e.to_string())),
            None => Ok(None),
        }
    }

    pub fn set_last_analysis_at(&self, channel_id: &str, t: i64) -> Result<(), PipelineError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| PipelineError::CacheError(e.to_string()))?;
        conn.execute(
            "INSERT OR REPLACE INTO cooldowns (channel_id, last_analysis_at) VALUES (?1, ?2)",
            params![channel_id, t],
        )
        .map_err(|e| PipelineError::CacheError(e.to_string()))?;
        Ok(())
    }

    // --- /config persistence ---

    pub fn set_config(&self, key: &str, value: &str) -> Result<(), PipelineError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| PipelineError::CacheError(e.to_string()))?;
        conn.execute(
            "INSERT OR REPLACE INTO config (key, value) VALUES (?1, ?2)",
            params![key, value],
        )
        .map_err(|e| PipelineError::CacheError(e.to_string()))?;
        Ok(())
    }

    pub fn get_config(&self, key: &str) -> Result<Option<String>, PipelineError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| PipelineError::CacheError(e.to_string()))?;
        let mut stmt = conn
            .prepare("SELECT value FROM config WHERE key = ?1")
            .map_err(|e| PipelineError::CacheError(e.to_string()))?;
        let mut rows = stmt
            .query_map(params![key], |r| r.get::<_, String>(0))
            .map_err(|e| PipelineError::CacheError(e.to_string()))?;
        match rows.next() {
            Some(Ok(v)) => Ok(Some(v)),
            Some(Err(e)) => Err(PipelineError::CacheError(e.to_string())),
            None => Ok(None),
        }
    }

    pub fn all_config(&self) -> Result<Vec<(String, String)>, PipelineError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| PipelineError::CacheError(e.to_string()))?;
        let mut stmt = conn
            .prepare("SELECT key, value FROM config ORDER BY key")
            .map_err(|e| PipelineError::CacheError(e.to_string()))?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| PipelineError::CacheError(e.to_string()))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| PipelineError::CacheError(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache() -> Cache {
        Cache::in_memory().unwrap()
    }

    #[test]
    fn put_get_roundtrip() {
        let c = cache();
        c.put("https://a.com/1", "ch1", "{}", "30d", "default", 1000)
            .unwrap();
        let got = c.get("https://a.com/1").unwrap().unwrap();
        assert_eq!(got.analysis_json, "{}");
        assert_eq!(got.bucket, "default");
        assert_eq!(got.created_at, 1000);
    }

    #[test]
    fn get_missing_returns_none() {
        let c = cache();
        assert!(c.get("https://nope.com/x").unwrap().is_none());
    }

    #[test]
    fn cache_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Cache>();
    }

    #[test]
    fn cooldown_roundtrip() {
        let c = cache();
        assert!(c.last_analysis_at("ch1").unwrap().is_none());
        c.set_last_analysis_at("ch1", 500).unwrap();
        assert_eq!(c.last_analysis_at("ch1").unwrap(), Some(500));
        c.set_last_analysis_at("ch1", 600).unwrap();
        assert_eq!(c.last_analysis_at("ch1").unwrap(), Some(600));
    }

    #[test]
    fn config_roundtrip() {
        let c = cache();
        assert!(c.get_config("reply_mode").unwrap().is_none());
        c.set_config("reply_mode", "split").unwrap();
        assert_eq!(c.get_config("reply_mode").unwrap().unwrap(), "split");
        assert_eq!(c.all_config().unwrap().len(), 1);
    }

    #[test]
    fn recent_orders_by_time() {
        let c = cache();
        c.put("https://a.com/1", "ch", "{}", "30d", "default", 100)
            .unwrap();
        c.put("https://a.com/2", "ch", "{}", "7d", "fast", 200)
            .unwrap();
        let r = c.recent(5).unwrap();
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].url, "https://a.com/2");
    }
}
