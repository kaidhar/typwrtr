//! Corrections table — `(wrong → right)` pairs plus tombstones. Drives the
//! Phase-2 self-learning replacement table and the Learning tab dashboard.

use rusqlite::params;

use super::{CorrectionRow, Db};

impl Db {
    /// Insert or merge a correction. If a tombstone exists for the same
    /// `(wrong, right, app_bundle_id)` triple, the call is a no-op so the
    /// user's "Forget this" decision sticks across sessions. Returns the
    /// post-write `count` (1 for fresh, n+1 after merge, 0 if tombstoned).
    pub fn upsert_correction(
        &self,
        transcription_id: i64,
        wrong: &str,
        right: &str,
        context: Option<&str>,
        app_bundle_id: Option<&str>,
        now_unix: i64,
        source: &str,
    ) -> Result<i64, String> {
        let conn = self.conn.lock().unwrap();
        let bundle_for_tombstone = app_bundle_id.unwrap_or("");
        let tombstoned: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM correction_tombstones
                 WHERE wrong = ?1 AND right = ?2 AND app_bundle_id = ?3",
                params![wrong, right, bundle_for_tombstone],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;
        if tombstoned > 0 {
            return Ok(0);
        }

        let existing: Option<(i64, i64)> = conn
            .query_row(
                "SELECT id, count FROM corrections
                 WHERE wrong = ?1 AND right = ?2
                   AND COALESCE(app_bundle_id, '') = COALESCE(?3, '')",
                params![wrong, right, app_bundle_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .ok();

        match existing {
            Some((id, count)) => {
                let new_count = count + 1;
                conn.execute(
                    "UPDATE corrections SET count = ?1, last_seen_at = ?2 WHERE id = ?3",
                    params![new_count, now_unix, id],
                )
                .map_err(|e| e.to_string())?;
                Ok(new_count)
            }
            None => {
                conn.execute(
                    "INSERT INTO corrections
                       (transcription_id, wrong, right, context, app_bundle_id, count, last_seen_at, source)
                     VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?7)",
                    params![
                        transcription_id,
                        wrong,
                        right,
                        context,
                        app_bundle_id,
                        now_unix,
                        source,
                    ],
                )
                .map_err(|e| e.to_string())?;
                Ok(1)
            }
        }
    }

    /// Top corrections by count for one app. Used by both the recorder (for
    /// prompt biasing + replacement table) and the Learning tab.
    pub fn top_corrections_for_app(
        &self,
        app_bundle_id: &str,
        limit: i64,
    ) -> Result<Vec<CorrectionRow>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT id, wrong, right, context, app_bundle_id, count, last_seen_at, source
                 FROM corrections
                 WHERE app_bundle_id = ?1
                 ORDER BY count DESC, last_seen_at DESC
                 LIMIT ?2",
            )
            .map_err(|e| e.to_string())?;
        let it = stmt
            .query_map(params![app_bundle_id, limit], |row| {
                Ok(CorrectionRow {
                    id: row.get(0)?,
                    wrong: row.get(1)?,
                    right: row.get(2)?,
                    context: row.get(3)?,
                    app_bundle_id: row.get(4)?,
                    count: row.get(5)?,
                    last_seen_at: row.get(6)?,
                    source: row.get(7)?,
                })
            })
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for r in it {
            out.push(r.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    /// Top corrections across all apps. Used by the Learning tab dashboard.
    pub fn top_corrections_global(&self, limit: i64) -> Result<Vec<CorrectionRow>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT id, wrong, right, context, app_bundle_id, count, last_seen_at, source
                 FROM corrections
                 ORDER BY count DESC, last_seen_at DESC
                 LIMIT ?1",
            )
            .map_err(|e| e.to_string())?;
        let it = stmt
            .query_map(params![limit], |row| {
                Ok(CorrectionRow {
                    id: row.get(0)?,
                    wrong: row.get(1)?,
                    right: row.get(2)?,
                    context: row.get(3)?,
                    app_bundle_id: row.get(4)?,
                    count: row.get(5)?,
                    last_seen_at: row.get(6)?,
                    source: row.get(7)?,
                })
            })
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for r in it {
            out.push(r.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    /// Forget a correction row: tombstone the (wrong, right, bundle) triple
    /// and drop the row. Future occurrences of the same delta won't re-learn.
    pub fn forget_correction(&self, id: i64, now_unix: i64) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        let row: Option<(String, String, Option<String>)> = conn
            .query_row(
                "SELECT wrong, right, app_bundle_id FROM corrections WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .ok();
        let Some((wrong, right, bundle)) = row else {
            return Ok(());
        };
        let bundle_for_tombstone = bundle.as_deref().unwrap_or("");
        conn.execute(
            "INSERT OR IGNORE INTO correction_tombstones
               (wrong, right, app_bundle_id, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![wrong, right, bundle_for_tombstone, now_unix],
        )
        .map_err(|e| e.to_string())?;
        conn.execute("DELETE FROM corrections WHERE id = ?1", params![id])
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}
