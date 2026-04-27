use rusqlite::{params, Connection};
use serde::Serialize;
use std::path::Path;
use std::sync::Mutex;

const DB_FILENAME: &str = "typwrtr.sqlite";

/// Runtime row counts for every table touched by self-learning. Surfaced via
/// the `db_health` Tauri command.
#[derive(Debug, Clone, Default, Serialize)]
pub struct DbHealth {
    pub transcriptions: i64,
    pub corrections: i64,
    pub vocabulary: i64,
    pub app_profiles: i64,
    pub snippets: i64,
}

/// Owns the SQLite handle. The connection is single-threaded; calls go through
/// a `Mutex` so we can stash this in Tauri-managed state alongside other
/// recorder/transcriber singletons.
pub struct Db {
    conn: Mutex<Connection>,
}

impl Db {
    pub fn open(app_dir: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(app_dir).map_err(|e| e.to_string())?;
        let path = app_dir.join(DB_FILENAME);
        Db::open_at(&path)
    }

    pub fn open_at(path: &Path) -> Result<Self, String> {
        let conn = Connection::open(path).map_err(|e| e.to_string())?;
        // Recommended pragmas for a desktop-app-local SQLite. WAL means readers
        // never block the writer; foreign_keys is OFF by default in sqlite, but
        // we want ON DELETE CASCADE to actually fire on `corrections`.
        conn.pragma_update(None, "journal_mode", &"WAL")
            .map_err(|e| e.to_string())?;
        conn.pragma_update(None, "foreign_keys", &"ON")
            .map_err(|e| e.to_string())?;

        let db = Self {
            conn: Mutex::new(conn),
        };
        db.migrate()?;
        Ok(db)
    }

    /// PRAGMA user_version-driven migrator. Each migration runs in its own
    /// transaction. Migrations are append-only — never edit a shipped one.
    pub fn migrate(&self) -> Result<(), String> {
        let mut conn = self.conn.lock().unwrap();
        let current: i32 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(|e| e.to_string())?;

        for (target, sql) in MIGRATIONS.iter() {
            if current >= *target {
                continue;
            }
            let tx = conn.transaction().map_err(|e| e.to_string())?;
            tx.execute_batch(sql).map_err(|e| e.to_string())?;
            tx.pragma_update(None, "user_version", &target)
                .map_err(|e| e.to_string())?;
            tx.commit().map_err(|e| e.to_string())?;
            println!("[typwrtr] DB migrated to v{}", target);
        }
        Ok(())
    }

    pub fn health(&self) -> Result<DbHealth, String> {
        let conn = self.conn.lock().unwrap();
        let mut h = DbHealth::default();
        h.transcriptions = count(&conn, "transcriptions")?;
        h.corrections = count(&conn, "corrections")?;
        h.vocabulary = count(&conn, "vocabulary")?;
        h.app_profiles = count(&conn, "app_profiles")?;
        h.snippets = count(&conn, "snippets")?;
        Ok(h)
    }

    /// Wipe everything self-learning. Tombstones (Phase 2) are wiped too —
    /// "clear all" should be a true reset. The `settings` table is preserved
    /// because it may hold non-learning preferences in future migrations.
    pub fn wipe_learning_data(&self) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(
            "BEGIN;
             DELETE FROM corrections;
             DELETE FROM vocabulary;
             DELETE FROM transcriptions;
             DELETE FROM app_profiles;
             DELETE FROM snippets;
             COMMIT;",
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }
}

/// `similar`'s `TextDiff::ratio()` over characters. Returns a value in [0.0, 1.0]
/// that's stable enough for fuzzy "did this selection come from this transcription?"
/// matching at the threshold (0.6) the plan calls for.
fn count(conn: &Connection, table: &str) -> Result<i64, String> {
    // Table names cannot be parameterised, so we whitelist by matching against
    // the known set rather than interpolating arbitrary input.
    let allowed = [
        "transcriptions",
        "corrections",
        "vocabulary",
        "app_profiles",
        "snippets",
    ];
    if !allowed.contains(&table) {
        return Err(format!("unknown table: {}", table));
    }
    let sql = format!("SELECT COUNT(*) FROM {}", table);
    conn.query_row(&sql, [], |row| row.get::<_, i64>(0))
        .map_err(|e| e.to_string())
}

/// Convenience: the next migration in the sequence. `MIGRATIONS` is `(version, sql)`.
const MIGRATIONS: &[(i32, &str)] = &[
    (1, MIGRATION_1),
    (2, MIGRATION_2),
    (3, MIGRATION_3),
    (4, MIGRATION_4),
    (5, MIGRATION_5),
    (6, MIGRATION_6),
];

