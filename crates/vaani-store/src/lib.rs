//! Dictation history in SQLite (`~/.local/share/vaani/history.db`).
//!
//! Every transcript is saved here *before* it is typed, so words are never lost.

pub mod secrets;

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, params};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("history database: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub id: i64,
    /// Unix seconds.
    pub at: i64,
    pub text: String,
    pub raw: String,
    pub engine: String,
    pub audio_ms: i64,
    pub latency_ms: i64,
    pub app: String,
    pub inserted: bool,
}

pub struct History {
    db: Connection,
}

impl History {
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let db = Connection::open(path)?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS history (
               id INTEGER PRIMARY KEY,
               at INTEGER NOT NULL,
               text TEXT NOT NULL,
               raw TEXT NOT NULL,
               engine TEXT NOT NULL,
               audio_ms INTEGER NOT NULL,
               latency_ms INTEGER NOT NULL DEFAULT 0,
               app TEXT NOT NULL DEFAULT '',
               inserted INTEGER NOT NULL DEFAULT 0
             );
             CREATE INDEX IF NOT EXISTS history_at ON history(at);
             CREATE TABLE IF NOT EXISTS totals (
               id INTEGER PRIMARY KEY CHECK (id = 1),
               dictations INTEGER NOT NULL,
               words INTEGER NOT NULL,
               audio_ms INTEGER NOT NULL
             );",
        )?;
        let h = Self { db };
        // Lifetime totals start from whatever history already holds.
        let has: i64 =
            h.db.query_row("SELECT COUNT(*) FROM totals", [], |r| r.get(0))?;
        if has == 0 {
            let mut n = 0i64;
            let mut words = 0i64;
            let mut audio = 0i64;
            {
                let mut st = h.db.prepare("SELECT text, audio_ms FROM history")?;
                for row in
                    st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
                {
                    let (t, a) = row?;
                    n += 1;
                    words += t.split_whitespace().count() as i64;
                    audio += a;
                }
            }
            h.db.execute(
                "INSERT INTO totals (id, dictations, words, audio_ms) VALUES (1, ?1, ?2, ?3)",
                params![n, words, audio],
            )?;
        }
        Ok(h)
    }

    /// Count a dictation in the lifetime totals (also when history is paused).
    pub fn count(&self, text: &str, audio_ms: i64) -> Result<(), StoreError> {
        self.db.execute(
            "UPDATE totals SET dictations = dictations + 1, words = words + ?1, audio_ms = audio_ms + ?2 WHERE id = 1",
            params![text.split_whitespace().count() as i64, audio_ms],
        )?;
        Ok(())
    }

    /// Lifetime totals: (dictations, words, seconds of speech).
    pub fn totals(&self) -> Result<(i64, i64, i64), StoreError> {
        Ok(self.db.query_row(
            "SELECT dictations, words, audio_ms / 1000 FROM totals WHERE id = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?)
    }

    /// Replace an entry's text (corrected in the History page).
    pub fn update_text(&self, id: i64, text: &str) -> Result<(), StoreError> {
        self.db.execute(
            "UPDATE history SET text = ?2 WHERE id = ?1",
            params![id, text],
        )?;
        Ok(())
    }

    pub fn open_in_memory() -> Result<Self, StoreError> {
        Self::open(Path::new(":memory:"))
    }

    pub fn add(
        &self,
        text: &str,
        raw: &str,
        engine: &str,
        audio_ms: i64,
        app: &str,
    ) -> Result<i64, StoreError> {
        let at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        self.db.execute(
            "INSERT INTO history (at, text, raw, engine, audio_ms, app) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![at, text, raw, engine, audio_ms, app],
        )?;
        Ok(self.db.last_insert_rowid())
    }

    pub fn mark_inserted(&self, id: i64, latency_ms: i64) -> Result<(), StoreError> {
        self.db.execute(
            "UPDATE history SET inserted = 1, latency_ms = ?2 WHERE id = ?1",
            params![id, latency_ms],
        )?;
        Ok(())
    }

    pub fn recent(&self, limit: usize) -> Result<Vec<Entry>, StoreError> {
        let mut st = self.db.prepare(
            "SELECT id, at, text, raw, engine, audio_ms, latency_ms, app, inserted
             FROM history ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = st.query_map(params![limit as i64], |r| {
            Ok(Entry {
                id: r.get(0)?,
                at: r.get(1)?,
                text: r.get(2)?,
                raw: r.get(3)?,
                engine: r.get(4)?,
                audio_ms: r.get(5)?,
                latency_ms: r.get(6)?,
                app: r.get(7)?,
                inserted: r.get::<_, i64>(8)? != 0,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn last(&self) -> Result<Option<Entry>, StoreError> {
        Ok(self.recent(1)?.into_iter().next())
    }

    pub fn delete(&self, id: i64) -> Result<(), StoreError> {
        self.db
            .execute("DELETE FROM history WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Delete all but the newest `keep` dictations. Returns how many were removed.
    pub fn trim(&self, keep: usize) -> Result<usize, StoreError> {
        let keep = i64::try_from(keep).unwrap_or(i64::MAX);
        let n = self.db.execute(
            "DELETE FROM history WHERE id NOT IN (SELECT id FROM history ORDER BY id DESC LIMIT ?1)",
            params![keep],
        )?;
        Ok(n)
    }

    pub fn clear(&self) -> Result<(), StoreError> {
        self.db.execute("DELETE FROM history", [])?;
        Ok(())
    }

    /// (dictations, total words) for the stats line in settings.
    pub fn stats(&self) -> Result<(i64, i64), StoreError> {
        let mut st = self.db.prepare("SELECT text FROM history")?;
        let mut n = 0;
        let mut words = 0;
        for t in st.query_map([], |r| r.get::<_, String>(0))? {
            n += 1;
            words += t?.split_whitespace().count() as i64;
        }
        Ok((n, words))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn totals_survive_trim() {
        let h = History::open_in_memory().unwrap();
        h.add("one two", "", "e", 2000, "").unwrap();
        h.count("one two", 2000).unwrap();
        h.count("three", 1000).unwrap();
        h.trim(0).unwrap();
        assert_eq!(h.totals().unwrap(), (2, 3, 3));
    }

    #[test]
    fn trim_keeps_newest() {
        let dir = tempfile::tempdir().unwrap();
        let h = History::open(&dir.path().join("h.db")).unwrap();
        for i in 0..5 {
            h.add(&format!("t{i}"), "", "e", 1, "").unwrap();
        }
        assert_eq!(h.trim(2).unwrap(), 3);
        let left: Vec<String> = h.recent(10).unwrap().into_iter().map(|e| e.text).collect();
        assert_eq!(left, vec!["t4", "t3"]);
        assert_eq!(h.trim(0).unwrap(), 2);
    }

    #[test]
    fn add_list_mark_delete() {
        let h = History::open_in_memory().unwrap();
        let a = h
            .add("Hello ", "hello", "parakeet", 1200, "firefox")
            .unwrap();
        let b = h.add("World ", "world", "parakeet", 800, "code").unwrap();
        h.mark_inserted(a, 340).unwrap();
        let r = h.recent(10).unwrap();
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].id, b);
        assert!(!r[0].inserted);
        assert!(r[1].inserted);
        assert_eq!(r[1].latency_ms, 340);
        assert_eq!(h.last().unwrap().unwrap().text, "World ");
        assert_eq!(h.stats().unwrap(), (2, 2));
        h.delete(b).unwrap();
        assert_eq!(h.recent(10).unwrap().len(), 1);
        h.clear().unwrap();
        assert!(h.last().unwrap().is_none());
    }

    #[test]
    fn file_db_persists() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("sub/history.db");
        History::open(&p)
            .unwrap()
            .add("x", "x", "e", 1, "")
            .unwrap();
        assert_eq!(History::open(&p).unwrap().recent(5).unwrap().len(), 1);
    }
}
