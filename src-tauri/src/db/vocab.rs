//! Vocabulary terms — promoted proper-nouns and right-side tokens. Drives
//! whisper's `initial_prompt` biasing per-app and globally. Tombstoned via
//! the Forget action so a forgotten term cannot re-learn.

use rusqlite::params;

use super::{Db, VocabularyRow};

impl Db {
    /// Upsert a vocabulary term with the given count-derived weight. Tombstone
    /// suppresses re-learning a forgotten term.
    pub fn upsert_vocabulary(
        &self,
        term: &str,
        weight: f64,
        source: &str,
        app_bundle_id: Option<&str>,
        now_unix: i64,
    ) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        let tombstoned: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM vocabulary_tombstones WHERE term = ?1",
                params![term],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;
        if tombstoned > 0 {
            return Ok(());
        }

        conn.execute(
            "INSERT INTO vocabulary (term, weight, source, app_bundle_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(term) DO UPDATE SET
               weight = MAX(vocabulary.weight, excluded.weight),
               app_bundle_id = COALESCE(vocabulary.app_bundle_id, excluded.app_bundle_id),
               source = vocabulary.source",
            params![term, weight, source, app_bundle_id, now_unix],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Top vocabulary terms scoped to one app, ordered by weight descending.
    pub fn top_vocab_for_app(
        &self,
        app_bundle_id: &str,
        limit: i64,
    ) -> Result<Vec<VocabularyRow>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT id, term, weight, source, app_bundle_id, created_at
                 FROM vocabulary
                 WHERE app_bundle_id = ?1
                 ORDER BY weight DESC, term ASC
                 LIMIT ?2",
            )
            .map_err(|e| e.to_string())?;
        let it = stmt
            .query_map(params![app_bundle_id, limit], |row| {
                Ok(VocabularyRow {
                    id: row.get(0)?,
                    term: row.get(1)?,
                    weight: row.get(2)?,
                    source: row.get(3)?,
                    app_bundle_id: row.get(4)?,
                    created_at: row.get(5)?,
                })
            })
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for r in it {
            out.push(r.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    /// Top global vocabulary terms (those not scoped to any app).
    pub fn top_vocab_global(&self, limit: i64) -> Result<Vec<VocabularyRow>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT id, term, weight, source, app_bundle_id, created_at
                 FROM vocabulary
                 WHERE app_bundle_id IS NULL
                 ORDER BY weight DESC, term ASC
                 LIMIT ?1",
            )
            .map_err(|e| e.to_string())?;
        let it = stmt
            .query_map(params![limit], |row| {
                Ok(VocabularyRow {
                    id: row.get(0)?,
                    term: row.get(1)?,
                    weight: row.get(2)?,
                    source: row.get(3)?,
                    app_bundle_id: row.get(4)?,
                    created_at: row.get(5)?,
                })
            })
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for r in it {
            out.push(r.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    /// Top vocabulary terms across both app-scoped and global rows. Used by the
    /// Learning tab.
    pub fn top_vocab_combined(&self, limit: i64) -> Result<Vec<VocabularyRow>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT id, term, weight, source, app_bundle_id, created_at
                 FROM vocabulary
                 ORDER BY weight DESC, term ASC
                 LIMIT ?1",
            )
            .map_err(|e| e.to_string())?;
        let it = stmt
            .query_map(params![limit], |row| {
                Ok(VocabularyRow {
                    id: row.get(0)?,
                    term: row.get(1)?,
                    weight: row.get(2)?,
                    source: row.get(3)?,
                    app_bundle_id: row.get(4)?,
                    created_at: row.get(5)?,
                })
            })
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for r in it {
            out.push(r.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    /// Forget a vocabulary term: tombstone it and drop the row.
    pub fn forget_vocabulary(&self, id: i64, now_unix: i64) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        let term: Option<String> = conn
            .query_row(
                "SELECT term FROM vocabulary WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .ok();
        let Some(term) = term else {
            return Ok(());
        };
        conn.execute(
            "INSERT OR IGNORE INTO vocabulary_tombstones (term, created_at) VALUES (?1, ?2)",
            params![term, now_unix],
        )
        .map_err(|e| e.to_string())?;
        conn.execute("DELETE FROM vocabulary WHERE id = ?1", params![id])
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}