const MIGRATION_1: &str = r#"
CREATE TABLE transcriptions (
  id              INTEGER PRIMARY KEY AUTOINCREMENT,
  created_at      INTEGER NOT NULL,
  audio_path      TEXT,
  raw_text        TEXT NOT NULL,
  cleaned_text    TEXT NOT NULL,
  final_text      TEXT,
  app_bundle_id   TEXT,
  app_title       TEXT,
  model           TEXT NOT NULL,
  duration_ms     INTEGER NOT NULL,
  latency_ms      INTEGER NOT NULL,
  source          TEXT NOT NULL
);

CREATE TABLE corrections (
  id               INTEGER PRIMARY KEY AUTOINCREMENT,
  transcription_id INTEGER NOT NULL REFERENCES transcriptions(id) ON DELETE CASCADE,
  wrong            TEXT NOT NULL,
  right            TEXT NOT NULL,
  context          TEXT,
  app_bundle_id    TEXT,
  count            INTEGER NOT NULL DEFAULT 1,
  last_seen_at     INTEGER NOT NULL
);
CREATE INDEX corrections_wrong_idx ON corrections(wrong);
CREATE INDEX corrections_app_idx   ON corrections(app_bundle_id);

CREATE TABLE vocabulary (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  term          TEXT NOT NULL UNIQUE,
  weight        REAL NOT NULL DEFAULT 1.0,
  source        TEXT NOT NULL,
  app_bundle_id TEXT,
  created_at    INTEGER NOT NULL
);
CREATE INDEX vocabulary_app_idx ON vocabulary(app_bundle_id);

