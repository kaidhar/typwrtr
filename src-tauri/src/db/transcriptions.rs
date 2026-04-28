//! Transcription rows. Hot-path `insert_transcription` plus the fix-up
//! flow's `find_recent_match` (fuzzy ratio against `cleaned_text`) and
//! `set_final_text` finaliser.

use rusqlite::params;

use super::{Db, NewTranscription, RecentTranscription};

impl Db {
    pub fn insert_transcription(&self, t: NewTranscription) -> Result<i64, String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO transcriptions
             (created_at, audio_path, raw_text, cleaned_text, app_bundle_id,
              app_title, model, duration_ms, latency_ms, source)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                t.created_at,
                t.audio_path.and_then(|p| p.to_str()),
                t.raw_text,
                t.cleaned_text,
                t.app_bundle_id,
                t.app_title,
                t.model,
                t.duration_ms,
                t.latency_ms,
                t.source,
            ],
        )
        .map_err(|e| e.to_string())?;
        Ok(conn.last_insert_rowid())
    }

    /// Find the most recent transcription whose `cleaned_text` plausibly matches
    /// the user's selected text. Used by the fix-up flow.
    ///
    /// Filters: created within `since_unix_secs`, optionally restricted to
    /// `app_bundle_id`. Ratio threshold is applied in Rust because SQLite has
    /// no Levenshtein UDF available; the candidate set is bounded by `LIMIT
    /// candidate_limit` (most recent first), then we keep the best match
    /// above `min_ratio`.
    pub fn find_recent_match(
        &self,
        selection: &str,
        app_bundle_id: Option<&str>,
        since_unix_secs: i64,
        candidate_limit: i64,
        min_ratio: f64,
    ) -> Result<Option<RecentTranscription>, String> {
        let conn = self.conn.lock().unwrap();
        let (sql, candidates): (&str, Vec<RecentTranscription>) = match app_bundle_id {
            Some(_) => (
                "SELECT id, created_at, cleaned_text, app_bundle_id
                 FROM transcriptions
                 WHERE created_at >= ?1
                   AND app_bundle_id = ?2
                 ORDER BY id DESC LIMIT ?3",
                Vec::new(),
            ),
            None => (
                "SELECT id, created_at, cleaned_text, app_bundle_id
                 FROM transcriptions
                 WHERE created_at >= ?1
                 ORDER BY id DESC LIMIT ?2",
                Vec::new(),
            ),
        };
        let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
        let rows: Vec<RecentTranscription> = match app_bundle_id {
            Some(bundle) => {
                let it = stmt
                    .query_map(params![since_unix_secs, bundle, candidate_limit], |row| {
                        Ok(RecentTranscription {
                            id: row.get(0)?,
                            created_at: row.get(1)?,
                            cleaned_text: row.get(2)?,
                            app_bundle_id: row.get(3)?,
                        })
                    })
                    .map_err(|e| e.to_string())?;
                let mut out = candidates;
                for r in it {
                    out.push(r.map_err(|e| e.to_string())?);
                }
                out
            }
            None => {
                let it = stmt
                    .query_map(params![since_unix_secs, candidate_limit], |row| {
                        Ok(RecentTranscription {
                            id: row.get(0)?,
                            created_at: row.get(1)?,
                            cleaned_text: row.get(2)?,
                            app_bundle_id: row.get(3)?,
                        })
                    })
                    .map_err(|e| e.to_string())?;
                let mut out = candidates;
                for r in it {
                    out.push(r.map_err(|e| e.to_string())?);
                }
                out
            }
        };

        let mut best: Option<(f64, RecentTranscription)> = None;
        for cand in rows {
            let ratio = crate::learning::similar_ratio(selection, &cand.cleaned_text);
            if ratio >= min_ratio {
                if best.as_ref().map(|(r, _)| ratio > *r).unwrap_or(true) {
                    best = Some((ratio, cand));
                }
            }
        }
        Ok(best.map(|(_, t)| t))
    }

    /// Set `final_text` on a transcription. Used by the fix-up save handler.
    pub fn set_final_text(&self, id: i64, final_text: &str) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE transcriptions SET final_text = ?1 WHERE id = ?2",
            params![final_text, id],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }
}
