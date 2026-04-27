//! Phase 2 self-learning: turn (cleaned, final) pairs into structured
//! corrections + vocabulary signals. Pure computation lives in `diff`; DB
//! storage lives in `db::mod` alongside the rest of the schema.

pub mod diff;

use crate::db::Db;
use diff::pairs_from_diff;

pub fn similar_ratio(a: &str, b: &str) -> f64 {
    let diff = similar::TextDiff::from_chars(a, b);
    diff.ratio() as f64
}

pub fn apply_correction_pairs(
    db: &Db,
    transcription_id: i64,
    cleaned_text: &str,
    final_text: &str,
    app_bundle_id: Option<&str>,
    now_unix: i64,
    source: &str,
) -> Result<u32, String> {
    db.set_final_text(transcription_id, final_text)?;

    let pairs = pairs_from_diff(cleaned_text, final_text, 4);
    let mut applied: u32 = 0;
    for p in pairs {
        if p.wrong.is_empty() && p.right.is_empty() {
            continue;
        }
        let new_count = db.upsert_correction(
            transcription_id,
            &p.wrong,
            &p.right,
            Some(&p.context),
            app_bundle_id,
            now_unix,
            source,
        )?;
        if new_count > 0 {
            applied += 1;
            if p.is_proper_noun_candidate() {
                let weight = (1.0_f64 + new_count as f64).ln();
                db.upsert_vocabulary(&p.right, weight, "correction", app_bundle_id, now_unix)?;
            }
        }
    }

    Ok(applied)
}
