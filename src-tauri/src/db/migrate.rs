//! `PRAGMA user_version`-driven SQL migrations. Append-only: never edit a
//! shipped one. Callers go through [`super::Db::migrate`].

pub(super) const MIGRATIONS: &[(i32, &str)] = &[
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
