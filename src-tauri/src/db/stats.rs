//! Aggregate queries over the `transcriptions` table for the Learning-tab
//! dashboard. Pure SQL — no new instrumentation. Word count is approximated
//! by `LENGTH(text) - LENGTH(REPLACE(text, ' ', '')) + 1` which is good
//! enough for the "you dictated 12,453 words" hero number; we don't need
//! POSIX word boundaries.

use rusqlite::params;
use serde::Serialize;

use super::Db;

/// One window-summary row. `since_unix_secs` is exclusive on the lower
/// bound so the same UI can ask for "last 30 days" without double-counting
/// rows on the boundary.
#[derive(Debug, Clone, Default, Serialize)]
pub struct UsageWindow {
    pub words: i64,
    pub dictations: i64,
    pub duration_ms_total: i64,
    pub edited: i64,
    pub avg_latency_ms: i64,
}

/// One day in the daily-bucket time series. `date` is `YYYY-MM-DD` in
/// the host's local timezone (SQLite's `date()` with the `'unixepoch'`
/// modifier renders UTC; we shift by the local offset before bucketing).
#[derive(Debug, Clone, Serialize)]
pub struct DailyBucket {
    pub date: String,
    pub words: i64,
    pub dictations: i64,
    pub edit_rate: f64,
}

/// One row of the "Where you dictate" breakdown.
#[derive(Debug, Clone, Serialize)]
pub struct AppBreakdown {
    pub bundle_id: String,
    pub display_name: String,
    pub words: i64,
    pub dictations: i64,
}

const WORD_COUNT_SQL: &str =
    "(LENGTH(TRIM(cleaned_text)) - LENGTH(REPLACE(TRIM(cleaned_text), ' ', '')) + 1)";

const EDITED_PRED: &str = "(final_text IS NOT NULL AND TRIM(final_text) != TRIM(cleaned_text))";

impl Db {
    /// Aggregate counts for transcriptions created at-or-after
    /// `since_unix_secs`. Empty `cleaned_text` rows can't reach here
    /// (`recorder.rs` short-circuits the insert) so the word-count math
    /// is safe — every counted row has at least one word.
    pub fn usage_window(&self, since_unix_secs: i64) -> Result<UsageWindow, String> {
        let conn = self.conn.lock().unwrap();
        let sql = format!(
            "SELECT
               COALESCE(SUM(CASE WHEN TRIM(cleaned_text) = '' THEN 0 ELSE {wc} END), 0) AS words,
               COUNT(*) AS dictations,
               COALESCE(SUM(duration_ms), 0) AS duration_ms_total,
               COALESCE(SUM(CASE WHEN {ed} THEN 1 ELSE 0 END), 0) AS edited,
               COALESCE(CAST(AVG(latency_ms) AS INTEGER), 0) AS avg_latency_ms
             FROM transcriptions
             WHERE created_at >= ?1",
            wc = WORD_COUNT_SQL,
            ed = EDITED_PRED,
        );
        conn.query_row(&sql, params![since_unix_secs], |row| {
            Ok(UsageWindow {
                words: row.get(0)?,
                dictations: row.get(1)?,
                duration_ms_total: row.get(2)?,
                edited: row.get(3)?,
                avg_latency_ms: row.get(4)?,
            })
        })
        .map_err(|e| e.to_string())
    }

    /// One row per local-calendar date for the last `days` days, oldest
    /// first. Days with zero dictations are emitted as a zero row so the
    /// frontend sparkline doesn't have to interpolate gaps.
    pub fn daily_buckets(&self, days: i64) -> Result<Vec<DailyBucket>, String> {
        if days <= 0 {
            return Ok(Vec::new());
        }
        let conn = self.conn.lock().unwrap();
        // Render dates in the SQLite host's local timezone via the
        // `'localtime'` modifier so users see their own calendar days.
        let sql = format!(
            "SELECT
               date(created_at, 'unixepoch', 'localtime') AS d,
               COALESCE(SUM(CASE WHEN TRIM(cleaned_text) = '' THEN 0 ELSE {wc} END), 0) AS words,
               COUNT(*) AS dictations,
               COALESCE(SUM(CASE WHEN {ed} THEN 1 ELSE 0 END), 0) AS edited
             FROM transcriptions
             WHERE created_at >= strftime('%s', 'now', ?1)
             GROUP BY d
             ORDER BY d ASC",
            wc = WORD_COUNT_SQL,
            ed = EDITED_PRED,
        );
        let since_modifier = format!("-{} days", days);
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let it = stmt
            .query_map(params![since_modifier], |row| {
                let date: String = row.get(0)?;
                let words: i64 = row.get(1)?;
                let dictations: i64 = row.get(2)?;
                let edited: i64 = row.get(3)?;
                let edit_rate = if dictations > 0 {
                    edited as f64 / dictations as f64
                } else {
                    0.0
                };
                Ok(DailyBucket {
                    date,
                    words,
                    dictations,
                    edit_rate,
                })
            })
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for r in it {
            out.push(r.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    /// Top-N apps by word count in the time window. `display_name` is the
    /// most recent `app_title` we saw for that bundle, falling back to the
    /// bundle id itself when no profile + no title exist.
    pub fn app_breakdown(
        &self,
        since_unix_secs: i64,
        limit: i64,
    ) -> Result<Vec<AppBreakdown>, String> {
        let conn = self.conn.lock().unwrap();
        let sql = format!(
            "SELECT
               t.app_bundle_id AS bundle_id,
               COALESCE(
                 (SELECT display_name FROM app_profiles WHERE bundle_id = t.app_bundle_id),
                 (SELECT app_title FROM transcriptions
                    WHERE app_bundle_id = t.app_bundle_id AND app_title IS NOT NULL
                    ORDER BY id DESC LIMIT 1),
                 t.app_bundle_id
               ) AS display_name,
               COALESCE(SUM(CASE WHEN TRIM(t.cleaned_text) = '' THEN 0 ELSE {wc} END), 0) AS words,
               COUNT(*) AS dictations
             FROM transcriptions t
             WHERE t.created_at >= ?1
               AND t.app_bundle_id IS NOT NULL
               AND t.app_bundle_id != ''
             GROUP BY t.app_bundle_id
             ORDER BY words DESC, dictations DESC
             LIMIT ?2",
            wc = WORD_COUNT_SQL.replace("cleaned_text", "t.cleaned_text"),
        );
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let it = stmt
            .query_map(params![since_unix_secs, limit], |row| {
                Ok(AppBreakdown {
                    bundle_id: row.get(0)?,
                    display_name: row.get(1)?,
                    words: row.get(2)?,
                    dictations: row.get(3)?,
                })
            })
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for r in it {
            out.push(r.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }
}
