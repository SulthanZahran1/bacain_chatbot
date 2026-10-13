//! SQLite telemetry + dedupe + rolling rate-limit store
//! (goal.md §5 Stage 8, ticket #4).
//!
//! A single-file, single-connection SQLite store in WAL mode, guarded by a
//! `std::sync::Mutex` — plenty for one bot process (no async I/O, no server).
//!
//! Tables:
//!
//! - `analyses` — one row per completed analysis (telemetry). Dedupe is a TTL
//!   query over an index on `(url, created_at)`, **not** a unique constraint:
//!   a unique index on `url` would destroy telemetry history when the same URL
//!   is re-analyzed after the TTL window expires.
//! - `triggers` — per-channel trigger timestamps for rolling rate limits,
//!   retained for 24 hours.
//!
//! Judge scores (ticket #7) attach to `analyses.id` via a child table — the
//! integer primary key is the extension point, so no schema rewrite is needed.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::clock::Clock;

const TRIGGER_RETAIN_SECS: i64 = 86_400;

/// One completed analysis — the telemetry row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalysisRecord {
    /// Row id — the extension point for judge scores (ticket #7).
    /// Ignored on insert (SQLite assigns it); populated on read.
    pub id: i64,
    /// Normalized URL (dedupe key).
    pub url: String,
    pub bucket: String,
    pub window_used: String,
    pub corpus_size: usize,
    pub rounds: usize,
    pub stop_reason: String,
    pub latency_ms: u64,
    pub llm_model: String,
    pub citations_rejected: usize,
    /// Unix seconds (injectable clock — deterministic in tests).
    pub created_at: i64,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("failed to open SQLite store at {path}: {source}")]
    Open {
        path: String,
        source: rusqlite::Error,
    },
    #[error("failed to create store directory {path}: {source}")]
    Mkdir {
        path: String,
        source: std::io::Error,
    },
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

/// SQLite-backed telemetry + dedupe + rolling rate-limit store.
pub struct Store {
    conn: Mutex<Connection>,
    clock: Clock,
    cache_ttl_secs: i64,
    retention_days: u64,
}

