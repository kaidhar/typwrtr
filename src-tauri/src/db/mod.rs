//! SQLite store for self-learning + per-app profiles + snippets +
//! transcriptions. The `Db` handle is single-threaded behind a `Mutex` so
//! it can ride alongside the recorder/transcriber singletons in Tauri
//! managed state.
//!
//! Code is split by domain:
//! * [`migrate`] — schema migrations (`PRAGMA user_version`).
//! * [`profiles`] — `app_profiles` rows and the Apps tab list query.
//! * [`transcriptions`] — `transcriptions` insert + fix-up fuzzy lookup.
//! * [`corrections`] — `(wrong → right)` learning + tombstones.
//! * [`vocab`] — vocabulary terms feeding whisper's `initial_prompt`.
//! * [`snippets`] — Snippets-tab CRUD.

use rusqlite::Connection;
use serde::Serialize;
use std::path::Path;
use std::sync::Mutex;

mod corrections;
mod migrate;
mod profiles;
mod snippets;
mod stats;
mod transcriptions;
mod vocab;

pub use stats::{AppBreakdown, DailyBucket, UsageWindow};

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
    pub(super) conn: Mutex<Connection>,
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

        for (target, sql) in migrate::MIGRATIONS.iter() {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrate::MIGRATIONS;

    fn fresh_db() -> Db {
        let path = std::env::temp_dir().join(format!(
            "typwrtr_test_{}_{}.sqlite",
            std::process::id(),
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
        assert_eq!(rows[0].bundle_id, "slack");
        assert_eq!(rows[0].use_count, 1);
        assert!(!rows[0].is_persisted);
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
        assert!(db.get_app_profile("slack").unwrap().is_none());
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
        assert!(db.top_corrections_for_app("code", 10).unwrap().is_empty());
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

    #[test]
    fn usage_window_counts_words_dictations_edits() {
        let db = fresh_db();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        // Two clean dictations + one edited.
        db.insert_transcription(NewTranscription {
            created_at: now,
            audio_path: None,
            raw_text: "x",
            cleaned_text: "Hello there friend.",
            app_bundle_id: Some("code"),
            app_title: Some("VS Code"),
            model: "medium.en",
            duration_ms: 2000,
            latency_ms: 800,
            source: "local",
        })
        .unwrap();
        db.insert_transcription(NewTranscription {
            created_at: now,
            audio_path: None,
            raw_text: "x",
            cleaned_text: "Two words.",
            app_bundle_id: Some("code"),
            app_title: Some("VS Code"),
            model: "medium.en",
            duration_ms: 1000,
            latency_ms: 600,
            source: "local",
        })
        .unwrap();
        let edited_id = db
            .insert_transcription(NewTranscription {
                created_at: now,
                audio_path: None,
                raw_text: "x",
                cleaned_text: "One two three four.",
                app_bundle_id: Some("slack"),
                app_title: Some("Slack"),
                model: "medium.en",
                duration_ms: 1500,
                latency_ms: 700,
                source: "local",
            })
            .unwrap();
        db.set_final_text(edited_id, "One two three.").unwrap();

        let w = db.usage_window(0).unwrap();
        // Hello/there/friend (3) + Two/words (2) + One/two/three/four (4) = 9
        assert_eq!(w.words, 9);
        assert_eq!(w.dictations, 3);
        assert_eq!(w.duration_ms_total, 4500);
        assert_eq!(w.edited, 1);
        assert_eq!(w.avg_latency_ms, 700);
    }

    #[test]
    fn daily_buckets_groups_by_date() {
        let db = fresh_db();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        // Two on the same "today", one yesterday.
        let yesterday = now - 86_400;
        db.insert_transcription(NewTranscription {
            created_at: now,
            audio_path: None,
            raw_text: "x",
            cleaned_text: "Hello world.",
            app_bundle_id: None,
            app_title: None,
            model: "m",
            duration_ms: 0,
            latency_ms: 0,
            source: "local",
        })
        .unwrap();
        db.insert_transcription(NewTranscription {
            created_at: now,
            audio_path: None,
            raw_text: "x",
            cleaned_text: "Three more words.",
            app_bundle_id: None,
            app_title: None,
            model: "m",
            duration_ms: 0,
            latency_ms: 0,
            source: "local",
        })
        .unwrap();
        db.insert_transcription(NewTranscription {
            created_at: yesterday,
            audio_path: None,
            raw_text: "x",
            cleaned_text: "Yesterday content.",
            app_bundle_id: None,
            app_title: None,
            model: "m",
            duration_ms: 0,
            latency_ms: 0,
            source: "local",
        })
        .unwrap();

        let buckets = db.daily_buckets(7).unwrap();
        // At least the two days we inserted; daylight-savings or month
        // boundary won't add extra rows since SQLite skips empty groups.
        assert!(buckets.len() >= 2, "got {:?}", buckets);
        let totals: i64 = buckets.iter().map(|b| b.dictations).sum();
        assert_eq!(totals, 3);
        let words: i64 = buckets.iter().map(|b| b.words).sum();
        // Hello/world (2) + Three/more/words (3) + Yesterday/content (2) = 7
        assert_eq!(words, 7);
    }

    #[test]
    fn app_breakdown_orders_by_word_count() {
        let db = fresh_db();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        // 5 words for code, 2 for slack.
        db.insert_transcription(NewTranscription {
            created_at: now,
            audio_path: None,
            raw_text: "x",
            cleaned_text: "Five whole words right here.",
            app_bundle_id: Some("code"),
            app_title: Some("VS Code"),
            model: "m",
            duration_ms: 0,
            latency_ms: 0,
            source: "local",
        })
        .unwrap();
        db.insert_transcription(NewTranscription {
            created_at: now,
            audio_path: None,
            raw_text: "x",
            cleaned_text: "Two words.",
            app_bundle_id: Some("slack"),
            app_title: Some("Slack"),
            model: "m",
            duration_ms: 0,
            latency_ms: 0,
            source: "local",
        })
        .unwrap();

        let rows = db.app_breakdown(0, 10).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].bundle_id, "code");
        assert_eq!(rows[0].words, 5);
        assert_eq!(rows[0].display_name, "VS Code");
        assert_eq!(rows[1].bundle_id, "slack");
        assert_eq!(rows[1].words, 2);
    }
}
