//! Snippets — CRUD over the `snippets` table. The recorder pulls this on
//! every dictation; the Snippets tab consumes the same list directly.

use rusqlite::params;

use super::{Db, SnippetRow};

impl Db {
    /// All snippets ordered by trigger A→Z. The Snippets tab consumes this
    /// directly; the recorder also pulls it on every dictation to drive
    /// inline expansion (caching is left for later if it bites).
    pub fn list_snippets(&self) -> Result<Vec<SnippetRow>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT id, trigger, expansion, is_dynamic, created_at
                 FROM snippets ORDER BY trigger COLLATE NOCASE ASC",
            )
            .map_err(|e| e.to_string())?;
        let it = stmt
            .query_map([], |row| {
                Ok(SnippetRow {
                    id: row.get(0)?,
                    trigger: row.get(1)?,
                    expansion: row.get(2)?,
                    is_dynamic: row.get::<_, i64>(3)? != 0,
                    created_at: row.get(4)?,
                })
            })
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for r in it {
            out.push(r.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    /// Insert or update a snippet. `trigger` is normalised (lowercased, trimmed)
    /// so the recorder's case-insensitive match always finds the row.
    pub fn upsert_snippet(&self, s: &SnippetRow, now_unix: i64) -> Result<i64, String> {
        let trigger = s.trigger.trim().to_lowercase();
        if trigger.is_empty() {
            return Err("Snippet trigger cannot be empty".to_string());
        }
        if s.expansion.is_empty() {
            return Err("Snippet expansion cannot be empty".to_string());
        }

        let conn = self.conn.lock().unwrap();
        if s.id > 0 {
            let conflict: Option<i64> = conn
                .query_row(
                    "SELECT id FROM snippets WHERE trigger = ?1 AND id != ?2",
                    params![trigger, s.id],
                    |row| row.get(0),
                )
                .ok();
            if conflict.is_some() {
                return Err(format!(
                    "Another snippet already uses the trigger '{}'",
                    trigger
                ));
            }
            conn.execute(
                "UPDATE snippets SET trigger = ?1, expansion = ?2, is_dynamic = ?3 WHERE id = ?4",
                params![trigger, s.expansion, if s.is_dynamic { 1 } else { 0 }, s.id],
            )
            .map_err(|e| e.to_string())?;
            Ok(s.id)
        } else {
            conn.execute(
                "INSERT INTO snippets (trigger, expansion, is_dynamic, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    trigger,
                    s.expansion,
                    if s.is_dynamic { 1 } else { 0 },
                    now_unix,
                ],
            )
            .map_err(|e| {
                if e.to_string().contains("UNIQUE") {
                    format!("A snippet with trigger '{}' already exists", trigger)
                } else {
                    e.to_string()
                }
            })?;
            Ok(conn.last_insert_rowid())
        }
    }

    pub fn delete_snippet(&self, id: i64) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM snippets WHERE id = ?1", params![id])
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}