impl Store {
    /// Open (or create) the store at `path`, applying the schema and WAL mode.
    /// Creates parent directories as needed. Prunes expired rows on open.
    pub fn open(
        path: impl AsRef<Path>,
        clock: Clock,
        cache_ttl_secs: i64,
        retention_days: u64,
    ) -> Result<Self, StoreError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|source| StoreError::Mkdir {
                    path: parent.display().to_string(),
                    source,
                })?;
            }
        }
        let conn = Connection::open(path).map_err(|source| StoreError::Open {
            path: path.display().to_string(),
            source,
        })?;
        Self::from_connection(conn, clock, cache_ttl_secs, retention_days)
    }

    /// In-memory store — tests only.
    pub fn open_in_memory(
        clock: Clock,
        cache_ttl_secs: i64,
        retention_days: u64,
    ) -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory()?;
        Self::from_connection(conn, clock, cache_ttl_secs, retention_days)
    }

    fn from_connection(
        conn: Connection,
        clock: Clock,
        cache_ttl_secs: i64,
        retention_days: u64,
    ) -> Result<Self, StoreError> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA foreign_keys=ON;",
        )?;
        // Schema migrations keyed off `user_version` (currently v1).
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version < 1 {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS analyses (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     url TEXT NOT NULL,
                     bucket TEXT NOT NULL,
                     window_used TEXT NOT NULL,
                     corpus_size INTEGER NOT NULL,
                     rounds INTEGER NOT NULL,
                     stop_reason TEXT NOT NULL,
                     latency_ms INTEGER NOT NULL,
                     llm_model TEXT NOT NULL,
                     citations_rejected INTEGER NOT NULL,
                     created_at INTEGER NOT NULL
                 );
                 CREATE INDEX IF NOT EXISTS idx_analyses_url_created
                     ON analyses(url, created_at);
                 CREATE INDEX IF NOT EXISTS idx_analyses_created_at
                     ON analyses(created_at);",
            )?;
            conn.pragma_update(None, "user_version", 1)?;
        }
        // Rate-limit history is also needed by existing v1 databases.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS triggers (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 channel_id TEXT NOT NULL,
                 at INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_triggers_channel_at
                 ON triggers(channel_id, at);",
        )?;
        let store = Self {
            conn: Mutex::new(conn),
            clock,
            cache_ttl_secs,
            retention_days,
        };
        let now = store.clock.now_unix();
        let _ = store.prune(now);
        Ok(store)
    }

    /// Persist one completed analysis. Returns the assigned row id.
    pub fn record_analysis(&self, rec: &AnalysisRecord) -> Result<i64, StoreError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO analyses
                 (url, bucket, window_used, corpus_size, rounds, stop_reason,
                  latency_ms, llm_model, citations_rejected, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                rec.url,
                rec.bucket,
                rec.window_used,
                rec.corpus_size as i64,
                rec.rounds as i64,
                rec.stop_reason,
                rec.latency_ms as i64,
                rec.llm_model,
                rec.citations_rejected as i64,
                rec.created_at,
            ],
        )?;
        let id = conn.last_insert_rowid();
        // Opportunistic retention pruning — cheap (indexed delete).
        let now = self.clock.now_unix();
        let _ = self.prune_locked(&conn, now);
        Ok(id)
    }

    /// Was `url` analyzed within the dedupe TTL? (dedupe gate)
    ///
    /// A TTL of 0 or less disables dedupe entirely — always `false`, so
    /// every post gets a fresh analysis (the bot's original behavior).
    /// Dedupe is opt-in via `DEDUPE_TTL_HOURS`.
    pub fn dedupe_hit(&self, url: &str, now: i64) -> Result<bool, StoreError> {
        if self.cache_ttl_secs <= 0 {
            return Ok(false);
        }
        let conn = self.conn.lock().unwrap();
        let cutoff = now - self.cache_ttl_secs;
        let found: Option<i64> = conn
            .query_row(
                "SELECT id FROM analyses WHERE url = ?1 AND created_at >= ?2 LIMIT 1",
                params![url, cutoff],
                |r| r.get(0),
            )
            .optional()?;
        Ok(found.is_some())
    }

    /// Number of triggers for a channel strictly newer than `since`.
    pub fn trigger_count(&self, channel_id: &str, since: i64) -> Result<i64, StoreError> {
        let conn = self.conn.lock().unwrap();
        let count = conn.query_row(
            "SELECT COUNT(*) FROM triggers WHERE channel_id = ?1 AND at > ?2",
            params![channel_id, since],
            |row| row.get(0),
        )?;
        Ok(count)
    }

    /// Record a channel trigger and prune trigger history older than 24 hours.
    pub fn trigger_record(&self, channel_id: &str, at: i64) -> Result<(), StoreError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO triggers (channel_id, at) VALUES (?1, ?2)",
            params![channel_id, at],
        )?;
        let cutoff = at - TRIGGER_RETAIN_SECS;
        conn.execute("DELETE FROM triggers WHERE at < ?1", params![cutoff])?;
        Ok(())
    }

    /// Most recent analyses, newest first.
    pub fn recent_analyses(&self, limit: usize) -> Result<Vec<AnalysisRecord>, StoreError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, url, bucket, window_used, corpus_size, rounds, stop_reason,
                    latency_ms, llm_model, citations_rejected, created_at
             FROM analyses ORDER BY created_at DESC, id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |row| {
            Ok(AnalysisRecord {
                id: row.get(0)?,
                url: row.get(1)?,
                bucket: row.get(2)?,
                window_used: row.get(3)?,
                corpus_size: row.get::<_, i64>(4)? as usize,
                rounds: row.get::<_, i64>(5)? as usize,
                stop_reason: row.get(6)?,
                latency_ms: row.get::<_, i64>(7)? as u64,
                llm_model: row.get(8)?,
                citations_rejected: row.get::<_, i64>(9)? as usize,
                created_at: row.get(10)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Delete analyses older than the retention window. Returns rows deleted.
    pub fn prune(&self, now: i64) -> Result<usize, StoreError> {
        let conn = self.conn.lock().unwrap();
        self.prune_locked(&conn, now)
    }

    fn prune_locked(&self, conn: &Connection, now: i64) -> Result<usize, StoreError> {
        let cutoff = now - (self.retention_days as i64) * 86_400;
        let n = conn.execute(
            "DELETE FROM analyses WHERE created_at < ?1",
            params![cutoff],
        )?;
        Ok(n)
    }

    /// Total analysis rows (health/status).
    pub fn count_analyses(&self) -> Result<usize, StoreError> {
        let conn = self.conn.lock().unwrap();
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM analyses", [], |r| r.get(0))?;
        Ok(n as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FakeClock;

    fn rec(url: &str, created_at: i64) -> AnalysisRecord {
        AnalysisRecord {
            id: 0,
            url: url.to_string(),
            bucket: "standard".into(),
            window_used: "30d".into(),
            corpus_size: 5,
            rounds: 2,
            stop_reason: "coverage(0.90)".into(),
            latency_ms: 1234,
            llm_model: "test-model".into(),
            citations_rejected: 0,
            created_at,
        }
    }

    fn store(now: i64) -> Store {
        Store::open_in_memory(
            std::sync::Arc::new(FakeClock::new(now)),
            24 * 3600, // 24h TTL
            30,        // 30d retention
        )
        .unwrap()
    }

    #[test]
    fn open_creates_schema_and_wal() {
        let s = store(1_000_000);
        assert_eq!(s.count_analyses().unwrap(), 0);
        let conn = s.conn.lock().unwrap();
        let triggers_table: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'triggers'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let triggers_index: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = 'idx_triggers_channel_at'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(triggers_table, 1);
        assert_eq!(triggers_index, 1);
        drop(conn);
        // WAL mode is active on a file-backed connection (in-memory DBs
        // always report "memory" — SQLite limitation).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("linkbot.db");
        let s = Store::open(&path, std::sync::Arc::new(FakeClock::new(0)), 3600, 30).unwrap();
        let conn = s.conn.lock().unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
    }

    #[test]
    fn open_existing_database_is_idempotent_and_preserves_legacy_cooldowns() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("linkbot.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE analyses (created_at INTEGER NOT NULL);
                 CREATE TABLE cooldowns (
                     channel_id TEXT PRIMARY KEY,
                     last_analyzed_at INTEGER NOT NULL
                 );
                 INSERT INTO cooldowns (channel_id, last_analyzed_at) VALUES ('legacy-channel', 42);
                 PRAGMA user_version = 1;",
            )
            .unwrap();
        }

        {
            let s = Store::open(&path, std::sync::Arc::new(FakeClock::new(0)), 3600, 30).unwrap();
            s.trigger_record("c1", 1_000_000).unwrap();
        }
        let s = Store::open(&path, std::sync::Arc::new(FakeClock::new(0)), 3600, 30).unwrap();
        assert_eq!(s.trigger_count("c1", 999_999).unwrap(), 1);
        let conn = s.conn.lock().unwrap();
        let legacy_cooldowns: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'cooldowns'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let legacy_row: i64 = conn
            .query_row(
                "SELECT last_analyzed_at FROM cooldowns WHERE channel_id = 'legacy-channel'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let triggers_index: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = 'idx_triggers_channel_at'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(legacy_cooldowns, 1);
        assert_eq!(legacy_row, 42);
        assert_eq!(triggers_index, 1);
    }

    #[test]
    fn record_and_recent_roundtrip() {
        let s = store(1_000_000);
        let id = s
            .record_analysis(&rec("https://a.com/1", 1_000_000))
            .unwrap();
        assert!(id > 0);
        let recent = s.recent_analyses(10).unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].url, "https://a.com/1");
        assert_eq!(recent[0].id, id);
        assert_eq!(recent[0].corpus_size, 5);
        assert_eq!(recent[0].latency_ms, 1234);
    }

    #[test]
    fn same_url_keeps_history_rows() {
        let s = store(1_000_000);
        s.record_analysis(&rec("https://a.com/1", 1_000_000))
            .unwrap();
        // Re-analysis after the TTL window must NOT be blocked by a unique
        // constraint — telemetry keeps one row per run.
        s.record_analysis(&rec("https://a.com/1", 1_000_000 + 48 * 3600))
            .unwrap();
        assert_eq!(s.count_analyses().unwrap(), 2);
    }

    #[test]
    fn dedupe_hit_within_ttl() {
        let s = store(1_000_000);
        s.record_analysis(&rec("https://a.com/1", 1_000_000))
            .unwrap();
        assert!(s.dedupe_hit("https://a.com/1", 1_000_000 + 3600).unwrap());
        // Exactly at the TTL boundary → still a hit (created_at >= cutoff).
        assert!(s
            .dedupe_hit("https://a.com/1", 1_000_000 + 24 * 3600)
            .unwrap());
    }

    #[test]
    fn dedupe_miss_after_ttl_and_unknown_url() {
        let s = store(1_000_000);
        s.record_analysis(&rec("https://a.com/1", 1_000_000))
            .unwrap();
        assert!(!s
            .dedupe_hit("https://a.com/1", 1_000_000 + 24 * 3600 + 1)
            .unwrap());
        assert!(!s.dedupe_hit("https://never.com/x", 1_000_000).unwrap());
    }

    #[test]
    fn dedupe_disabled_when_ttl_zero() {
        // TTL 0 = dedupe disabled: even a just-recorded URL is never a hit,
        // so every post gets a fresh analysis (the bot's original behavior).
        let s = Store::open_in_memory(
            std::sync::Arc::new(FakeClock::new(1_000_000)),
            0, // disabled
            30,
        )
        .unwrap();
        s.record_analysis(&rec("https://a.com/1", 1_000_000))
            .unwrap();
        assert!(!s.dedupe_hit("https://a.com/1", 1_000_000).unwrap());
        assert!(!s.dedupe_hit("https://a.com/1", 1_000_000 + 60).unwrap());
    }

    #[test]
    fn trigger_count_excludes_the_since_boundary() {
        let s = store(1_000_000);
        s.trigger_record("c1", 1_000_000).unwrap();
        s.trigger_record("c1", 1_000_001).unwrap();

        assert_eq!(s.trigger_count("c1", 1_000_000).unwrap(), 1);
        assert_eq!(s.trigger_count("c2", 1_000_000).unwrap(), 0);
    }

    #[test]
    fn trigger_record_inserts_and_prunes_rows_older_than_24_hours() {
        let s = store(1_000_000);
        let now = 1_000_000;
        s.trigger_record("c1", now - TRIGGER_RETAIN_SECS - 1)
            .unwrap();
        s.trigger_record("c1", now - TRIGGER_RETAIN_SECS).unwrap();
        s.trigger_record("c1", now - TRIGGER_RETAIN_SECS + 1)
            .unwrap();
        s.trigger_record("c1", now).unwrap();

        // The row older than 24h is pruned; exact-boundary, younger, and new
        // rows remain, and the latest trigger was inserted.
        assert_eq!(s.trigger_count("c1", i64::MIN).unwrap(), 3);
        assert_eq!(s.trigger_count("c1", now - 1).unwrap(), 1);
    }

    #[test]
    fn retention_prune_removes_old_keeps_new() {
        let s = store(1_000_000);
        // The old row is pruned opportunistically on the second insert
        // (record_analysis runs prune), so the end state is what matters.
        s.record_analysis(&rec("https://old.com/1", 1_000_000 - 31 * 86_400))
            .unwrap();
        s.record_analysis(&rec("https://new.com/1", 1_000_000))
            .unwrap();
        let recent = s.recent_analyses(10).unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].url, "https://new.com/1");
        // Explicit prune is idempotent — nothing left to delete.
        assert_eq!(s.prune(1_000_000).unwrap(), 0);
    }

    #[test]
    fn retention_boundary_keeps_exactly_at_cutoff() {
        let s = store(1_000_000);
        // Exactly 30 days old → cutoff is created_at >= now - 30d, so kept.
        s.record_analysis(&rec("https://edge.com/1", 1_000_000 - 30 * 86_400))
            .unwrap();
        assert_eq!(s.prune(1_000_000).unwrap(), 0);
        assert_eq!(s.count_analyses().unwrap(), 1);
    }

    #[test]
    fn recent_analyses_orders_newest_first_and_limits() {
        let s = store(1_000_000);
        for i in 0..5 {
            s.record_analysis(&rec(&format!("https://a.com/{i}"), 1_000_000 + i))
                .unwrap();
        }
        let recent = s.recent_analyses(3).unwrap();
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[0].url, "https://a.com/4");
        assert_eq!(recent[2].url, "https://a.com/2");
    }

    #[test]
    fn open_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("deep").join("linkbot.db");
        let s = Store::open(&path, std::sync::Arc::new(FakeClock::new(0)), 3600, 30).unwrap();
        assert!(path.exists());
        assert_eq!(s.count_analyses().unwrap(), 0);
    }

    #[test]
    fn open_fails_when_path_is_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        let err = Store::open(dir.path(), std::sync::Arc::new(FakeClock::new(0)), 3600, 30);
        assert!(err.is_err());
    }

    #[test]
    fn persistence_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("linkbot.db");
        {
            let s = Store::open(
                &path,
                std::sync::Arc::new(FakeClock::new(1_000_000)),
                3600,
                30,
            )
            .unwrap();
            s.record_analysis(&rec("https://persist.com/1", 1_000_000))
                .unwrap();
            s.trigger_record("c9", 1_000_000).unwrap();
        }
        // Reopen: data survives (this is the whole point of the ticket).
        let s = Store::open(
            &path,
            std::sync::Arc::new(FakeClock::new(1_000_000)),
            3600,
            30,
        )
        .unwrap();
        assert_eq!(s.count_analyses().unwrap(), 1);
        assert_eq!(
            s.recent_analyses(10).unwrap()[0].url,
            "https://persist.com/1"
        );
        assert_eq!(s.trigger_count("c9", 999_999).unwrap(), 1);
    }

    #[test]
    fn prune_runs_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("linkbot.db");
        {
            let s = Store::open(
                &path,
                std::sync::Arc::new(FakeClock::new(1_000_000)),
                3600,
                30,
            )
            .unwrap();
            s.record_analysis(&rec("https://old.com/1", 1_000_000 - 40 * 86_400))
                .unwrap();
            s.record_analysis(&rec("https://new.com/1", 1_000_000))
                .unwrap();
        }
        let s = Store::open(
            &path,
            std::sync::Arc::new(FakeClock::new(1_000_000)),
            3600,
            30,
        )
        .unwrap();
        assert_eq!(s.count_analyses().unwrap(), 1);
        assert_eq!(s.recent_analyses(10).unwrap()[0].url, "https://new.com/1");
    }
}