CREATE TABLE app_profiles (
  bundle_id        TEXT PRIMARY KEY,
  display_name     TEXT NOT NULL,
  prompt_template  TEXT,
  postprocess_mode TEXT NOT NULL DEFAULT 'default',
  preferred_model  TEXT,
  enabled          INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE snippets (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  trigger     TEXT NOT NULL UNIQUE,
  expansion   TEXT NOT NULL,
  is_dynamic  INTEGER NOT NULL DEFAULT 0,
  created_at  INTEGER NOT NULL
);

CREATE TABLE settings (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
"#;

/// Migration 2 — Phase 2 self-learning.
///
/// * Adds `app_profiles.auto_apply_replacements` so the user can opt out per app.
/// * Adds tombstones so "Forget this" prevents the same row from re-learning.
const MIGRATION_2: &str = r#"
ALTER TABLE app_profiles ADD COLUMN auto_apply_replacements INTEGER NOT NULL DEFAULT 1;

CREATE TABLE correction_tombstones (
  wrong         TEXT NOT NULL,
  right         TEXT NOT NULL,
  app_bundle_id TEXT NOT NULL DEFAULT '',
  created_at    INTEGER NOT NULL,
  PRIMARY KEY (wrong, right, app_bundle_id)
);

CREATE TABLE vocabulary_tombstones (
  term       TEXT NOT NULL PRIMARY KEY,
  created_at INTEGER NOT NULL
);
"#;

/// Migration 3 — Phase 4 postprocess.
///
/// Adds `app_profiles.code_case` so the `code` postprocess mode knows whether
/// to render `snake_case`, `camelCase`, or `kebab-case`. Default `snake`.
const MIGRATION_3: &str = r#"
ALTER TABLE app_profiles ADD COLUMN code_case TEXT NOT NULL DEFAULT 'snake';
"#;

/// Migration 4 — Phase 6 snippets.
///
/// Seeds the four defaults from the plan. `INSERT OR IGNORE` against the
/// `trigger` UNIQUE constraint so re-running the migration on an upgraded DB
/// won't clobber user edits, and deleted defaults won't come back.
const MIGRATION_4: &str = r#"
INSERT OR IGNORE INTO snippets (trigger, expansion, is_dynamic, created_at) VALUES
  ('insert date',              '{{date}}',                                                          1, strftime('%s','now')),
  ('insert time',              '{{time}}',                                                          1, strftime('%s','now')),
  ('insert email signature',   'Best,' || char(10) || '[your name]',                                0, strftime('%s','now')),
  ('insert standup template',  'Yesterday: ' || char(10) || 'Today: ' || char(10) || 'Blockers: ', 0, strftime('%s','now'));
"#;

/// Migration 5 — Phase 2 polish (auto-learn vs manual provenance).
///
/// Adds `corrections.source` so the Learning tab can distinguish rows that
/// came from the manual fix-up hotkey (`'manual'`) from rows the recorder
/// auto-learned by watching the focused app after paste (`'auto'`).
const MIGRATION_5: &str = r#"
ALTER TABLE corrections ADD COLUMN source TEXT NOT NULL DEFAULT 'manual';
"#;

/// Migration 6 — phonetic replacement.
///
/// Adds `app_profiles.phonetic_match`. When on (default), the recorder layers
/// a Metaphone-based fuzzy pass after the exact-match replacement, so a single
/// learned correction (e.g. `Bahb → Bob`) covers homophone variants
/// (`Baab`, `Bawb`, `Bohb` all encode the same Metaphone key).
const MIGRATION_6: &str = r#"
ALTER TABLE app_profiles ADD COLUMN phonetic_match INTEGER NOT NULL DEFAULT 1;
"#;

/// Allowed values for `app_profiles.postprocess_mode`. Validated server-side
/// before any upsert so a malformed UI payload can't poison the table.
pub const POSTPROCESS_MODES: &[&str] = &["default", "markdown", "plain", "code"];

/// Allowed values for `app_profiles.code_case`. Used by the `code` postprocess
/// mode to pick the identifier convention.
pub const CODE_CASES: &[&str] = &["snake", "camel", "kebab"];

/// One row of the most recent transcription used for fix-up matching.
#[derive(Debug, Clone, Serialize)]
pub struct RecentTranscription {
    pub id: i64,
    pub created_at: i64,
    pub cleaned_text: String,
    pub app_bundle_id: Option<String>,
}

/// One row of the corrections table, joined for display in the Learning tab.
#[derive(Debug, Clone, Serialize)]
pub struct CorrectionRow {
    pub id: i64,
    pub wrong: String,
    pub right: String,
    pub context: Option<String>,
    pub app_bundle_id: Option<String>,
    pub count: i64,
    pub last_seen_at: i64,
    /// `"manual"` (fix-up hotkey) or `"auto"` (recorder watcher). Default
    /// `"manual"` from migration 5 keeps existing rows valid.
    pub source: String,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct SnippetRow {
    /// Empty when creating; populated on read.
    #[serde(default)]
    pub id: i64,
    pub trigger: String,
    pub expansion: String,
    pub is_dynamic: bool,
    #[serde(default)]
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct VocabularyRow {
    pub id: i64,
    pub term: String,
    pub weight: f64,
    pub source: String,
    pub app_bundle_id: Option<String>,
    pub created_at: i64,
}

/// One row in the Apps tab. Joins `app_profiles` against `transcriptions` so
/// the UI shows every bundle_id we've ever seen — including those without a
/// profile row yet.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct AppProfileRow {
    pub bundle_id: String,
    pub display_name: String,
    pub prompt_template: Option<String>,
    pub postprocess_mode: String,
    pub preferred_model: Option<String>,
    pub enabled: bool,
    /// When true (default), the recorder will substitute high-confidence
    /// learned corrections into transcribed text before pasting.
    #[serde(default = "default_true")]
    pub auto_apply_replacements: bool,
    /// When true (default), phonetic-key fuzzy matching layers on top of the
    /// exact-match replacement table — one `Bahb → Bob` correction covers
    /// `Baab`, `Bawb`, `Bohb`. Off for apps where homophone collisions matter
    /// (legal names, scientific nomenclature, etc.).
    #[serde(default = "default_true")]
    pub phonetic_match: bool,
    /// Code identifier convention used by the `code` postprocess mode.
    /// One of `snake`, `camel`, `kebab`. Defaults to `snake`.
    #[serde(default = "default_code_case")]
    pub code_case: String,
    /// Unix seconds, NULL if no transcription has ever come from this app.
    pub last_used_at: Option<i64>,
    pub use_count: i64,
    /// True if a row in `app_profiles` actually exists. False means the row
    /// is synthesised from `transcriptions` and the user hasn't customised it
    /// yet.
    pub is_persisted: bool,
}

fn default_true() -> bool {
    true
}

fn default_code_case() -> String {
    "snake".to_string()
}

/// Inputs for inserting one finished transcription. Phase 0.3 fills these in
/// from the recorder; Phase 2 reads `id` to attach corrections.
pub struct NewTranscription<'a> {
    pub created_at: i64,
    pub audio_path: Option<&'a Path>,
    pub raw_text: &'a str,
    pub cleaned_text: &'a str,
    pub app_bundle_id: Option<&'a str>,
    pub app_title: Option<&'a str>,
    pub model: &'a str,
    pub duration_ms: i64,
    pub latency_ms: i64,
    pub source: &'a str,
}

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

    /// Insert or merge a correction. If a tombstone exists for the same
    /// `(wrong, right, app_bundle_id)` triple, the call is a no-op so the user's
    /// "Forget this" decision sticks across sessions. Returns the post-write
    /// `count` (1 for fresh, n+1 after merge, 0 if tombstoned).
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

        // Try to find an existing row to bump.
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
            // Update existing — guard the trigger uniqueness ourselves so the
            // user gets a friendly error instead of a SQLite constraint dump.
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_db() -> Db {
        let path = std::env::temp_dir().join(format!(
            "typwrtr_test_{}_{}.sqlite",
            std::process::id(),
            // bump per-test to avoid WAL collisions when tests run in parallel
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&path);
        Db::open_at(&path).expect("db opens")
    }

    #[test]
    fn migrate_brings_user_version_to_latest() {
        let db = fresh_db();
        let conn = db.conn.lock().unwrap();
        let v: i32 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, MIGRATIONS.last().unwrap().0);
    }

    #[test]
    fn migrate_is_idempotent() {
        let db = fresh_db();
        db.migrate().unwrap();
        db.migrate().unwrap();
        let h = db.health().unwrap();
        assert_eq!(h.transcriptions, 0);
    }

    #[test]
    fn health_counts_zero_on_fresh_db_except_seeded_snippets() {
        let db = fresh_db();
        let h = db.health().unwrap();
        assert_eq!(h.transcriptions, 0);
        assert_eq!(h.corrections, 0);
        assert_eq!(h.vocabulary, 0);
        assert_eq!(h.app_profiles, 0);
        // Migration 4 seeds the four default snippets from the plan.
        assert_eq!(h.snippets, 4);
    }

    #[test]
    fn insert_transcription_and_count() {
        let db = fresh_db();
        let id = db
            .insert_transcription(NewTranscription {
                created_at: 100,
                audio_path: None,
                raw_text: "hello world",
                cleaned_text: "Hello world.",
                app_bundle_id: Some("com.example.app"),
                app_title: Some("Editor"),
                model: "medium.en",
                duration_ms: 2500,
                latency_ms: 700,
                source: "local",
            })
            .unwrap();
        assert!(id > 0);
        assert_eq!(db.health().unwrap().transcriptions, 1);
    }

    #[test]
    fn upsert_then_get_app_profile_roundtrips() {
        let db = fresh_db();
        let p = AppProfileRow {
            bundle_id: "code".into(),
            display_name: "VS Code".into(),
            prompt_template: Some("TypeScript, Tauri, rusqlite".into()),
            postprocess_mode: "default".into(),
            preferred_model: Some("medium.en".into()),
            enabled: true,
            auto_apply_replacements: true,
            phonetic_match: true,
            code_case: "snake".into(),
            last_used_at: None,
            use_count: 0,
            is_persisted: true,
        };
        db.upsert_app_profile(&p).unwrap();
        let got = db.get_app_profile("code").unwrap().unwrap();
        assert_eq!(got.display_name, "VS Code");
        assert_eq!(
            got.prompt_template.as_deref(),
            Some("TypeScript, Tauri, rusqlite")
        );
        assert_eq!(got.preferred_model.as_deref(), Some("medium.en"));
        assert!(got.enabled);
    }

    #[test]
    fn upsert_rejects_invalid_postprocess_mode() {
        let db = fresh_db();
        let p = AppProfileRow {
            bundle_id: "code".into(),
            display_name: "VS Code".into(),
            prompt_template: None,
            postprocess_mode: "weird".into(),
            preferred_model: None,
            enabled: true,
            auto_apply_replacements: true,
            phonetic_match: true,
            code_case: "snake".into(),
            last_used_at: None,
            use_count: 0,
            is_persisted: true,
        };
        let err = db.upsert_app_profile(&p).unwrap_err();
        assert!(err.contains("invalid postprocess_mode"));
    }

    #[test]
    fn list_app_profiles_includes_unpersisted_apps_from_history() {
        let db = fresh_db();
        // No profile row for "slack", but a transcription exists.
        db.insert_transcription(NewTranscription {
            created_at: 50,
            audio_path: None,
            raw_text: "hi",
            cleaned_text: "Hi.",
            app_bundle_id: Some("slack"),
            app_title: Some("Slack"),
            model: "medium.en",
            duration_ms: 500,
            latency_ms: 200,
            source: "local",
        })
        .unwrap();
        // Persisted profile for "code" with no transcription history.
        db.upsert_app_profile(&AppProfileRow {
            bundle_id: "code".into(),
            display_name: "VS Code".into(),
            prompt_template: Some("p".into()),
            postprocess_mode: "default".into(),
            preferred_model: None,
            enabled: true,
            auto_apply_replacements: true,
            phonetic_match: true,
            code_case: "snake".into(),
            last_used_at: None,
            use_count: 0,
            is_persisted: true,
        })
        .unwrap();

        let rows = db.list_app_profiles().unwrap();
        assert_eq!(rows.len(), 2);
        // Slack has history, sorts first.
        assert_eq!(rows[0].bundle_id, "slack");
        assert_eq!(rows[0].use_count, 1);
        assert!(!rows[0].is_persisted);
        // Code has a profile but no usage; sorts to the bottom.
        assert_eq!(rows[1].bundle_id, "code");
        assert_eq!(rows[1].use_count, 0);
        assert!(rows[1].is_persisted);
    }

    #[test]
    fn delete_app_profile_keeps_history() {
        let db = fresh_db();
        db.insert_transcription(NewTranscription {
            created_at: 50,
            audio_path: None,
            raw_text: "hi",
            cleaned_text: "Hi.",
            app_bundle_id: Some("slack"),
            app_title: Some("Slack"),
            model: "medium.en",
            duration_ms: 500,
            latency_ms: 200,
            source: "local",
        })
        .unwrap();
        db.upsert_app_profile(&AppProfileRow {
            bundle_id: "slack".into(),
            display_name: "Slack".into(),
            prompt_template: Some("teamname".into()),
            postprocess_mode: "default".into(),
            preferred_model: None,
            enabled: false,
            auto_apply_replacements: true,
            phonetic_match: true,
            code_case: "snake".into(),
            last_used_at: None,
            use_count: 0,
            is_persisted: true,
        })
        .unwrap();
        db.delete_app_profile("slack").unwrap();
        // Profile gone …
        assert!(db.get_app_profile("slack").unwrap().is_none());
        // … but the transcription row is still there, listed from history.
        let rows = db.list_app_profiles().unwrap();
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].is_persisted);
    }

    #[test]
    fn migration_4_seeds_default_snippets() {
        let db = fresh_db();
        let snippets = db.list_snippets().unwrap();
        let triggers: Vec<&str> = snippets.iter().map(|s| s.trigger.as_str()).collect();
        assert!(triggers.contains(&"insert date"));
        assert!(triggers.contains(&"insert time"));
        assert!(triggers.contains(&"insert email signature"));
        assert!(triggers.contains(&"insert standup template"));
    }

    #[test]
    fn upsert_snippet_normalises_trigger_and_rejects_duplicates() {
        let db = fresh_db();
        let id = db
            .upsert_snippet(
                &SnippetRow {
                    id: 0,
                    trigger: "  Hello World  ".into(),
                    expansion: "hi there".into(),
                    is_dynamic: false,
                    created_at: 0,
                },
                100,
            )
            .unwrap();
        assert!(id > 0);
        let rows = db.list_snippets().unwrap();
        assert!(rows.iter().any(|r| r.trigger == "hello world"));

        let err = db
            .upsert_snippet(
                &SnippetRow {
                    id: 0,
                    trigger: "HELLO WORLD".into(),
                    expansion: "different expansion".into(),
                    is_dynamic: false,
                    created_at: 0,
                },
                200,
            )
            .unwrap_err();
        assert!(err.contains("already exists"));
    }

    #[test]
    fn delete_snippet_removes_row() {
        let db = fresh_db();
        // The seed includes 'insert date'; delete it and confirm it's gone.
        let id = db
            .list_snippets()
            .unwrap()
            .into_iter()
            .find(|s| s.trigger == "insert date")
            .unwrap()
            .id;
        db.delete_snippet(id).unwrap();
        assert!(!db
            .list_snippets()
            .unwrap()
            .iter()
            .any(|s| s.trigger == "insert date"));
    }

    #[test]
    fn correction_upsert_increments_count_on_repeat() {
        let db = fresh_db();
        let tid = db
            .insert_transcription(NewTranscription {
                created_at: 0,
                audio_path: None,
                raw_text: "x",
                cleaned_text: "x",
                app_bundle_id: Some("code"),
                app_title: None,
                model: "m",
                duration_ms: 0,
                latency_ms: 0,
                source: "local",
            })
            .unwrap();

        let c1 = db
            .upsert_correction(tid, "Kaidhar", "KD", Some("ctx"), Some("code"), 100, "manual")
            .unwrap();
        let c2 = db
            .upsert_correction(tid, "Kaidhar", "KD", Some("ctx"), Some("code"), 200, "manual")
            .unwrap();
        let c3 = db
            .upsert_correction(tid, "Kaidhar", "KD", Some("ctx"), Some("code"), 300, "manual")
            .unwrap();
        assert_eq!((c1, c2, c3), (1, 2, 3));
    }

    #[test]
    fn forget_correction_tombstones_and_blocks_relearning() {
        let db = fresh_db();
        let tid = db
            .insert_transcription(NewTranscription {
                created_at: 0,
                audio_path: None,
                raw_text: "x",
                cleaned_text: "x",
                app_bundle_id: Some("code"),
                app_title: None,
                model: "m",
                duration_ms: 0,
                latency_ms: 0,
                source: "local",
            })
            .unwrap();
        db.upsert_correction(tid, "rust SQL light", "rusqlite", None, Some("code"), 100, "manual")
            .unwrap();
        let row = db.top_corrections_for_app("code", 10).unwrap();
        assert_eq!(row.len(), 1);
        let cid = row[0].id;
        db.forget_correction(cid, 200).unwrap();
        // Row gone.
        assert!(db.top_corrections_for_app("code", 10).unwrap().is_empty());
        // Re-learn attempt is suppressed.
        let count_after = db
            .upsert_correction(tid, "rust SQL light", "rusqlite", None, Some("code"), 300, "manual")
            .unwrap();
        assert_eq!(count_after, 0);
        assert!(db.top_corrections_for_app("code", 10).unwrap().is_empty());
    }

    #[test]
    fn vocab_tombstone_blocks_reupsert() {
        let db = fresh_db();
        db.upsert_vocabulary("rusqlite", 1.5, "correction", Some("code"), 100)
            .unwrap();
        let id = db.top_vocab_for_app("code", 10).unwrap()[0].id;
        db.forget_vocabulary(id, 200).unwrap();
        db.upsert_vocabulary("rusqlite", 2.0, "correction", Some("code"), 300)
            .unwrap();
        assert!(db.top_vocab_for_app("code", 10).unwrap().is_empty());
    }

    #[test]
    fn find_recent_match_picks_best_above_threshold() {
        let db = fresh_db();
        for (i, text) in [
            "send the report to Kaidhar tomorrow",
            "buy milk and eggs",
            "the rust SQL light handle returns",
        ]
        .iter()
        .enumerate()
        {
            db.insert_transcription(NewTranscription {
                created_at: 100 + i as i64,
                audio_path: None,
                raw_text: text,
                cleaned_text: text,
                app_bundle_id: Some("code"),
                app_title: None,
                model: "m",
                duration_ms: 0,
                latency_ms: 0,
                source: "local",
            })
            .unwrap();
        }
        let m = db
            .find_recent_match("send the report to Kaidhar", Some("code"), 0, 20, 0.6)
            .unwrap()
            .expect("should match the first row");
        assert!(m.cleaned_text.contains("Kaidhar"));
    }

    #[test]
    fn find_recent_match_returns_none_below_threshold() {
        let db = fresh_db();
        db.insert_transcription(NewTranscription {
            created_at: 100,
            audio_path: None,
            raw_text: "the cat sat",
            cleaned_text: "the cat sat",
            app_bundle_id: Some("code"),
            app_title: None,
            model: "m",
            duration_ms: 0,
            latency_ms: 0,
            source: "local",
        })
        .unwrap();
        let m = db
            .find_recent_match("totally unrelated string", Some("code"), 0, 20, 0.6)
            .unwrap();
        assert!(m.is_none());
    }

    #[test]
    fn wipe_clears_transcriptions() {
        let db = fresh_db();
        db.insert_transcription(NewTranscription {
            created_at: 1,
            audio_path: None,
            raw_text: "a",
            cleaned_text: "A.",
            app_bundle_id: None,
            app_title: None,
            model: "m",
            duration_ms: 0,
            latency_ms: 0,
            source: "local",
        })
        .unwrap();
        assert_eq!(db.health().unwrap().transcriptions, 1);
        db.wipe_learning_data().unwrap();
        assert_eq!(db.health().unwrap().transcriptions, 0);
    }
}
