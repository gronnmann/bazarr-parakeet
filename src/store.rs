use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;

pub struct Store(Connection);
impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL;
            CREATE TABLE IF NOT EXISTS jobs (
                id INTEGER PRIMARY KEY, cache_key TEXT NOT NULL, status TEXT NOT NULL,
                error TEXT, created_at TEXT DEFAULT CURRENT_TIMESTAMP, updated_at TEXT DEFAULT CURRENT_TIMESTAMP);
            CREATE TABLE IF NOT EXISTS cache (
                cache_key TEXT PRIMARY KEY, response TEXT NOT NULL, created_at TEXT DEFAULT CURRENT_TIMESTAMP);
            UPDATE jobs SET status='interrupted', error='service restarted', updated_at=CURRENT_TIMESTAMP
                WHERE status='running';")?;
        Ok(Self(conn))
    }
    pub fn cached(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .0
            .query_row("SELECT response FROM cache WHERE cache_key=?", [key], |r| {
                r.get(0)
            })
            .optional()?)
    }
    pub fn start(&self, key: &str) -> Result<i64> {
        self.0.execute(
            "INSERT INTO jobs(cache_key,status) VALUES (?,'running')",
            [key],
        )?;
        Ok(self.0.last_insert_rowid())
    }
    pub fn finish(
        &mut self,
        id: i64,
        key: &str,
        result: &Result<String>,
        limit: usize,
    ) -> Result<()> {
        let tx = self.0.transaction()?;
        match result {
            Ok(response) => {
                tx.execute(
                    "UPDATE jobs SET status='completed', updated_at=CURRENT_TIMESTAMP WHERE id=?",
                    [id],
                )?;
                if limit > 0 {
                    tx.execute(
                        "INSERT OR REPLACE INTO cache(cache_key,response) VALUES (?,?)",
                        params![key, response],
                    )?;
                }
            }
            Err(error) => {
                tx.execute("UPDATE jobs SET status='failed', error=?, updated_at=CURRENT_TIMESTAMP WHERE id=?",
                    params![format!("{error:#}"), id])?;
            }
        }
        tx.execute("DELETE FROM cache WHERE rowid NOT IN (SELECT rowid FROM cache ORDER BY rowid DESC LIMIT ?)", [limit as i64])?;
        tx.execute(
            "DELETE FROM jobs WHERE id NOT IN (SELECT id FROM jobs ORDER BY id DESC LIMIT 1000)",
            [],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn jobs(&self) -> Result<serde_json::Value> {
        let mut stmt = self.0.prepare(
            "SELECT id,status,error,created_at,updated_at FROM jobs ORDER BY id DESC LIMIT 50",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(serde_json::json!({
                    "id": r.get::<_,i64>(0)?, "status": r.get::<_,String>(1)?,
                    "error": r.get::<_,Option<String>>(2)?, "created_at": r.get::<_,String>(3)?,
                    "updated_at": r.get::<_,String>(4)?
                }))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(serde_json::json!(rows))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retains_completed_results_marks_interrupted_jobs_and_bounds_cache() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("jobs.sqlite");
        {
            let mut store = Store::open(&path).unwrap();
            for key in ["first", "second", "third"] {
                let id = store.start(key).unwrap();
                store
                    .finish(id, key, &Ok(format!("subtitle-{key}")), 2)
                    .unwrap();
            }
            store.start("unfinished").unwrap();
        }
        let store = Store::open(&path).unwrap();
        assert_eq!(store.cached("first").unwrap(), None);
        assert_eq!(store.cached("third").unwrap().unwrap(), "subtitle-third");
        let jobs = store.jobs().unwrap();
        assert_eq!(jobs[0]["status"], "interrupted");
        assert_eq!(jobs[1]["status"], "completed");
    }
}
