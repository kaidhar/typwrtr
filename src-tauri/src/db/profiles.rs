//! Per-app profile rows. Lists every bundle_id ever observed (via
//! `app_profiles` joined against `transcriptions`), and the upsert / delete /
//! get operations the Apps tab and recorder hot path call.

use rusqlite::params;

use super::{AppProfileRow, Db, CODE_CASES, POSTPROCESS_MODES};

impl Db {
    /// Returns one row per bundle_id ever observed (in `transcriptions` or
    /// `app_profiles`), enriched with profile customisations and recency.
    /// Sorted most-recently-used first; apps with no transcription history
    /// (i.e. a profile created with no usage) sort to the bottom alphabetically.
    pub fn list_app_profiles(&self) -> Result<Vec<AppProfileRow>, String> {
        let conn = self.conn.lock().unwrap();
        let sql = r#"
            WITH all_apps AS (
                SELECT DISTINCT app_bundle_id AS bundle_id
                FROM transcriptions
                WHERE app_bundle_id IS NOT NULL AND app_bundle_id != ''
                UNION
                SELECT bundle_id FROM app_profiles
            )
            SELECT
                a.bundle_id,
                COALESCE(
                    p.display_name,
                    (SELECT app_title FROM transcriptions
                       WHERE app_bundle_id = a.bundle_id AND app_title IS NOT NULL
                       ORDER BY id DESC LIMIT 1),
                    a.bundle_id
                ) AS display_name,
                p.prompt_template,
                COALESCE(p.postprocess_mode, 'default') AS postprocess_mode,
                p.preferred_model,
                COALESCE(p.enabled, 1) AS enabled,
                COALESCE(p.auto_apply_replacements, 1) AS auto_apply_replacements,
                COALESCE(p.phonetic_match, 1) AS phonetic_match,
                COALESCE(p.code_case, 'snake') AS code_case,
                (SELECT MAX(created_at) FROM transcriptions WHERE app_bundle_id = a.bundle_id) AS last_used_at,
                (SELECT COUNT(*)        FROM transcriptions WHERE app_bundle_id = a.bundle_id) AS use_count,
                CASE WHEN p.bundle_id IS NULL THEN 0 ELSE 1 END AS is_persisted
            FROM all_apps a
            LEFT JOIN app_profiles p ON p.bundle_id = a.bundle_id
            ORDER BY (last_used_at IS NULL) ASC, last_used_at DESC, a.bundle_id ASC
        "#;

        let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| {
                Ok(AppProfileRow {
                    bundle_id: row.get(0)?,
                    display_name: row.get(1)?,
                    prompt_template: row.get(2)?,
                    postprocess_mode: row.get(3)?,
                    preferred_model: row.get(4)?,
                    enabled: row.get::<_, i64>(5)? != 0,
                    auto_apply_replacements: row.get::<_, i64>(6)? != 0,
                    phonetic_match: row.get::<_, i64>(7)? != 0,
                    code_case: row.get(8)?,
                    last_used_at: row.get(9)?,
                    use_count: row.get(10)?,
                    is_persisted: row.get::<_, i64>(11)? != 0,
                })
            })
            .map_err(|e| e.to_string())?;

        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    /// INSERT-OR-REPLACE one profile row. The caller must have validated
    /// `postprocess_mode` against `POSTPROCESS_MODES`; we double-check here so
    /// the table can't be corrupted from any path.
    pub fn upsert_app_profile(&self, p: &AppProfileRow) -> Result<(), String> {
        if !POSTPROCESS_MODES.contains(&p.postprocess_mode.as_str()) {
            return Err(format!(
                "invalid postprocess_mode '{}'; expected one of {:?}",
                p.postprocess_mode, POSTPROCESS_MODES
            ));
        }
        if !CODE_CASES.contains(&p.code_case.as_str()) {
            return Err(format!(
                "invalid code_case '{}'; expected one of {:?}",
                p.code_case, CODE_CASES
            ));
        }
        if p.bundle_id.trim().is_empty() {
            return Err("bundle_id is required".to_string());
        }

        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO app_profiles
                 (bundle_id, display_name, prompt_template, postprocess_mode,
                  preferred_model, enabled, auto_apply_replacements, phonetic_match, code_case)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(bundle_id) DO UPDATE SET
                 display_name = excluded.display_name,
                 prompt_template = excluded.prompt_template,
                 postprocess_mode = excluded.postprocess_mode,
                 preferred_model = excluded.preferred_model,
                 enabled = excluded.enabled,
                 auto_apply_replacements = excluded.auto_apply_replacements,
                 phonetic_match = excluded.phonetic_match,
                 code_case = excluded.code_case",
            params![
                p.bundle_id,
                p.display_name,
                p.prompt_template,
                p.postprocess_mode,
                p.preferred_model,
                if p.enabled { 1 } else { 0 },
                if p.auto_apply_replacements { 1 } else { 0 },
                if p.phonetic_match { 1 } else { 0 },
                p.code_case,
            ],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Drop the profile row for `bundle_id`. Past transcriptions for the same
    /// app stay; only the customisation goes away.
    pub fn delete_app_profile(&self, bundle_id: &str) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM app_profiles WHERE bundle_id = ?1",
            params![bundle_id],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Resolve the profile for the current foreground app, or `None` if none
    /// is persisted. Hot path — called once per dictation.
    pub fn get_app_profile(&self, bundle_id: &str) -> Result<Option<AppProfileRow>, String> {
        let conn = self.conn.lock().unwrap();
        let sql = r#"
            SELECT bundle_id, display_name, prompt_template,
                   postprocess_mode, preferred_model, enabled,
                   auto_apply_replacements, phonetic_match, code_case
            FROM app_profiles WHERE bundle_id = ?1
        "#;
        let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
        let mut iter = stmt
            .query_map(params![bundle_id], |row| {
                Ok(AppProfileRow {
                    bundle_id: row.get(0)?,
                    display_name: row.get(1)?,
                    prompt_template: row.get(2)?,
                    postprocess_mode: row.get(3)?,
                    preferred_model: row.get(4)?,
                    enabled: row.get::<_, i64>(5)? != 0,
                    auto_apply_replacements: row.get::<_, i64>(6)? != 0,
                    phonetic_match: row.get::<_, i64>(7)? != 0,
                    code_case: row.get(8)?,
                    last_used_at: None,
                    use_count: 0,
                    is_persisted: true,
                })
            })
            .map_err(|e| e.to_string())?;
        match iter.next() {
            Some(r) => Ok(Some(r.map_err(|e| e.to_string())?)),
            None => Ok(None),
        }
    }
}
